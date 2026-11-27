pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request itself is wrong: a blank prompt, an unknown runner, a path
    /// that leaves its repo.
    #[error("{0}")]
    Invalid(String),
    #[error("unknown session {0}")]
    NotFound(String),
    /// Something other than a session does not exist: a repo, a permission
    /// request, a file.
    #[error("{0}")]
    Missing(String),
    /// The request is fine but the current state does not allow it.
    #[error("{0}")]
    Conflict(String),
    #[error("the store is closed")]
    Closed,
    #[error("another Calin process is using the data directory {}", .0.display())]
    InUse(std::path::PathBuf),
    #[error("the log of session {session} is corrupt: {detail}")]
    Corrupt { session: String, detail: String },
    /// `git` or the file system failed in a way the caller cannot fix.
    #[error("git: {0}")]
    Git(String),
    #[error("storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("index: {0}")]
    Index(#[from] rusqlite::Error),
    #[error("encoding: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// True when the caller asked for something that cannot be done, as
    /// opposed to the server failing at something it should be able to do.
    pub fn is_refusal(&self) -> bool {
        matches!(
            self,
            Self::Invalid(_) | Self::NotFound(_) | Self::Missing(_) | Self::Conflict(_)
        )
    }
}

/// The mapping `calin-git` documents: each of its variants is one HTTP status.
impl From<calin_git::Error> for Error {
    fn from(err: calin_git::Error) -> Self {
        use calin_git::Error as Git;
        match err {
            Git::InvalidPath { .. } | Git::NotARepository { .. } => Self::Invalid(err.to_string()),
            Git::NotFound(_) => Self::Missing(err.to_string()),
            Git::Conflict(_) => Self::Conflict(err.to_string()),
            Git::Git { .. } | Git::Io { .. } => Self::Git(err.to_string()),
        }
    }
}
