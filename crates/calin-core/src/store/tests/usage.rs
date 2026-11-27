//! The usage projection and `GET /usage` (integrations spec, section 1).
//! Logs are written by hand here, so that every event has the time the
//! test needs, and indexed the way a restart indexes them.

use calin_proto::{IssueRef, UsageBy, UsageRow, UsageTotals};
use jiff::civil::DateTime;

use super::*;
use crate::clock::TimeZone;
use crate::store::index::usage_delta;
use crate::store::usage::MAX_SESSION_ROWS;

/// Five hours behind UTC: a day here starts at 05:00 UTC.
fn zone() -> TimeZone {
    TimeZone::fixed(jiff::tz::offset(-5))
}

/// A local time of [`zone`], in Unix milliseconds.
fn local(text: &str) -> i64 {
    let local: DateTime = text.parse().unwrap();
    zone().to_timestamp(local).unwrap().as_millisecond()
}

fn spawned(agent_id: &str, session_id: &str, runner: &str, model: Option<&str>) -> EventBody {
    EventBody::AgentSpawned {
        agent: Agent {
            runner: runner.to_owned(),
            model: model.map(str::to_owned),
            ..agent(agent_id, session_id)
        },
    }
}

fn attached(session_id: &str, repo_id: &str, primary: bool) -> EventBody {
    EventBody::RepoAttached {
        repo: SessionRepo {
            id: format!("{session_id}-{repo_id}"),
            session_id: session_id.to_owned(),
            repo_id: repo_id.to_owned(),
            name: format!("{repo_id}-name"),
            mode: RepoMode::Write,
            path: format!("/work/{repo_id}"),
            branch: Some("calin/x".to_owned()),
            base_ref: Some("abc".to_owned()),
            primary,
        },
    }
}

fn totals(cost_usd: f64, tokens_in: u64, tokens_out: u64) -> EventBody {
    EventBody::Usage {
        cost_usd,
        tokens_in,
        tokens_out,
    }
}

/// Appends `lines` to a session's empty log as the writer would have:
/// each event at the time given, by the agent given.
fn write_log(home: &Home, session_id: &str, lines: Vec<(i64, Option<&str>, EventBody)>) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(home.log_path(session_id))
        .unwrap();
    for (index, (ts, agent_id, body)) in lines.into_iter().enumerate() {
        let event = Event {
            id: format!("{session_id}-{index:04}"),
            session_id: session_id.to_owned(),
            seq: index as u64 + 1,
            ts,
            agent_id: agent_id.map(str::to_owned),
            parent_id: None,
            body,
        };
        let mut line = serde_json::to_vec(&event).unwrap();
        line.push(b'\n');
        file.write_all(&line).unwrap();
    }
}

fn query(from: &str, to: &str, by: UsageBy) -> UsageQuery {
    UsageQuery {
        from: local(from),
        to: local(to),
        by,
        zone: zone(),
    }
}

fn row(key: &str, label: &str, cost_usd: f64, tokens: (u64, u64), sessions: u32) -> UsageRow {
    UsageRow {
        key: key.to_owned(),
        label: label.to_owned(),
        totals: UsageTotals {
            cost_usd,
            tokens_in: tokens.0,
            tokens_out: tokens.1,
            sessions,
        },
    }
}

/// Three root sessions and a child: `a` on repo `api`, its child `b` on
/// `web`, `c` without a repo, and `d`, which spent long before the range.
struct Spent {
    _dir: TempDir,
    store: Store,
    a: String,
    c: String,
}

