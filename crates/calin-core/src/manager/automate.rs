//! Running automations (integrations spec, section 3): saving them, firing
//! them on their schedule or for new GitHub items, and the background task
//! that wakes when the next one is due.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use calin_proto::{
    AttachRepoRequest, Automation, AutomationOutcome, AutomationRequest, AutomationRun,
    AutomationSession, AutomationTrigger, CreateSessionRequest, CronPreview, FromIssue,
    IssueDetail, PermissionMode, Repo, RunAutomationRequest, Session, SessionStatus,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use ulid::Ulid;

use super::{Inner, SessionManager, at_work, put_first, status_name};
use crate::automations::cron::Cron;
use crate::automations::template::{ITEM_VARS, SCHEDULE_VARS, Template, Values, Var};
use crate::automations::{MAX_FAILURES, MAX_NAME_CHARS, Stored, StoredRun, failures_in_a_row};
use crate::clock::local_date;
use crate::error::{Error, Result};
use crate::settings::filled;
use crate::store::LiveEnd;

/// The longest the scheduler sleeps without looking again: a clock that
/// jumped (a machine that slept) is noticed within this.
const MAX_SLEEP: Duration = Duration::from_secs(60 * 60);
/// How many times the cron preview shows.
const PREVIEW_COUNT: usize = 3;
const CAUSE_SCHEDULE: &str = "schedule";
const CAUSE_MANUAL: &str = "manual";

