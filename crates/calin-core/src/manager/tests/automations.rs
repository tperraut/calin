//! Automations (integrations spec, section 3), driven by hand: the clock is
//! the test's and the scheduler only looks when the test calls
//! `automations_tick`. GitHub is a stand-in that answers from files.

use std::collections::HashMap;

use calin_proto::{
    Automation, AutomationOutcome, AutomationRequest, AutomationRun, AutomationSession,
    AutomationTrigger, RunAutomationRequest,
};
use serde_json::json;

use super::orchestration::{Played, play};
use super::*;
use crate::clock::{ManualClock, SharedClock};
use crate::testing::write_executable;

/// Monday 2026-11-30, 07:59:30 at UTC+1, the zone of these tests.
const T0: i64 = 1_796_021_970_000;
const MINUTE: i64 = 60_000;
const DAY: i64 = 24 * 60 * MINUTE;
const POLL: Duration = Duration::from_secs(5 * 60);
const POLL_MS: i64 = 5 * MINUTE;

/// Answers `gh` from files beside it: `issues.json` (or
/// `issues-<label>.json` when a label is asked for), `issue-<n>.json`,
/// `pulls.json`, `pull-<n>.json`. With `fail` there, listing fails. Every
/// call is logged.
const TRIGGER_GH: &str = r#"#!/bin/sh
root=$(dirname "$0")
printf '%s\n' "$*" >> "$root/log"
case "$1 $2" in
"auth status") exit 0 ;;
"issue list"|"pr list")
  if [ -f "$root/fail" ]; then echo "HTTP 502: Bad Gateway" >&2; exit 1; fi
  file="$root/pulls.json"
  if [ "$1" = issue ]; then
    file="$root/issues.json"
    for arg in "$@"; do case "$arg" in --label=*) file="$root/issues-${arg#--label=}.json" ;; esac; done
  fi
  cat "$file" 2>/dev/null || echo '[]'
  ;;
"issue view")
  cat "$root/issue-$3.json" 2>/dev/null || { echo "GraphQL: Could not resolve to an issue or pull request with the number of $3." >&2; exit 1; }
  ;;
"pr view")
  cat "$root/pull-$3.json" 2>/dev/null || { echo "GraphQL: Could not resolve to a PullRequest with the number of $3." >&2; exit 1; }
  ;;
*) echo "unexpected: $*" >&2; exit 2 ;;
esac
"#;

struct TriggerGh {
    dir: TempDir,
}

impl TriggerGh {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        write_executable(&dir.path().join("gh"), TRIGGER_GH);
        Self { dir }
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("gh")
    }

    fn write(&self, name: &str, value: serde_json::Value) {
        let draft = self.dir.path().join(format!("{name}.draft"));
        std::fs::write(&draft, value.to_string()).unwrap();
        std::fs::rename(draft, self.dir.path().join(name)).unwrap();
    }

    /// The open issues, as (number, labels), newest last.
    fn issues(&self, open: &[(u32, &[&str])]) {
        let issue = |number: u32, labels: &[&str]| {
            json!({
                "number": number,
                "title": format!("Issue {number}"),
                "state": "OPEN",
                "labels": labels.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
                "author": {"login": "octocat"},
                "updatedAt": "2026-11-20T08:30:00Z",
                "url": format!("https://github.com/acme/api/issues/{number}"),
                "body": format!("Body of {number}"),
            })
        };
        let mut by_label: HashMap<&str, Vec<serde_json::Value>> = HashMap::new();
        let mut all = Vec::new();
        for (number, labels) in open {
            self.write(&format!("issue-{number}.json"), issue(*number, labels));
            all.push(issue(*number, labels));
            for label in *labels {
                by_label
                    .entry(label)
                    .or_default()
                    .push(issue(*number, labels));
            }
        }
        self.write("issues.json", json!(all));
        for (label, issues) in by_label {
            self.write(&format!("issues-{label}.json"), json!(issues));
        }
    }

    fn pulls(&self, open: &[u32]) {
        let pull = |number: u32| {
            json!({
                "number": number,
                "title": format!("Pull {number}"),
                "body": null,
                "url": format!("https://github.com/acme/api/pull/{number}"),
                "author": {"login": "dependabot[bot]"},
                "updatedAt": "2026-11-20T08:30:00Z",
            })
        };
        for number in open {
            self.write(&format!("pull-{number}.json"), pull(*number));
        }
        self.write(
            "pulls.json",
            json!(open.iter().map(|n| pull(*n)).collect::<Vec<_>>()),
        );
    }

    fn fail(&self, on: bool) {
        let flag = self.dir.path().join("fail");
        if on {
            std::fs::write(flag, "").unwrap();
        } else {
            std::fs::remove_file(flag).unwrap();
        }
    }

    /// The calls that listed something.
    fn lists(&self) -> Vec<String> {
        let log = std::fs::read_to_string(self.dir.path().join("log")).unwrap_or_default();
        log.lines()
            .filter(|line| line.contains(" list "))
            .map(str::to_owned)
            .collect()
    }
}

