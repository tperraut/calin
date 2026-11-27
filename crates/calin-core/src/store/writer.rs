//! The store's writer: one thread that owns the SQLite connection and every
//! log file. Running all appends and all subscription cuts on one thread is
//! what makes sequence numbers gapless and replay exact without any locking.

use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, ErrorKind, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use calin_proto::{
    AttachRepoRequest, Event, EventBody, Graph, Session, SessionDetail, SessionStatus, Timeline,
    Usage, WorkflowRun,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use ulid::Ulid;

use super::index::{self, Indexed};
use super::{
    Dispatch, EventDraft, NewSession, OpenPermission, OpenQuestion, SessionSetup, UsageQuery, log,
    usage, views,
};
use crate::error::{Error, Result};
use crate::home::Home;
use crate::title::default_title;

/// Live events a subscriber may fall behind by before it is cut off and has
/// to subscribe again. Appends never wait for a subscriber.
pub(super) const LIVE_BUFFER: usize = 1024;
const UPSERT_BUFFER: usize = 256;

/// How long to wait for the lock on the data directory. A store that was
/// just closed in this process can still hold it for an instant: a child
/// process being spawned at that moment (git, an agent) has a copy of the
/// descriptor until it executes.
const LOCK_PATIENCE: Duration = Duration::from_millis(250);

/// How often a session that keeps receiving events is announced again, so
/// `head_seq` and `updated_at` in lists stay fresh without a frame per event.
pub(super) const UPSERT_INTERVAL: Duration = Duration::from_secs(1);

type Reply<T> = oneshot::Sender<Result<T>>;

pub(super) enum Cmd {
    CreateSession {
        new: NewSession,
        reply: Reply<Session>,
    },
    Append {
        session_id: String,
        draft: EventDraft,
        reply: Reply<Arc<Event>>,
    },
    Cut {
        session_id: String,
        after_seq: u64,
        reply: Reply<Cut>,
    },
    SubscribeSessions {
        reply: Reply<(Vec<Session>, broadcast::Receiver<Session>)>,
    },
    Sessions {
        reply: Reply<Vec<Session>>,
    },
    Session {
        session_id: String,
        reply: Reply<SessionDetail>,
    },
    Setup {
        session_id: String,
        reply: Reply<SessionSetup>,
    },
    RepoInUse {
        repo_id: String,
        reply: Reply<bool>,
    },
    PermissionResolved {
        session_id: String,
        request_id: String,
        reply: Reply<Option<bool>>,
    },
    OpenPermissions {
        session_id: Option<String>,
        reply: Reply<Vec<OpenPermission>>,
    },
    LastPullRequestNumber {
        repo_id: String,
        url_prefix: String,
        reply: Reply<u32>,
    },
    QuestionAnswered {
        session_id: String,
        question_id: String,
        reply: Reply<Option<bool>>,
    },
    OpenQuestions {
        session_id: Option<String>,
        reply: Reply<Vec<OpenQuestion>>,
    },
    Graph {
        session_id: String,
        at: Option<i64>,
        reply: Reply<Graph>,
    },
    Timeline {
        session_id: String,
        reply: Reply<Timeline>,
    },
    WorkflowRun {
        session_id: String,
        reply: Reply<Option<WorkflowRun>>,
    },
    TreeCost {
        session_id: String,
        reply: Reply<f64>,
    },
    Usage {
        query: UsageQuery,
        reply: Reply<Usage>,
    },
    /// Sent by the store's timer when a delayed upsert is due.
    FlushUpserts,
    Close {
        reply: oneshot::Sender<()>,
    },
}

/// The point where a subscription switches from the log to live delivery.
pub(super) struct Cut {
    pub head_seq: u64,
    /// Byte range of the log holding the backlog; `None` when there is none.
    pub backlog: Option<(u64, u64)>,
    /// Receives exactly the events with `seq > head_seq`.
    pub live: broadcast::Receiver<Arc<Event>>,
}

/// What the writer remembers about a session it has touched.
struct LogState {
    head_seq: u64,
    /// Length of the log in bytes, which is where the next line starts.
    len: u64,
    /// Open while the session is being written to.
    file: Option<File>,
    /// Present while somebody is subscribed.
    live: Option<broadcast::Sender<Arc<Event>>>,
    /// When the session was last announced on the sessions topic.
    upserted: Option<Instant>,
    /// Events arrived since then; an upsert is owed once the interval is over.
    stale: bool,
}

impl LogState {
    fn new(indexed: Indexed) -> Self {
        Self {
            head_seq: indexed.head_seq,
            len: indexed.log_len,
            file: None,
            live: None,
            upserted: None,
            stale: false,
        }
    }

    /// The log open for appending, opened at `path` when it is not open yet.
    fn file(&mut self, path: &Path) -> Result<&mut File> {
        Ok(match &mut self.file {
            Some(file) => file,
            empty => empty.insert(log::open_for_append(path)?),
        })
    }

    fn send_live(&mut self, event: &Arc<Event>) {
        if let Some(live) = &self.live
            && live.send(Arc::clone(event)).is_err()
        {
            // Nobody is subscribed any more.
            self.live = None;
        }
    }
}

/// Appends a whole line at `offset`, the end of the log, or nothing: half a
/// line must not stay behind, the next event would be glued to it.
fn write_line(file: &mut File, line: &[u8], offset: u64) -> std::io::Result<()> {
    let written = file.write_all(line).and_then(|()| file.flush());
    if written.is_err() {
        let _ = file.set_len(offset);
    }
    written
}

/// `session.json`: what a session was created with. The session itself,
/// plus the parts of the request that no event and no wire type carries.
#[derive(Serialize, Deserialize)]
struct Meta {
    #[serde(flatten)]
    session: Session,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    repos: Vec<AttachRepoRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dispatch: Option<Dispatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<String>,
}

pub(super) struct Writer {
    home: Home,
    /// Locked for as long as the writer lives.
    _lock: File,
    db: Connection,
    ids: ulid::Generator,
    logs: HashMap<String, LogState>,
    upserts: broadcast::Sender<Session>,
    /// Wakes the store's timer, which answers with [`Cmd::FlushUpserts`] at
    /// the given instant. The writer thread itself only ever waits for commands.
    timer: mpsc::UnboundedSender<Instant>,
    /// True while a wake-up is pending, so there is at most one at a time.
    timer_set: bool,
}

impl Writer {
    /// Opens the index and brings it in line with the logs on disk.
    pub(super) fn open(home: Home, timer: mpsc::UnboundedSender<Instant>) -> Result<Self> {
        std::fs::create_dir_all(home.sessions_dir())?;
        let lock = lock(&home)?;
        let db = index::open(&home.db_path())?;
        let mut writer = Self {
            home,
            _lock: lock,
            db,
            ids: ulid::Generator::new(),
            logs: HashMap::new(),
            upserts: broadcast::channel(UPSERT_BUFFER).0,
            timer,
            timer_set: false,
        };
        writer.catch_up()?;
        Ok(writer)
    }

    pub(super) fn run(mut self, mut commands: mpsc::Receiver<Cmd>) {
        // A dropped reply means the caller stopped waiting; nothing to do about it.
        while let Some(cmd) = commands.blocking_recv() {
            match cmd {
                Cmd::CreateSession { new, reply } => {
                    let _ = reply.send(self.create_session(new));
                }
                Cmd::Append {
                    session_id,
                    draft,
                    reply,
                } => {
                    let _ = reply.send(self.append(&session_id, draft));
                }
                Cmd::Cut {
                    session_id,
                    after_seq,
                    reply,
                } => {
                    let _ = reply.send(self.cut(&session_id, after_seq));
                }
                Cmd::SubscribeSessions { reply } => {
                    let _ = reply.send(self.subscribe_sessions());
                }
                Cmd::Sessions { reply } => {
                    let _ = reply.send(index::sessions(&self.db));
                }
                Cmd::Session { session_id, reply } => {
                    let _ = reply.send(self.detail(&session_id));
                }
                Cmd::Setup { session_id, reply } => {
                    let _ = reply.send(self.setup(&session_id));
                }
                Cmd::RepoInUse { repo_id, reply } => {
                    let _ = reply.send(index::repo_in_use(&self.db, &repo_id));
                }
                Cmd::PermissionResolved {
                    session_id,
                    request_id,
                    reply,
                } => {
                    let _ = reply.send(index::permission_resolved(
                        &self.db,
                        &session_id,
                        &request_id,
                    ));
                }
                Cmd::OpenPermissions { session_id, reply } => {
                    let _ = reply.send(index::open_permissions(&self.db, session_id.as_deref()));
                }
                Cmd::LastPullRequestNumber {
                    repo_id,
                    url_prefix,
                    reply,
                } => {
                    let _ = reply.send(index::last_pull_request_number(
                        &self.db,
                        &repo_id,
                        &url_prefix,
                    ));
                }
                Cmd::QuestionAnswered {
                    session_id,
                    question_id,
                    reply,
                } => {
                    let _ = reply.send(index::question_answered(
                        &self.db,
                        &session_id,
                        &question_id,
                    ));
                }
                Cmd::OpenQuestions { session_id, reply } => {
                    let _ = reply.send(index::open_questions(&self.db, session_id.as_deref()));
                }
                Cmd::Graph {
                    session_id,
                    at,
                    reply,
                } => {
                    let _ = reply.send(self.graph(session_id, at));
                }
                Cmd::Timeline { session_id, reply } => {
                    let _ = reply.send(self.timeline(session_id));
                }
                Cmd::WorkflowRun { session_id, reply } => {
                    let _ = reply.send(self.workflow_run(session_id));
                }
                Cmd::TreeCost { session_id, reply } => {
                    let _ = reply.send(index::tree_cost(&self.db, &session_id));
                }
                Cmd::Usage { query, reply } => {
                    let _ = reply.send(usage::usage(&self.db, &query));
                }
                Cmd::FlushUpserts => self.flush_upserts(),
                Cmd::Close { reply } => {
                    // Closing the connection first checkpoints the WAL before the caller moves on.
                    drop(self);
                    let _ = reply.send(());
                    return;
                }
            }
        }
    }

    fn subscribe_sessions(&self) -> Result<(Vec<Session>, broadcast::Receiver<Session>)> {
        let receiver = self.upserts.subscribe();
        index::sessions(&self.db).map(|all| (all, receiver))
    }

    fn graph(&self, session_id: String, at: Option<i64>) -> Result<Graph> {
        views::graph(&self.db, &session_id, at)?.ok_or(Error::NotFound(session_id))
    }

    fn timeline(&self, session_id: String) -> Result<Timeline> {
        views::timeline(&self.db, &session_id)?.ok_or(Error::NotFound(session_id))
    }

    /// `None` for a session that runs no workflow; an unknown session is not found.
    fn workflow_run(&self, session_id: String) -> Result<Option<WorkflowRun>> {
        if index::indexed(&self.db, &session_id)?.is_none() {
            return Err(Error::NotFound(session_id));
        }
        index::workflow_run(&self.db, &session_id)
    }

    fn create_session(&mut self, new: NewSession) -> Result<Session> {
        let id = self.next_id();
        let now = now_ms();
        let session = Session {
            id: id.clone(),
            title: new.title,
            prompt: new.prompt,
            status: SessionStatus::Queued,
            runner: new.runner,
            permission_mode: new.permission_mode,
            workflow: new.workflow,
            issue: new.issue,
            automation_id: new.automation_id,
            branch: None,
            parent_session_id: new
                .dispatch
                .as_ref()
                .map(|dispatch| dispatch.parent_session_id.clone()),
            root_session_id: match &new.dispatch {
                Some(dispatch) => dispatch.root_session_id.clone(),
                None => id.clone(),
            },
            head_seq: 0,
            created_at: now,
            updated_at: now,
        };
        let meta = Meta {
            session,
            model: new.model,
            repos: new.repos,
            dispatch: new.dispatch,
            context: new.context,
        };
        // Disk before index, like events: a reindex must be able to find the session.
        std::fs::create_dir_all(self.home.session_dir(&id))?;
        std::fs::write(self.home.meta_path(&id), serde_json::to_vec(&meta)?)?;
        File::create(self.home.log_path(&id))?;
        let session = meta.session;
        index::insert_session(&self.db, &session)?;
        let mut state = LogState::new(Indexed {
            head_seq: 0,
            log_len: 0,
        });
        // Creation is announced right here, so the interval starts now.
        state.upserted = Some(Instant::now());
        self.logs.insert(id, state);
        let _ = self.upserts.send(session.clone());
        Ok(session)
    }

    fn append(&mut self, session_id: &str, draft: EventDraft) -> Result<Arc<Event>> {
        self.load(session_id)?;
        let id = self.next_id();
        let Some(state) = self.logs.get_mut(session_id) else {
            return Err(Error::NotFound(session_id.to_owned()));
        };
        let event = Event {
            id,
            session_id: session_id.to_owned(),
            seq: state.head_seq + 1,
            ts: now_ms(),
            agent_id: draft.agent_id,
            parent_id: draft.parent_id,
            body: draft.body,
        };
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        let offset = state.len;
        let end = offset + line.len() as u64;

        // Log first. The file is unbuffered, so once `write_all` returns the
        // line is with the kernel and survives a crash of this process.
        let file = state.file(&self.home.log_path(session_id))?;
        if let Err(err) = write_line(file, &line, offset) {
            self.logs.remove(session_id);
            return Err(err.into());
        }

        // Only now may the index and the subscribers learn of the event.
        if let Err(err) = index_in_transaction(&mut self.db, &event, offset, end) {
            // The log has the event and the index does not. Forgetting the
            // session makes the next access catch up from the log, and
            // dropping `live` disconnects subscribers so they replay it.
            self.logs.remove(session_id);
            return Err(err);
        }
        state.head_seq = event.seq;
        state.len = end;

        let event = Arc::new(event);
        state.send_live(&event);
        self.upsert_after(session_id, &event.body);
        Ok(event)
    }

    /// Announces the session after an event was appended: a status change
    /// at once, anything else at most once per interval.
    fn upsert_after(&mut self, session_id: &str, body: &EventBody) {
        let Some(state) = self.logs.get_mut(session_id) else {
            return;
        };
        let now = Instant::now();
        let due = state.upserted.map(|at| at + UPSERT_INTERVAL);
        if let EventBody::SessionStatus { status, .. } = body {
            if matches!(
                status,
                SessionStatus::Done | SessionStatus::Failed | SessionStatus::Cancelled
            ) {
                // Nothing more is expected; do not hold a descriptor per finished session.
                state.file = None;
            }
            self.upsert(session_id, now);
        } else if due.is_none_or(|due| due <= now) {
            self.upsert(session_id, now);
        } else if let Some(due) = due {
            // Too soon after the last one: owed when the interval is over.
            state.stale = true;
            self.set_timer(due);
        }
    }

    /// Announces the session as it is now on the sessions topic.
    fn upsert(&mut self, session_id: &str, now: Instant) {
        if let Some(state) = self.logs.get_mut(session_id) {
            state.upserted = Some(now);
            state.stale = false;
        }
        if let Ok(Some(session)) = index::session(&self.db, session_id) {
            let _ = self.upserts.send(session);
        }
    }

    fn set_timer(&mut self, due: Instant) {
        // A closed timer means the store is shutting down: nobody to tell.
        if !self.timer_set && self.timer.send(due).is_ok() {
            self.timer_set = true;
        }
    }

    /// Sends the upserts that are owed and due, and asks to be woken again
    /// for the ones that are not due yet.
    fn flush_upserts(&mut self) {
        self.timer_set = false;
        let now = Instant::now();
        let mut due_now = Vec::new();
        let mut next: Option<Instant> = None;
        for (id, state) in &self.logs {
            if !state.stale {
                continue;
            }
            match state.upserted.map(|at| at + UPSERT_INTERVAL) {
                Some(due) if due > now => next = Some(next.map_or(due, |at| at.min(due))),
                _ => due_now.push(id.clone()),
            }
        }
        for id in due_now {
            self.upsert(&id, now);
        }
        if let Some(due) = next {
            self.set_timer(due);
        }
    }

    fn cut(&mut self, session_id: &str, after_seq: u64) -> Result<Cut> {
        self.load(session_id)?;
        let Some(state) = self.logs.get_mut(session_id) else {
            return Err(Error::NotFound(session_id.to_owned()));
        };
        let backlog = if after_seq >= state.head_seq {
            None
        } else {
            let start =
                index::log_offset(&self.db, session_id, after_seq + 1)?.ok_or_else(|| {
                    Error::Corrupt {
                        session: session_id.to_owned(),
                        detail: format!("event {} is not indexed", after_seq + 1),
                    }
                })?;
            Some((start, state.len))
        };
        // Appends run on this thread too, so none can slip between reading
        // the head and creating the receiver.
        let live = state
            .live
            .get_or_insert_with(|| broadcast::channel(LIVE_BUFFER).0)
            .subscribe();
        Ok(Cut {
            head_seq: state.head_seq,
            backlog,
            live,
        })
    }

    fn detail(&self, session_id: &str) -> Result<SessionDetail> {
        let session = index::session(&self.db, session_id)?
            .ok_or_else(|| Error::NotFound(session_id.to_owned()))?;
        let agents = index::agents(&self.db, session_id)?;
        let repos = index::session_repos(&self.db, session_id)?;
        let pull_requests = index::pull_requests(&self.db, session_id)?;
        Ok(SessionDetail {
            session,
            agents,
            repos,
            pull_requests,
        })
    }

    /// The parts of the creation request that only `session.json` holds.
    /// A session whose file is gone or unreadable has none of them.
    fn setup(&self, session_id: &str) -> Result<SessionSetup> {
        // The id may come straight from a client: ask the index before the disk.
        if index::indexed(&self.db, session_id)?.is_none() {
            return Err(Error::NotFound(session_id.to_owned()));
        }
        let meta = std::fs::read(self.home.meta_path(session_id))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Meta>(&bytes).ok());
        Ok(match meta {
            Some(meta) => SessionSetup {
                model: meta.model,
                repos: meta.repos,
                dispatch: meta.dispatch,
                context: meta.context,
            },
            None => SessionSetup::default(),
        })
    }

    /// Makes sure the session's state is in memory and its index is level with its log.
    fn load(&mut self, session_id: &str) -> Result<()> {
        if self.logs.contains_key(session_id) {
            return Ok(());
        }
        // The id may come straight from a client: nothing touches the file
        // system until the index confirms the session exists.
        let Some(mut indexed) = index::indexed(&self.db, session_id)? else {
            return Err(Error::NotFound(session_id.to_owned()));
        };
        let log_len = log::len(&self.home.log_path(session_id))?;
        if log_len < indexed.log_len {
            // The index describes events the log no longer has. The log wins.
            index::reset_session(&self.db, session_id)?;
            indexed = Indexed {
                head_seq: 0,
                log_len: 0,
            };
        }
        if log_len > indexed.log_len {
            indexed = self.replay(session_id, indexed)?;
        }
        self.logs
            .insert(session_id.to_owned(), LogState::new(indexed));
        Ok(())
    }

    /// Indexes the log lines that follow `from` and returns where the index stands after.
    fn replay(&mut self, session_id: &str, from: Indexed) -> Result<Indexed> {
        let path = self.home.log_path(session_id);
        let mut reader = BufReader::new(File::open(&path)?);
        reader.seek(SeekFrom::Start(from.log_len))?;
        let tx = self.db.transaction()?;
        let (at, corrupt) = index_lines(&tx, &mut reader, &path, session_id, from)?;
        // What precedes a corrupt line is still good and stays indexed.
        tx.commit()?;
        match corrupt {
            Some(err) => Err(err),
            None => Ok(at),
        }
    }

    /// Startup: drop index rows whose session is gone from disk, index
    /// sessions the index has never seen (all of them when `calin.db` was
    /// deleted) and catch up on events logged but not indexed before a crash.
    fn catch_up(&mut self) -> Result<()> {
        for id in index::session_ids(&self.db)? {
            if !self.home.session_dir(&id).is_dir() {
                index::delete_session(&self.db, &id)?;
            }
        }
        for id in session_dirs(&self.home)? {
            // One unreadable session must not keep the others from loading.
            if let Err(err) = self.catch_up_session(&id) {
                tracing::warn!(session = %id, %err, "could not index the session from its log");
            }
        }
        Ok(())
    }

    fn catch_up_session(&mut self, session_id: &str) -> Result<()> {
        if index::indexed(&self.db, session_id)?.is_none() {
            let Some(session) = self.read_meta(session_id)? else {
                return Ok(());
            };
            index::insert_session(&self.db, &session)?;
        }
        self.load(session_id)
    }

    /// The session as it was created, before any event. `None` when the
    /// directory holds nothing to rebuild a session from.
    fn read_meta(&self, session_id: &str) -> Result<Option<Session>> {
        let mut session = match std::fs::read(self.home.meta_path(session_id)) {
            Ok(bytes) => serde_json::from_slice::<Meta>(&bytes)?.session,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return self.derive_meta(session_id);
            }
            Err(err) => return Err(err.into()),
        };
        if session.id != session_id {
            return Err(Error::Corrupt {
                session: session_id.to_owned(),
                detail: format!("session.json describes session {}", session.id),
            });
        }
        session.status = SessionStatus::Queued;
        session.head_seq = 0;
        session.updated_at = session.created_at;
        Ok(Some(session))
    }

    /// Best effort for a log without `session.json`: the prompt and the
    /// runner are in the events, a custom title is not.
    fn derive_meta(&self, session_id: &str) -> Result<Option<Session>> {
        let file = match File::open(self.home.log_path(session_id)) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        let Some(start) = log_start(file)? else {
            return Ok(None);
        };
        let created_at = start.created_at;
        let prompt = start.prompt.unwrap_or_default();
        Ok(Some(Session {
            id: session_id.to_owned(),
            title: default_title(&prompt),
            prompt,
            status: SessionStatus::Queued,
            runner: start.runner.unwrap_or_else(|| "unknown".to_owned()),
            permission_mode: Default::default(),
            // Which file it was is not in the log; the run itself still is.
            workflow: None,
            issue: None,
            automation_id: None,
            branch: None,
            parent_session_id: None,
            root_session_id: session_id.to_owned(),
            head_seq: 0,
            created_at,
            updated_at: created_at,
        }))
    }

    /// Ids from one generator sort in the order they were made, even within a millisecond.
    fn next_id(&mut self) -> String {
        match self.ids.generate() {
            Ok(id) => id,
            Err(overflow) => overflow.commit_overflow_increment(),
        }
        .to_string()
    }
}