/// The scheduler's state in the manager.
pub(super) struct Scheduler {
    /// Wakes the background task to plan again: an automation changed.
    wake: Arc<Notify>,
    /// When the scheduler last looked. A schedule fires for a matching
    /// minute after it; set to the start time, so nothing missed while the
    /// server was down is made up.
    last_tick: Mutex<i64>,
    /// One fire at a time, so two never both find no active session.
    firing: tokio::sync::Mutex<()>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Scheduler {
    pub(super) fn new(now: i64) -> Self {
        Self {
            wake: Arc::new(Notify::new()),
            last_tick: Mutex::new(now),
            firing: tokio::sync::Mutex::new(()),
            tasks: Mutex::new(Vec::new()),
        }
    }

    fn last_tick(&self) -> MutexGuard<'_, i64> {
        // One read or one write per critical section.
        self.last_tick
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn tasks(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks the background task to look again now.
    pub(super) fn wake(&self) {
        self.wake.notify_one();
    }

    pub(super) fn stop(&self) {
        for task in self.tasks().drain(..) {
            task.abort();
        }
    }
}

/// Starts the background tasks: the one that fires automations when they
/// are due, and the one that switches off an automation whose sessions
/// keep failing as soon as the third one does.
pub(super) fn start(manager: &SessionManager) {
    let inner = Arc::downgrade(&manager.inner);
    let wake = Arc::clone(&manager.inner.scheduler.wake);
    let mut tasks = manager.inner.scheduler.tasks();
    tasks.push(tokio::spawn(schedule(inner.clone(), wake)));
    tasks.push(tokio::spawn(watch_failures(inner)));
}

async fn schedule(inner: Weak<Inner>, wake: Arc<Notify>) {
    loop {
        let sleep = {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let manager = SessionManager { inner };
            manager.automations_tick().await;
            let now = manager.inner.config.clock.now_ms();
            match manager.next_due(now).await {
                Some(at) => Duration::from_millis(at.saturating_sub(now).max(0) as u64),
                None => MAX_SLEEP,
            }
            .min(MAX_SLEEP)
        };
        tokio::select! {
            () = tokio::time::sleep(sleep) => {}
            () = wake.notified() => {}
        }
    }
}

async fn watch_failures(inner: Weak<Inner>) {
    let subscribed = match inner.upgrade() {
        Some(inner) => inner.store.subscribe_sessions().await,
        None => return,
    };
    let Ok((_, mut live)) = subscribed else {
        return;
    };
    loop {
        let failed = match live.recv().await {
            Ok(session) if session.status == SessionStatus::Failed => session.automation_id,
            Ok(_) => None,
            // Some were missed: the next tick looks at every automation.
            Err(LiveEnd::Lagged(_)) => None,
            Err(LiveEnd::Closed) => return,
        };
        let (Some(id), Some(inner)) = (failed, inner.upgrade()) else {
            continue;
        };
        SessionManager { inner }.check_failures(&id).await;
    }
}

/// A GitHub item an automation fires for.
#[derive(Clone, Debug)]
struct Item {
    number: u32,
    title: String,
    body: String,
    url: String,
    author: Option<String>,
}

impl From<IssueDetail> for Item {
    fn from(detail: IssueDetail) -> Self {
        Self {
            number: detail.issue.number,
            title: detail.issue.title,
            body: detail.body,
            url: detail.issue.url,
            author: detail.issue.author,
        }
    }
}

impl From<calin_git::PullRequestItem> for Item {
    fn from(item: calin_git::PullRequestItem) -> Self {
        Self {
            number: item.number,
            title: item.title,
            body: item.body,
            url: item.url,
            author: item.author,
        }
    }
}

/// Why an automation fires.
struct Firing {
    /// In words, as the run records it.
    cause: String,
    item: Option<Item>,
    /// Asked for through the API: a session still active is a conflict,
    /// not a skip.
    manual: bool,
}

fn is_schedule(trigger: &AutomationTrigger) -> bool {
    matches!(trigger, AutomationTrigger::Schedule { .. })
}

fn unknown(id: &str) -> Error {
    Error::Missing(format!("unknown automation {id}"))
}

fn items<T: Into<Item>>(found: Vec<T>) -> Vec<Item> {
    found.into_iter().map(Into::into).collect()
}

/// A fire while the automation's previous session is still at work: a
/// conflict when asked for through the API, else a skipped run.
fn skip(run: &mut StoredRun, active: &Session, manual: bool) -> Result<()> {
    let why = format!(
        "its previous session {} is still {}",
        active.id,
        status_name(active.status)
    );
    if manual {
        return Err(Error::Conflict(format!(
            "the automation cannot run now: {why}"
        )));
    }
    run.outcome = AutomationOutcome::Skipped;
    run.detail = Some(why);
    Ok(())
}

/// Gives a stored automation the definition `request` has.
fn redefine(stored: &mut Stored, request: AutomationRequest, now: i64) {
    let new_trigger = stored.trigger != request.trigger;
    let switched_on = request.enabled && !stored.enabled;
    look_again(stored, new_trigger, switched_on, now);
    let watches = request.enabled && !is_schedule(&request.trigger);
    stored.needs_baseline = watches && (new_trigger || switched_on || stored.needs_baseline);
    stored.name = request.name;
    stored.enabled = request.enabled;
    stored.trigger = request.trigger;
    stored.session = request.session;
}

/// A trigger that is new forgets what it saw; one that is new or switched
/// on polls at once; one switched on forgets why it was switched off.
fn look_again(stored: &mut Stored, new_trigger: bool, switched_on: bool, now: i64) {
    if new_trigger {
        stored.seen.clear();
    }
    if new_trigger || switched_on {
        stored.last_poll_at = None;
        stored.last_poll_problem = None;
    }
    if switched_on {
        stored.enabled_at = Some(now);
        stored.disabled_reason = None;
    }
}

fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("the name must not be blank".to_owned());
    }
    let too_long = name.chars().count() > MAX_NAME_CHARS;
    too_long.then(|| format!("the name must be at most {MAX_NAME_CHARS} characters"))
}

fn is_known(repos: &[Repo], repo_id: &str) -> bool {
    repos.iter().any(|repo| repo.id == repo_id)
}

/// Checks a trigger and tidies it: one space between the fields of a
/// cron expression, a trimmed label. Returns what its prompt may use.
fn check_trigger(
    trigger: &mut AutomationTrigger,
    repos: &[Repo],
    problems: &mut Vec<String>,
) -> &'static [Var] {
    match trigger {
        AutomationTrigger::Schedule { cron } => {
            *cron = cron.split_whitespace().collect::<Vec<_>>().join(" ");
            if let Err(why) = Cron::parse(cron) {
                problems.push(format!("the schedule is not valid: {why}"));
            }
            return SCHEDULE_VARS;
        }
        AutomationTrigger::Issue { repo_id, label } => {
            problems.extend(trigger_repo_problem(repos, repo_id));
            problems.extend(label.as_mut().and_then(label_problem));
        }
        AutomationTrigger::PullRequest { repo_id } => {
            problems.extend(trigger_repo_problem(repos, repo_id));
        }
    }
    ITEM_VARS
}

