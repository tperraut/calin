//! Changes an agent made without a tool call that names them: through its
//! shell (`sed -i`, a redirect, `git merge`, a generator, a formatter). At
//! the end of each turn of the lead agent's process, every tracked repo's
//! change list is compared with the one Calin last knew, and what changed
//! without a `file_changed` gets one.
//!
//! Calin knows a repo's changes from a snapshot taken when a turn begins
//! (the agent's start included), when a repo is attached while the agent
//! runs, and when a turn ends; a save from the editor updates its file. A
//! turn that begins right as another ends needs no new one.
//!
//! The snapshots live in memory only, from the agent's start to its end. A
//! restart loses them: the next start of the agent takes a new one, so what
//! changed while the server was down is attributed to nobody. That is
//! deliberate; the log cannot say who made such a change.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use calin_git::FileState;
use calin_proto::{ChangeStatus, FileOp, RepoMode, SessionRepo};

use crate::error::Result;
use crate::repos::blocking;

/// At most this many `file_changed` per repo per turn. A turn that touched
/// more (a vendored tree, a mass rename, a generator gone wrong) gets one
/// `error` that points at the Changes view, which lists them all: thousands
/// of events would bury the transcript and the graph without telling more.
pub(super) const MAX_CHANGES_PER_TURN: usize = 500;

/// What a path's difference from the base is, precisely enough that any
/// change to the file changes it.
///
/// The content identity is the SHA-256 of the bytes on disk (see
/// [`calin_git::file_states`]) rather than size and mtime: an edit that keeps
/// the size and lands within the filesystem's timestamp tick would slip past
/// a stat check, and a missed edit is wrong where a hash is only slower. It
/// costs one read of each changed file per turn, and no extra process.
/// Status and line counts are left out: the content and the base decide
/// them, and `untracked` versus `added` only says whether the agent ran
/// `git add`, which is not a change to the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Fingerprint {
    kind: Kind,
    disk: FileState,
}

/// How a path differs from the base, without the line counts.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    /// Not in the base: `added` or `untracked`.
    New,
    Modified,
    Deleted,
    /// A rename git detected, from the path in the base.
    Renamed(Option<String>),
}

impl Kind {
    fn of(status: ChangeStatus, old_path: Option<String>) -> Self {
        match status {
            ChangeStatus::Added | ChangeStatus::Untracked => Self::New,
            ChangeStatus::Modified => Self::Modified,
            ChangeStatus::Deleted => Self::Deleted,
            ChangeStatus::Renamed => Self::Renamed(old_path),
        }
    }
}

/// A repo's change list: path to fingerprint.
pub(super) type Snapshot = BTreeMap<String, Fingerprint>;

/// One session's agent process, as far as changes go.
#[derive(Default)]
pub(super) struct Tracker {
    /// By session repo id. A repo that could not be read has none until a
    /// later snapshot of it succeeds.
    repos: HashMap<String, Snapshot>,
    /// Paths a tool call reported during the current turn, by session repo
    /// id and path, with their state on disk when it was reported.
    reported: HashMap<(String, String), FileState>,
}

/// The trackers of the sessions whose agent is running. A session's own
/// lock serializes a turn's end with the user's saves, so a save is never
/// half seen.
pub(super) type Trackers = Mutex<HashMap<String, Arc<tokio::sync::Mutex<Tracker>>>>;

pub(super) fn tracker(
    trackers: &Trackers,
    session_id: &str,
) -> Option<Arc<tokio::sync::Mutex<Tracker>>> {
    // One lookup, insert or remove per critical section.
    let map = trackers.lock().unwrap_or_else(PoisonError::into_inner);
    map.get(session_id).cloned()
}

/// A new, empty tracker for the session, replacing any older one, locked:
/// the caller fills it before a save can see it. A save in flight on the
/// older one finishes first, so the new snapshot includes it.
pub(super) async fn fresh(
    trackers: &Trackers,
    session_id: &str,
) -> tokio::sync::OwnedMutexGuard<Tracker> {
    let older = match tracker(trackers, session_id) {
        Some(older) => Some(older.lock_owned().await),
        None => None,
    };
    let tracker = Arc::new(tokio::sync::Mutex::new(Tracker::default()));
    // Nobody else holds a mutex created a line ago.
    let guard = match Arc::clone(&tracker).try_lock_owned() {
        Ok(guard) => guard,
        Err(_) => unreachable!("a new mutex is unlocked"),
    };
    let mut map = trackers.lock().unwrap_or_else(PoisonError::into_inner);
    map.insert(session_id.to_owned(), tracker);
    drop(older);
    guard
}

