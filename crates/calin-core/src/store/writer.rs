//! The store's writer: one thread that owns the SQLite connection and every
//! log file. Running all appends and all subscription cuts on one thread is
//! what makes sequence numbers gapless and replay exact without any locking.

use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, ErrorKind, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use calin_proto::{Event, EventBody, Session, SessionDetail, SessionStatus};
use rusqlite::Connection;
use tokio::sync::{broadcast, mpsc, oneshot};
use ulid::Ulid;

use super::index::{self, Indexed};
use super::{EventDraft, NewSession, log};
use crate::error::{Error, Result};
use crate::home::Home;
use crate::title::default_title;

/// Live events a subscriber may fall behind by before it is cut off and has
/// to subscribe again. Appends never wait for a subscriber.
pub(super) const LIVE_BUFFER: usize = 1024;
const UPSERT_BUFFER: usize = 256;

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
}

pub(super) struct Writer {
    home: Home,
    /// Locked for as long as the writer lives.
    _lock: File,
    db: Connection,
    ids: ulid::Generator,
    logs: HashMap<String, LogState>,
    upserts: broadcast::Sender<Session>,
}

impl Writer {
    /// Opens the index and brings it in line with the logs on disk.
    pub(super) fn open(home: Home) -> Result<Self> {
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
                    let receiver = self.upserts.subscribe();
                    let _ = reply.send(index::sessions(&self.db).map(|all| (all, receiver)));
                }
                Cmd::Sessions { reply } => {
                    let _ = reply.send(index::sessions(&self.db));
                }
                Cmd::Session { session_id, reply } => {
                    let _ = reply.send(self.detail(&session_id));
                }
                Cmd::Close { reply } => {
                    // Closing the connection first checkpoints the WAL before the caller moves on.
                    drop(self);
                    let _ = reply.send(());
                    return;
                }
            }
        }
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
            branch: None,
            parent_session_id: None,
            root_session_id: id.clone(),
            head_seq: 0,
            created_at: now,
            updated_at: now,
        };
        // Disk before index, like events: a reindex must be able to find the session.
        std::fs::create_dir_all(self.home.session_dir(&id))?;
        std::fs::write(self.home.meta_path(&id), serde_json::to_vec(&session)?)?;
        File::create(self.home.log_path(&id))?;
        index::insert_session(&self.db, &session)?;
        self.logs.insert(
            id,
            LogState {
                head_seq: 0,
                len: 0,
                file: None,
                live: None,
            },
        );
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
        let file = match &mut state.file {
            Some(file) => file,
            empty => empty.insert(log::open_for_append(&self.home.log_path(session_id))?),
        };
        if let Err(err) = file.write_all(&line).and_then(|()| file.flush()) {
            // Half a line must not stay behind: the next event would be glued to it.
            let _ = file.set_len(offset);
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
        if let Some(live) = &state.live
            && live.send(Arc::clone(&event)).is_err()
        {
            // Nobody is subscribed any more.
            state.live = None;
        }
        if let EventBody::SessionStatus { status, .. } = &event.body {
            if matches!(
                status,
                SessionStatus::Done | SessionStatus::Failed | SessionStatus::Cancelled
            ) {
                // Nothing more is expected; do not hold a descriptor per finished session.
                state.file = None;
            }
            if let Ok(Some(session)) = index::session(&self.db, session_id) {
                let _ = self.upserts.send(session);
            }
        }
        Ok(event)
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
        Ok(SessionDetail { session, agents })
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
        self.logs.insert(
            session_id.to_owned(),
            LogState {
                head_seq: indexed.head_seq,
                len: indexed.log_len,
                file: None,
                live: None,
            },
        );
        Ok(())
    }

    /// Indexes the log lines that follow `from` and returns where the index stands after.
    fn replay(&mut self, session_id: &str, from: Indexed) -> Result<Indexed> {
        let path = self.home.log_path(session_id);
        let mut reader = BufReader::new(File::open(&path)?);
        reader.seek(SeekFrom::Start(from.log_len))?;
        let mut at = from;
        let mut corrupt = None;
        let mut line = Vec::new();

        let tx = self.db.transaction()?;
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)? as u64;
            if read == 0 {
                break;
            }
            if line.pop() != Some(b'\n') {
                // A crash interrupted the last write. Nobody saw that event,
                // and the next append must start on a line boundary.
                OpenOptions::new()
                    .write(true)
                    .open(&path)?
                    .set_len(at.log_len)?;
                break;
            }
            match log::parse_line(&line, session_id, at.head_seq + 1) {
                Ok(event) => {
                    let end = at.log_len + read;
                    index::index_event(&tx, &event, at.log_len, end)?;
                    at = Indexed {
                        head_seq: event.seq,
                        log_len: end,
                    };
                }
                Err(err) => {
                    corrupt = Some(err);
                    break;
                }
            }
        }
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
        for entry in std::fs::read_dir(self.home.sessions_dir())? {
            let entry = entry?;
            let Ok(id) = entry.file_name().into_string() else {
                continue;
            };
            if Ulid::from_string(&id).is_err() || !entry.path().is_dir() {
                continue;
            }
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
        let mut session: Session = match std::fs::read(self.home.meta_path(session_id)) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
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
        let Some(created_at) = created_at else {
            return Ok(None);
        };
        let prompt = prompt.unwrap_or_default();
        Ok(Some(Session {
            id: session_id.to_owned(),
            title: default_title(&prompt),
            prompt,
            status: SessionStatus::Queued,
            runner: runner.unwrap_or_else(|| "unknown".to_owned()),
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

/// Two processes appending to the same logs would each hand out the same
/// sequence numbers, so the data directory belongs to one process at a time.
fn lock(home: &Home) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.lock_path())?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(Error::InUse(home.root().to_owned())),
        // Some file systems cannot lock. Starting anyway beats refusing to run.
        Err(TryLockError::Error(err)) => {
            tracing::warn!(%err, "cannot lock the data directory; make sure only one server uses it");
        }
    }
    Ok(file)
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
