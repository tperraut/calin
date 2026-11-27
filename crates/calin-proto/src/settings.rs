use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Machine-local settings, stored in the data directory.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct Settings {
    /// Absolute path of the home repo: the repo sessions are launched from,
    /// which lists the other repos in `.calin/repos.yaml`. Absent until set.
    #[serde(default)]
    pub home_repo: Option<String>,
    /// Directories searched for local clones of the repos the home repo declares.
    #[serde(default)]
    pub scan_roots: Vec<String>,
    /// Where a declared repo with no local clone is cloned to. The server
    /// fills in its default (`<data dir>/repos`) when this is absent.
    #[serde(default)]
    pub clone_root: Option<String>,
    /// How many root sessions may be preparing or running at once. Others wait
    /// as `queued`, oldest first. The server fills in its default (4) when
    /// this is absent. Child sessions do not count: they have their own limit.
    #[serde(default)]
    pub max_parallel: Option<u32>,
}

/// A named group of repos from the home repo's `.calin/repos.yaml`.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RepoSet {
    pub name: String,
    /// In the order the home repo lists them. Unknown names are dropped.
    pub repo_ids: Vec<String>,
}
