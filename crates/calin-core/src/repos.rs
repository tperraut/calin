//! The repos this machine knows: the ones registered by hand, kept in
//! `<home>/repos.json`, and the ones the home repo declares in its
//! `.calin/repos.yaml`, resolved to local clones by scanning.
//!
//! `repos.json` is the source of truth for what it holds: it is
//! configuration, and no event could rebuild it. That is why it is not in the
//! SQLite index, which stays a pure projection of the session logs. Besides
//! the registered repos it keeps the id of every declared repo and of the
//! home repo, so that an id means the same repo after a restart or a rescan.

mod declared;
mod scan;
mod url;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use calin_proto::{Repo, RepoSet, Settings};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use self::declared::{Declared, HOME_FILE};
use self::scan::Found;
use crate::error::{Error, Result};
use crate::json_file;
use crate::store::Store;

const FILE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Default)]
struct RegistryFile {
    version: u32,
    repos: Vec<Registered>,
    /// The id of each repo the home repo ever declared, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    declared: BTreeMap<String, String>,
    /// The id of each home repo that was not registered by hand, by path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    home: BTreeMap<String, String>,
}

/// A repo registered by hand. The fields a declaration or the scan adds to a
/// [`Repo`] are worked out when the list is asked for, never stored.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Registered {
    id: String,
    name: String,
    path: String,
    default_branch: Option<String>,
    remote_url: Option<String>,
}

/// Repo ids held by sessions that were created but have not attached their
/// repos yet. The index cannot know about those, and removing a repo from
/// under one would fail its preparation.
type Reserved = Mutex<HashMap<u64, Vec<String>>>;

