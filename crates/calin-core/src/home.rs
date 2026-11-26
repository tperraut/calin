//! The data directory. Every path Calin writes to is derived here.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Overrides the data directory.
pub const ENV_HOME: &str = "CALIN_HOME";

const DEFAULT_DIR: &str = ".calin";

#[derive(Debug, thiserror::Error)]
pub enum HomeError {
    #[error("cannot find the user's home directory; set {ENV_HOME} or pass --home")]
    NoUserHome,
    #[error("cannot resolve the data directory: {0}")]
    Io(#[from] std::io::Error),
}

/// The root of Calin's machine-local state (design spec, "State on disk").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The SQLite index.
    pub fn db_path(&self) -> PathBuf {
        self.root.join("calin.db")
    }

    /// Held locked by the process that owns the data directory.
    pub fn lock_path(&self) -> PathBuf {
        self.root.join("calin.lock")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(session_id)
    }

    /// The session's append-only event log.
    pub fn log_path(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("events.ndjson")
    }

    /// What the session was created with. The events do not carry the title
    /// or the runner, so a reindex reads them from here.
    pub fn meta_path(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("session.json")
    }
}

/// The data directory: `explicit` (the `--home` flag) if given, else
/// `CALIN_HOME`, else `~/.calin`. This is the only place that reads the
/// environment for it, and only the binary calls it.
pub fn resolve_home(explicit: Option<PathBuf>) -> Result<Home, HomeError> {
    let root = pick_root(explicit, std::env::var_os(ENV_HOME), std::env::home_dir())
        .ok_or(HomeError::NoUserHome)?;
    Ok(Home::new(std::path::absolute(root)?))
}

fn pick_root(
    explicit: Option<PathBuf>,
    env_home: Option<OsString>,
    user_home: Option<PathBuf>,
) -> Option<PathBuf> {
    explicit
        .or_else(|| {
            env_home
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| user_home.map(|home| home.join(DEFAULT_DIR)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_wins_over_the_environment_and_the_default() {
        let user = Some(PathBuf::from("/home/u"));
        assert_eq!(
            pick_root(Some("/flag".into()), Some("/env".into()), user.clone()),
            Some(PathBuf::from("/flag"))
        );
        assert_eq!(
            pick_root(None, Some("/env".into()), user.clone()),
            Some(PathBuf::from("/env"))
        );
        assert_eq!(
            pick_root(None, None, user.clone()),
            Some(PathBuf::from("/home/u/.calin"))
        );
        assert_eq!(
            pick_root(None, Some(OsString::new()), user),
            Some(PathBuf::from("/home/u/.calin"))
        );
        assert_eq!(pick_root(None, None, None), None);
    }

    #[test]
    fn every_path_is_under_the_root() {
        let home = Home::new("/data");
        assert_eq!(home.db_path(), PathBuf::from("/data/calin.db"));
        assert_eq!(
            home.log_path("s1"),
            PathBuf::from("/data/sessions/s1/events.ndjson")
        );
        assert_eq!(
            home.meta_path("s1"),
            PathBuf::from("/data/sessions/s1/session.json")
        );
    }
}