/// What the first events of a log say: when it began, the first prompt and
/// the runner of the first agent.
struct LogStart {
    created_at: i64,
    prompt: Option<String>,
    runner: Option<String>,
}

/// Reads up to the first line that is not an event; `None` for a log
/// without a single event.
fn log_start(file: File) -> Result<Option<LogStart>> {
    let (mut created_at, mut prompt, mut runner) = (None, None, None);
    for line in BufReader::new(file).lines() {
        let Ok(event) = serde_json::from_str::<Event>(&line?) else {
            break;
        };
        created_at.get_or_insert(event.ts);
        match event.body {
            EventBody::UserMessage { text } if prompt.is_none() => prompt = Some(text),
            EventBody::AgentSpawned { agent } if runner.is_none() => {
                runner = Some(agent.runner);
            }
            _ => {}
        }
        if prompt.is_some() && runner.is_some() {
            break;
        }
    }
    Ok(created_at.map(|created_at| LogStart {
        created_at,
        prompt,
        runner,
    }))
}

/// The sessions on disk: the directories of `sessions/` named with a ULID.
fn session_dirs(home: &Home) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(home.sessions_dir())? {
        let entry = entry?;
        let Ok(id) = entry.file_name().into_string() else {
            continue;
        };
        if Ulid::from_string(&id).is_ok() && entry.path().is_dir() {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Two processes appending to the same logs would each hand out the same
/// sequence numbers, so the data directory belongs to one process at a time.
fn lock(home: &Home) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.lock_path())?;
    let deadline = Instant::now() + LOCK_PATIENCE;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(TryLockError::WouldBlock) => return Err(Error::InUse(home.root().to_owned())),
            // Some file systems cannot lock. Starting anyway beats refusing to run.
            Err(TryLockError::Error(err)) => {
                tracing::warn!(%err, "cannot lock the data directory; make sure only one server uses it");
                break;
            }
        }
    }
    Ok(file)
}