async fn spent() -> Spent {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let a = store.create_session(new_session("a")).await.unwrap().id;
    let child = NewSession {
        dispatch: Some(Dispatch {
            parent_session_id: a.clone(),
            root_session_id: a.clone(),
            agent_id: "a1".to_owned(),
            lead_agent_id: "b1".to_owned(),
            message_id: "m1".to_owned(),
            start_points: Default::default(),
        }),
        ..new_session("b")
    };
    let b = store.create_session(child).await.unwrap().id;
    let c = store.create_session(new_session("c")).await.unwrap().id;
    let d = store.create_session(new_session("d")).await.unwrap().id;
    store.close().await;

    let home = Home::new(dir.path());
    let start = local("2026-11-10T08:00:00");
    write_log(
        &home,
        &a,
        vec![
            (start, None, spawned("a1", &a, "claude", Some("opus"))),
            (start, None, attached(&a, "api", true)),
            (start, None, attached(&a, "docs", false)),
            // Late on the 10th here, already the 11th in UTC.
            (
                local("2026-11-10T23:30:00"),
                Some("a1"),
                totals(1.0, 100, 10),
            ),
            (
                local("2026-11-11T00:30:00"),
                Some("a1"),
                totals(1.5, 150, 15),
            ),
            // Nothing new: no usage.
            (
                local("2026-11-11T01:00:00"),
                Some("a1"),
                totals(1.5, 150, 15),
            ),
            // The process started over without resuming: from zero again.
            (
                local("2026-11-11T10:00:00"),
                Some("a1"),
                totals(0.25, 20, 2),
            ),
            (
                local("2026-11-11T11:00:00"),
                Some("a1"),
                totals(0.375, 30, 3),
            ),
            // One total went down: a restart too, never negative usage.
            (local("2026-11-12T09:00:00"), Some("a1"), totals(0.5, 25, 4)),
        ],
    );
    write_log(
        &home,
        &b,
        vec![
            (start, None, spawned("b1", &b, "claude", Some("sonnet"))),
            (start, None, attached(&b, "web", true)),
            (
                local("2026-11-12T12:00:00"),
                Some("b1"),
                totals(2.0, 200, 20),
            ),
        ],
    );
    write_log(
        &home,
        &c,
        vec![
            (start, None, spawned("c1", &c, "mock", None)),
            (
                local("2026-11-12T13:00:00"),
                Some("c1"),
                totals(0.25, 40, 4),
            ),
            // At the end of the range, which it does not include.
            (
                local("2026-11-13T00:00:00"),
                Some("c1"),
                totals(0.75, 80, 8),
            ),
        ],
    );
    write_log(
        &home,
        &d,
        vec![
            (
                local("2026-10-01T08:00:00"),
                None,
                spawned("d1", &d, "claude", Some("opus")),
            ),
            (
                local("2026-10-01T09:00:00"),
                Some("d1"),
                totals(9.0, 900, 90),
            ),
        ],
    );
    let store = open(&dir).await;
    Spent {
        _dir: dir,
        store,
        a,
        c,
    }
}

const FROM: &str = "2026-11-10T00:00:00";
const TO: &str = "2026-11-13T00:00:00";

#[test]
fn a_delta_is_what_the_totals_grew_by_and_a_decrease_is_a_restart() {
    assert_eq!(usage_delta((1.0, 100, 10), (1.5, 150, 15)), (0.5, 50, 5));
    assert_eq!(usage_delta((1.0, 100, 10), (1.0, 100, 10)), (0.0, 0, 0));
    // From zero: everything is new.
    assert_eq!(usage_delta((0.0, 0, 0), (0.25, 20, 2)), (0.25, 20, 2));
    // Any total that went down means the process started over.
    assert_eq!(usage_delta((1.5, 150, 15), (0.25, 20, 2)), (0.25, 20, 2));
    assert_eq!(usage_delta((0.375, 30, 3), (0.5, 25, 4)), (0.5, 25, 4));
    assert_eq!(usage_delta((0.375, 30, 3), (0.5, 31, 2)), (0.5, 31, 2));
    assert_eq!(usage_delta((0.375, 30, 3), (0.25, 31, 4)), (0.25, 31, 4));
    // A cost rounded a hair below is not a restart.
    let (cost, tokens_in, _) = usage_delta((0.3, 10, 1), (0.3 - 1e-12, 12, 1));
    assert_eq!((cost, tokens_in), (0.0, 2));
}

