//! The store: an SQLite index at `<home>/calin.db` and one append-only NDJSON
//! log per session. See the invariants in `AGENTS.md`: append-only, log
//! first, exact replay.

mod index;
mod log;
mod writer;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Instant;

use calin_proto::{AttachRepoRequest, Event, EventBody, PermissionMode, Session, SessionDetail};
use tokio::sync::{broadcast, mpsc, oneshot};

use self::writer::{Cmd, Writer};
use crate::error::{Error, Result};
use crate::home::Home;

/// Commands waiting for the writer thread. A full queue slows callers down
/// instead of growing without bound.
const COMMAND_BUFFER: usize = 256;

/// What a session is created with. The store assigns the id and the timestamps.
#[derive(Clone, Debug)]
pub struct NewSession {
    pub title: String,
    pub prompt: String,
    pub runner: String,
    pub permission_mode: PermissionMode,
    /// The model the request asked for, if any.
    pub model: Option<String>,
    /// The repos the request asked for, in order.
    pub repos: Vec<AttachRepoRequest>,
}

/// What a session was asked to run with, beyond what [`Session`] carries.
/// Kept so the agent can be started again the way it was first started.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionSetup {
    pub model: Option<String>,
    pub repos: Vec<AttachRepoRequest>,
}

/// A permission request nobody answered yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenPermission {
    pub session_id: String,
    pub request_id: String,
    /// The agent that asked.
    pub agent_id: Option<String>,
    /// The `permission_requested` event.
    pub event_id: String,
}

/// An event before the store gives it an id, a sequence number and a timestamp.
#[derive(Clone, Debug)]
pub struct EventDraft {
    pub agent_id: Option<String>,
    pub parent_id: Option<String>,
    pub body: EventBody,
}

impl EventDraft {
    /// An event of the user or the orchestrator, with no cause recorded.
    pub fn new(body: EventBody) -> Self {
        Self {
            agent_id: None,
            parent_id: None,
            body,
        }
    }

    pub fn agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    pub fn parent(mut self, parent_id: Option<String>) -> Self {
        self.parent_id = parent_id;
        self
    }
}

/// A session's events from a point on: `backlog` holds every event after the
/// requested sequence number up to `head_seq`, `live` yields every later one.
/// Together they contain each event exactly once.
pub struct Subscription {
    pub backlog: Vec<Event>,
    /// Newest sequence number when the subscription was made.
    pub head_seq: u64,
    pub live: Live<Arc<Event>>,
}

/// A stream of updates that never slows the writer down. A receiver that
/// falls too far behind is told so and has to subscribe again.
pub struct Live<T> {
    receiver: broadcast::Receiver<T>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveEnd {
    /// This many updates were dropped because the receiver was too slow.
    Lagged(u64),
    /// The store closed or had to give up on the session.
    Closed,
}

impl<T: Clone> Live<T> {
    pub async fn recv(&mut self) -> std::result::Result<T, LiveEnd> {
        self.receiver.recv().await.map_err(|err| match err {
            broadcast::error::RecvError::Lagged(missed) => LiveEnd::Lagged(missed),
            broadcast::error::RecvError::Closed => LiveEnd::Closed,
        })
    }
}

/// A handle to the store. Cheap to clone; all clones talk to one writer
/// thread, so SQLite and file calls never run on the tokio runtime.
#[derive(Clone)]
pub struct Store {
    commands: mpsc::Sender<Cmd>,
    home: Arc<Home>,
}

impl Store {
    /// Opens the store under `home`, creating it if needed and bringing the
    /// index in line with the logs.
    pub async fn open(home: Home) -> Result<Self> {
        let (commands, receiver) = mpsc::channel(COMMAND_BUFFER);
        let (ready, opened) = oneshot::channel();
        let writer_home = home.clone();
        let (timer, wake_ups) = mpsc::unbounded_channel();
        // Weak, so the timer never keeps the writer alive after the last handle.
        tokio::spawn(run_timer(wake_ups, commands.downgrade()));
        std::thread::Builder::new()
            .name("calin-store".to_owned())
            .spawn(move || match Writer::open(writer_home, timer) {
                Ok(writer) => {
                    let _ = ready.send(Ok(()));
                    writer.run(receiver);
                }
                Err(err) => {
                    let _ = ready.send(Err(err));
                }
            })?;
        opened.await.map_err(|_| Error::Closed)??;
        Ok(Self {
            commands,
            home: Arc::new(home),
        })
    }

    pub fn home(&self) -> &Home {
        &self.home
    }