struct Auto {
    manager: SessionManager,
    starts: Starts,
    clock: Arc<ManualClock>,
    gh: TriggerGh,
    home: TempDir,
    _clones: TempDir,
    api: Repo,
    dry_run: bool,
}

fn auto_config(clock: &Arc<ManualClock>, gh: &TriggerGh) -> ManagerConfig {
    ManagerConfig {
        gh_bin: gh.bin(),
        clock: SharedClock::new(Arc::clone(clock)),
        time_zone: TimeZone::fixed(jiff::tz::offset(1)),
        trigger_poll: POLL,
        automations: false,
        ..config()
    }
}

async fn auto(dry_run: bool) -> Auto {
    let clock = Arc::new(ManualClock::new(T0));
    let gh = TriggerGh::new();
    let home = TempDir::new().unwrap();
    let clones = TempDir::new().unwrap();
    let (manager, starts) = manager_with(&home, dry_run, auto_config(&clock, &gh)).await;
    let api = registered(&manager, clones.path(), "api").await;
    git(
        Path::new(&api.path),
        &["remote", "add", "origin", "https://github.com/acme/api.git"],
    );
    Auto {
        manager,
        starts,
        clock,
        gh,
        home,
        _clones: clones,
        api,
        dry_run,
    }
}

impl Auto {
    /// The same data directory, as a server started again would find it.
    async fn restart(&mut self) {
        self.manager.shutdown().await;
        self.manager.store().close().await;
        let config = auto_config(&self.clock, &self.gh);
        let (manager, starts) = manager_with(&self.home, self.dry_run, config).await;
        self.manager = manager;
        self.starts = starts;
    }

    async fn tick_at(&self, at: i64) {
        self.clock.set(at);
        within(self.manager.automations_tick()).await;
    }

    async fn runs(&self, id: &str) -> Vec<AutomationRun> {
        self.manager.automation_runs(id).await.unwrap()
    }

    async fn get(&self, id: &str) -> Automation {
        self.manager.automation(id).await.unwrap()
    }

    fn issue_trigger(&self, label: Option<&str>) -> AutomationTrigger {
        AutomationTrigger::Issue {
            repo_id: self.api.id.clone(),
            label: label.map(str::to_owned),
        }
    }

    /// Ends the turn of a puppet session's agent, so that the session is in
    /// review: no longer at work.
    /// The agent's end is the caller's to keep: dropped, the session fails.
    async fn finish(&self, played: &Played) {
        emit(&played.io, RunnerOutput::TurnEnded).await;
        events_until(
            &self.manager,
            &played.id,
            0,
            is_status(SessionStatus::Review),
        )
        .await;
    }
}

fn schedule(cron: &str) -> AutomationTrigger {
    AutomationTrigger::Schedule {
        cron: cron.to_owned(),
    }
}

fn automation(trigger: AutomationTrigger, prompt: &str, enabled: bool) -> AutomationRequest {
    AutomationRequest {
        name: "Triage".to_owned(),
        enabled,
        trigger,
        session: AutomationSession {
            prompt: prompt.to_owned(),
            workflow: None,
            repos: Vec::new(),
            runner: Some("puppet".to_owned()),
            model: None,
            permission_mode: PermissionMode::Autonomous,
        },
    }
}

fn causes(runs: &[AutomationRun]) -> Vec<(String, AutomationOutcome)> {
    runs.iter()
        .map(|run| (run.cause.clone(), run.outcome))
        .collect()
}

fn started(cause: &str) -> (String, AutomationOutcome) {
    (cause.to_owned(), AutomationOutcome::Started)
}

fn skipped(cause: &str) -> (String, AutomationOutcome) {
    (cause.to_owned(), AutomationOutcome::Skipped)
}

/// A change that spoils a valid request.
type Change = Box<dyn Fn(&mut AutomationRequest)>;