fn trigger_repo_problem(repos: &[Repo], repo_id: &str) -> Option<String> {
    (!is_known(repos, repo_id)).then(|| format!("the trigger's repo {repo_id} is unknown"))
}

/// Trims the label.
fn label_problem(label: &mut String) -> Option<String> {
    *label = label.trim().to_owned();
    label
        .is_empty()
        .then(|| "the label must not be blank; leave it out to take every issue".to_owned())
}

/// The prompt must parse with what the trigger offers, and someone must be
/// able to answer what the session asks.
fn check_prompt(session: &AutomationSession, offered: &[Var], problems: &mut Vec<String>) {
    if session.prompt.trim().is_empty() {
        problems.push("the prompt must not be blank".to_owned());
    } else if let Err(why) = Template::parse(&session.prompt, offered) {
        problems.push(why);
    }
    if session.permission_mode == PermissionMode::Ask {
        problems.push(
            "the permission mode must be autonomous or accept_edits: nobody is there to \
             answer an automated session"
                .to_owned(),
        );
    }
}

/// Every repo of the session must be known, and listed once.
fn check_repos(wanted: &[AttachRepoRequest], repos: &[Repo], problems: &mut Vec<String>) {
    for (index, repo) in wanted.iter().enumerate() {
        if !is_known(repos, &repo.repo_id) {
            problems.push(format!("the session's repo {} is unknown", repo.repo_id));
        } else if wanted[..index]
            .iter()
            .any(|other| other.repo_id == repo.repo_id)
        {
            problems.push(format!("the session lists repo {} twice", repo.repo_id));
        }
    }
}

