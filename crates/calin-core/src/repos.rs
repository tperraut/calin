//! The repos registered on this machine, kept in `<home>/repos.json`.
//!
//! The file is the source of truth: it is configuration, and no event could
//! rebuild it. That is why it is not in the SQLite index, which stays a pure
//! projection of the session logs.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use calin_proto::Repo;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::store::Store;

const FILE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct RegistryFile {
    version: u32,
    repos: Vec<Repo>,
}

/// Repo ids held by sessions that were created but have not attached their
/// repos yet. The index cannot know about those, and removing a repo from
/// under one would fail its preparation.
type Reserved = Mutex<HashMap<u64, Vec<String>>>;

pub(crate) struct RepoRegistry {
    path: PathBuf,
    /// Held across the checks and the file write of each change, so two
    /// changes never interleave.
    state: tokio::sync::Mutex<State>,
    reserved: Arc<Reserved>,
}

struct State {
    repos: Vec<Repo>,
    next_reservation: u64,
}

/// Keeps the repos of one session from being removed until it is dropped.
pub(crate) struct Reservation {
    key: u64,
    reserved: Arc<Reserved>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        lock(&self.reserved).remove(&self.key);
    }
}

fn lock(reserved: &Reserved) -> std::sync::MutexGuard<'_, HashMap<u64, Vec<String>>> {
    // Every critical section is one insert, remove or scan of the map.
    reserved.lock().unwrap_or_else(PoisonError::into_inner)
}

impl RepoRegistry {
    /// Reads the registry. A missing or unreadable file is an empty registry:
    /// the server must start whatever state the file is in.
    pub(crate) async fn load(path: PathBuf) -> Self {
        let file = path.clone();
        let repos = blocking(move || Ok(read(&file))).await.unwrap_or_default();
        Self {
            path,
            state: tokio::sync::Mutex::new(State {
                repos,
                next_reservation: 0,
            }),
            reserved: Arc::default(),
        }
    }

    /// Every registered repo, by name.
    pub(crate) async fn list(&self) -> Vec<Repo> {
        let mut repos = self.state.lock().await.repos.clone();
        repos.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        repos
    }

    pub(crate) async fn get(&self, repo_id: &str) -> Option<Repo> {
        let state = self.state.lock().await;
        state.repos.iter().find(|repo| repo.id == repo_id).cloned()
    }

    /// Registers the clone at `path` under `name`, or under its folder name.
    pub(crate) async fn register(&self, path: &str, name: Option<&str>) -> Result<Repo> {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(Error::Invalid(
                "the path of a repo must be absolute".to_owned(),
            ));
        }
        let clone = blocking(move || Ok(calin_git::inspect_clone(&path)?)).await?;
        let name = match name.map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) => name.to_owned(),
            None => clone
                .root
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or_else(|| {
                    Error::Invalid("this folder has no usable name; give the repo one".to_owned())
                })?,
        };
        check_name(&name)?;
        let Some(root) = clone.root.to_str() else {
            return Err(Error::Invalid(
                "the path of a repo must be valid UTF-8".to_owned(),
            ));
        };

        let mut state = self.state.lock().await;
        if let Some(existing) = state.repos.iter().find(|repo| repo.path == root) {
            return Err(Error::Conflict(format!(
                "{root} is already registered as {}",
                existing.name
            )));
        }
        if state.repos.iter().any(|repo| repo.name == name) {
            return Err(Error::Conflict(format!(
                "a repo named {name} is already registered"
            )));
        }
        let repo = Repo {
            id: Ulid::generate().to_string(),
            name,
            path: root.to_owned(),
            default_branch: clone.default_branch,
            remote_url: clone.remote_url,
        };
        let mut repos = state.repos.clone();
        repos.push(repo.clone());
        self.save(&repos).await?;
        state.repos = repos;
        Ok(repo)
    }

    /// Removes the registry entry. The clone, its worktrees and its branches
    /// are left alone.
    pub(crate) async fn remove(&self, repo_id: &str, store: &Store) -> Result<()> {
        let mut state = self.state.lock().await;
        let Some(repo) = state.repos.iter().find(|repo| repo.id == repo_id) else {
            return Err(Error::Missing(format!("unknown repo {repo_id}")));
        };
        let reserved = lock(&self.reserved)
            .values()
            .any(|ids| ids.iter().any(|id| id == repo_id));
        if reserved || store.repo_in_use(repo_id).await? {
            return Err(Error::Conflict(format!(
                "{} is attached to a session that is still open",
                repo.name
            )));
        }
        let repos: Vec<Repo> = state
            .repos
            .iter()
            .filter(|repo| repo.id != repo_id)
            .cloned()
            .collect();
        self.save(&repos).await?;
        state.repos = repos;
        Ok(())
    }

    /// Checks that every id names a registered repo and that none is named
    /// twice, then holds them against removal.
    pub(crate) async fn reserve(&self, repo_ids: &[&str]) -> Result<Reservation> {
        let mut state = self.state.lock().await;
        for (index, repo_id) in repo_ids.iter().enumerate() {
            if !state.repos.iter().any(|repo| repo.id == *repo_id) {
                return Err(Error::Invalid(format!("unknown repo {repo_id}")));
            }
            if repo_ids[..index].contains(repo_id) {
                return Err(Error::Invalid(format!(
                    "repo {repo_id} is attached more than once"
                )));
            }
        }
        let key = state.next_reservation;
        state.next_reservation += 1;
        let ids = repo_ids.iter().map(|id| (*id).to_owned()).collect();
        lock(&self.reserved).insert(key, ids);
        Ok(Reservation {
            key,
            reserved: Arc::clone(&self.reserved),
        })
    }

    async fn save(&self, repos: &[Repo]) -> Result<()> {
        let path = self.path.clone();
        let file = RegistryFile {
            version: FILE_VERSION,
            repos: repos.to_vec(),
        };
        blocking(move || write(&path, &file)).await
    }
}

