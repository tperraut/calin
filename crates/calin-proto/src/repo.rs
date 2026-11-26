use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A local git clone Calin knows about.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Repo {
    pub id: String,
    /// Unique, and used as the folder name of the repo inside a session.
    pub name: String,
    /// Absolute path of the clone on this machine.
    pub path: String,
    pub default_branch: Option<String>,
    pub remote_url: Option<String>,
}

/// How a repo takes part in a session.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RepoMode {
    /// A git worktree on the session's branch, inside the session folder.
    #[default]
    Write,
    /// The clone itself, with edits denied.
    Read,
    /// The clone itself, edited in place on whatever branch it is on.
    Live,
}

/// One repo of a session's repo set.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct SessionRepo {
    pub id: String,
    pub session_id: String,
    pub repo_id: String,
    /// The repo's name, unique within the session.
    pub name: String,
    pub mode: RepoMode,
    /// Absolute path the agent works in: the worktree, or the clone itself.
    pub path: String,
    /// The session's branch in this repo. Absent for `read` and `live`.
    pub branch: Option<String>,
    /// The commit the session's changes are measured against.
    pub base_ref: Option<String>,
    /// True for the repo the agent's working directory is in.
    pub primary: bool,
}

/// What happened to a file.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum FileOp {
    Create,
    Edit,
    Delete,
    Rename,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RegisterRepoRequest {
    /// Absolute path of a git clone on the server's machine.
    pub path: String,
    /// Defaults to the folder name.
    #[serde(default)]
    #[ts(optional)]
    pub name: Option<String>,
}

/// A repo to attach when a session is created.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct AttachRepoRequest {
    pub repo_id: String,
    /// `write` when absent.
    #[serde(default)]
    #[ts(optional)]
    pub mode: Option<RepoMode>,
}

/// How a path differs from the session's base in one repo.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    /// New and not yet known to git.
    Untracked,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ChangedFile {
    /// Relative to the repo root, with `/` separators.
    pub path: String,
    /// The previous path of a renamed file.
    pub old_path: Option<String>,
    pub status: ChangeStatus,
    pub added: u32,
    pub removed: u32,
    pub binary: bool,
}

/// Everything a session changed in one repo: committed on its branch,
/// staged, unstaged and untracked, all measured against `base_ref`.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RepoChanges {
    pub session_repo_id: String,
    pub name: String,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub files: Vec<ChangedFile>,
    pub added: u32,
    pub removed: u32,
}

/// Both sides of one changed file, for a side-by-side or inline diff view.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    /// Content at the base; absent for a new file, a binary file or one that is too large.
    pub old: Option<String>,
    /// Content in the working tree; absent for a deleted file, a binary file or one that is too large.
    pub new: Option<String>,
    pub binary: bool,
    /// True when a side was left out because it exceeds the size limit.
    pub too_large: bool,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// One entry of a directory listing.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct TreeEntry {
    pub name: String,
    /// Relative to the repo root, with `/` separators.
    pub path: String,
    pub kind: EntryKind,
    /// Bytes, for files.
    #[ts(type = "number | null")]
    pub size: Option<u64>,
    /// True when git ignores the entry.
    pub ignored: bool,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct FileContent {
    pub path: String,
    /// Absent for a binary file or one that is too large to edit.
    pub text: Option<String>,
    pub binary: bool,
    pub too_large: bool,
    #[ts(type = "number")]
    pub size: u64,
    /// Identifies this exact content. Send it back as `expected_hash` when saving.
    pub hash: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct WriteFileRequest {
    pub text: String,
    /// The `hash` of the content the edit started from. When it no longer
    /// matches the file on disk the save is refused with 409, so an agent's
    /// concurrent edit is never overwritten silently. Absent when creating a file.
    #[serde(default)]
    #[ts(optional)]
    pub expected_hash: Option<String>,
}