impl SessionManager {
    /// Every automation, by name.
    pub async fn list_automations(&self) -> Result<Vec<Automation>> {
        let (names, statuses) = self.lookups().await;
        let (now, zone) = (self.now(), &self.inner.config.time_zone);
        let mut all: Vec<Automation> = self
            .inner
            .automations
            .all()
            .await
            .iter()
            .map(|stored| stored.view(now, zone, &names, &statuses))
            .collect();
        all.sort_by(|a, b| {
            let key = |automation: &Automation| automation.name.to_lowercase();
            key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id))
        });
        Ok(all)
    }

    /// One automation. `Missing` when there is none with that id.
    pub async fn automation(&self, id: &str) -> Result<Automation> {
        let stored = self
            .inner
            .automations
            .get(id)
            .await
            .ok_or_else(|| unknown(id))?;
        Ok(self.view(&stored).await)
    }

    /// Saves a new automation. `Invalid`, naming every problem, when it
    /// does not validate. An enabled trigger starts by marking every item
    /// that exists as seen.
    pub async fn create_automation(&self, request: AutomationRequest) -> Result<Automation> {
        let request = self.validate_automation(request).await?;
        let now = self.now();
        let trigger = !is_schedule(&request.trigger);
        let stored = Stored {
            id: Ulid::generate().to_string(),
            name: request.name,
            enabled: request.enabled,
            trigger: request.trigger,
            session: request.session,
            created_at: now,
            enabled_at: request.enabled.then_some(now),
            disabled_reason: None,
            last_poll_at: None,
            last_poll_problem: None,
            seen: Default::default(),
            needs_baseline: request.enabled && trigger,
            runs: Vec::new(),
        };
        let saved = stored.clone();
        self.inner
            .automations
            .change(move |all| {
                all.push(saved);
                Ok(())
            })
            .await?;
        self.inner.scheduler.wake();
        Ok(self.view(&stored).await)
    }

    /// Replaces an automation's definition; its runs stay. Switching it on
    /// clears why it was switched off, and a trigger that is switched on
    /// or changed marks what exists as seen again. `Missing` for an
    /// unknown id, `Invalid` as for [`Self::create_automation`].
    pub async fn update_automation(
        &self,
        id: &str,
        request: AutomationRequest,
    ) -> Result<Automation> {
        let request = self.validate_automation(request).await?;
        let now = self.now();
        let stored = self
            .inner
            .automations
            .change(|all| {
                let stored = all
                    .iter_mut()
                    .find(|stored| stored.id == id)
                    .ok_or_else(|| unknown(id))?;
                redefine(stored, request, now);
                Ok(stored.clone())
            })
            .await?;
        self.inner.scheduler.wake();
        Ok(self.view(&stored).await)
    }

    /// Forgets an automation and its runs. The sessions it started stay.
    pub async fn delete_automation(&self, id: &str) -> Result<()> {
        self.inner
            .automations
            .change(|all| {
                let before = all.len();
                all.retain(|stored| stored.id != id);
                if all.len() == before {
                    return Err(unknown(id));
                }
                Ok(())
            })
            .await?;
        self.inner.scheduler.wake();
        Ok(())
    }

    /// Fires an automation now, enabled or not. An issue or pull request
    /// trigger runs on the item `request` names (`Invalid` without one,
    /// `Missing` when there is no such item); a schedule takes none.
    /// `Conflict` while its previous session is still at work.
    pub async fn run_automation(
        &self,
        id: &str,
        request: RunAutomationRequest,
    ) -> Result<AutomationRun> {
        let stored = self
            .inner
            .automations
            .get(id)
            .await
            .ok_or_else(|| unknown(id))?;
        let item = match (&stored.trigger, request.number) {
            (AutomationTrigger::Schedule { .. }, None) => None,
            (AutomationTrigger::Schedule { .. }, Some(_)) => {
                return Err(Error::Invalid(
                    "a schedule runs on no issue or pull request: leave out the number".to_owned(),
                ));
            }
            (_, None) => {
                return Err(Error::Invalid(
                    "the number of the issue or pull request to run on is required".to_owned(),
                ));
            }
            (AutomationTrigger::Issue { repo_id, .. }, Some(number)) => {
                Some(Item::from(self.issue(repo_id, number).await?))
            }
            (AutomationTrigger::PullRequest { repo_id }, Some(number)) => {
                Some(Item::from(self.pull_request_item(repo_id, number).await?))
            }
        };
        let firing = Firing {
            cause: CAUSE_MANUAL.to_owned(),
            item,
            manual: true,
        };
        self.fire(id, firing).await
    }

    /// The automation's last runs, newest first.
    pub async fn automation_runs(&self, id: &str) -> Result<Vec<AutomationRun>> {
        let stored = self
            .inner
            .automations
            .get(id)
            .await
            .ok_or_else(|| unknown(id))?;
        let statuses = self.statuses().await;
        Ok(stored.runs.iter().map(|run| run.view(&statuses)).collect())
    }

    /// What a cron expression means, and the next three times it fires.
    pub fn cron_preview(&self, expr: &str) -> CronPreview {
        match Cron::parse(expr) {
            Ok(cron) => CronPreview {
                text: Some(cron.describe()),
                next: cron.next_times(self.now(), &self.inner.config.time_zone, PREVIEW_COUNT),
                problem: None,
            },
            Err(problem) => CronPreview {
                text: None,
                next: Vec::new(),
                problem: Some(problem),
            },
        }
    }

    /// What the scheduler does each time it wakes: fire the schedules that
    /// matched a minute since it last looked, ask GitHub for the triggers
    /// whose poll is due, and switch off what keeps failing. The background
    /// task calls it; so do tests, with a clock of their own.
    pub async fn automations_tick(&self) {
        let now = self.now();
        let since = std::mem::replace(&mut *self.inner.scheduler.last_tick(), now);
        for stored in self.inner.automations.all().await {
            if !stored.enabled || self.check_failures(&stored.id).await {
                continue;
            }
            match &stored.trigger {
                AutomationTrigger::Schedule { cron } => {
                    self.fire_if_due(&stored, cron, since, now).await;
                }
                // Dry run: GitHub triggers do not poll.
                _ if self.inner.registry.dry_run() => {}
                _ => self.poll_if_due(&stored, now).await,
            }
        }
    }

    /// Fires a schedule that matched a minute after `since`.
    async fn fire_if_due(&self, stored: &Stored, cron: &str, since: i64, now: i64) {
        let Ok(cron) = Cron::parse(cron) else {
            return;
        };
        // Not for a minute before it was switched on.
        let from = since.max(stored.enabled_at.unwrap_or(since));
        let next = cron.next_after(from, &self.inner.config.time_zone);
        if !next.is_some_and(|at| at <= now) {
            return;
        }
        let firing = Firing {
            cause: CAUSE_SCHEDULE.to_owned(),
            item: None,
            manual: false,
        };
        self.fire_or_warn(&stored.id, firing).await;
    }

    async fn poll_if_due(&self, stored: &Stored, now: i64) {
        let poll_every = self.poll_every();
        let due = stored
            .last_poll_at
            .is_none_or(|at| now.saturating_sub(at) >= poll_every);
        if due {
            self.poll(stored, now).await;
        }
    }

    fn poll_every(&self) -> i64 {
        self.inner.config.trigger_poll.as_millis() as i64
    }

    /// When the next automation is due, if any is.
    async fn next_due(&self, now: i64) -> Option<i64> {
        let poll_every = self.poll_every();
        let zone = &self.inner.config.time_zone;
        let dry_run = self.inner.registry.dry_run();
        self.inner
            .automations
            .all()
            .await
            .iter()
            .filter(|stored| stored.enabled)
            .filter_map(|stored| match &stored.trigger {
                AutomationTrigger::Schedule { cron } => {
                    Cron::parse(cron).ok()?.next_after(now, zone)
                }
                _ if dry_run => None,
                _ => Some(stored.last_poll_at.map_or(now, |at| at + poll_every)),
            })
            .min()
    }

    /// Asks GitHub for the trigger's open items and fires once for each
    /// that was not seen, oldest first, until one is skipped: that one and
    /// the rest stay unseen for the next poll. A first poll after the
    /// trigger was switched on only marks what is there as seen. A poll
    /// that fails is recorded and tried again at the next one.
    async fn poll(&self, stored: &Stored, now: i64) {
        let Some(items) = self.open_items(&stored.trigger).await else {
            return;
        };
        let Some(seen) = self.record_poll(&stored.id, now, &items).await else {
            return;
        };
        let Ok(mut items) = items else {
            return;
        };
        items.retain(|item| !seen.contains(&item.number));
        items.sort_by_key(|item| item.number);
        self.fire_new(stored, items).await;
    }

    /// The trigger's open items on GitHub; `None` for a schedule.
    async fn open_items(&self, trigger: &AutomationTrigger) -> Option<Result<Vec<Item>>> {
        let items = match trigger {
            AutomationTrigger::Issue { repo_id, label } => {
                self.open_issues(repo_id, label.clone()).await.map(items)
            }
            AutomationTrigger::PullRequest { repo_id } => {
                self.open_pull_requests(repo_id).await.map(items)
            }
            AutomationTrigger::Schedule { .. } => return None,
        };
        Some(items)
    }

    /// Records when the trigger was polled and what went wrong. Returns the
    /// items seen before, or `None` when there is nothing to fire: the
    /// automation is gone, the poll could not be recorded, or it only marked
    /// what is there as seen.
    async fn record_poll(
        &self,
        id: &str,
        now: i64,
        items: &Result<Vec<Item>>,
    ) -> Option<BTreeSet<u32>> {
        let numbers: Vec<u32> = items.iter().flatten().map(|item| item.number).collect();
        let problem = items.as_ref().err().map(ToString::to_string);
        let recorded = self
            .inner
            .automations
            .change(|all| {
                let Some(stored) = all.iter_mut().find(|stored| stored.id == id) else {
                    return Ok(None);
                };
                stored.last_poll_at = Some(now);
                stored.last_poll_problem = problem.clone();
                if problem.is_none() && stored.needs_baseline {
                    stored.seen.extend(numbers.iter().copied());
                    stored.needs_baseline = false;
                    return Ok(None);
                }
                Ok(Some(stored.seen.clone()))
            })
            .await;
        recorded.unwrap_or_else(|err| {
            tracing::warn!(automation = %id, %err, "could not record the poll");
            None
        })
    }

    /// Fires once per item, in order, until one is skipped or cannot fire.
    async fn fire_new(&self, stored: &Stored, items: Vec<Item>) {
        let noun = match stored.trigger {
            AutomationTrigger::Issue { .. } => "issue",
            _ => "pull request",
        };
        for item in items {
            let firing = Firing {
                cause: format!("{noun} #{}", item.number),
                item: Some(item),
                manual: false,
            };
            let fired = self.fire_or_warn(&stored.id, firing).await;
            if fired.is_none_or(|run| run.outcome == AutomationOutcome::Skipped) {
                break;
            }
        }
    }

    /// [`Self::fire`] for the scheduler, which has nobody to tell but the log.
    async fn fire_or_warn(&self, id: &str, firing: Firing) -> Option<AutomationRun> {
        self.fire(id, firing)
            .await
            .inspect_err(|err| {
                tracing::warn!(automation = %id, %err, "could not fire the automation");
            })
            .ok()
    }

    /// Starts the automation's session, or records why it did not, and
    /// marks the item it fired for as seen unless it was skipped.
    async fn fire(&self, id: &str, firing: Firing) -> Result<AutomationRun> {
        let _one_at_a_time = self.inner.scheduler.firing.lock().await;
        let stored = self
            .inner
            .automations
            .get(id)
            .await
            .ok_or_else(|| unknown(id))?;
        let now = self.now();
        let mut run = StoredRun {
            id: Ulid::generate().to_string(),
            at: now,
            cause: firing.cause.clone(),
            outcome: AutomationOutcome::Started,
            session_id: None,
            detail: None,
        };
        match self.active_session(id).await? {
            Some(active) => skip(&mut run, &active, firing.manual)?,
            None => {
                let item = firing.item.as_ref();
                self.start_session(&mut run, &stored, item, now).await;
            }
        }
        let seen = firing
            .item
            .as_ref()
            .map(|item| item.number)
            .filter(|_| run.outcome != AutomationOutcome::Skipped);
        self.keep_run(id, run.clone(), seen).await?;
        drop(_one_at_a_time);
        if run.outcome == AutomationOutcome::Failed {
            self.check_failures(id).await;
        }
        let statuses = self.statuses().await;
        Ok(run.view(&statuses))
    }

    /// The automation's session that is still at work, if there is one.
    async fn active_session(&self, id: &str) -> Result<Option<Session>> {
        let sessions = self.inner.store.sessions().await?;
        Ok(sessions.into_iter().find(|session| {
            session.automation_id.as_deref() == Some(id) && at_work(session.status)
        }))
    }

    /// Starts the run's session. A run whose session could not start says why.
    async fn start_session(
        &self,
        run: &mut StoredRun,
        stored: &Stored,
        item: Option<&Item>,
        now: i64,
    ) {
        let created = match self.session_request(stored, item, now) {
            Ok(request) => self.create(request, Some(stored.id.clone())).await,
            Err(problem) => Err(Error::Invalid(problem)),
        };
        match created {
            Ok(detail) => run.session_id = Some(detail.session.id),
            Err(err) => {
                run.outcome = AutomationOutcome::Failed;
                run.detail = Some(err.to_string());
            }
        }
    }

    /// Keeps the run, and the item it fired for as seen. Deleted meanwhile:
    /// its session still runs, and nothing is kept.
    async fn keep_run(&self, id: &str, run: StoredRun, seen: Option<u32>) -> Result<()> {
        self.inner
            .automations
            .change(|all| {
                if let Some(stored) = all.iter_mut().find(|stored| stored.id == id) {
                    stored.record(run);
                    stored.seen.extend(seen);
                }
                Ok(())
            })
            .await
    }

    /// The session request of one fire: the rendered prompt, the issue for
    /// an issue trigger, the trigger's repo first for a pull request one.
    fn session_request(
        &self,
        stored: &Stored,
        item: Option<&Item>,
        now: i64,
    ) -> std::result::Result<CreateSessionRequest, String> {
        let offered = if is_schedule(&stored.trigger) {
            SCHEDULE_VARS
        } else {
            ITEM_VARS
        };
        let template = Template::parse(&stored.session.prompt, offered)?;
        let mut values = Values {
            date: local_date(&self.inner.config.time_zone, now).to_string(),
            ..Values::default()
        };
        if let Some(item) = item {
            values.number = item.number.to_string();
            values.title = item.title.clone();
            values.body = item.body.clone();
            values.url = item.url.clone();
            values.author = item.author.clone().unwrap_or_default();
        }
        let mut repos = stored.session.repos.clone();
        let issue = match (&stored.trigger, item) {
            (AutomationTrigger::Issue { repo_id, .. }, Some(item)) => Some(FromIssue {
                repo_id: repo_id.clone(),
                number: item.number,
            }),
            (AutomationTrigger::PullRequest { repo_id }, Some(_)) => {
                put_first(&mut repos, repo_id);
                None
            }
            _ => None,
        };
        let title = match item {
            Some(item) => format!("{} #{}", stored.name, item.number),
            None => stored.name.clone(),
        };
        Ok(CreateSessionRequest {
            prompt: template.render(&values),
            title: Some(title),
            runner: stored.session.runner.clone(),
            model: stored.session.model.clone(),
            permission_mode: Some(stored.session.permission_mode),
            workflow: stored.session.workflow.clone(),
            issue,
            repos,
        })
    }

    /// Switches the automation off when its last runs since it was switched
    /// on failed [`MAX_FAILURES`] times in a row. True when it is off now.
    async fn check_failures(&self, id: &str) -> bool {
        let statuses = self.statuses().await;
        let switched = self
            .inner
            .automations
            .change(|all| {
                let Some(stored) = all.iter_mut().find(|stored| stored.id == id) else {
                    return Ok(true);
                };
                if !stored.enabled {
                    return Ok(true);
                }
                let failed = failures_in_a_row(&stored.runs, stored.enabled_at, &statuses);
                if failed < MAX_FAILURES {
                    return Ok(false);
                }
                let last = stored.runs.first().map(|run| match (&run.detail, &run.session_id) {
                    (Some(detail), _) => detail.clone(),
                    (None, Some(session)) => format!("session {session} failed"),
                    (None, None) => "it failed".to_owned(),
                });
                stored.enabled = false;
                stored.needs_baseline = false;
                stored.disabled_reason = Some(format!(
                    "Switched off after {failed} runs in a row failed; the last one: {}",
                    last.unwrap_or_default()
                ));
                tracing::warn!(automation = %id, "switched an automation off after repeated failures");
                Ok(true)
            })
            .await;
        switched.unwrap_or_else(|err| {
            tracing::warn!(automation = %id, %err, "could not switch the automation off");
            false
        })
    }

    /// Checks a request, trims its name and blank options, and says every
    /// problem it has at once.
    async fn validate_automation(&self, request: AutomationRequest) -> Result<AutomationRequest> {
        let mut problems: Vec<String> = Vec::new();
        let name = request.name.trim().to_owned();
        problems.extend(name_problem(&name));
        let repos = self.inner.repos.list().await;
        let mut trigger = request.trigger;
        let offered = check_trigger(&mut trigger, &repos, &mut problems);
        let mut session = request.session;
        check_prompt(&session, offered, &mut problems);
        session.workflow = filled(session.workflow);
        problems.extend(self.workflow_problem(session.workflow.as_deref()).await);
        check_repos(&session.repos, &repos, &mut problems);
        session.runner = filled(session.runner);
        problems.extend(self.runner_problem(session.runner.as_deref()));
        session.model = filled(session.model);
        if !problems.is_empty() {
            return Err(Error::Invalid(problems.join("; ")));
        }
        Ok(AutomationRequest {
            name,
            enabled: request.enabled,
            trigger,
            session,
        })
    }

    async fn workflow_problem(&self, workflow: Option<&str>) -> Option<String> {
        let workflow = workflow?;
        let resolved = self.inner.workflows.resolve(Some(workflow)).await;
        resolved.err().map(|err| err.to_string())
    }

    fn runner_problem(&self, runner: Option<&str>) -> Option<String> {
        let runner = runner?;
        let unknown = self.inner.registry.resolve(Some(runner)).is_none();
        unknown.then(|| format!("unknown runner {runner}"))
    }

    async fn view(&self, stored: &Stored) -> Automation {
        let (names, statuses) = self.lookups().await;
        stored.view(self.now(), &self.inner.config.time_zone, &names, &statuses)
    }

    /// Repo names by id, and session statuses by id.
    async fn lookups(&self) -> (HashMap<String, String>, HashMap<String, SessionStatus>) {
        let names = self
            .inner
            .repos
            .list()
            .await
            .into_iter()
            .map(|repo| (repo.id, repo.name))
            .collect();
        (names, self.statuses().await)
    }

    async fn statuses(&self) -> HashMap<String, SessionStatus> {
        match self.inner.store.sessions().await {
            Ok(sessions) => sessions
                .into_iter()
                .map(|session| (session.id, session.status))
                .collect(),
            Err(err) => {
                tracing::warn!(%err, "could not read the sessions' statuses");
                HashMap::new()
            }
        }
    }

    fn now(&self) -> i64 {
        self.inner.config.clock.now_ms()
    }
}