/// A repo's name is the folder of its worktree inside a session.
fn check_name(name: &str) -> Result<()> {
    let reason = if name.is_empty() {
        "must not be empty"
    } else if name == "." || name == ".." {
        "must not be . or .."
    } else if name.contains(['/', '\\', '\0']) {
        "must not contain a path separator"
    } else {
        return Ok(());
    };
    Err(Error::Invalid(format!("the name of a repo {reason}")))
}

fn read(path: &Path) -> Vec<Repo> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "cannot read the repo registry; starting with none");
            return Vec::new();
        }
    };
    match serde_json::from_slice::<RegistryFile>(&bytes) {
        Ok(file) => file.repos,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "the repo registry is corrupt; starting with none");
            Vec::new()
        }
    }
}

/// Writes the whole file next to the old one and renames it into place, so a
/// crash leaves the previous registry or the new one, never half of either.
fn write(path: &Path, file: &RegistryFile) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut bytes = serde_json::to_vec_pretty(file)?;
    bytes.push(b'\n');
    let temp = path.with_extension("json.tmp");
    let written = std::fs::File::create(&temp).and_then(|mut out| {
        out.write_all(&bytes)?;
        out.sync_all()
    });
    if let Err(err) = written.and_then(|()| std::fs::rename(&temp, path)) {
        // Best effort: the write already failed, and that is the error to report.
        let _ = std::fs::remove_file(&temp);
        return Err(err.into());
    }
    Ok(())
}

