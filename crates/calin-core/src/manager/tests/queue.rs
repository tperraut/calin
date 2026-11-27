//! The session queue (the "queue" semantics in `calin-proto/src/api.rs`):
//! how many root sessions run at once, who starts next, and what gives a
//! slot back. Agents are the puppet runner's, so a turn lasts until the test
//! ends it.

use calin_agents::RunnerInput;
use calin_proto::{PermissionDecision, Settings};

use super::orchestration::{Played, exit_on_stop, input, nothing_for, play, status};
use super::*;
use crate::manager::driver::WAITING_FOR_SLOT;

/// A manager that lets `limit` root sessions run at once.
async fn limited(limit: u32) -> Fixture {
    let fixture = fixture().await;
    set_limit(&fixture.manager, limit).await;
    fixture
}

async fn set_limit(manager: &SessionManager, limit: u32) {
    let settings = Settings {
        max_parallel: Some(limit),
        ..Settings::default()
    };
    let stored = manager.update_settings(settings).await.unwrap();
    assert_eq!(stored.max_parallel, Some(limit));
}

async fn create(manager: &SessionManager, prompt: &str) -> String {
    // In `ask` mode, so that a test can make a session wait on the user.
    let wanted = request_in(PermissionMode::Ask, prompt, Some("puppet"));
    let created = manager.create_session(wanted);
    created.await.unwrap().session.id
}

/// Creates a session that finds no slot.
async fn create_queued(manager: &SessionManager, prompt: &str) -> String {
    let id = create(manager, prompt).await;
    let events = manager.store().subscribe(&id, 0).await.unwrap().backlog;
    assert_eq!(
        events[0].body,
        EventBody::SessionStatus {
            status: SessionStatus::Queued,
            reason: Some(WAITING_FOR_SLOT.to_owned()),
        }
    );
    assert_eq!(labels(&events[1..]), [format!("user_message:{prompt}")]);
    id
}

fn nobody_started(starts: &mut Starts) {
    assert!(starts.try_recv().is_err(), "an agent was started");
}

async fn end_turn(manager: &SessionManager, played: &Played) {
    emit(&played.io, RunnerOutput::TurnEnded).await;
    events_until(manager, &played.id, 0, is_status(SessionStatus::Review)).await;
}

async fn statuses(manager: &SessionManager, ids: &[&String]) -> Vec<SessionStatus> {
    let mut all = Vec::new();
    for id in ids {
        all.push(status(manager, id).await);
    }
    all
}

use SessionStatus::{Cancelled, Queued, Review, Running, Waiting};

#[tokio::test]
async fn the_limit_holds_and_a_freed_slot_starts_the_oldest_waiting_session() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = limited(2).await;
    let a = create(&manager, "a").await;
    let b = create(&manager, "b").await;
    let played_a = play(&mut starts).await;
    let played_b = play(&mut starts).await;
    assert_eq!(
        (played_a.id.as_str(), played_b.id.as_str()),
        (a.as_str(), b.as_str())
    );
    let c = create_queued(&manager, "c").await;
    let d = create_queued(&manager, "d").await;
    let e = create_queued(&manager, "e").await;
    nobody_started(&mut starts);
    let all = [&a, &b, &c, &d, &e];
    assert_eq!(
        statuses(&manager, &all).await,
        [Running, Running, Queued, Queued, Queued]
    );

    // A session that reaches `review` holds no slot: the oldest one starts.
    end_turn(&manager, &played_a).await;
    let mut played_c = play(&mut starts).await;
    assert_eq!(played_c.id, c);
    assert_eq!(played_c.params.prompt, "c");
    nobody_started(&mut starts);
    assert_eq!(
        statuses(&manager, &all).await,
        [Review, Running, Running, Queued, Queued]
    );
    let events = events_until(&manager, &c, 0, |event| label(event) == "agent_spawned").await;
    assert_eq!(
        labels(&events),
        [
            "session_status:queued",
            "user_message:c",
            "session_status:running",
            "agent_spawned"
        ]
    );

    // Neither does one that waits on the user.
    emit(&played_b.io, ask("p1", None)).await;
    let played_d = play(&mut starts).await;
    assert_eq!(played_d.id, d);
    assert_eq!(
        statuses(&manager, &all).await,
        [Review, Waiting, Running, Running, Queued]
    );

    // Nor one that was stopped.
    let stopping = manager.stop(&c);
    let (stopped, ()) = tokio::join!(stopping, exit_on_stop(&mut played_c));
    stopped.unwrap();
    let played_e = play(&mut starts).await;
    assert_eq!(played_e.id, e);
    assert_eq!(
        statuses(&manager, &all).await,
        [Review, Waiting, Cancelled, Running, Running]
    );
    drop((played_d, played_e));
}