/// The session's tracker, locked, while it is still the current one: a
/// save that waited on a tracker that was replaced or forgotten meanwhile
/// moves on to the new one, or to none.
pub(super) async fn current(
    trackers: &Trackers,
    session_id: &str,
) -> Option<tokio::sync::OwnedMutexGuard<Tracker>> {
    loop {
        let found = tracker(trackers, session_id)?;
        let guard = Arc::clone(&found).lock_owned().await;
        let now = tracker(trackers, session_id);
        if now.is_some_and(|now| Arc::ptr_eq(&now, &found)) {
            return Some(guard);
        }
    }
}

pub(super) fn forget(trackers: &Trackers, session_id: &str) {
    let mut map = trackers.lock().unwrap_or_else(PoisonError::into_inner);
    map.remove(session_id);
}

/// A repo whose changes are measured: it has a base and the agent may write it.
pub(super) fn is_tracked(repo: &SessionRepo) -> bool {
    repo.base_ref.is_some() && repo.mode != RepoMode::Read
}

/// Blocking: the repo's change list with a fingerprint per path.
fn take(repo: &SessionRepo) -> Result<Snapshot> {
    let Some(base) = &repo.base_ref else {
        return Ok(Snapshot::new());
    };
    let dir = PathBuf::from(&repo.path);
    let files = calin_git::changes(&dir, base)?;
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    let states = calin_git::file_states(&dir, &paths)?;
    Ok(files
        .iter()
        .zip(states)
        .map(|(file, disk)| {
            let fingerprint = Fingerprint {
                kind: Kind::of(file.status, file.old_path.clone()),
                disk,
            };
            (file.path.clone(), fingerprint)
        })
        .collect())
}

/// Takes the snapshot of every tracked repo of `repos` into `tracker`. A
/// repo that cannot be read is skipped, with a warning.
pub(super) async fn baseline(tracker: &mut Tracker, session_id: &str, repos: &[SessionRepo]) {
    for repo in repos.iter().filter(|repo| is_tracked(repo)) {
        let owned = repo.clone();
        match blocking(move || take(&owned)).await {
            Ok(snapshot) => {
                tracker.repos.insert(repo.id.clone(), snapshot);
            }
            Err(err) => {
                tracing::warn!(session = %session_id, repo = %repo.name, %err, "could not take a snapshot of the repo's changes");
            }
        }
    }
}

/// Blocking: the state on disk of one path of a repo, for a change a tool
/// call reported. `None` when the repo is gone.
pub(super) fn state_of(repo: &SessionRepo, path: &str) -> Option<FileState> {
    let dir = PathBuf::from(&repo.path);
    calin_git::file_states(&dir, &[path])
        .ok()
        .and_then(|mut states| states.pop())
}

impl Tracker {
    pub(super) fn tracks(&self, session_repo_id: &str) -> bool {
        self.repos.contains_key(session_repo_id)
    }

    /// Remembers that a tool call reported `path` in this turn, in `state`.
    pub(super) fn reported(&mut self, session_repo_id: &str, path: &str, state: FileState) {
        let key = (session_repo_id.to_owned(), path.to_owned());
        self.reported.insert(key, state);
    }

    /// Brings one path of a repo up to date after the user saved it: what
    /// the user did is never the agent's at the next turn end.
    pub(super) async fn saved(&mut self, repo: &SessionRepo, path: &str) -> Result<()> {
        if !self.tracks(&repo.id) {
            return Ok(());
        }
        let owned = repo.clone();
        let now = blocking(move || take(&owned)).await?;
        let Some(snapshot) = self.repos.get_mut(&repo.id) else {
            return Ok(());
        };
        match now.get(path) {
            Some(fingerprint) => snapshot.insert(path.to_owned(), fingerprint.clone()),
            None => snapshot.remove(path),
        };
        Ok(())
    }