#[tokio::test]
async fn an_automation_that_does_not_validate_is_refused_with_every_problem() {
    let a = auto(false).await;
    let valid = || automation(schedule("0 8 * * *"), "Triage on {{date}}", false);
    let api = a.api.id.clone();
    let cases: Vec<(Change, &str)> = vec![
        (
            Box::new(|r| r.name = "  ".into()),
            "the name must not be blank",
        ),
        (
            Box::new(|r| r.name = "x".repeat(81)),
            "the name must be at most 80 characters",
        ),
        (
            Box::new(|r| r.trigger = schedule("61 * * * *")),
            "the schedule is not valid: the minute field: 61 is out of range 0-59",
        ),
        (
            Box::new(|r| r.trigger = schedule("")),
            "the schedule is not valid: the expression is empty",
        ),
        (
            Box::new(|r| {
                r.trigger = AutomationTrigger::Issue {
                    repo_id: "nope".into(),
                    label: None,
                }
            }),
            "the trigger's repo nope is unknown",
        ),
        (
            Box::new(|r| {
                r.trigger = AutomationTrigger::PullRequest {
                    repo_id: "nope".into(),
                }
            }),
            "the trigger's repo nope is unknown",
        ),
        (
            Box::new(|r| r.session.prompt = " \n ".into()),
            "the prompt must not be blank",
        ),
        (
            Box::new(|r| r.session.prompt = "Fix {{title}}".into()),
            "the prompt uses `{{title}}`, which this trigger does not offer",
        ),
        (
            Box::new(|r| r.session.prompt = "Fix {{date".into()),
            "`{{` is never closed",
        ),
        (
            Box::new(|r| r.session.permission_mode = PermissionMode::Ask),
            "the permission mode must be autonomous or accept_edits",
        ),
        (
            Box::new(|r| r.session.workflow = Some("nope".into())),
            "unknown workflow nope",
        ),
        (
            Box::new(|r| {
                r.session.repos = vec![AttachRepoRequest {
                    repo_id: "nope".into(),
                    mode: None,
                }]
            }),
            "the session's repo nope is unknown",
        ),
        (
            Box::new(|r| r.session.runner = Some("nope".into())),
            "unknown runner nope",
        ),
    ];
    for (change, problem) in cases {
        let mut request = valid();
        change(&mut request);
        let err = a
            .manager
            .create_automation(request.clone())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Invalid(why) if why.contains(problem)),
            "{problem}: {err:?}"
        );
    }
    let label = automation(a.issue_trigger(Some(" ")), "Fix #{{number}}", false);
    let err = a.manager.create_automation(label).await.unwrap_err();
    assert!(
        err.to_string().contains("the label must not be blank"),
        "{err}"
    );
    let mut twice = valid();
    let repo = AttachRepoRequest {
        repo_id: api.clone(),
        mode: None,
    };
    twice.session.repos = vec![repo.clone(), repo];
    let err = a.manager.create_automation(twice).await.unwrap_err();
    assert!(err.to_string().contains("lists repo"), "{err}");
    let item = automation(a.issue_trigger(None), "Fix {{nope}}", false);
    let err = a.manager.create_automation(item).await.unwrap_err();
    assert!(err.to_string().contains("`{{nope}}`"), "{err}");

    // Every problem at once.
    let mut wrong = valid();
    wrong.name = String::new();
    wrong.trigger = schedule("* *");
    wrong.session.permission_mode = PermissionMode::Ask;
    let err = a
        .manager
        .create_automation(wrong)
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(err.matches("; ").count(), 2, "{err}");
    // Nothing was saved, and the same holds for an update.
    assert_eq!(a.manager.list_automations().await.unwrap(), []);
    let saved = a.manager.create_automation(valid()).await.unwrap();
    let mut wrong = valid();
    wrong.session.prompt = String::new();
    assert!(a.manager.update_automation(&saved.id, wrong).await.is_err());
    assert_eq!(a.get(&saved.id).await, saved);

    // What is valid is kept tidy.
    let mut tidy = valid();
    tidy.name = format!("  {}  ", "é".repeat(80));
    tidy.trigger = schedule(" 0  8 * *   1-5 ");
    tidy.session.prompt = "On {{ date }}".into();
    tidy.session.runner = Some(" ".into());
    tidy.session.model = Some(" opus ".into());
    tidy.session.workflow = Some("".into());
    let item = a.manager.create_automation(tidy).await.unwrap();
    assert_eq!(item.name, "é".repeat(80));
    assert_eq!(item.trigger, schedule("0 8 * * 1-5"));
    assert_eq!(item.trigger_text, "Every weekday at 08:00");
    assert_eq!(item.session.runner, None);
    assert_eq!(item.session.model.as_deref(), Some("opus"));
    assert_eq!(item.session.workflow, None);
    let issues = automation(
        a.issue_trigger(Some(" agent ")),
        "{{number}} {{title}} {{body}} {{url}} {{author}} {{date}}",
        false,
    );
    let issues = a.manager.create_automation(issues).await.unwrap();
    assert_eq!(issues.trigger_text, "New issue labelled `agent` in api");
}