#[tokio::test]
async fn a_queued_session_can_be_stopped_and_never_starts() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = limited(1).await;
    let a = create(&manager, "a").await;
    let played_a = play(&mut starts).await;
    let b = create_queued(&manager, "b").await;
    let c = create_queued(&manager, "c").await;

    manager.stop(&b).await.unwrap();
    let events = manager.store().subscribe(&b, 0).await.unwrap().backlog;
    assert_eq!(
        labels(&events),
        [
            "session_status:queued",
            "user_message:b",
            "session_status:cancelled"
        ]
    );
    let again = manager.stop(&b).await;
    assert!(matches!(again, Err(Error::Conflict(_))), "{again:?}");
    // There is no turn to interrupt in the queue.
    let none = manager.interrupt(&c).await;
    assert!(matches!(none, Err(Error::Conflict(_))), "{none:?}");

    // The slot goes past the cancelled session to the next one.
    end_turn(&manager, &played_a).await;
    assert_eq!(play(&mut starts).await.id, c);
    nobody_started(&mut starts);
    assert_eq!(
        statuses(&manager, &[&a, &b, &c]).await,
        [Review, Cancelled, Running]
    );
}

#[tokio::test]
async fn raising_the_limit_starts_waiting_sessions_at_once_and_lowering_it_stops_none() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = limited(1).await;
    let a = create(&manager, "a").await;
    let played_a = play(&mut starts).await;
    let b = create_queued(&manager, "b").await;
    let c = create_queued(&manager, "c").await;
    let d = create_queued(&manager, "d").await;

    set_limit(&manager, 3).await;
    let first = play(&mut starts).await;
    let second = play(&mut starts).await;
    let mut started = [first.id.clone(), second.id.clone()];
    started.sort();
    assert_eq!(started, [b.clone(), c.clone()]);
    nobody_started(&mut starts);
    let all = [&a, &b, &c, &d];
    assert_eq!(
        statuses(&manager, &all).await,
        [Running, Running, Running, Queued]
    );

    set_limit(&manager, 1).await;
    assert_eq!(
        statuses(&manager, &all).await,
        [Running, Running, Running, Queued]
    );
    // Three run where one may: all of them must end before the next starts.
    end_turn(&manager, &played_a).await;
    end_turn(&manager, &first).await;
    nobody_started(&mut starts);
    end_turn(&manager, &second).await;
    assert_eq!(play(&mut starts).await.id, d);

    // Out of range is refused, and nothing changes.
    for limit in [0, 65] {
        let settings = Settings {
            max_parallel: Some(limit),
            ..Settings::default()
        };
        let refused = manager.update_settings(settings).await;
        assert!(matches!(refused, Err(Error::Invalid(_))), "{limit}");
    }
    assert_eq!(manager.settings().await.max_parallel, Some(1));
}

#[tokio::test]
async fn child_sessions_run_outside_the_queue() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = limited(1).await;
    manager.set_tool_endpoint("http://127.0.0.1:4680/api/v1/mcp");
    let root = create(&manager, "root").await;
    let lead = play(&mut starts).await;
    let waiting = create_queued(&manager, "another root").await;

    // The limit is reached, and the children start all the same.
    let mut children = Vec::new();
    for title in ["one", "two", "three"] {
        let dispatched = manager.agent_dispatch(&lead.agent, title, "Do it.", None);
        let dispatched = within(dispatched).await.unwrap();
        let child = play(&mut starts).await;
        assert_eq!(child.id, dispatched.session.id);
        assert_eq!(status(&manager, &child.id).await, Running);
        children.push(child);
    }
    assert_eq!(
        statuses(&manager, &[&root, &waiting]).await,
        [Running, Queued]
    );

    // A child that ends frees nothing for a root session.
    emit(&children[0].io, RunnerOutput::TurnEnded).await;
    events_until(&manager, &children[0].id, 0, is_status(Review)).await;
    nobody_started(&mut starts);
    assert_eq!(status(&manager, &waiting).await, Queued);
}

