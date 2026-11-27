use std::io;
use std::path::{Path, PathBuf};

/// Why an operation failed. Each variant maps to one HTTP status in the server.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A path from outside is not acceptable: absolute, contains `..` or
    /// `.git`, leaves the root through a symlink, or names the wrong kind of
    /// entry (a directory where a file is expected, and so on). 400.
    #[error("invalid path {path:?}: {reason}")]
    InvalidPath { path: String, reason: String },

    /// The file, directory, commit or start point does not exist. 404.
    #[error("not found: {0}")]
    NotFound(String),

    /// Not possible in the current state: the file changed since it was read,
    /// the name is already in use, the branch is checked out elsewhere. 409.
    #[error("conflict: {0}")]
    Conflict(String),

    /// The directory is not the root of a git work tree. 400.
    #[error("{} is not the root of a git work tree: {reason}", path.display())]
    NotARepository { path: PathBuf, reason: String },

    /// `git` (or `gh`) ran and failed for a reason none of the variants above
    /// covers, or answered with something that makes no sense. 500.
    #[error("`{command}` failed: {stderr}")]
    Git {
        /// The command line, for logs.
        command: String,
        /// Trimmed standard error, or the exit status when nothing was printed.
        stderr: String,
    },

    /// The filesystem, or starting `git` itself, failed. 500.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn invalid_path(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidPath {
            path: path.into(),
            reason: reason.into(),
        }
    }

    /// Wraps a filesystem error on `path`, turning "no such file" into [`Error::NotFound`].
    pub(crate) fn io(path: &Path, source: io::Error) -> Self {
        if source.kind() == io::ErrorKind::NotFound {
            Self::NotFound(path.display().to_string())
        } else {
            Self::Io {
                context: path.display().to_string(),
                source,
            }
        }
    }
}
