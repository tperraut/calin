//! Machine-local settings, kept in `<home>/settings.json`.

use std::path::{Path, PathBuf};

use calin_proto::Settings;

use crate::error::{Error, Result};
use crate::home::Home;
use crate::json_file;
use crate::repos::blocking;

/// How many root sessions prepare or run at once when the settings do not
/// say (the "queue" semantics in `calin-proto/src/api.rs`).
pub(crate) const DEFAULT_MAX_PARALLEL: u32 = 4;
/// The most the limit can be set to: each running session is an agent process.
const MAX_MAX_PARALLEL: u32 = 64;

pub(crate) struct SettingsStore {
    path: PathBuf,
    default_clone_root: PathBuf,
    /// Held across the validation and the file write of a change.
    current: tokio::sync::Mutex<Settings>,
}

impl SettingsStore {
    /// Reads the settings. A file that is missing is the defaults; one that
    /// cannot be read or parsed is the defaults too, with one warning: the
    /// server must start whatever state the file is in.
    pub(crate) async fn load(home: &Home) -> Self {
        let path = home.settings_path();
        let file = path.clone();
        let current = blocking(move || Ok(read(&file))).await.unwrap_or_default();
        Self {
            path,
            default_clone_root: home.default_clone_root(),
            current: tokio::sync::Mutex::new(current),
        }
    }

    /// The settings in effect: what is stored, with the clone root and the
    /// session limit filled in.
    pub(crate) async fn get(&self) -> Settings {
        self.effective(self.current.lock().await.clone())
    }

    /// Validates `wanted`, stores it in place of the current settings and
    /// returns the settings then in effect.
    pub(crate) async fn replace(&self, wanted: Settings) -> Result<Settings> {
        let mut current = self.current.lock().await;
        let settings = blocking(move || validate(wanted)).await?;
        let (path, stored) = (self.path.clone(), settings.clone());
        blocking(move || json_file::write(&path, &stored)).await?;
        *current = settings.clone();
        Ok(self.effective(settings))
    }

    fn effective(&self, mut settings: Settings) -> Settings {
        if settings.clone_root.is_none() {
            settings.clone_root = Some(self.default_clone_root.to_string_lossy().into_owned());
        }
        // A file edited by hand may hold a limit `replace` would refuse.
        let limit = settings.max_parallel.unwrap_or(DEFAULT_MAX_PARALLEL);
        settings.max_parallel = Some(limit.clamp(1, MAX_MAX_PARALLEL));
        settings
    }
}

fn read(path: &Path) -> Settings {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        // The state of a server nobody configured yet.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "cannot read the settings; using the defaults");
            return Settings::default();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        tracing::warn!(path = %path.display(), %err, "the settings are corrupt; using the defaults");
        Settings::default()
    })
}

/// Blocking. Checks the settings against the disk and puts every path in
/// its canonical form, so that later comparisons are plain string ones.
fn validate(wanted: Settings) -> Result<Settings> {
    let home_repo = match filled(wanted.home_repo) {
        None => None,
        Some(path) => {
            let path = absolute("the home repo", &path)?;
            let clone = calin_git::inspect_clone(&path)
                .map_err(|err| Error::Invalid(format!("the home repo: {err}")))?;
            Some(utf8("the home repo", &clone.root)?)
        }
    };
    let mut scan_roots: Vec<String> = Vec::new();
    for root in wanted.scan_roots {
        // A blank line of a form is not a root.
        let Some(root) = filled(Some(root)) else {
            continue;
        };
        let path = absolute("a scan root", &root)?;
        let real = std::fs::canonicalize(&path)
            .ok()
            .filter(|real| real.is_dir())
            .ok_or_else(|| {
                Error::Invalid(format!("the scan root {root} is not an existing directory"))
            })?;
        let real = utf8("a scan root", &real)?;
        if !scan_roots.contains(&real) {
            scan_roots.push(real);
        }
    }
    let clone_root = match filled(wanted.clone_root) {
        None => None,
        Some(root) => {
            let path = absolute("the clone root", &root)?;
            // It may not exist yet: the first clone creates it.
            if path.exists() && !path.is_dir() {
                return Err(Error::Invalid(format!(
                    "the clone root {root} is not a directory"
                )));
            }
            Some(utf8("the clone root", &path)?)
        }
    };
    if let Some(limit) = wanted.max_parallel
        && !(1..=MAX_MAX_PARALLEL).contains(&limit)
    {
        return Err(Error::Invalid(format!(
            "max_parallel must be between 1 and {MAX_MAX_PARALLEL}; it is {limit}"
        )));
    }
    Ok(Settings {
        home_repo,
        scan_roots,
        clone_root,
        max_parallel: wanted.max_parallel,
    })
}

/// The trimmed value, or `None` when there is nothing in it: how every
/// optional text of a request or a file is read.
pub(crate) fn filled(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn absolute(what: &str, path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(Error::Invalid(format!(
            "the path of {what} must be absolute"
        )))
    }
}

