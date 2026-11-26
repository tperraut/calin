//! Everything in Calin that runs `git` or touches a checkout.
//!
//! # Model
//!
//! - A **clone** is a repository the user registered: the root of an ordinary
//!   (non-bare) work tree. [`inspect_clone`] validates and describes it.
//! - A **worktree** is a `git worktree` of a clone, on the session's branch
//!   ([`session_branch_name`]). [`create_worktree`] and [`remove_worktree`]
//!   manage it; the branch outlives the worktree, which is how a session
//!   resumes.
//! - A **work directory** is where a session's files are: a worktree, or the
//!   clone itself for a repo attached `read` or `live`. The functions that
//!   take a `dir` want the root of one. They do not search parent
//!   directories for a repository.
//! - The **base** is the commit a session's changes are measured against: the
//!   commit its worktree started at ([`Worktree::base_ref`]), or
//!   [`head_commit`] at attach time for a repo edited in place. [`changes`]
//!   and [`file_diff`] compare the base with the files on disk, so commits,
//!   staged, unstaged and untracked work all count.
//!
//! # Blocking
//!
//! Every function here blocks: it runs `git` and waits, or reads and writes
//! files. Call them from `tokio::task::spawn_blocking`, never directly on the
//! runtime.
//!
//! # Paths
//!
//! A path that comes from outside (`path` and `old_path` parameters) is
//! relative to the root, uses `/`, and goes through [`resolve_path`] before
//! anything touches the disk. Paths in results have the same form.
//!
//! # What this crate never does
//!
//! Delete a branch, force-push, or reset or clean a checkout. Read-only git
//! commands take no optional locks, so they do not get in the way of an
//! agent's own git in the same work tree.
//!
//! # Errors
//!
//! One [`Error`], whose variants map to HTTP statuses: `InvalidPath` and
//! `NotARepository` to 400, `NotFound` to 404, `Conflict` to 409, `Git` and
//! `Io` to 500.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//!
//! # fn main() -> calin_git::Result<()> {
//! let clone = calin_git::inspect_clone(Path::new("/home/me/code/api"))?;
//! let branch = calin_git::session_branch_name("Fix the login bug", "01KB2M5S7Q9ZC4X8HVTPA3DWEN");
//! let session_dir = Path::new("/home/me/.calin/sessions/01KB2M5S7Q9ZC4X8HVTPA3DWEN/api");
//! let worktree = calin_git::create_worktree(&clone.root, session_dir, &branch, None)?;
//!
//! // The agent works in `worktree.path`. Store `worktree.base_ref` with the session.
//! for file in calin_git::changes(&worktree.path, &worktree.base_ref)? {
//!     let diff = calin_git::file_diff(
//!         &worktree.path,
//!         &worktree.base_ref,
//!         &file.path,
//!         file.old_path.as_deref(),
//!         1024 * 1024,
//!     )?;
//!     println!("{} +{} -{} binary={}", file.path, file.added, file.removed, diff.binary);
//! }
//!
//! // The branch stays; `create_worktree` with the same arguments brings the worktree back.
//! calin_git::remove_worktree(&clone.root, &worktree.path)?;
//! # Ok(())
//! # }
//! ```

mod branch;
mod changes;
mod cmd;
mod content;
mod error;
mod files;
mod path;
mod repo;
#[cfg(test)]
mod testing;
mod worktree;

pub use branch::session_branch_name;
pub use changes::{changes, file_diff};
pub use error::{Error, Result};
pub use files::{list_dir, read_file, write_file};
pub use path::resolve_path;
pub use repo::{CloneInfo, head_commit, inspect_clone};
pub use worktree::{Worktree, WorktreeOutcome, create_worktree, remove_worktree};