pub(crate) struct RepoRegistry {
    path: PathBuf,
    /// Held across the checks and the file write of each change, so two
    /// changes never interleave. Never held while git or a scan runs.
    state: tokio::sync::Mutex<State>,
    reserved: Arc<Reserved>,
    /// One scan at a time: the later one must also be the one that stays.
    refreshing: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State {
    file: RegistryFile,
    /// What the last scan found. The list is composed from it on demand.
    catalog: Catalog,
    /// Names of the declared repos being cloned.
    cloning: HashSet<String>,
    /// Why the last clone of a declared repo failed, by name.
    clone_problems: HashMap<String, String>,
    next_reservation: u64,
}

/// The home repo and the clones on this machine, as of the last scan.
#[derive(Clone, Debug, Default)]
struct Catalog {
    home: Option<HomeRepo>,
    declared: Vec<Declared>,
    sets: Vec<(String, Vec<String>)>,
    found: Vec<Found>,
}

#[derive(Clone, Debug)]
struct HomeRepo {
    /// As the settings spell it: the canonical root.
    path: String,
    /// `None` when the path is not a work tree any more.
    clone: Option<(Option<String>, Option<String>)>,
    problem: Option<String>,
}

/// One line of the repo list, and where it comes from.
struct Entry {
    repo: Repo,
    registered: bool,
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
    /// the server must start whatever state the file is in. Nothing is
    /// scanned yet: see [`Self::refresh`].
    pub(crate) async fn load(path: PathBuf) -> Self {
        let file = path.clone();
        let file = blocking(move || Ok(read(&file))).await.unwrap_or_default();
        Self {
            path,
            state: tokio::sync::Mutex::new(State {
                file,
                ..State::default()
            }),
            reserved: Arc::default(),
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    /// Every repo: the home repo first, then by name.
    pub(crate) async fn list(&self) -> Vec<Repo> {
        let state = self.state.lock().await;
        state
            .entries()
            .into_iter()
            .map(|entry| entry.repo)
            .collect()
    }

    pub(crate) async fn get(&self, repo_id: &str) -> Option<Repo> {
        self.list()
            .await
            .into_iter()
            .find(|repo| repo.id == repo_id)
    }

    /// The home repo's named sets, in its order. A name that is no repo here
    /// is dropped.
    pub(crate) async fn sets(&self) -> Vec<RepoSet> {
        let state = self.state.lock().await;
        let repos: Vec<Repo> = state
            .entries()
            .into_iter()
            .map(|entry| entry.repo)
            .collect();
        let sets = state.catalog.sets.iter();
        sets.map(|(name, members)| {
            let mut repo_ids: Vec<String> = Vec::new();
            for member in members {
                let id = repos
                    .iter()
                    .find(|repo| repo.name == *member)
                    .map(|r| &r.id);
                if let Some(id) = id.filter(|id| !repo_ids.contains(id)) {
                    repo_ids.push(id.clone());
                }
            }
            RepoSet {
                name: name.clone(),
                repo_ids,
            }
        })
        .collect()
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
        check_name(&name)
            .map_err(|reason| Error::Invalid(format!("the name of a repo {reason}")))?;
        let Some(root) = clone.root.to_str() else {
            return Err(Error::Invalid(
                "the path of a repo must be valid UTF-8".to_owned(),
            ));
        };

        let mut state = self.state.lock().await;
        if let Some(existing) = state.file.repos.iter().find(|repo| repo.path == root) {
            return Err(Error::Conflict(format!(
                "{root} is already registered as {}",
                existing.name
            )));
        }
        // A name the home repo declares is not taken: registering it says
        // where that repo is on this machine, and it keeps its id. The same
        // goes for the home repo's own folder.
        let id = match state
            .entries()
            .into_iter()
            .find(|entry| entry.repo.name == name)
        {
            None => Ulid::generate().to_string(),
            Some(entry) if entry.registered => {
                return Err(Error::Conflict(format!(
                    "a repo named {name} is already registered"
                )));
            }
            Some(entry) if entry.repo.declared || entry.repo.path == root => entry.repo.id,
            Some(_) => {
                return Err(Error::Conflict(format!(
                    "the home repo is already listed as {name}"
                )));
            }
        };
        state.file.repos.push(Registered {
            id: id.clone(),
            name,
            path: root.to_owned(),
            default_branch: clone.default_branch,
            remote_url: clone.remote_url,
        });
        if let Err(err) = self.save(&state).await {
            state.file.repos.pop();
            return Err(err);
        }
        state
            .entries()
            .into_iter()
            .map(|entry| entry.repo)
            .find(|repo| repo.id == id)
            .ok_or_else(|| Error::Missing(format!("unknown repo {id}")))
    }

    /// Removes the registry entry. The clone, its worktrees and its branches
    /// are left alone. A repo the home repo declares stays for as long as it
    /// is declared, and the home repo for as long as the settings name it.
    pub(crate) async fn remove(&self, repo_id: &str, store: &Store) -> Result<()> {
        let mut state = self.state.lock().await;
        let entries = state.entries();
        let Some(entry) = entries.iter().find(|entry| entry.repo.id == repo_id) else {
            return Err(Error::Missing(format!("unknown repo {repo_id}")));
        };
        let name = &entry.repo.name;
        if entry.repo.declared {
            return Err(Error::Conflict(format!(
                "{name} is declared by the home repo; remove it from {HOME_FILE} instead"
            )));
        }
        if !entry.registered {
            return Err(Error::Conflict(format!(
                "{name} is the home repo; choose another one in the settings instead"
            )));
        }
        let reserved = lock(&self.reserved)
            .values()
            .any(|ids| ids.iter().any(|id| id == repo_id));
        if reserved || store.repo_in_use(repo_id).await? {
            return Err(Error::Conflict(format!(
                "{name} is attached to a session that is still open"
            )));
        }
        // The home repo stays listed, and keeps the id sessions know it by.
        let keeps_id = entry.repo.home.then(|| entry.repo.path.clone());
        let before = state.file.repos.clone();
        state.file.repos.retain(|repo| repo.id != repo_id);
        if let Some(path) = keeps_id {
            state.file.home.insert(path, repo_id.to_owned());
        }
        if let Err(err) = self.save(&state).await {
            state.file.repos = before;
            return Err(err);
        }
        Ok(())
    }

    /// Checks that every id names a repo with a clone on this machine and
    /// that none is named twice, then holds them against removal.
    pub(crate) async fn reserve(&self, repo_ids: &[&str]) -> Result<Reservation> {
        let mut state = self.state.lock().await;
        let entries = state.entries();
        for (index, repo_id) in repo_ids.iter().enumerate() {
            let Some(entry) = entries.iter().find(|entry| entry.repo.id == *repo_id) else {
                return Err(Error::Invalid(format!("unknown repo {repo_id}")));
            };
            if repo_ids[..index].contains(repo_id) {
                return Err(Error::Invalid(format!(
                    "repo {repo_id} is attached more than once"
                )));
            }
            if !entry.repo.available {
                return Err(not_available(&entry.repo));
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

    /// Reads the home repo's file again and scans for clones, off the
    /// runtime, then gives every newly declared repo its id. `settings` are
    /// the ones in effect.
    pub(crate) async fn refresh(&self, settings: &Settings) -> Result<()> {
        let _one_at_a_time = self.refreshing.lock().await;
        let settings = settings.clone();
        let catalog = blocking(move || Ok(read_catalog(&settings))).await?;
        let mut state = self.state.lock().await;
        state.catalog = catalog;

        let mut new_ids = false;
        let names: Vec<String> = state
            .catalog
            .declared
            .iter()
            .map(|d| d.name.clone())
            .collect();
        for name in names {
            let known = state.file.declared.contains_key(&name)
                || state.file.repos.iter().any(|repo| repo.name == name);
            if !known {
                state
                    .file
                    .declared
                    .insert(name, Ulid::generate().to_string());
                new_ids = true;
            }
        }
        if let Some(path) = state.catalog.home.as_ref().map(|home| home.path.clone()) {
            let known = state.file.home.contains_key(&path)
                || state.file.repos.iter().any(|repo| repo.path == path);
            if !known {
                state.file.home.insert(path, Ulid::generate().to_string());
                new_ids = true;
            }
        }
        // A failure is only worth showing while the repo still needs a clone.
        let missing: HashSet<String> = state
            .entries()
            .into_iter()
            .filter(|entry| entry.repo.declared && !entry.repo.available)
            .map(|entry| entry.repo.name)
            .collect();
        state
            .clone_problems
            .retain(|name, _| missing.contains(name));
        if new_ids {
            self.save(&state).await?;
        }
        Ok(())
    }

    /// Starts cloning a declared repo into `<clone root>/<name>` and returns
    /// at once. The repo reports `cloning` until the clone is found by a
    /// rescan, or failed and left its reason as the repo's `problem`.
    pub(crate) async fn start_clone(
        self: &Arc<Self>,
        repo_id: &str,
        settings: Settings,
    ) -> Result<()> {
        let (name, url, dest) = {
            let mut state = self.state.lock().await;
            let entries = state.entries();
            let Some(entry) = entries.iter().find(|entry| entry.repo.id == repo_id) else {
                return Err(Error::Missing(format!("unknown repo {repo_id}")));
            };
            let repo = &entry.repo;
            if !repo.declared {
                return Err(Error::Conflict(format!(
                    "{} is not declared by the home repo, so there is nothing to clone it from",
                    repo.name
                )));
            }
            if repo.available {
                return Err(Error::Conflict(format!(
                    "{} already has a clone at {}",
                    repo.name, repo.path
                )));
            }
            let Some(url) = repo.url.clone() else {
                return Err(Error::Conflict(format!(
                    "the home repo declares no url for {}",
                    repo.name
                )));
            };
            if repo.cloning {
                return Err(Error::Conflict(format!(
                    "{} is already being cloned",
                    repo.name
                )));
            }
            // The effective settings always name a clone root.
            let root = settings.clone_root.clone().unwrap_or_default();
            let dest = Path::new(&root).join(&repo.name);
            let name = repo.name.clone();
            state.cloning.insert(name.clone());
            state.clone_problems.remove(&name);
            (name, url, dest)
        };
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            let cloned = blocking(move || Ok(calin_git::clone_repo(&url, &dest)?)).await;
            let problem = match cloned {
                // The rescan is what finds the clone and makes the repo available.
                Ok(_) => registry.refresh(&settings).await.err(),
                Err(err) => Some(err),
            };
            let mut state = registry.state.lock().await;
            state.cloning.remove(&name);
            if let Some(err) = problem {
                tracing::warn!(repo = %name, %err, "cloning failed");
                state.clone_problems.insert(name, clone_problem(&err));
            }
        });
        Ok(())
    }

    async fn save(&self, state: &State) -> Result<()> {
        let path = self.path.clone();
        let file = RegistryFile {
            version: FILE_VERSION,
            repos: state.file.repos.clone(),
            declared: state.file.declared.clone(),
            home: state.file.home.clone(),
        };
        blocking(move || json_file::write(&path, &file)).await
    }
}

impl State {
    /// The repo list: registered and declared repos merged by name, each
    /// declared one resolved to a clone, and the home repo marked or added.
    fn entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = self
            .file
            .repos
            .iter()
            .map(|repo| Entry {
                repo: Repo {
                    id: repo.id.clone(),
                    name: repo.name.clone(),
                    path: repo.path.clone(),
                    default_branch: repo.default_branch.clone(),
                    remote_url: repo.remote_url.clone(),
                    available: true,
                    declared: false,
                    home: false,
                    url: None,
                    setup: None,
                    checks: Vec::new(),
                    cloning: false,
                    problem: None,
                },
                registered: true,
            })
            .collect();

        for declared in &self.catalog.declared {
            let describe = |repo: &mut Repo| {
                repo.declared = true;
                repo.url.clone_from(&declared.url);
                repo.setup.clone_from(&declared.setup);
                repo.checks.clone_from(&declared.checks);
            };
            // Registered by hand under this name: that is where it is.
            if let Some(entry) = entries.iter_mut().find(|e| e.repo.name == declared.name) {
                describe(&mut entry.repo);
                continue;
            }
            // Refresh gave every declared name an id before this could run.
            let Some(id) = self.file.declared.get(&declared.name) else {
                continue;
            };
            let found = self.resolve(declared);
            let mut repo = Repo {
                id: id.clone(),
                name: declared.name.clone(),
                path: found.map(|clone| clone.path.clone()).unwrap_or_default(),
                default_branch: found.and_then(|clone| clone.default_branch.clone()),
                remote_url: found.and_then(|clone| clone.remote_url.clone()),
                available: found.is_some(),
                declared: true,
                home: false,
                url: None,
                setup: None,
                checks: Vec::new(),
                cloning: found.is_none() && self.cloning.contains(&declared.name),
                problem: None,
            };
            if found.is_none() {
                repo.problem = self.clone_problems.get(&declared.name).cloned();
            }
            describe(&mut repo);
            entries.push(Entry {
                repo,
                registered: false,
            });
        }

        if let Some(home) = &self.catalog.home {
            let listed = entries
                .iter_mut()
                .find(|entry| entry.repo.available && entry.repo.path == home.path);
            match listed {
                Some(entry) => {
                    entry.repo.home = true;
                    if home.problem.is_some() {
                        entry.repo.problem.clone_from(&home.problem);
                    }
                }
                None => {
                    if let Some(id) = self.file.home.get(&home.path) {
                        let (default_branch, remote_url) = home.clone.clone().unwrap_or_default();
                        let repo = Repo {
                            id: id.clone(),
                            name: home_name(&home.path, &entries),
                            path: home.path.clone(),
                            default_branch,
                            remote_url,
                            available: home.clone.is_some(),
                            declared: false,
                            home: true,
                            url: None,
                            setup: None,
                            checks: Vec::new(),
                            cloning: false,
                            problem: home.problem.clone(),
                        };
                        entries.push(Entry {
                            repo,
                            registered: false,
                        });
                    }
                }
            }
        }

        entries.sort_by(|a, b| {
            // `false < true`, so the home repo sorts first.
            (!a.repo.home, &a.repo.name, &a.repo.id).cmp(&(!b.repo.home, &b.repo.name, &b.repo.id))
        });
        entries
    }

    /// The clone of a declared repo: the one whose remote is the declared
    /// URL, or, for a repo declared without one, the folder of its name
    /// directly under a scan root.
    fn resolve(&self, declared: &Declared) -> Option<&Found> {
        match &declared.url {
            Some(declared_url) => {
                let key = url::key(declared_url);
                let by_remote = |clone: &&Found| clone.key.as_deref() == Some(key.as_str());
                self.catalog.found.iter().find(by_remote)
            }
            None => {
                let by_name = |clone: &&Found| clone.direct && clone.name == declared.name;
                self.catalog.found.iter().find(by_name)
            }
        }
    }
}

/// The home repo's folder name, made different from every name in use.
fn home_name(path: &str, entries: &[Entry]) -> String {
    let folder = Path::new(path).file_name().and_then(|name| name.to_str());
    let base = folder
        .filter(|name| check_name(name).is_ok())
        .unwrap_or("home");
    let taken = |name: &str| entries.iter().any(|entry| entry.repo.name == name);
    if !taken(base) {
        return base.to_owned();
    }
    let mut name = format!("{base}-home");
    let mut count = 1;
    while taken(&name) {
        count += 1;
        name = format!("{base}-home-{count}");
    }
    name
}

/// Blocking. Looks at the home repo, reads its file and scans for clones.
/// Nothing here fails: what is wrong becomes the home repo's `problem`.
fn read_catalog(settings: &Settings) -> Catalog {
    let mut catalog = Catalog::default();
    if let Some(path) = &settings.home_repo {
        let mut home = HomeRepo {
            path: path.clone(),
            clone: None,
            problem: None,
        };
        match calin_git::inspect_clone(Path::new(path)) {
            Ok(clone) => {
                home.clone = Some((clone.default_branch, clone.remote_url));
                match read_home_file(&clone.root) {
                    Ok(file) => {
                        catalog.declared = file.repos;
                        catalog.sets = file.sets;
                        if !file.skipped.is_empty() {
                            home.problem =
                                Some(format!("{HOME_FILE}: {}", file.skipped.join("; ")));
                        }
                    }
                    Err(problem) => home.problem = Some(problem),
                }
            }
            Err(err) => home.problem = Some(err.to_string()),
        }
        catalog.home = Some(home);
    }
    // The clone root last: a clone the user made themselves wins over one
    // Calin made.
    let mut roots: Vec<PathBuf> = settings.scan_roots.iter().map(PathBuf::from).collect();
    roots.extend(settings.clone_root.as_ref().map(PathBuf::from));
    catalog.found = scan::scan(&roots);
    catalog
}

fn read_home_file(home: &Path) -> std::result::Result<declared::HomeFile, String> {
    let path = home.join(HOME_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("the home repo has no {HOME_FILE}"));
        }
        Err(err) => return Err(format!("{HOME_FILE} cannot be read: {err}")),
    };
    declared::parse(&text).map_err(|err| format!("{HOME_FILE} does not parse: {err}"))
}

/// What a failed clone left to say, without the command line around it.
fn clone_problem(err: &Error) -> String {
    let text = err.to_string();
    // `git: `git clone ...` failed: <git's words>`.
    let words = text
        .split_once("` failed: ")
        .map_or(text.as_str(), |(_, words)| words);
    words.trim().to_owned()
}

fn not_available(repo: &Repo) -> Error {
    let hint = if repo.url.is_some() {
        "clone it first"
    } else {
        "register its clone first"
    };
    Error::Conflict(format!(
        "{} has no clone on this machine yet; {hint}",
        repo.name
    ))
}

/// A repo's name is the folder of its worktree inside a session. The error
/// completes "the name of a repo ...".
fn check_name(name: &str) -> std::result::Result<(), &'static str> {
    if name.is_empty() {
        Err("must not be empty")
    } else if name == "." || name == ".." {
        Err("must not be . or ..")
    } else if name.contains(['/', '\\', '\0']) {
        Err("must not contain a path separator")
    } else {
        Ok(())
    }
}

fn read(path: &Path) -> RegistryFile {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return RegistryFile::default(),
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "cannot read the repo registry; starting with none");
            return RegistryFile::default();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        tracing::warn!(path = %path.display(), %err, "the repo registry is corrupt; starting with none");
        RegistryFile::default()
    })
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
mod tests;
