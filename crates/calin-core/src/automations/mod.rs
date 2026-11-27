//! Automations (integrations spec, section 3): what they are, where they
//! are kept, and the rules that need no session to check. Starting their
//! sessions is the manager's (`manager/automate.rs`).
//!
//! They live in `<home>/automations.json` with what goes with them: their
//! last runs, the trigger items already seen and the last poll. The file
//! is written whole and renamed into place; one that cannot be read means
//! no automations, with one warning, and is kept aside for the user.

pub(crate) mod cron;
pub(crate) mod template;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use calin_proto::{
    Automation, AutomationOutcome, AutomationRun, AutomationSession, AutomationTrigger,
    SessionStatus,
};
use serde::{Deserialize, Serialize};

use self::cron::Cron;
use crate::clock::TimeZone;
use crate::error::Result;
use crate::home::Home;
use crate::json_file;
use crate::repos::blocking;

/// The runs kept per automation, newest first.
pub(crate) const MAX_RUNS: usize = 50;
/// Runs in a row that ended `failed` before an automation switches off.
pub(crate) const MAX_FAILURES: usize = 3;
/// The longest name.
pub(crate) const MAX_NAME_CHARS: usize = 80;
/// What the copy of a file that could not be read is called.
const CORRUPT_SUFFIX: &str = "corrupt";

/// The file: a version, so that a later shape can tell an older one.
#[derive(Default, Serialize, Deserialize)]
struct File {
    version: u32,
    automations: Vec<Stored>,
}

const VERSION: u32 = 1;