    /// Ends a turn: takes a new snapshot of every tracked repo of `repos`,
    /// and returns per repo what changed since the last one without a
    /// report, in path order. The new snapshots replace the old ones.
    pub(super) async fn end_turn(
        &mut self,
        session_id: &str,
        repos: &[SessionRepo],
    ) -> Vec<(SessionRepo, Vec<(String, FileOp)>)> {
        let reported = std::mem::take(&mut self.reported);
        let mut found = Vec::new();
        for repo in repos.iter().filter(|repo| is_tracked(repo)) {
            let old = self.repos.get(&repo.id).cloned();
            let owned = repo.clone();
            let gone: Vec<String> = old.iter().flat_map(|old| old.keys().cloned()).collect();
            let taken = blocking(move || {
                let now = take(&owned)?;
                // The paths that left the list: reverted to the base, or
                // removed when they were not in it.
                let left: Vec<&str> = gone
                    .iter()
                    .map(String::as_str)
                    .filter(|path| !now.contains_key(*path))
                    .collect();
                let states = calin_git::file_states(&PathBuf::from(&owned.path), &left)?;
                let left: HashMap<String, FileState> =
                    left.into_iter().map(str::to_owned).zip(states).collect();
                Ok((now, left))
            })
            .await;
            let (now, left) = match taken {
                Ok(taken) => taken,
                Err(err) => {
                    // The old snapshot stays: the repo may come back.
                    tracing::warn!(session = %session_id, repo = %repo.name, %err, "could not look for unreported changes in the repo");
                    continue;
                }
            };
            if let Some(old) = old {
                let already = |path: &str| reported.get(&(repo.id.clone(), path.to_owned()));
                let changes = unreported(&old, &now, &left, already);
                if !changes.is_empty() {
                    found.push((repo.clone(), changes));
                }
            }
            // A repo without an old snapshot starts being tracked here.
            self.repos.insert(repo.id.clone(), now);
        }
        found
    }

    /// Starts tracking a repo attached while the agent runs.
    pub(super) async fn attached(&mut self, session_id: &str, repo: &SessionRepo) {
        baseline(self, session_id, std::slice::from_ref(repo)).await;
    }
}

/// What changed between two snapshots of one repo, in path order, leaving
/// out what a tool call already reported in the state it is in now.
/// `left` holds the state on disk of each path of `old` missing from `new`.
fn unreported<'a>(
    old: &Snapshot,
    new: &Snapshot,
    left: &HashMap<String, FileState>,
    already: impl Fn(&str) -> Option<&'a FileState>,
) -> Vec<(String, FileOp)> {
    let paths: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    paths
        .into_iter()
        .filter_map(|path| {
            let (op, disk) = change(old.get(path), new.get(path), left.get(path.as_str()))?;
            (already(path) != Some(disk)).then(|| (path.clone(), op))
        })
        .collect()
}

/// How one path changed between two snapshots, with its state on disk
/// now; `None` when it did not change, or left with no state to show.
fn change<'s>(
    before: Option<&'s Fingerprint>,
    after: Option<&'s Fingerprint>,
    left: Option<&'s FileState>,
) -> Option<(FileOp, &'s FileState)> {
    match (before, after) {
        (Some(before), Some(after)) if before == after => None,
        (None, Some(after)) => Some((appeared(&after.kind), &after.disk)),
        (Some(before), Some(after)) => Some((changed(&before.kind, &after.kind), &after.disk)),
        (Some(_), None) => {
            let disk = left?;
            // Back to the base when it is still there; otherwise a file
            // the base never had was removed.
            let op = match disk {
                FileState::Absent => FileOp::Delete,
                _ => FileOp::Edit,
            };
            Some((op, disk))
        }
        (None, None) => None,
    }
}

/// The change of a path that was not in the list before.
fn appeared(kind: &Kind) -> FileOp {
    match kind {
        Kind::New => FileOp::Create,
        Kind::Modified => FileOp::Edit,
        Kind::Deleted => FileOp::Delete,
        Kind::Renamed(_) => FileOp::Rename,
    }
}

