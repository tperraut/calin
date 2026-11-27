use calin_proto::PermissionMode;
use tempfile::TempDir;

use super::*;

fn names() -> HashMap<String, String> {
    HashMap::from([("r-api".to_owned(), "api".to_owned())])
}

fn schedule(cron: &str) -> AutomationTrigger {
    AutomationTrigger::Schedule {
        cron: cron.to_owned(),
    }
}

#[test]
fn triggers_are_put_in_words() {
    for (trigger, words) in [
        (schedule("0 8 * * 1-5"), "Every weekday at 08:00"),
        (schedule("0 8 * * *"), "Every day at 08:00"),
        (schedule("*/15 * * * *"), "Every 15 minutes"),
        (schedule("30 9 * * 1"), "At 09:30 on Mondays"),
        (schedule("0 0 1 * *"), "On the 1st of every month at 00:00"),
        (schedule("0 8 * 1-6 *"), "Cron: 0 8 * 1-6 *"),
        // A file edited by hand may hold anything; it is shown as it is.
        (schedule("  not   a cron "), "Cron: not a cron"),
        (
            AutomationTrigger::Issue {
                repo_id: "r-api".to_owned(),
                label: Some("agent".to_owned()),
            },
            "New issue labelled `agent` in api",
        ),
        (
            AutomationTrigger::Issue {
                repo_id: "r-api".to_owned(),
                label: None,
            },
            "New issue in api",
        ),
        (
            AutomationTrigger::PullRequest {
                repo_id: "r-api".to_owned(),
            },
            "New pull request in api",
        ),
        // A repo that is gone is named by its id.
        (
            AutomationTrigger::PullRequest {
                repo_id: "r-gone".to_owned(),
            },
            "New pull request in r-gone",
        ),
    ] {
        assert_eq!(trigger_text(&trigger, &names()), words, "{trigger:?}");
    }
}

fn run(at: i64, outcome: AutomationOutcome, session_id: Option<&str>) -> StoredRun {
    StoredRun {
        id: format!("run-{at}"),
        at,
        cause: "schedule".to_owned(),
        outcome,
        session_id: session_id.map(str::to_owned),
        detail: None,
    }
}

#[test]
fn failures_in_a_row_count_failed_sessions_and_failed_starts_since_enabling() {
    use AutomationOutcome::{Failed, Skipped, Started};
    let statuses = HashMap::from([
        ("failed-1".to_owned(), SessionStatus::Failed),
        ("failed-2".to_owned(), SessionStatus::Failed),
        ("review".to_owned(), SessionStatus::Review),
        ("running".to_owned(), SessionStatus::Running),
    ]);
    let count = |runs: &[StoredRun], since| failures_in_a_row(runs, since, &statuses);
    // Newest first.
    let three = [
        run(5, Started, Some("failed-2")),
        run(4, Skipped, None),
        run(3, Failed, None),
        run(2, Started, Some("failed-1")),
        run(1, Started, Some("review")),
    ];
    assert_eq!(count(&three, None), 3);
    // Only what ran since it was switched on.
    assert_eq!(count(&three, Some(3)), 2);
    // A success breaks the row; so does a session still at work.
    let broken = [
        run(4, Started, Some("failed-2")),
        run(3, Started, Some("review")),
        run(2, Failed, None),
        run(1, Failed, None),
    ];
    assert_eq!(count(&broken, None), 1);
    let at_work = [run(2, Started, Some("running")), run(1, Failed, None)];
    assert_eq!(count(&at_work, None), 0);
    // A session nobody knows any more is no failure.
    assert_eq!(count(&[run(1, Started, Some("gone"))], None), 0);
    assert_eq!(count(&[], None), 0);
}

