//! When to send a push (remote-access spec, section 6): one task per server
//! watches the store's session upserts, turns each status change that needs
//! the user into a notification, and a second task sends them, so a slow
//! push service never holds up the watching and never the session itself.
//!
//! Calin has one shared workspace and no per-user sessions (spec section 8),
//! so every identity on the allow-list gets every notification; those
//! without a subscription cost nothing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use calin_core::{LiveEnd, Store};
use calin_proto::{Event, EventBody, PushKind, PushPayload, Session, SessionStatus};
use calin_push::SendReport;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::AppState;
use crate::push::{Push, Viewers};

/// Notifications waiting to be sent. Past this many, new ones are dropped
/// with a warning rather than piling up behind a push service that hangs.
const QUEUE: usize = 256;
/// Events read back from the end of a session's log to find why it stopped.
const TAIL: u64 = 512;
/// A body is one line; the library cuts at 300 characters, this is shorter.
const MAX_LINE_CHARS: usize = 160;

/// What the tests learn about each decision, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decision {
    pub(crate) session_id: String,
    pub(crate) status: SessionStatus,
    /// `None` when the change notifies nobody (a child's review, say).
    pub(crate) kind: Option<PushKind>,
    /// `None` with `kind`: the change itself was not notified.
    pub(crate) identity: Option<String>,
    pub(crate) outcome: Outcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The change is not one that notifies.
    Ignored,
    /// The identity has the session's topic open on a socket.
    Viewing,
    /// The library's answer: so many push services accepted it.
    Sent {
        accepted: usize,
    },
    Disabled,
    Throttled,
    Refused,
}

pub(crate) type Observer = mpsc::UnboundedSender<Decision>;

struct Notification {
    session_id: String,
    status: SessionStatus,
    kind: PushKind,
    payload: PushPayload,
}