#[tokio::test]
async fn usage_by_day_has_every_local_day_of_the_range() {
    let spent = spent().await;
    let usage = spent
        .store
        .usage(query(FROM, "2026-11-16T00:00:00", UsageBy::Day))
        .await
        .unwrap();
    assert_eq!(usage.from, local(FROM));
    assert_eq!(usage.by, UsageBy::Day);
    assert_eq!(
        usage.rows,
        [
            row("2026-11-10", "2026-11-10", 1.0, (100, 10), 1),
            row("2026-11-11", "2026-11-11", 0.875, (80, 8), 1),
            // The child's usage counts for its root, `a`.
            row("2026-11-12", "2026-11-12", 2.75, (265, 28), 2),
            // `c`'s report at midnight on the 13th is inside this longer range.
            row("2026-11-13", "2026-11-13", 0.5, (40, 4), 1),
            row("2026-11-14", "2026-11-14", 0.0, (0, 0), 0),
            row("2026-11-15", "2026-11-15", 0.0, (0, 0), 0),
        ]
    );
    // `to` is not part of the range.
    let usage = spent
        .store
        .usage(query(FROM, TO, UsageBy::Day))
        .await
        .unwrap();
    assert_eq!(usage.rows.len(), 3);
    assert_eq!(
        usage.totals,
        UsageTotals {
            cost_usd: 4.625,
            tokens_in: 445,
            tokens_out: 46,
            sessions: 2,
        }
    );
    // In UTC the same events fall on other days.
    let mut utc = query(FROM, TO, UsageBy::Day);
    utc.zone = TimeZone::UTC;
    let rows = spent.store.usage(utc).await.unwrap().rows;
    let days: Vec<(&str, f64)> = rows
        .iter()
        .map(|r| (r.key.as_str(), r.totals.cost_usd))
        .collect();
    assert_eq!(
        days,
        [
            ("2026-11-10", 0.0),
            ("2026-11-11", 1.875),
            ("2026-11-12", 2.75),
            ("2026-11-13", 0.0)
        ]
    );
}

#[tokio::test]
async fn usage_by_session_repo_runner_and_model() {
    let spent = spent().await;
    let by = |by| spent.store.usage(query(FROM, TO, by));
    assert_eq!(
        by(UsageBy::Session).await.unwrap().rows,
        [
            row(&spent.a, "title of a", 4.375, (405, 42), 1),
            row(&spent.c, "title of c", 0.25, (40, 4), 1),
        ]
    );
    assert_eq!(
        by(UsageBy::Repo).await.unwrap().rows,
        [
            row("api", "api-name", 2.375, (205, 22), 1),
            row("web", "web-name", 2.0, (200, 20), 1),
            row("", "No repo", 0.25, (40, 4), 1),
        ]
    );
    assert_eq!(
        by(UsageBy::Runner).await.unwrap().rows,
        [
            row("claude", "claude", 4.375, (405, 42), 1),
            row("mock", "mock", 0.25, (40, 4), 1),
        ]
    );
    assert_eq!(
        by(UsageBy::Model).await.unwrap().rows,
        [
            row("opus", "opus", 2.375, (205, 22), 1),
            row("sonnet", "sonnet", 2.0, (200, 20), 1),
            row("", "Unknown model", 0.25, (40, 4), 1),
        ]
    );
    // Totals do not depend on the breakdown.
    for which in [
        UsageBy::Session,
        UsageBy::Repo,
        UsageBy::Runner,
        UsageBy::Model,
    ] {
        assert_eq!(by(which).await.unwrap().totals.cost_usd, 4.625);
    }
}

#[tokio::test]
async fn usage_is_the_same_after_a_reindex() {
    let spent = spent().await;
    let all = [
        UsageBy::Day,
        UsageBy::Session,
        UsageBy::Repo,
        UsageBy::Runner,
        UsageBy::Model,
    ];
    let mut before = Vec::new();
    for by in all {
        before.push(spent.store.usage(query(FROM, TO, by)).await.unwrap());
    }
    let home = spent.store.home().clone();
    spent.store.close().await;
    std::fs::remove_file(home.db_path()).unwrap();
    let reopened = Store::open(home).await.unwrap();
    for (by, before) in all.into_iter().zip(before) {
        assert_eq!(
            reopened.usage(query(FROM, TO, by)).await.unwrap(),
            before,
            "{by:?}"
        );
    }
}

#[tokio::test]
async fn by_session_lists_the_200_most_expensive_roots() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let mut ids = Vec::new();
    for n in 0..MAX_SESSION_ROWS + 5 {
        ids.push(
            store
                .create_session(new_session(&format!("s{n}")))
                .await
                .unwrap()
                .id,
        );
    }
    store.close().await;
    let home = Home::new(dir.path());
    let at = local("2026-11-11T12:00:00");
    for (n, id) in ids.iter().enumerate() {
        let lines = vec![
            (at, None, spawned("lead", id, "mock", None)),
            (at, Some("lead"), totals(0.25 * (n + 1) as f64, 1, 1)),
        ];
        write_log(&home, id, lines);
    }
    let store = open(&dir).await;
    let usage = store
        .usage(query(FROM, TO, UsageBy::Session))
        .await
        .unwrap();
    assert_eq!(usage.rows.len(), MAX_SESSION_ROWS);
    assert_eq!(usage.rows[0].key, ids[ids.len() - 1]);
    let costs: Vec<f64> = usage.rows.iter().map(|row| row.totals.cost_usd).collect();
    assert!(costs.windows(2).all(|pair| pair[0] > pair[1]));
    // The cheapest five are left out of the rows, not of the totals.
    assert_eq!(usage.totals.sessions as usize, MAX_SESSION_ROWS + 5);
    assert!(!usage.rows.iter().any(|row| ids[..5].contains(&row.key)));
}