/// The change of a path that was in the list and still is, differently.
fn changed(before: &Kind, after: &Kind) -> FileOp {
    match after {
        Kind::Deleted => FileOp::Delete,
        Kind::Renamed(_) if before != after => FileOp::Rename,
        _ => FileOp::Edit,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::testing::{git, git_repo, write_file};

    fn file(content: &str) -> FileState {
        FileState::File {
            sha256: content.to_owned(),
            executable: false,
        }
    }

    fn print(kind: Kind, content: &str) -> Fingerprint {
        Fingerprint {
            kind,
            disk: file(content),
        }
    }

    fn snapshot(paths: &[(&str, Fingerprint)]) -> Snapshot {
        paths
            .iter()
            .map(|(path, print)| ((*path).to_owned(), print.clone()))
            .collect()
    }

    #[test]
    fn what_changed_between_two_snapshots_is_named_by_what_it_is_now() {
        let renamed = Kind::Renamed(Some("old.rs".to_owned()));
        let old = snapshot(&[
            ("same.rs", print(Kind::Modified, "a")),
            ("edited.rs", print(Kind::Modified, "a")),
            ("deleted.rs", print(Kind::Modified, "a")),
            ("moved.rs", print(Kind::New, "a")),
            ("still_moved.rs", print(renamed.clone(), "a")),
            ("reverted.rs", print(Kind::Modified, "a")),
            ("removed.rs", print(Kind::New, "a")),
            ("unread.rs", print(Kind::Modified, "a")),
        ]);
        let new = snapshot(&[
            ("same.rs", print(Kind::Modified, "a")),
            ("edited.rs", print(Kind::Modified, "b")),
            ("deleted.rs", print(Kind::Deleted, "")),
            ("moved.rs", print(renamed.clone(), "a")),
            ("still_moved.rs", print(renamed.clone(), "b")),
            ("created.rs", print(Kind::New, "c")),
            ("touched.rs", print(Kind::Modified, "c")),
            ("gone.rs", print(Kind::Deleted, "")),
            ("arrived.rs", print(renamed, "c")),
        ]);
        let left = HashMap::from([
            ("reverted.rs".to_owned(), file("base")),
            ("removed.rs".to_owned(), FileState::Absent),
        ]);
        let changes = unreported(&old, &new, &left, |_| None);
        assert_eq!(
            changes,
            [
                ("arrived.rs".to_owned(), FileOp::Rename),
                ("created.rs".to_owned(), FileOp::Create),
                ("deleted.rs".to_owned(), FileOp::Delete),
                ("edited.rs".to_owned(), FileOp::Edit),
                ("gone.rs".to_owned(), FileOp::Delete),
                ("moved.rs".to_owned(), FileOp::Rename),
                ("removed.rs".to_owned(), FileOp::Delete),
                ("reverted.rs".to_owned(), FileOp::Edit),
                ("still_moved.rs".to_owned(), FileOp::Edit),
                ("touched.rs".to_owned(), FileOp::Edit),
            ]
        );
        // What a tool call reported in the state it is in now is left out;
        // in another state it is not.
        let reported = HashMap::from([
            ("edited.rs".to_owned(), file("b")),
            ("created.rs".to_owned(), file("old")),
            ("removed.rs".to_owned(), FileState::Absent),
        ]);
        let changes = unreported(&old, &new, &left, |path| reported.get(path));
        let paths: Vec<&str> = changes.iter().map(|(path, _)| path.as_str()).collect();
        assert!(!paths.contains(&"edited.rs"), "{paths:?}");
        assert!(!paths.contains(&"removed.rs"), "{paths:?}");
        assert!(paths.contains(&"created.rs"), "{paths:?}");
        assert_eq!(paths.len(), 8);
    }

    /// What a snapshot costs: `cargo test --release -p calin-core -- --ignored
    /// --nocapture snapshot_cost`. Not a check, a measurement.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn snapshot_cost() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = git_repo(tmp.path(), "big");
        let body = "fn f() -> u32 {\n    42\n}\n".repeat(40);
        for n in 0..3000 {
            write_file(&dir, &format!("src/m{:02}/f{n:04}.rs", n % 50), &body);
        }
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "files"]);
        let base = git(&dir, &["rev-parse", "HEAD"]);
        for n in (0..3000).step_by(10) {
            write_file(&dir, &format!("src/m{:02}/f{n:04}.rs", n % 50), "changed\n");
        }
        for n in 0..50 {
            write_file(&dir, &format!("new/n{n:03}.rs"), &body);
        }
        let repo = SessionRepo {
            id: "r".to_owned(),
            session_id: "s".to_owned(),
            repo_id: "big".to_owned(),
            name: "big".to_owned(),
            mode: RepoMode::Write,
            path: dir.to_str().unwrap().to_owned(),
            branch: None,
            base_ref: Some(base),
            primary: true,
        };
        let _ = take(&repo).unwrap();
        let runs = 10;
        let started = Instant::now();
        let mut size = 0;
        for _ in 0..runs {
            size = take(&repo).unwrap().len();
        }
        let each = started.elapsed() / runs;
        let start = Instant::now();
        for _ in 0..runs {
            calin_git::changes(&dir, repo.base_ref.as_deref().unwrap()).unwrap();
        }
        let git_only = start.elapsed() / runs;
        println!(
            "{size} changed paths of 3050: snapshot {each:?}, of which `changes` {git_only:?}"
        );
    }
}