#[tokio::test]
async fn a_session_that_runs_again_takes_a_slot_or_waits_for_one() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = limited(1).await;
    let a = create(&manager, "a").await;
    let mut played_a = play(&mut starts).await;
    end_turn(&manager, &played_a).await;
    let b = create(&manager, "b").await;
    let mut played_b = play(&mut starts).await;

    // From `review`, with the slot taken: queued, and the agent is handed
    // nothing until the session may run.
    let head = manager.get(&a).await.unwrap().session.head_seq;
    manager.send_message(&a, "one more thing").await.unwrap();
    let events = events_until(&manager, &a, head, is_status(Queued)).await;
    assert_eq!(
        labels(&events),
        ["user_message:one more thing", "session_status:queued"]
    );
    assert_eq!(
        events[1].body,
        EventBody::SessionStatus {
            status: Queued,
            reason: Some(WAITING_FOR_SLOT.to_owned()),
        }
    );
    nothing_for(&mut played_a);

    // `b` waits on the user: its slot is `a`'s now.
    emit(&played_b.io, ask("p1", None)).await;
    assert_eq!(input(&mut played_a).await, "one more thing");
    assert_eq!(statuses(&manager, &[&a, &b]).await, [Running, Waiting]);

    // From `waiting`, with the slot taken: the answer is on record, the
    // session is queued, and the decision waits with it.
    let head = manager.get(&b).await.unwrap().session.head_seq;
    manager
        .resolve_permission(&b, "p1", PermissionDecision::Allow, None)
        .await
        .unwrap();
    let events = events_until(&manager, &b, head, is_status(Queued)).await;
    assert_eq!(
        labels(&events),
        ["permission_resolved:allow:user", "session_status:queued"]
    );
    nothing_for(&mut played_b);
    let again = manager
        .resolve_permission(&b, "p1", PermissionDecision::Allow, None)
        .await;
    assert!(matches!(again, Err(Error::Conflict(_))), "{again:?}");

    end_turn(&manager, &played_a).await;
    let decided = within(played_b.io.inputs.recv()).await;
    assert!(
        matches!(
            &decided,
            Some(RunnerInput::PermissionDecision { request_id, decision, .. })
                if request_id == "p1" && *decision == PermissionDecision::Allow
        ),
        "{decided:?}"
    );
    events_until(&manager, &b, head, is_status(Running)).await;
    assert_eq!(statuses(&manager, &[&a, &b]).await, [Review, Running]);

    // A turn that waits for a slot can still be interrupted.
    emit(&played_b.io, ask("p2", None)).await;
    events_until(&manager, &b, head, is_status(Waiting)).await;
    manager.send_message(&a, "and another").await.unwrap();
    assert_eq!(input(&mut played_a).await, "and another");
    manager
        .resolve_permission(&b, "p2", PermissionDecision::Allow, None)
        .await
        .unwrap();
    assert_eq!(status(&manager, &b).await, Queued);
    manager.interrupt(&b).await.unwrap();
    assert_eq!(
        within(played_b.io.inputs.recv()).await,
        Some(RunnerInput::Interrupt)
    );
    emit(&played_b.io, RunnerOutput::TurnEnded).await;
    events_until(&manager, &b, head, is_status(Review)).await;
    assert_eq!(statuses(&manager, &[&a, &b]).await, [Running, Review]);
}

#[tokio::test]
async fn sessions_queued_before_a_restart_are_failed_like_the_ones_that_ran() {
    let dir = TempDir::new().unwrap();
    let (manager, mut starts) = manager_in(&dir, false).await;
    set_limit(&manager, 1).await;
    let a = create(&manager, "a").await;
    let played = play(&mut starts).await;
    let b = create_queued(&manager, "b").await;
    manager.store().close().await;
    drop((manager, played, starts));

    // The limit is a setting: it is still 1. The queue is not resumed.
    let (manager, mut starts) = manager_in(&dir, false).await;
    assert_eq!(manager.settings().await.max_parallel, Some(1));
    assert_eq!(
        statuses(&manager, &[&a, &b]).await,
        [SessionStatus::Failed; 2]
    );
    nobody_started(&mut starts);
    // Continuing one starts it, and its prompt goes with the message.
    manager.send_message(&b, "now").await.unwrap();
    assert_eq!(play(&mut starts).await.params.prompt, "b\n\nnow");
}