#[tokio::test]
async fn usage_of_an_agent_the_log_never_announced_is_all_new_on_the_sessions_runner() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let id = store.create_session(new_session("ghost")).await.unwrap().id;
    store.close().await;
    let home = Home::new(dir.path());
    let at = local("2026-11-11T10:00:00");
    write_log(&home, &id, vec![(at, Some("ghost"), totals(0.5, 50, 5))]);

    let store = Store::open(home).await.unwrap();
    let by = |by| store.usage(query(FROM, TO, by));
    assert_eq!(
        by(UsageBy::Runner).await.unwrap().rows,
        [row("mock", "mock", 0.5, (50, 5), 1)]
    );
    assert_eq!(
        by(UsageBy::Model).await.unwrap().rows,
        [row("", "Unknown model", 0.5, (50, 5), 1)]
    );
}

#[tokio::test]
async fn the_default_range_includes_what_was_spent_just_now() {
    let spent = spent().await;
    // The moment `d` reported its usage.
    let now = local("2026-10-01T09:00:00");
    let (from, to) = resolve_range(None, None, now).unwrap();
    let query = UsageQuery {
        from,
        to,
        by: UsageBy::Runner,
        zone: zone(),
    };
    let usage = spent.store.usage(query).await.unwrap();
    assert_eq!(usage.totals.sessions, 1);
    assert_eq!((usage.totals.tokens_in, usage.totals.tokens_out), (900, 90));
}

#[test]
fn the_range_defaults_to_30_days_and_is_cut_to_366() {
    const DAY: i64 = 24 * 60 * 60 * 1000;
    let now = 1_800_000_000_000;
    assert_eq!(
        resolve_range(None, None, now).unwrap(),
        (now + 1 - 30 * DAY, now + 1)
    );
    assert_eq!(
        resolve_range(Some(now - DAY), None, now).unwrap(),
        (now - DAY, now + 1)
    );
    assert_eq!(
        resolve_range(None, Some(now - 10 * DAY), now).unwrap(),
        (now - 40 * DAY, now - 10 * DAY)
    );
    // Longer than 366 days: cut at the start.
    assert_eq!(
        resolve_range(Some(0), Some(now), now).unwrap(),
        (now - 366 * DAY, now)
    );
    assert_eq!(
        resolve_range(Some(now - 366 * DAY), Some(now), now).unwrap(),
        (now - 366 * DAY, now)
    );
    // In the future is fine; backwards, empty or negative is not.
    assert!(resolve_range(Some(now), Some(now + DAY), now).is_ok());
    for (from, to) in [
        (Some(now), Some(now)),
        (Some(now), Some(now - 1)),
        (Some(-1), None),
    ] {
        let err = resolve_range(from, to, now).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{from:?} {to:?}: {err:?}");
    }
}

#[tokio::test]
async fn the_issue_and_the_automation_of_a_session_survive_a_reindex() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let issue = IssueRef {
        repo_id: "api".to_owned(),
        number: 12,
        title: "Login fails".to_owned(),
        url: "https://github.com/acme/api/issues/12".to_owned(),
    };
    let created = store
        .create_session(NewSession {
            issue: Some(issue.clone()),
            context: Some("the issue's text".to_owned()),
            automation_id: Some("auto-1".to_owned()),
            ..new_session("from an issue")
        })
        .await
        .unwrap();
    assert_eq!(created.issue.as_ref(), Some(&issue));
    assert_eq!(created.automation_id.as_deref(), Some("auto-1"));
    store.close().await;
    std::fs::remove_file(store.home().db_path()).unwrap();

    let reopened = open(&dir).await;
    let session = reopened.session(&created.id).await.unwrap().session;
    assert_eq!(session.issue, Some(issue));
    assert_eq!(session.automation_id.as_deref(), Some("auto-1"));
    let setup = reopened.setup(&created.id).await.unwrap();
    assert_eq!(setup.context.as_deref(), Some("the issue's text"));
    assert_eq!(reopened.sessions().await.unwrap()[0], session);
}