/// An automation as it is kept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Stored {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub trigger: AutomationTrigger,
    pub session: AutomationSession,
    pub created_at: i64,
    /// When it was last switched on: failures before it do not count, and
    /// a schedule does not fire for a time before it.
    #[serde(default)]
    pub enabled_at: Option<i64>,
    #[serde(default)]
    pub disabled_reason: Option<String>,
    #[serde(default)]
    pub last_poll_at: Option<i64>,
    #[serde(default)]
    pub last_poll_problem: Option<String>,
    /// The trigger items that already fired, or existed when it was enabled.
    #[serde(default)]
    pub seen: BTreeSet<u32>,
    /// The next poll marks every item there is as seen, and fires for none.
    #[serde(default)]
    pub needs_baseline: bool,
    /// Newest first, at most [`MAX_RUNS`].
    #[serde(default)]
    pub runs: Vec<StoredRun>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct StoredRun {
    pub id: String,
    pub at: i64,
    pub cause: String,
    pub outcome: AutomationOutcome,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

impl StoredRun {
    pub(crate) fn view(&self, statuses: &HashMap<String, SessionStatus>) -> AutomationRun {
        AutomationRun {
            id: self.id.clone(),
            at: self.at,
            cause: self.cause.clone(),
            outcome: self.outcome,
            session_id: self.session_id.clone(),
            session_status: self
                .session_id
                .as_ref()
                .and_then(|id| statuses.get(id).copied()),
            detail: self.detail.clone(),
        }
    }
}

impl Stored {
    /// The automation as the API shows it at `now`.
    pub(crate) fn view(
        &self,
        now: i64,
        zone: &TimeZone,
        repo_names: &HashMap<String, String>,
        statuses: &HashMap<String, SessionStatus>,
    ) -> Automation {
        let schedule = match &self.trigger {
            AutomationTrigger::Schedule { cron } => Some(Cron::parse(cron)),
            _ => None,
        };
        let next_run_at = match &schedule {
            Some(Ok(cron)) if self.enabled => cron.next_after(now, zone),
            _ => None,
        };
        let is_trigger = schedule.is_none();
        Automation {
            id: self.id.clone(),
            name: self.name.clone(),
            enabled: self.enabled,
            trigger: self.trigger.clone(),
            session: self.session.clone(),
            trigger_text: trigger_text(&self.trigger, repo_names),
            next_run_at,
            last_poll_at: self.last_poll_at.filter(|_| is_trigger),
            last_poll_problem: self.last_poll_problem.clone().filter(|_| is_trigger),
            last_run: self.runs.first().map(|run| run.view(statuses)),
            disabled_reason: self.disabled_reason.clone(),
            created_at: self.created_at,
        }
    }

    /// Keeps a run, newest first, and forgets the oldest past [`MAX_RUNS`].
    pub(crate) fn record(&mut self, run: StoredRun) {
        self.runs.insert(0, run);
        self.runs.truncate(MAX_RUNS);
    }
}

/// The trigger in words: "Every weekday at 08:00", "New issue labelled
/// `agent` in api", "New pull request in web".
pub(crate) fn trigger_text(
    trigger: &AutomationTrigger,
    repo_names: &HashMap<String, String>,
) -> String {
    let name = |repo_id: &String| repo_names.get(repo_id).unwrap_or(repo_id).clone();
    match trigger {
        AutomationTrigger::Schedule { cron } => match Cron::parse(cron) {
            Ok(cron) => cron.describe(),
            Err(_) => format!(
                "Cron: {}",
                cron.split_whitespace().collect::<Vec<_>>().join(" ")
            ),
        },
        AutomationTrigger::Issue {
            repo_id,
            label: Some(label),
        } => format!("New issue labelled `{label}` in {}", name(repo_id)),
        AutomationTrigger::Issue { repo_id, .. } => format!("New issue in {}", name(repo_id)),
        AutomationTrigger::PullRequest { repo_id } => {
            format!("New pull request in {}", name(repo_id))
        }
    }
}

/// How many of the newest runs since `since` ended `failed`, in a row:
/// a run whose session could not be created, or whose session failed.
/// Skipped runs neither count nor break the row; anything else breaks it.
pub(crate) fn failures_in_a_row(
    runs: &[StoredRun],
    since: Option<i64>,
    statuses: &HashMap<String, SessionStatus>,
) -> usize {
    let mut failed = 0;
    for run in runs
        .iter()
        .filter(|run| since.is_none_or(|since| run.at >= since))
    {
        match run.outcome {
            AutomationOutcome::Skipped => {}
            AutomationOutcome::Failed => failed += 1,
            AutomationOutcome::Started => {
                let status = run.session_id.as_ref().and_then(|id| statuses.get(id));
                if status == Some(&SessionStatus::Failed) {
                    failed += 1;
                } else {
                    break;
                }
            }
        }
    }
    failed
}

/// The automations of a data directory, in memory, written back whole on
/// every change.
pub(crate) struct AutomationStore {
    path: PathBuf,
    state: tokio::sync::Mutex<Vec<Stored>>,
}

impl AutomationStore {
    pub(crate) async fn load(home: &Home) -> Self {
        let path = home.automations_path();
        let file = path.clone();
        let loaded = blocking(move || Ok(read(&file))).await.unwrap_or_default();
        Self {
            path,
            state: tokio::sync::Mutex::new(loaded),
        }
    }

    /// Every automation as it is now.
    pub(crate) async fn all(&self) -> Vec<Stored> {
        self.state.lock().await.clone()
    }

    pub(crate) async fn get(&self, id: &str) -> Option<Stored> {
        let state = self.state.lock().await;
        state.iter().find(|stored| stored.id == id).cloned()
    }

    /// Runs `change` on the automations, then writes them to the file. On
    /// an error from either, nothing changes.
    pub(crate) async fn change<T>(
        &self,
        change: impl FnOnce(&mut Vec<Stored>) -> Result<T>,
    ) -> Result<T> {
        let mut state = self.state.lock().await;
        let mut changed = state.clone();
        let answer = change(&mut changed)?;
        if changed != *state {
            let (path, file) = (
                self.path.clone(),
                File {
                    version: VERSION,
                    automations: changed.clone(),
                },
            );
            blocking(move || json_file::write(&path, &file)).await?;
            *state = changed;
        }
        Ok(answer)
    }
}

/// Blocking. The automations in the file; none when it is missing, and
/// none with a warning when it cannot be read, after keeping a copy of it
/// beside it so that the next write does not lose what the user had.
fn read(path: &Path) -> Vec<Stored> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "cannot read the automations; starting with none");
            return Vec::new();
        }
    };
    match serde_json::from_slice::<File>(&bytes) {
        Ok(file) => file.automations,
        Err(err) => {
            let aside = path.with_extension(format!("json.{CORRUPT_SUFFIX}"));
            let kept = std::fs::copy(path, &aside).is_ok();
            tracing::warn!(
                path = %path.display(),
                %err,
                kept = %if kept { aside.display().to_string() } else { "nowhere".to_owned() },
                "the automations file is corrupt; starting with none"
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests;