fn stored(id: &str) -> Stored {
    Stored {
        id: id.to_owned(),
        name: "Nightly".to_owned(),
        enabled: true,
        trigger: schedule("0 3 * * *"),
        session: AutomationSession {
            prompt: "Triage on {{date}}".to_owned(),
            workflow: None,
            repos: vec![],
            runner: None,
            model: None,
            permission_mode: PermissionMode::Autonomous,
        },
        created_at: 1,
        enabled_at: Some(1),
        disabled_reason: None,
        last_poll_at: None,
        last_poll_problem: None,
        seen: BTreeSet::from([3, 7]),
        needs_baseline: false,
        runs: vec![run(2, AutomationOutcome::Started, Some("s1"))],
    }
}

#[tokio::test]
async fn automations_are_kept_in_a_file_that_survives_a_restart() {
    let dir = TempDir::new().unwrap();
    let home = Home::new(dir.path());
    let store = AutomationStore::load(&home).await;
    assert_eq!(store.all().await, []);
    // Nothing is written until something changes.
    assert!(!home.automations_path().exists());
    store
        .change(|all| {
            all.push(stored("a1"));
            Ok(())
        })
        .await
        .unwrap();
    // A change that fails changes nothing.
    let refused = store
        .change(|all| {
            all.clear();
            Err::<(), _>(crate::Error::Invalid("no".to_owned()))
        })
        .await;
    assert!(refused.is_err());
    assert_eq!(store.all().await, [stored("a1")]);

    let again = AutomationStore::load(&home).await;
    assert_eq!(again.all().await, [stored("a1")]);
    assert_eq!(again.get("a1").await, Some(stored("a1")));
    assert_eq!(again.get("nope").await, None);
    // No temporary file is left behind.
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["automations.json"]);
}

#[tokio::test]
async fn a_corrupt_file_means_no_automations_and_is_kept_aside() {
    let dir = TempDir::new().unwrap();
    let home = Home::new(dir.path());
    for corrupt in [
        "{ not json",
        "[]",
        r#"{"version": 1, "automations": [{"id": 3}]}"#,
    ] {
        std::fs::write(home.automations_path(), corrupt).unwrap();
        let store = AutomationStore::load(&home).await;
        assert_eq!(store.all().await, [], "{corrupt}");
        let aside = dir.path().join("automations.json.corrupt");
        assert_eq!(std::fs::read_to_string(&aside).unwrap(), corrupt);
        // Saving does not lose the copy.
        store
            .change(|all| {
                all.push(stored("a2"));
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&aside).unwrap(), corrupt);
    }
    // A file that is a directory cannot be read either.
    let other = TempDir::new().unwrap();
    let home = Home::new(other.path());
    std::fs::create_dir(home.automations_path()).unwrap();
    assert_eq!(AutomationStore::load(&home).await.all().await, []);
}

#[test]
fn runs_are_kept_newest_first_and_at_most_fifty() {
    let mut automation = stored("a3");
    automation.runs.clear();
    for at in 0..60 {
        automation.record(run(at, AutomationOutcome::Skipped, None));
    }
    assert_eq!(automation.runs.len(), MAX_RUNS);
    assert_eq!(automation.runs[0].at, 59);
    assert_eq!(automation.runs[MAX_RUNS - 1].at, 10);
}

#[test]
fn the_view_says_when_a_schedule_fires_next_and_hides_poll_state_of_schedules() {
    let zone = TimeZone::UTC;
    let statuses = HashMap::from([("s1".to_owned(), SessionStatus::Review)]);
    let mut automation = stored("a4");
    automation.last_poll_at = Some(5);
    // 2026-11-28T10:00:00Z: the next 03:00 is tomorrow's.
    let now = 1_795_860_000_000;
    let view = automation.view(now, &zone, &names(), &statuses);
    assert_eq!(view.next_run_at, Some(now + 17 * 60 * 60 * 1000));
    assert_eq!(view.trigger_text, "Every day at 03:00");
    assert_eq!(view.last_poll_at, None);
    let last = view.last_run.unwrap();
    assert_eq!(last.session_status, Some(SessionStatus::Review));
    automation.enabled = false;
    assert_eq!(
        automation.view(now, &zone, &names(), &statuses).next_run_at,
        None
    );
}
