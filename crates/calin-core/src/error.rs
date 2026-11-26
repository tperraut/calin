pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request itself is wrong: a blank prompt, an unknown runner.
    #[error("{0}")]
    Invalid(String),
    #[error("unknown session {0}")]
    NotFound(String),
    /// The request is fine but the session's state does not allow it.
    #[error("{0}")]
    Conflict(String),
    #[error("the store is closed")]
    Closed,
    #[error("another Calin process is using the data directory {}", .0.display())]
    InUse(std::path::PathBuf),
    #[error("the log of session {session} is corrupt: {detail}")]
    Corrupt { session: String, detail: String },
    #[error("storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("index: {0}")]
    Index(#[from] rusqlite::Error),
    #[error("encoding: {0}")]
    Json(#[from] serde_json::Error),
}