/// Runs git or file system work off the runtime.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(std::io::Error::other)?
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::home::Home;
    use crate::testing::{git_repo, write_file};

    async fn registry(dir: &TempDir) -> RepoRegistry {
        RepoRegistry::load(Home::new(dir.path()).repos_path()).await
    }

    #[tokio::test]
    async fn repos_are_registered_listed_by_name_and_persisted() {
        let dir = TempDir::new().unwrap();
        let repos = TempDir::new().unwrap();
        let zeta = git_repo(repos.path(), "zeta");
        let alpha = git_repo(repos.path(), "alpha");
        let registry = registry(&dir).await;
        assert!(registry.list().await.is_empty());

        let first = registry
            .register(zeta.to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(first.name, "zeta");
        assert_eq!(first.path, zeta.to_str().unwrap());
        assert_eq!(first.default_branch.as_deref(), Some("main"));
        assert_eq!(first.remote_url, None);
        let second = registry
            .register(alpha.to_str().unwrap(), Some("  api "))
            .await
            .unwrap();
        assert_eq!(second.name, "api");
        assert_ne!(first.id, second.id);
        assert_eq!(registry.list().await, [second.clone(), first.clone()]);
        assert_eq!(registry.get(&first.id).await, Some(first.clone()));
        assert_eq!(registry.get("nope").await, None);

        // A new process reads the same registry from the file.
        let reloaded = self::registry(&dir).await;
        assert_eq!(reloaded.list().await, [second, first]);
        assert!(!dir.path().join("repos.json.tmp").exists());
    }

    #[tokio::test]
    async fn what_is_not_a_clone_root_is_invalid() {
        let dir = TempDir::new().unwrap();
        let repos = TempDir::new().unwrap();
        let repo = git_repo(repos.path(), "repo");
        write_file(&repo, "src/lib.rs", "");
        let registry = registry(&dir).await;
        let plain = repos.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        for path in [
            repo.join("src"),
            repo.join(".git"),
            repo.join("missing"),
            plain,
            PathBuf::from("relative/path"),
            PathBuf::new(),
        ] {
            let result = registry.register(path.to_str().unwrap(), None).await;
            assert!(matches!(result, Err(Error::Invalid(_))), "{path:?}");
        }
        assert!(registry.list().await.is_empty());
        assert!(!dir.path().join("repos.json").exists());
    }

    #[tokio::test]
    async fn names_must_be_unique_and_usable_as_a_folder() {
        let dir = TempDir::new().unwrap();
        let repos = TempDir::new().unwrap();
        let one = git_repo(repos.path(), "one");
        let two = git_repo(repos.path(), "two");
        let registry = registry(&dir).await;
        let one_path = one.to_str().unwrap();
        let two_path = two.to_str().unwrap();

        for name in [".", "..", "a/b", "a\\b", "/abs"] {
            let result = registry.register(one_path, Some(name)).await;
            assert!(matches!(result, Err(Error::Invalid(_))), "{name}");
        }
        // A blank name falls back to the folder name.
        let repo = registry.register(one_path, Some("  ")).await.unwrap();
        assert_eq!(repo.name, "one");

        let taken = registry.register(two_path, Some("one")).await;
        assert!(matches!(taken, Err(Error::Conflict(_))));
        // The same clone twice, whatever the name and however the path is spelled.
        let again = registry.register(one_path, Some("other")).await;
        assert!(matches!(again, Err(Error::Conflict(_))));
        let dotted = format!("{}/../one", one_path);
        let again = registry.register(&dotted, Some("other")).await;
        assert!(matches!(again, Err(Error::Conflict(_))));
        assert_eq!(registry.list().await.len(), 1);
    }

    #[tokio::test]
    async fn a_corrupt_or_unreadable_file_is_an_empty_registry() {
        let dir = TempDir::new().unwrap();
        let repos = TempDir::new().unwrap();
        let repo = git_repo(repos.path(), "repo");
        let path = Home::new(dir.path()).repos_path();
        for garbage in ["", "{\"version\": 1, \"repos\": [{\"id\"", "[1, 2]", "null"] {
            std::fs::write(&path, garbage).unwrap();
            assert!(registry(&dir).await.list().await.is_empty(), "{garbage}");
        }
        // A directory where the file should be cannot be read at all.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(registry(&dir).await.list().await.is_empty());
        std::fs::remove_dir(&path).unwrap();

        // Registering replaces the garbage with a good file.
        std::fs::write(&path, "garbage").unwrap();
        let registry = self::registry(&dir).await;
        registry
            .register(repo.to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(self::registry(&dir).await.list().await.len(), 1);
    }

    #[tokio::test]
    async fn reservations_check_the_ids_and_block_removal() {
        let dir = TempDir::new().unwrap();
        let repos = TempDir::new().unwrap();
        let store = Store::open(Home::new(dir.path())).await.unwrap();
        let registry = registry(&dir).await;
        let one = registry
            .register(git_repo(repos.path(), "one").to_str().unwrap(), None)
            .await
            .unwrap();
        let two = registry
            .register(git_repo(repos.path(), "two").to_str().unwrap(), None)
            .await
            .unwrap();

        let unknown = registry.reserve(&[&one.id, "nope"]).await;
        assert!(matches!(unknown.map(|_| ()), Err(Error::Invalid(_))));
        let twice = registry.reserve(&[&one.id, &two.id, &one.id]).await;
        assert!(matches!(twice.map(|_| ()), Err(Error::Invalid(_))));

        let held = registry.reserve(&[&one.id]).await.unwrap();
        assert!(matches!(
            registry.remove(&one.id, &store).await,
            Err(Error::Conflict(_))
        ));
        registry.remove(&two.id, &store).await.unwrap();
        drop(held);
        registry.remove(&one.id, &store).await.unwrap();
        assert!(matches!(
            registry.remove(&one.id, &store).await,
            Err(Error::Missing(_))
        ));
        assert!(self::registry(&dir).await.list().await.is_empty());
    }
}
