//! Plan limits and usage, as the manager records and serves them
//! (integrations spec, section 1).

use calin_proto::{LimitWindow, RunnerLimits, UsageBy};

use super::orchestration::play;
use super::*;
use crate::clock::{ManualClock, SharedClock};

const T0: i64 = 1_800_000_000_000;

fn window(used: f64, refused: bool) -> LimitWindow {
    LimitWindow {
        name: "five_hour".to_owned(),
        used,
        resets_at: Some(T0 + 3_600_000),
        refused,
    }
}

#[tokio::test]
async fn limits_are_logged_at_most_once_a_minute_unless_they_move() {
    let clock = Arc::new(ManualClock::new(T0));
    let config = ManagerConfig {
        clock: SharedClock::new(Arc::clone(&clock)),
        automations: false,
        ..config()
    };
    let dir = TempDir::new().unwrap();
    let (manager, mut starts) = manager_with(&dir, false, config).await;
    let id = manager
        .create_session(request("work", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let played = play(&mut starts).await;
    // Each report is followed by a marker, and the clock moves only once
    // the driver has passed it: the driver reads the clock when it handles a report.
    let mut seen = 0;
    let mut report = async |used: f64, refused: bool| {
        emit(
            &played.io,
            RunnerOutput::Limits(vec![window(used, refused)]),
        )
        .await;
        let marker = EventBody::PlanUpdated { entries: vec![] };
        emit(&played.io, RunnerOutput::Event(marker)).await;
        let until = |event: &Event| matches!(event.body, EventBody::PlanUpdated { .. });
        seen = events_until(&manager, &id, seen, until)
            .await
            .last()
            .unwrap()
            .seq;
    };
    // Logged: the first.
    report(0.40, false).await;
    clock.advance(1_000);
    // Not logged: half a point, within the minute.
    report(0.405, false).await;
    clock.advance(1_000);
    // Logged: two points.
    report(0.42, false).await;
    clock.advance(1_000);
    // Logged: refusing now.
    report(0.42, true).await;
    clock.advance(30_000);
    // Not logged: nothing moved within the minute.
    report(0.42, true).await;
    clock.advance(30_000);
    // Logged: a minute since the last one.
    report(0.42, true).await;
    emit(&played.io, RunnerOutput::TurnEnded).await;

    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    let lead = manager.get(&id).await.unwrap().agents[0].id.clone();
    let logged: Vec<(f64, bool)> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::Limits { windows } => {
                // The agent's, caused by the input of its turn.
                assert_eq!(event.agent_id.as_deref(), Some(lead.as_str()));
                assert_eq!(event.parent_id.as_ref(), Some(&events[1].id));
                Some((windows[0].used, windows[0].refused))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        logged,
        [(0.40, false), (0.42, false), (0.42, true), (0.42, true)]
    );
    // The newest report, logged or not, is what the runner's limits are.
    assert_eq!(
        manager.limits(),
        [RunnerLimits {
            runner: "puppet".to_owned(),
            windows: vec![window(0.42, true)],
            reported_at: T0 + 63_000,
        }]
    );
}

#[tokio::test]
async fn a_dry_run_reports_usage_and_limits() {
    let dir = TempDir::new().unwrap();
    let config = ManagerConfig {
        time_zone: TimeZone::UTC,
        ..config()
    };
    let (manager, _starts) = manager_with(&dir, true, config).await;
    assert_eq!(manager.limits(), []);
    for prompt in ["one", "two"] {
        let id = manager
            .create_session(request(prompt, None))
            .await
            .unwrap()
            .session
            .id;
        events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    }
    let usage = manager.usage(None, None, UsageBy::Day).await.unwrap();
    assert_eq!(usage.totals.sessions, 2);
    assert_eq!(
        (usage.totals.tokens_in, usage.totals.tokens_out),
        (2400, 700)
    );
    assert!((usage.totals.cost_usd - 0.025).abs() < 1e-9);
    assert_eq!(usage.to - usage.from, 30 * 24 * 60 * 60 * 1000);
    // Thirty days back from now touch 31 dates, unless now is midnight.
    assert!(
        (30..=31).contains(&usage.rows.len()),
        "{}",
        usage.rows.len()
    );
    let last = usage.rows.last().unwrap();
    assert_eq!(last.totals.sessions, 2);
    let by_runner = manager.usage(None, None, UsageBy::Runner).await.unwrap();
    assert_eq!(by_runner.rows.len(), 1);
    assert_eq!(by_runner.rows[0].key, "mock");

    let limits = manager.limits();
    assert_eq!(limits.len(), 1);
    assert_eq!(limits[0].runner, "mock");
    assert_eq!(limits[0].windows[0].name, "five_hour");

    let backwards = manager.usage(Some(10), Some(5), UsageBy::Day).await;
    assert!(matches!(backwards, Err(Error::Invalid(_))), "{backwards:?}");
}