/// Starts watching, in public mode with push set up; `None` otherwise.
/// Stops when `state`'s shutdown token is cancelled.
pub(crate) fn spawn(state: &AppState, observer: Option<Observer>) -> Option<JoinHandle<()>> {
    let push = state.push.clone()?;
    let identities = state.sign_in.as_ref()?.allowed_identities();
    let (queue, queued) = mpsc::channel(QUEUE);
    tokio::spawn(deliver(
        push,
        identities,
        state.viewers.clone(),
        observer.clone(),
        queued,
    ));
    let watcher = Watcher {
        store: state.manager.store().clone(),
        queue,
        observer,
    };
    Some(tokio::spawn(watcher.run(state.shutdown.clone())))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

struct Watcher {
    store: Store,
    queue: mpsc::Sender<Notification>,
    observer: Option<Observer>,
}

impl Watcher {
    async fn run(self, shutdown: CancellationToken) {
        // The manager settled the previous run's sessions before the server
        // was built, so the first snapshot already holds what recovery did:
        // only changes after it are news. A change stamped before this
        // moment is a replay, never news.
        let started = now_ms();
        let mut known: HashMap<String, SessionStatus> = HashMap::new();
        let mut first = true;
        loop {
            let subscribed = tokio::select! {
                () = shutdown.cancelled() => return,
                subscribed = self.store.subscribe_sessions() => subscribed,
            };
            let (sessions, mut live) = match subscribed {
                Ok(subscribed) => subscribed,
                Err(error) => {
                    tracing::warn!(%error, "push notifications stop: the store is closed");
                    return;
                }
            };
            // After a lag, the snapshot says what changed while updates were lost.
            for session in sessions {
                let previous = known.insert(session.id.clone(), session.status);
                if !first {
                    self.changed(previous, &session, started).await;
                }
            }
            first = false;
            loop {
                let update = tokio::select! {
                    () = shutdown.cancelled() => return,
                    update = live.recv() => update,
                };
                match update {
                    Ok(session) => {
                        let previous = known.insert(session.id.clone(), session.status);
                        self.changed(previous, &session, started).await;
                    }
                    Err(LiveEnd::Lagged(missed)) => {
                        tracing::warn!(missed, "push notifications fell behind; catching up");
                        break;
                    }
                    Err(LiveEnd::Closed) => return,
                }
            }
        }
    }

    async fn changed(&self, previous: Option<SessionStatus>, session: &Session, started: i64) {
        // A session first seen now was just created: it starts `queued`.
        let Some(previous) = previous else {
            return;
        };
        if previous == session.status || session.updated_at < started {
            return;
        }
        let child = session.parent_session_id.is_some();
        let kind = match session.status {
            // A child blocked on the user blocks its whole tree.
            SessionStatus::Waiting => Some(PushKind::Waiting),
            SessionStatus::Review if !child => Some(PushKind::Review),
            SessionStatus::Failed if !child => Some(PushKind::Failed),
            SessionStatus::Done if !child && previous == SessionStatus::Shipping => {
                Some(PushKind::Merged)
            }
            _ => None,
        };
        let Some(kind) = kind else {
            if matches!(
                session.status,
                SessionStatus::Waiting
                    | SessionStatus::Review
                    | SessionStatus::Failed
                    | SessionStatus::Done
            ) {
                observe(
                    self.observer.as_ref(),
                    Decision {
                        session_id: session.id.clone(),
                        status: session.status,
                        kind: None,
                        identity: None,
                        outcome: Outcome::Ignored,
                    },
                );
            }
            return;
        };
        let reason = match kind {
            PushKind::Waiting => waiting_reason(&self.store, session).await,
            PushKind::Review => review_summary(&self.store, session).await,
            PushKind::Failed => failure_reason(&self.store, session).await,
            PushKind::Merged | PushKind::Test => "All pull requests merged.".to_owned(),
        };
        let (title, body) = if child {
            let root = match self.store.session(&session.root_session_id).await {
                Ok(root) => root.session.title,
                Err(_) => session.title.clone(),
            };
            (
                root,
                format!("In \"{}\": {reason}", one_line(&session.title)),
            )
        } else {
            (session.title.clone(), reason)
        };
        let notification = Notification {
            session_id: session.id.clone(),
            status: session.status,
            kind,
            payload: PushPayload {
                kind,
                title,
                body,
                path: format!("/s/{}", session.id),
                session_id: Some(session.id.clone()),
            },
        };
        if self.queue.try_send(notification).is_err() {
            tracing::warn!(session = %session.id, "a push notification was dropped: too many pending");
        }
    }
}

fn observe(observer: Option<&Observer>, decision: Decision) {
    if let Some(observer) = observer {
        let _ = observer.send(decision);
    }
}

/// Sends each notification to every allowed identity that does not have the
/// session open. Failures are the library's to log; nothing is retried.
async fn deliver(
    push: Arc<Push>,
    identities: Vec<String>,
    viewers: Viewers,
    observer: Option<Observer>,
    mut queue: mpsc::Receiver<Notification>,
) {
    while let Some(notification) = queue.recv().await {
        for identity in &identities {
            let outcome = if viewers.is_viewing(identity, &notification.session_id) {
                Outcome::Viewing
            } else {
                let sent = push
                    .sender()
                    .send(identity, notification.kind, notification.payload.clone())
                    .await;
                match sent {
                    Ok(SendReport::Sent(deliveries)) => Outcome::Sent {
                        accepted: deliveries
                            .iter()
                            .filter(|delivery| {
                                delivery.outcome == calin_push::DeliveryOutcome::Accepted
                            })
                            .count(),
                    },
                    Ok(SendReport::Disabled) => Outcome::Disabled,
                    Ok(SendReport::Throttled) => Outcome::Throttled,
                    Err(error) => {
                        tracing::warn!(session = %notification.session_id, %error, "push not sent");
                        Outcome::Refused
                    }
                }
            };
            observe(
                observer.as_ref(),
                Decision {
                    session_id: notification.session_id.clone(),
                    status: notification.status,
                    kind: Some(notification.kind),
                    identity: Some(identity.clone()),
                    outcome,
                },
            );
        }
    }
}

// --- What the body says ---

/// The first line of `text`, trimmed and cut to [`MAX_LINE_CHARS`].
fn one_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty());
    let line = line.unwrap_or("");
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(MAX_LINE_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// The end of the session's log, and whether it is the whole log.
async fn tail(store: &Store, session: &Session, window: Option<u64>) -> (Vec<Event>, bool) {
    let after = window.map_or(0, |window| session.head_seq.saturating_sub(window));
    match store.subscribe(&session.id, after).await {
        Ok(subscription) => (subscription.backlog, after == 0),
        Err(error) => {
            tracing::warn!(session = %session.id, %error, "could not read the session for a push");
            (Vec::new(), false)
        }
    }
}

/// Why the session waits: its newest open permission request or question.
async fn waiting_reason(store: &Store, session: &Session) -> String {
    let fallback = "Waiting for you.".to_owned();
    let mut open: HashSet<String> = HashSet::new();
    match store.open_permissions(Some(&session.id)).await {
        Ok(requests) => open.extend(requests.into_iter().map(|request| request.event_id)),
        Err(error) => tracing::warn!(%error, "could not list open permission requests"),
    }
    match store.open_questions(Some(&session.id)).await {
        Ok(questions) => open.extend(questions.into_iter().map(|question| question.event_id)),
        Err(error) => tracing::warn!(%error, "could not list open questions"),
    }
    if open.is_empty() {
        return fallback;
    }
    let newest = |events: &[Event]| {
        events
            .iter()
            .rev()
            .find(|event| open.contains(&event.id))
            .and_then(|event| match &event.body {
                EventBody::PermissionRequested { request } => {
                    Some(format!("Wants to run: {}", one_line(&request.title)))
                }
                // A gate's text is its message; a question's first line says it.
                EventBody::QuestionAsked { question } => Some(one_line(&question.text)),
                _ => None,
            })
    };
    let (events, whole) = tail(store, session, Some(TAIL)).await;
    if let Some(reason) = newest(&events) {
        return reason;
    }
    if whole {
        return fallback;
    }
    let (events, _) = tail(store, session, None).await;
    newest(&events).unwrap_or(fallback)
}

/// The reason the newest `failed` status gives.
async fn failure_reason(store: &Store, session: &Session) -> String {
    let (events, _) = tail(store, session, Some(TAIL)).await;
    events
        .iter()
        .rev()
        .find_map(|event| match &event.body {
            EventBody::SessionStatus {
                status: SessionStatus::Failed,
                reason,
            } => Some(reason.as_deref().map(one_line).unwrap_or_default()),
            _ => None,
        })
        .filter(|reason| !reason.is_empty())
        .unwrap_or_else(|| "The session failed.".to_owned())
}

/// What the turn that just ended changed, when the end of the log holds the
/// whole turn: the files changed since the previous `review`.
async fn review_summary(store: &Store, session: &Session) -> String {
    let fallback = "Ready for review.".to_owned();
    let (events, whole) = tail(store, session, Some(TAIL)).await;
    let is_review = |event: &Event| {
        matches!(
            event.body,
            EventBody::SessionStatus {
                status: SessionStatus::Review,
                ..
            }
        )
    };
    // The newest review is this one; the turn starts after the one before.
    let Some(this) = events.iter().rposition(is_review) else {
        return fallback;
    };
    let start = match events[..this].iter().rposition(is_review) {
        Some(previous) => previous + 1,
        None if whole => 0,
        // The turn began before the part that was read.
        None => return fallback,
    };
    let mut files = HashSet::new();
    let mut repos = HashSet::new();
    for event in &events[start..this] {
        if let EventBody::FileChanged {
            session_repo_id: Some(repo),
            path,
            ..
        } = &event.body
        {
            files.insert((repo.clone(), path.clone()));
            repos.insert(repo.clone());
        }
    }
    match (files.len(), repos.len()) {
        (0, _) => fallback,
        (files, repos) => format!(
            "Finished. {files} {} changed in {repos} {}.",
            if files == 1 { "file" } else { "files" },
            if repos == 1 { "repo" } else { "repos" },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_the_first_non_blank_one_cut_short() {
        assert_eq!(one_line("  \n Which color?\nMore."), "Which color?");
        assert_eq!(one_line(""), "");
        let long = "x".repeat(MAX_LINE_CHARS + 10);
        let cut = one_line(&long);
        assert_eq!(cut.chars().count(), MAX_LINE_CHARS);
        assert!(cut.ends_with('…'));
        let exact = "y".repeat(MAX_LINE_CHARS);
        assert_eq!(one_line(&exact), exact);
    }
}