    /// Stops the writer after the commands already queued. Later calls on
    /// any clone fail with [`Error::Closed`].
    pub async fn close(&self) {
        let (reply, closed) = oneshot::channel();
        if self.commands.send(Cmd::Close { reply }).await.is_ok() {
            let _ = closed.await;
        }
    }

    pub async fn create_session(&self, new: NewSession) -> Result<Session> {
        self.call(|reply| Cmd::CreateSession { new, reply }).await
    }

    /// Appends an event to the session's log, then indexes it, then hands it
    /// to subscribers. Returns it with its id, sequence number and timestamp.
    pub async fn append(&self, session_id: &str, draft: EventDraft) -> Result<Arc<Event>> {
        let session_id = session_id.to_owned();
        self.call(|reply| Cmd::Append {
            session_id,
            draft,
            reply,
        })
        .await
    }

    /// Everything after `after_seq`: the backlog from the log, then live events.
    pub async fn subscribe(&self, session_id: &str, after_seq: u64) -> Result<Subscription> {
        let cut = self
            .call(|reply| Cmd::Cut {
                session_id: session_id.to_owned(),
                after_seq,
                reply,
            })
            .await?;
        let backlog = match cut.backlog {
            None => Vec::new(),
            Some((start, end)) => {
                // The cut fixed the byte range, so reading it needs no lock
                // and does not hold up the writer.
                let path = self.home.log_path(session_id);
                let session_id = session_id.to_owned();
                tokio::task::spawn_blocking(move || {
                    log::read_range(&path, &session_id, after_seq + 1, start, end)
                })
                .await
                .map_err(std::io::Error::other)??
            }
        };
        Ok(Subscription {
            backlog,
            head_seq: cut.head_seq,
            live: Live { receiver: cut.live },
        })
    }

    /// Every session, newest first, and the upserts that follow: one when a
    /// session is created, one each time its status changes, and at most
    /// about one a second while other events arrive.
    pub async fn subscribe_sessions(&self) -> Result<(Vec<Session>, Live<Session>)> {
        let (sessions, receiver) = self.call(|reply| Cmd::SubscribeSessions { reply }).await?;
        Ok((sessions, Live { receiver }))
    }

    /// Every session, newest first.
    pub async fn sessions(&self) -> Result<Vec<Session>> {
        self.call(|reply| Cmd::Sessions { reply }).await
    }

    /// A session, its agents in spawn order and its repo set, primary first.
    pub async fn session(&self, session_id: &str) -> Result<SessionDetail> {
        let session_id = session_id.to_owned();
        self.call(|reply| Cmd::Session { session_id, reply }).await
    }

    /// What the session was created with that [`Session`] does not carry.
    pub async fn setup(&self, session_id: &str) -> Result<SessionSetup> {
        let session_id = session_id.to_owned();
        self.call(|reply| Cmd::Setup { session_id, reply }).await
    }

    /// True when the repo is attached to a session that is not `done`,
    /// `failed` or `cancelled`.
    pub async fn repo_in_use(&self, repo_id: &str) -> Result<bool> {
        let repo_id = repo_id.to_owned();
        self.call(|reply| Cmd::RepoInUse { repo_id, reply }).await
    }

    /// Whether a permission request was answered. `None` when the session
    /// never had a request with this id.
    pub async fn permission_resolved(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<Option<bool>> {
        let (session_id, request_id) = (session_id.to_owned(), request_id.to_owned());
        self.call(|reply| Cmd::PermissionResolved {
            session_id,
            request_id,
            reply,
        })
        .await
    }

    /// The permission requests still waiting for an answer, oldest first:
    /// those of one session, or of every session when `session_id` is `None`.
    pub async fn open_permissions(&self, session_id: Option<&str>) -> Result<Vec<OpenPermission>> {
        let session_id = session_id.map(str::to_owned);
        self.call(|reply| Cmd::OpenPermissions { session_id, reply })
            .await
    }

    async fn call<T>(&self, command: impl FnOnce(oneshot::Sender<Result<T>>) -> Cmd) -> Result<T> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| Error::Closed)?;
        answer.await.map_err(|_| Error::Closed)?
    }
}

/// Turns each wake-up the writer asks for into a [`Cmd::FlushUpserts`] at
/// that instant. Ends when the writer or the last store handle is gone.
async fn run_timer(
    mut wake_ups: mpsc::UnboundedReceiver<Instant>,
    commands: mpsc::WeakSender<Cmd>,
) {
    while let Some(due) = wake_ups.recv().await {
        tokio::time::sleep_until(due.into()).await;
        let Some(commands) = commands.upgrade() else {
            return;
        };
        if commands.send(Cmd::FlushUpserts).await.is_err() {
            return;
        }
    }
}