/// Indexes the lines `reader` has left, from `from` on, until the end, a
/// torn last line or a corrupt one. Returns where the index stands, and the
/// corruption when that is what stopped it.
fn index_lines(
    db: &Connection,
    reader: &mut BufReader<File>,
    path: &Path,
    session_id: &str,
    from: Indexed,
) -> Result<(Indexed, Option<Error>)> {
    let mut at = from;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)? as u64;
        if read == 0 {
            return Ok((at, None));
        }
        if line.pop() != Some(b'\n') {
            cut_torn_line(path, at.log_len)?;
            return Ok((at, None));
        }
        let event = match log::parse_line(&line, session_id, at.head_seq + 1) {
            Ok(event) => event,
            Err(err) => return Ok((at, Some(err))),
        };
        let end = at.log_len + read;
        index::index_event(db, &event, at.log_len, end)?;
        at = Indexed {
            head_seq: event.seq,
            log_len: end,
        };
    }
}

/// A crash interrupted the last write. Nobody saw that event, and the next
/// append must start on a line boundary.
fn cut_torn_line(path: &Path, len: u64) -> Result<()> {
    OpenOptions::new().write(true).open(path)?.set_len(len)?;
    Ok(())
}

fn index_in_transaction(db: &mut Connection, event: &Event, offset: u64, end: u64) -> Result<()> {
    let tx = db.transaction()?;
    index::index_event(&tx, event, offset, end)?;
    tx.commit()?;
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