fn utf8(what: &str, path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid(format!("the path of {what} must be valid UTF-8")))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::testing::{git_repo, write_file};

    fn text(path: &Path) -> String {
        path.to_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn defaults_fill_in_the_clone_root_and_nothing_is_written_until_asked() {
        let dir = TempDir::new().unwrap();
        let home = Home::new(dir.path());
        let store = SettingsStore::load(&home).await;
        assert_eq!(
            store.get().await,
            Settings {
                home_repo: None,
                scan_roots: Vec::new(),
                clone_root: Some(text(&dir.path().join("repos"))),
                max_parallel: Some(4),
            }
        );
        assert!(!home.settings_path().exists());
    }

    #[tokio::test]
    async fn settings_are_validated_normalized_persisted_and_replaced_whole() {
        let dir = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let home = Home::new(dir.path());
        let repo = git_repo(work.path(), "home");
        let code = std::fs::canonicalize(work.path()).unwrap().join("code");
        std::fs::create_dir(&code).unwrap();
        let store = SettingsStore::load(&home).await;

        let stored = store
            .replace(Settings {
                // Spelled the long way round, with blanks around it.
                home_repo: Some(format!("  {}/../home ", repo.display())),
                scan_roots: vec![text(&code), "  ".to_owned(), format!("{}/", code.display())],
                clone_root: Some(text(&work.path().join("not/yet/there"))),
                max_parallel: Some(2),
            })
            .await
            .unwrap();
        let expected = Settings {
            home_repo: Some(text(&repo)),
            scan_roots: vec![text(&code)],
            clone_root: Some(text(&work.path().join("not/yet/there"))),
            max_parallel: Some(2),
        };
        assert_eq!(stored, expected);
        assert_eq!(store.get().await, expected);
        // A new process reads the same settings from the file.
        assert_eq!(SettingsStore::load(&home).await.get().await, expected);
        assert!(!dir.path().join("settings.json.tmp").exists());

        // A replacement is whole: what it leaves out is gone, and blank
        // values are absent ones.
        let cleared = store
            .replace(Settings {
                home_repo: Some(String::new()),
                scan_roots: Vec::new(),
                clone_root: Some("  ".to_owned()),
                max_parallel: None,
            })
            .await
            .unwrap();
        assert_eq!(cleared.home_repo, None);
        assert_eq!(cleared.clone_root, Some(text(&dir.path().join("repos"))));
        // The default is filled in on read and never stored.
        assert_eq!(cleared.max_parallel, Some(4));
        let on_disk: Settings =
            serde_json::from_slice(&std::fs::read(home.settings_path()).unwrap()).unwrap();
        assert_eq!(on_disk, Settings::default());
    }

    #[tokio::test]
    async fn what_does_not_hold_on_disk_is_invalid_and_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let repo = git_repo(work.path(), "home");
        write_file(&repo, "src/lib.rs", "");
        let file = work.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();
        let store = SettingsStore::load(&Home::new(dir.path())).await;
        let good = Settings {
            home_repo: Some(text(&repo)),
            ..Settings::default()
        };
        store.replace(good.clone()).await.unwrap();

        let with_home = |home: &str| Settings {
            home_repo: Some(home.to_owned()),
            ..Settings::default()
        };
        let with_root = |root: &str| Settings {
            scan_roots: vec![text(work.path()), root.to_owned()],
            ..good.clone()
        };
        let with_clone_root = |root: &str| Settings {
            clone_root: Some(root.to_owned()),
            ..good.clone()
        };
        let with_limit = |limit: u32| Settings {
            max_parallel: Some(limit),
            ..good.clone()
        };
        for limit in [1, 64] {
            store.replace(with_limit(limit)).await.unwrap();
            assert_eq!(store.get().await.max_parallel, Some(limit));
        }
        let before = store.replace(good.clone()).await.unwrap();
        let bad = [
            with_limit(0),
            with_limit(65),
            // Not the root of a work tree, in each way a path can fail to be.
            with_home(&text(&repo.join("src"))),
            with_home(&text(work.path())),
            with_home(&text(&work.path().join("missing"))),
            with_home("relative/home"),
            with_root(&text(&work.path().join("missing"))),
            with_root(&text(&file)),
            with_root("relative/root"),
            with_clone_root("relative/clones"),
            with_clone_root(&text(&file)),
        ];
        for settings in bad {
            let result = store.replace(settings.clone()).await;
            assert!(matches!(result, Err(Error::Invalid(_))), "{settings:?}");
        }
        assert_eq!(store.get().await, before);
    }

    #[tokio::test]
    async fn a_corrupt_or_unreadable_file_is_the_defaults() {
        let dir = TempDir::new().unwrap();
        let home = Home::new(dir.path());
        let path = home.settings_path();
        let defaults = SettingsStore::load(&home).await.get().await;
        for garbage in ["", "{\"home_repo\": 7}", "[1, 2]", "{\"scan_roots\""] {
            std::fs::write(&path, garbage).unwrap();
            assert_eq!(
                SettingsStore::load(&home).await.get().await,
                defaults,
                "{garbage}"
            );
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(SettingsStore::load(&home).await.get().await, defaults);
        std::fs::remove_dir(&path).unwrap();

        // A limit out of range in a file edited by hand is brought into it.
        for (stored, limit) in [("0", 1), ("900", 64), ("7", 7)] {
            std::fs::write(&path, format!("{{\"max_parallel\": {stored}}}")).unwrap();
            let read = SettingsStore::load(&home).await.get().await;
            assert_eq!(read.max_parallel, Some(limit), "{stored}");
        }

        // Unknown keys of a newer version are ignored, and saving replaces garbage.
        std::fs::write(&path, "{\"scan_roots\": [], \"later\": true}").unwrap();
        let store = SettingsStore::load(&home).await;
        assert_eq!(store.get().await, defaults);
        std::fs::write(&path, "garbage").unwrap();
        store.replace(Settings::default()).await.unwrap();
        assert_eq!(SettingsStore::load(&home).await.get().await, defaults);
    }
}