#[tokio::test]
async fn automations_are_listed_by_name_changed_and_deleted_without_their_sessions() {
    let a = auto(false).await;
    let mut second = automation(schedule("0 8 * * *"), "Second", false);
    second.name = "b second".to_owned();
    let mut first = automation(schedule("0 9 * * *"), "First", false);
    first.name = "A first".to_owned();
    let second = a.manager.create_automation(second).await.unwrap();
    let first = a.manager.create_automation(first).await.unwrap();
    assert!(!second.enabled);
    assert_eq!(second.created_at, T0);
    assert_eq!(second.next_run_at, None);
    let names: Vec<String> = a
        .manager
        .list_automations()
        .await
        .unwrap()
        .into_iter()
        .map(|automation| automation.name)
        .collect();
    assert_eq!(names, ["A first", "b second"]);

    // A manual run, then a change: the run stays.
    let run = a
        .manager
        .run_automation(&first.id, RunAutomationRequest::default())
        .await
        .unwrap();
    let session_id = run.session_id.clone().unwrap();
    let mut changed = automation(schedule("30 9 * * *"), "Changed", true);
    changed.name = "A first".to_owned();
    let changed = a
        .manager
        .update_automation(&first.id, changed)
        .await
        .unwrap();
    assert_eq!(changed.created_at, first.created_at);
    assert_eq!(changed.trigger_text, "Every day at 09:30");
    // 07:59:30 at UTC+1: 09:30 today.
    assert_eq!(changed.next_run_at, Some(T0 + 90 * MINUTE + 30_000));
    assert_eq!(changed.last_run.unwrap().id, run.id);

    a.manager.delete_automation(&first.id).await.unwrap();
    for gone in [
        a.manager.automation(&first.id).await.err(),
        a.manager.delete_automation(&first.id).await.err(),
        a.manager.automation_runs(&first.id).await.err(),
        a.manager
            .run_automation(&first.id, RunAutomationRequest::default())
            .await
            .err(),
        a.manager
            .update_automation(&first.id, automation(schedule("* * * * *"), "x", false))
            .await
            .err(),
    ] {
        assert!(matches!(gone, Some(Error::Missing(_))), "{gone:?}");
    }
    // Its session is untouched.
    let session = a.manager.get(&session_id).await.unwrap().session;
    assert_eq!(session.automation_id.as_deref(), Some(first.id.as_str()));
    assert_eq!(a.manager.list_automations().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_schedule_fires_once_per_matching_minute_and_never_for_time_missed_while_down() {
    let mut a = auto(false).await;
    let daily = a
        .manager
        .create_automation(automation(
            schedule("0 8 * * *"),
            "Triage on {{date}}",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(daily.next_run_at, Some(T0 + 30_000));
    a.tick_at(T0).await;
    assert_eq!(a.runs(&daily.id).await, []);
    // 08:00:10: the minute matched since the last look.
    a.tick_at(T0 + 40_000).await;
    let runs = a.runs(&daily.id).await;
    assert_eq!(causes(&runs), [started("schedule")]);
    let session_id = runs[0].session_id.clone().unwrap();
    let session = a.manager.get(&session_id).await.unwrap().session;
    assert_eq!(session.automation_id.as_deref(), Some(daily.id.as_str()));
    assert_eq!(session.title, "Triage");
    assert_eq!(session.prompt, "Triage on 2026-11-30");
    assert_eq!(session.permission_mode, PermissionMode::Autonomous);
    let played = play(&mut a.starts).await;
    assert_eq!(played.id, session_id);
    // Later in the same minute, and again later: nothing more.
    a.tick_at(T0 + 80_000).await;
    a.tick_at(T0 + 2 * MINUTE).await;
    assert_eq!(a.runs(&daily.id).await.len(), 1);
    assert_eq!(a.get(&daily.id).await.next_run_at, Some(T0 + 30_000 + DAY));

    // The next day, with the session still at work: skipped, and said why.
    a.tick_at(T0 + DAY + 40_000).await;
    let runs = a.runs(&daily.id).await;
    assert_eq!(causes(&runs), [skipped("schedule"), started("schedule")]);
    assert!(
        runs[0].detail.as_ref().unwrap().contains("still running"),
        "{runs:?}"
    );
    assert_eq!(runs[0].session_id, None);
    assert_eq!(runs[1].session_status, Some(SessionStatus::Running));
    assert_eq!(a.get(&daily.id).await.last_run.unwrap(), runs[0]);
    a.finish(&played).await;
    a.tick_at(T0 + 2 * DAY + 40_000).await;
    assert_eq!(a.runs(&daily.id).await.len(), 3);
    assert_eq!(
        a.runs(&daily.id).await[0].outcome,
        AutomationOutcome::Started
    );

    // Down for three days: nothing is made up when the server is back.
    a.restart().await;
    a.clock.set(T0 + 5 * DAY + 2 * MINUTE);
    a.restart().await;
    a.tick_at(T0 + 5 * DAY + 3 * MINUTE).await;
    assert_eq!(a.runs(&daily.id).await.len(), 3);
    a.tick_at(T0 + 6 * DAY + 40_000).await;
    assert_eq!(a.runs(&daily.id).await.len(), 4);
}

#[tokio::test]
async fn a_schedule_does_not_pile_up_and_starts_after_it_was_switched_on() {
    let mut a = auto(false).await;
    let every = a
        .manager
        .create_automation(automation(schedule("* * * * *"), "Tick", false))
        .await
        .unwrap();
    // Off: nothing fires.
    a.tick_at(T0 + 5 * MINUTE).await;
    assert_eq!(a.runs(&every.id).await, []);
    a.clock.set(T0 + 10 * MINUTE + 5_000);
    let on = a
        .manager
        .update_automation(&every.id, automation(schedule("* * * * *"), "Tick", true))
        .await
        .unwrap();
    assert!(on.enabled);
    // The minutes before it was switched on do not count, even unseen.
    a.tick_at(T0 + 10 * MINUTE + 20_000).await;
    assert_eq!(a.runs(&every.id).await, []);
    // Ten minutes in one look: one session.
    a.tick_at(T0 + 21 * MINUTE).await;
    assert_eq!(a.runs(&every.id).await.len(), 1);
    let played = play(&mut a.starts).await;
    a.finish(&played).await;
    a.tick_at(T0 + 22 * MINUTE).await;
    assert_eq!(a.runs(&every.id).await.len(), 2);
}

#[tokio::test]
async fn an_issue_trigger_marks_what_exists_then_fires_once_per_new_issue() {
    let mut a = auto(false).await;
    a.gh.issues(&[(1, &[]), (2, &["agent"])]);
    let prompt = "Fix #{{number}}: {{title}} ({{url}}, by {{author}})\n{{body}}";
    let trigger = a
        .manager
        .create_automation(automation(a.issue_trigger(None), prompt, true))
        .await
        .unwrap();
    assert_eq!(trigger.trigger_text, "New issue in api");
    // The first poll only marks what is there.
    a.tick_at(T0).await;
    assert_eq!(a.runs(&trigger.id).await, []);
    assert_eq!(a.get(&trigger.id).await.last_poll_at, Some(T0));
    assert_eq!(a.gh.lists().len(), 1);

    a.gh.issues(&[(1, &[]), (2, &["agent"]), (3, &[])]);
    // Not five minutes yet: GitHub is not asked.
    a.tick_at(T0 + POLL_MS - 1).await;
    assert_eq!(a.gh.lists().len(), 1);
    a.tick_at(T0 + POLL_MS).await;
    let runs = a.runs(&trigger.id).await;
    assert_eq!(causes(&runs), [started("issue #3")]);
    let session = a
        .manager
        .get(runs[0].session_id.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(session.session.title, "Triage #3");
    assert_eq!(
        session.session.prompt,
        "Fix #3: Issue 3 (https://github.com/acme/api/issues/3, by octocat)\nBody of 3"
    );
    // Started from the issue: its pull request will close it.
    let issue = session.session.issue.unwrap();
    assert_eq!(
        (issue.repo_id.as_str(), issue.number),
        (a.api.id.as_str(), 3)
    );
    let three = play(&mut a.starts).await;

    // Two more while #3's session works: #4 is skipped and stays unseen,
    // #5 waits behind it.
    a.gh.issues(&[(1, &[]), (2, &["agent"]), (3, &[]), (4, &[]), (5, &[])]);
    a.tick_at(T0 + 2 * POLL_MS).await;
    let runs = a.runs(&trigger.id).await;
    assert_eq!(causes(&runs), [skipped("issue #4"), started("issue #3")]);
    a.finish(&three).await;

    // What was seen is remembered across a restart.
    a.restart().await;
    a.tick_at(T0 + 3 * POLL_MS).await;
    let runs = a.runs(&trigger.id).await;
    assert_eq!(
        causes(&runs),
        [
            skipped("issue #5"),
            started("issue #4"),
            skipped("issue #4"),
            started("issue #3")
        ]
    );
    let four = play(&mut a.starts).await;
    a.finish(&four).await;
    a.tick_at(T0 + 4 * POLL_MS).await;
    a.tick_at(T0 + 5 * POLL_MS).await;
    let runs = a.runs(&trigger.id).await;
    assert_eq!(causes(&runs)[0], started("issue #5"));
    assert_eq!(
        runs.iter()
            .filter(|run| run.outcome == AutomationOutcome::Started)
            .count(),
        3
    );
}

#[tokio::test]
async fn a_label_filters_issues_and_a_failing_poll_is_recorded_and_tried_again() {
    let a = auto(false).await;
    a.gh.issues(&[(1, &["agent"]), (2, &[])]);
    let labelled = a
        .manager
        .create_automation(automation(
            a.issue_trigger(Some("agent")),
            "Fix #{{number}}",
            true,
        ))
        .await
        .unwrap();
    a.tick_at(T0).await;
    assert!(
        a.gh.lists()[0].contains("--label=agent"),
        "{:?}",
        a.gh.lists()
    );

    a.gh.fail(true);
    a.tick_at(T0 + POLL_MS).await;
    let failed = a.get(&labelled.id).await;
    assert_eq!(failed.last_poll_at, Some(T0 + POLL_MS));
    assert!(failed.last_poll_problem.unwrap().contains("HTTP 502"));
    assert!(failed.enabled);

    a.gh.fail(false);
    a.gh.issues(&[(1, &["agent"]), (2, &[]), (3, &[]), (4, &["agent", "bug"])]);
    a.tick_at(T0 + 2 * POLL_MS).await;
    let polled = a.get(&labelled.id).await;
    assert_eq!(polled.last_poll_problem, None);
    // Only the labelled one, though #3 is newer than what was seen too.
    assert_eq!(causes(&a.runs(&labelled.id).await), [started("issue #4")]);
}

#[tokio::test]
async fn a_failing_first_poll_marks_nothing_and_the_next_one_does() {
    let a = auto(false).await;
    a.gh.fail(true);
    a.gh.issues(&[(1, &[])]);
    let trigger = a
        .manager
        .create_automation(automation(a.issue_trigger(None), "Fix #{{number}}", true))
        .await
        .unwrap();
    a.tick_at(T0).await;
    assert!(a.get(&trigger.id).await.last_poll_problem.is_some());
    a.gh.fail(false);
    a.tick_at(T0 + POLL_MS).await;
    // The baseline came with the poll that worked: #1 existed, it does not fire.
    assert_eq!(a.runs(&trigger.id).await, []);
    a.gh.issues(&[(1, &[]), (2, &[])]);
    a.tick_at(T0 + 2 * POLL_MS).await;
    assert_eq!(causes(&a.runs(&trigger.id).await), [started("issue #2")]);
}

#[tokio::test]
async fn a_pull_request_trigger_fires_for_new_pull_requests_one_at_a_time() {
    let mut a = auto(false).await;
    a.gh.pulls(&[10]);
    let trigger = AutomationTrigger::PullRequest {
        repo_id: a.api.id.clone(),
    };
    let reviews = a
        .manager
        .create_automation(automation(trigger, "Review {{url}} by {{author}}", true))
        .await
        .unwrap();
    assert_eq!(reviews.trigger_text, "New pull request in api");
    a.tick_at(T0).await;
    a.gh.pulls(&[10, 11, 12]);
    a.tick_at(T0 + POLL_MS).await;
    let runs = a.runs(&reviews.id).await;
    assert_eq!(
        causes(&runs),
        [skipped("pull request #12"), started("pull request #11")]
    );
    let detail = a
        .manager
        .get(runs[1].session_id.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(
        detail.session.prompt,
        "Review https://github.com/acme/api/pull/11 by dependabot[bot]"
    );
    assert_eq!(detail.session.issue, None);
    // The trigger's repo is the session's primary one.
    let eleven = play(&mut a.starts).await;
    let detail = a.manager.get(&eleven.id).await.unwrap();
    assert_eq!(detail.repos[0].repo_id, a.api.id);
    assert!(detail.repos[0].primary);
    a.finish(&eleven).await;
    a.tick_at(T0 + 2 * POLL_MS).await;
    assert_eq!(
        causes(&a.runs(&reviews.id).await)[0],
        started("pull request #12")
    );
}

/// A look at `minute` past [`T0`]; returns the newest run once its
/// session, if it started one, has settled.
async fn look(a: &Auto, id: &str, minute: i64) -> AutomationRun {
    a.tick_at(T0 + minute * MINUTE).await;
    let run = a.runs(id).await.remove(0);
    if let Some(session_id) = &run.session_id
        && run.at == T0 + minute * MINUTE
    {
        events_until(&a.manager, session_id, 0, |event| {
            matches!(&event.body, EventBody::SessionStatus { status, .. } if status.is_settled())
        })
        .await;
    }
    run
}

#[tokio::test]
async fn three_failed_runs_in_a_row_switch_an_automation_off_two_and_a_success_do_not() {
    let mut a = auto(false).await;
    let mut request = automation(schedule("* * * * *"), "Tick", true);
    request.session.runner = Some("broken".to_owned());
    let flaky = a.manager.create_automation(request.clone()).await.unwrap();
    let with_runner = |runner: &str| {
        let mut request = request.clone();
        request.session.runner = Some(runner.to_owned());
        request
    };
    // Two failures, then a session that works, then two failures again.
    look(&a, &flaky.id, 1).await;
    look(&a, &flaky.id, 2).await;
    a.manager
        .update_automation(&flaky.id, with_runner("puppet"))
        .await
        .unwrap();
    a.tick_at(T0 + 3 * MINUTE).await;
    let worked = play(&mut a.starts).await;
    a.finish(&worked).await;
    a.manager
        .update_automation(&flaky.id, with_runner("broken"))
        .await
        .unwrap();
    look(&a, &flaky.id, 4).await;
    look(&a, &flaky.id, 5).await;
    // Two in a row: still on, and the look fires a third.
    let third = look(&a, &flaky.id, 6).await;
    assert_eq!(third.outcome, AutomationOutcome::Started);
    let still = a.get(&flaky.id).await;
    assert!(still.enabled, "{still:?}");
    assert_eq!(still.disabled_reason, None);
    let statuses: Vec<Option<SessionStatus>> = a
        .runs(&flaky.id)
        .await
        .iter()
        .map(|run| run.session_status)
        .collect();
    use SessionStatus::{Failed, Review};
    assert_eq!(
        statuses,
        [
            Some(Failed),
            Some(Failed),
            Some(Failed),
            Some(Review),
            Some(Failed),
            Some(Failed)
        ]
    );
    // Three in a row: off, with the reason, and nothing fires any more.
    a.tick_at(T0 + 7 * MINUTE).await;
    let off = a.get(&flaky.id).await;
    assert!(!off.enabled);
    let reason = off.disabled_reason.unwrap();
    assert!(reason.contains("3 runs in a row failed"), "{reason}");
    assert!(reason.contains("failed"), "{reason}");
    assert_eq!(a.runs(&flaky.id).await.len(), 6);
    a.tick_at(T0 + 8 * MINUTE).await;
    assert_eq!(a.runs(&flaky.id).await.len(), 6);

    // Switched on again: the reason goes, and old failures no longer count.
    a.clock.set(T0 + 9 * MINUTE);
    let on = a
        .manager
        .update_automation(&flaky.id, with_runner("broken"))
        .await
        .unwrap();
    assert!(on.enabled);
    assert_eq!(on.disabled_reason, None);
    look(&a, &flaky.id, 10).await;
    a.tick_at(T0 + 11 * MINUTE).await;
    assert!(a.get(&flaky.id).await.enabled);
    assert_eq!(a.runs(&flaky.id).await.len(), 8);
}

#[tokio::test]
async fn a_failed_start_is_a_failed_run() {
    let a = auto(false).await;
    // A repo that went away makes the session impossible to create.
    let other = TempDir::new().unwrap();
    let gone = registered(&a.manager, other.path(), "gone").await;
    let mut request = automation(schedule("* * * * *"), "Tick", false);
    request.session.repos = vec![attach(&gone, None)];
    let saved = a.manager.create_automation(request).await.unwrap();
    a.manager.remove_repo(&gone.id).await.unwrap();
    let run = a
        .manager
        .run_automation(&saved.id, RunAutomationRequest::default())
        .await
        .unwrap();
    assert_eq!(run.outcome, AutomationOutcome::Failed);
    assert_eq!(run.session_id, None);
    assert!(run.detail.unwrap().contains("unknown repo"));
}

#[tokio::test]
async fn a_manual_run_works_switched_off_and_names_its_item() {
    let mut a = auto(false).await;
    let daily = a
        .manager
        .create_automation(automation(schedule("0 8 * * *"), "Now {{date}}", false))
        .await
        .unwrap();
    let run = a
        .manager
        .run_automation(&daily.id, RunAutomationRequest::default())
        .await
        .unwrap();
    assert_eq!(
        (run.cause.as_str(), run.outcome),
        ("manual", AutomationOutcome::Started)
    );
    assert_eq!(run.at, T0);
    assert!(run.session_status.is_some(), "{run:?}");
    // Still at work: a conflict, and nothing recorded.
    let busy = a
        .manager
        .run_automation(&daily.id, RunAutomationRequest::default())
        .await;
    assert!(
        matches!(&busy, Err(Error::Conflict(why)) if why.contains("still")),
        "{busy:?}"
    );
    assert_eq!(a.runs(&daily.id).await.len(), 1);
    let with_number = a
        .manager
        .run_automation(&daily.id, RunAutomationRequest { number: Some(3) })
        .await;
    assert!(
        matches!(with_number, Err(Error::Invalid(_))),
        "{with_number:?}"
    );
    let played = play(&mut a.starts).await;
    a.finish(&played).await;

    // An issue trigger, switched on with nothing to mark.
    let trigger = a
        .manager
        .create_automation(automation(a.issue_trigger(None), "Fix #{{number}}", true))
        .await
        .unwrap();
    a.tick_at(T0 + MINUTE).await;
    a.gh.issues(&[(7, &[])]);
    let missing = a
        .manager
        .run_automation(&trigger.id, RunAutomationRequest::default())
        .await;
    assert!(matches!(missing, Err(Error::Invalid(_))), "{missing:?}");
    let unknown = a
        .manager
        .run_automation(&trigger.id, RunAutomationRequest { number: Some(99) })
        .await;
    assert!(matches!(unknown, Err(Error::Missing(_))), "{unknown:?}");
    let run = a
        .manager
        .run_automation(&trigger.id, RunAutomationRequest { number: Some(7) })
        .await
        .unwrap();
    assert_eq!(run.cause, "manual");
    let session = a
        .manager
        .get(run.session_id.as_ref().unwrap())
        .await
        .unwrap()
        .session;
    assert_eq!(session.prompt, "Fix #7");
    assert_eq!(session.issue.unwrap().number, 7);
    let seven = play(&mut a.starts).await;
    a.finish(&seven).await;
    // #7 ran by hand: the next poll does not run it again.
    a.tick_at(T0 + MINUTE + POLL_MS).await;
    assert_eq!(causes(&a.runs(&trigger.id).await), [started("manual")]);
}

#[tokio::test]
async fn in_dry_run_schedules_fire_and_triggers_do_not_poll() {
    let a = auto(true).await;
    let daily = a
        .manager
        .create_automation(automation(schedule("0 8 * * *"), "Morning", true))
        .await
        .unwrap();
    let trigger = a
        .manager
        .create_automation(automation(a.issue_trigger(None), "Fix #{{number}}", true))
        .await
        .unwrap();
    a.tick_at(T0 + MINUTE).await;
    a.tick_at(T0 + DAY).await;
    assert_eq!(causes(&a.runs(&daily.id).await), [started("schedule")]);
    assert_eq!(a.runs(&trigger.id).await, []);
    assert_eq!(a.gh.lists(), Vec::<String>::new());
    assert_eq!(a.get(&trigger.id).await.last_poll_at, None);
    // A manual run takes a sample issue.
    let run = a
        .manager
        .run_automation(&trigger.id, RunAutomationRequest { number: Some(4) })
        .await
        .unwrap();
    let session = a
        .manager
        .get(run.session_id.as_ref().unwrap())
        .await
        .unwrap()
        .session;
    assert!(session.issue.unwrap().title.starts_with("Sample issue: "));
}

#[tokio::test]
async fn the_cron_preview_says_what_an_expression_means_and_when_it_fires() {
    let a = auto(false).await;
    let preview = a.manager.cron_preview("0 8 * * 1-5");
    assert_eq!(preview.text.as_deref(), Some("Every weekday at 08:00"));
    // From Monday 07:59:30 at UTC+1.
    assert_eq!(
        preview.next,
        [T0 + 30_000, T0 + 30_000 + DAY, T0 + 30_000 + 2 * DAY]
    );
    assert_eq!(preview.problem, None);
    let wrong = a.manager.cron_preview("0 25 * * *");
    assert_eq!(wrong.text, None);
    assert_eq!(wrong.next, Vec::<i64>::new());
    assert!(wrong.problem.unwrap().contains("the hour field"));
}

#[tokio::test]
async fn the_scheduler_task_fires_what_is_due_on_its_own() {
    let clock = Arc::new(ManualClock::new(T0));
    let gh = TriggerGh::new();
    let home = TempDir::new().unwrap();
    let config = ManagerConfig {
        automations: true,
        ..auto_config(&clock, &gh)
    };
    let (manager, _starts) = manager_with(&home, true, config).await;
    // Due 30 seconds after the clock's now, which is real time to the task.
    let created = manager
        .create_automation(automation(schedule("0 8 * * *"), "Morning", true))
        .await
        .unwrap();
    // The clock moves on to the minute; the task wakes when it planned to
    // and finds it due. It is asked to look again now, so this takes no 30 s.
    clock.set(T0 + 31_000);
    manager.inner.scheduler.wake();
    let runs = within(async {
        loop {
            let runs = manager.automation_runs(&created.id).await.unwrap();
            if !runs.is_empty() {
                return runs;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(causes(&runs), [started("schedule")]);
    manager.shutdown().await;
}
