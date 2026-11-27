//! A session's files: what it changed, diffs, directory listings, reading
//! and writing. Everything here is `calin-git` behind `spawn_blocking`.

use std::path::{Path, PathBuf};

use calin_proto::{
    EventBody, FileContent, FileDiff, FileOp, RepoChanges, RepoMode, SessionRepo, TreeEntry,
    WriteFileRequest,
};

use crate::error::{Error, Result};
use crate::manager::SessionManager;
use crate::repos::blocking;
use crate::store::EventDraft;

/// The largest file, or side of a diff, whose content is sent to a client.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

impl SessionManager {
    /// What the session changed, one entry per attached repo, primary first.
    /// A repo without a base, or attached `read`, has no files.
    pub async fn changes(&self, session_id: &str) -> Result<Vec<RepoChanges>> {
        let repos = self.get(session_id).await?.repos;
        let mut all = Vec::with_capacity(repos.len());
        for repo in repos {
            let files = match &repo.base_ref {
                Some(base) if repo.mode != RepoMode::Read => {
                    let (dir, base) = (PathBuf::from(&repo.path), base.clone());
                    blocking(move || Ok(calin_git::changes(&dir, &base)?)).await?
                }
                _ => Vec::new(),
            };
            all.push(RepoChanges {
                added: files.iter().map(|file| file.added).sum(),
                removed: files.iter().map(|file| file.removed).sum(),
                session_repo_id: repo.id,
                name: repo.name,
                branch: repo.branch,
                base_ref: repo.base_ref,
                files,
            });
        }
        Ok(all)
    }

    /// Both sides of one file: at the repo's base and on disk.
    pub async fn file_diff(
        &self,
        session_id: &str,
        session_repo_id: &str,
        path: &str,
    ) -> Result<FileDiff> {
        let repo = self.session_repo(session_id, session_repo_id).await?;
        let Some(base) = repo.base_ref else {
            return Err(Error::Conflict(format!(
                "{} has no commit to compare with",
                repo.name
            )));
        };
        let (dir, path) = (PathBuf::from(repo.path), path.to_owned());
        blocking(move || {
            // The library shows a link as git stores it, without following
            // it. The contract is stricter: a path that leads outside the
            // repo is refused on every route, this one included.
            calin_git::resolve_path(&dir, &path)?;
            let diff = calin_git::file_diff(&dir, &base, &path, None, MAX_FILE_BYTES)?;
            if diff.old.is_some() {
                return Ok(diff);
            }
            // Nothing at this path in the base. The route cannot say where a
            // renamed file came from, so look the old path up.
            let renamed_from = calin_git::changes(&dir, &base)?
                .into_iter()
                .find(|file| file.path == diff.path)
                .and_then(|file| file.old_path);
            match renamed_from {
                Some(old) => Ok(calin_git::file_diff(
                    &dir,
                    &base,
                    &path,
                    Some(&old),
                    MAX_FILE_BYTES,
                )?),
                None => Ok(diff),
            }
        })
        .await
    }

    /// One level of a directory of a session repo. An empty `path` is the root.
    pub async fn tree(
        &self,
        session_id: &str,
        session_repo_id: &str,
        path: &str,
    ) -> Result<Vec<TreeEntry>> {
        let repo = self.session_repo(session_id, session_repo_id).await?;
        let (dir, path) = (PathBuf::from(repo.path), path.to_owned());
        blocking(move || Ok(calin_git::list_dir(&dir, &path)?)).await
    }

    pub async fn read_file(
        &self,
        session_id: &str,
        session_repo_id: &str,
        path: &str,
    ) -> Result<FileContent> {
        let repo = self.session_repo(session_id, session_repo_id).await?;
        let (dir, path) = (PathBuf::from(repo.path), path.to_owned());
        blocking(move || Ok(calin_git::read_file(&dir, &path, MAX_FILE_BYTES)?)).await
    }

    /// Saves a file from the editor and records the change in the session's
    /// log, attributed to the user.
    pub async fn write_file(
        &self,
        session_id: &str,
        session_repo_id: &str,
        path: &str,
        request: WriteFileRequest,
    ) -> Result<FileContent> {
        let repo = self.session_repo(session_id, session_repo_id).await?;
        if repo.mode == RepoMode::Read {
            return Err(Error::Conflict(format!(
                "{} is attached read-only",
                repo.name
            )));
        }
        // Without a hash the library only writes a file that does not exist yet.
        let op = match request.expected_hash {
            Some(_) => FileOp::Edit,
            None => FileOp::Create,
        };
        let (dir, path) = (PathBuf::from(repo.path), path.to_owned());
        let content = blocking(move || {
            Ok(calin_git::write_file(
                &dir,
                &path,
                &request.text,
                request.expected_hash.as_deref(),
            )?)
        })
        .await?;
        let changed = EventBody::FileChanged {
            session_repo_id: Some(repo.id),
            path: content.path.clone(),
            op,
        };
        self.store()
            .append(session_id, EventDraft::new(changed))
            .await?;
        Ok(content)
    }

    /// Where a terminal of the session starts: the root of the given repo,
    /// or the agent's working directory.
    pub async fn terminal_dir(
        &self,
        session_id: &str,
        session_repo_id: Option<&str>,
    ) -> Result<PathBuf> {
        if let Some(session_repo_id) = session_repo_id {
            let repo = self.session_repo(session_id, session_repo_id).await?;
            return Ok(PathBuf::from(repo.path));
        }
        let detail = self.get(session_id).await?;
        let home = self.store().home().clone();
        working_directory(&home.work_dir(session_id), &detail.repos).await
    }

    async fn session_repo(&self, session_id: &str, session_repo_id: &str) -> Result<SessionRepo> {
        self.get(session_id)
            .await?
            .repos
            .into_iter()
            .find(|repo| repo.id == session_repo_id)
            .ok_or_else(|| {
                Error::Missing(format!(
                    "session {session_id} has no repo {session_repo_id}"
                ))
            })
    }
}

/// The directory the session's agent works in: its primary repo, or the
/// session's own work folder (created here) when it has no repo.
pub(crate) async fn working_directory(work_dir: &Path, repos: &[SessionRepo]) -> Result<PathBuf> {
    if let Some(primary) = primary(repos) {
        return Ok(PathBuf::from(&primary.path));
    }
    let work_dir = work_dir.to_owned();
    blocking(move || {
        std::fs::create_dir_all(&work_dir)?;
        Ok(work_dir)
    })
    .await
}

pub(crate) fn primary(repos: &[SessionRepo]) -> Option<&SessionRepo> {
    repos
        .iter()
        .find(|repo| repo.primary)
        .or_else(|| repos.first())
}

/// Places a path an agent reported in the session's repo set: the id of the
/// repo whose root is the longest prefix of it and the path below that root
/// with `/` separators, or `None` and the path as it is when no repo holds it.
pub(crate) fn locate(repos: &[SessionRepo], location: &str) -> (Option<String>, String) {
    let path = Path::new(location);
    let mut best: Option<(&SessionRepo, usize, String)> = None;
    for repo in repos {
        let root = Path::new(&repo.path);
        let Ok(below) = path.strip_prefix(root) else {
            continue;
        };
        let parts: Option<Vec<&str>> = below.iter().map(|part| part.to_str()).collect();
        // The root itself is not a file of the repo.
        let Some(parts) = parts.filter(|parts| !parts.is_empty()) else {
            continue;
        };
        let depth = root.components().count();
        if best.as_ref().is_none_or(|(_, deepest, _)| depth > *deepest) {
            best = Some((repo, depth, parts.join("/")));
        }
    }
    match best {
        Some((repo, _, relative)) => (Some(repo.id.clone()), relative),
        None => (None, location.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(id: &str, path: &str) -> SessionRepo {
        SessionRepo {
            id: id.to_owned(),
            session_id: "s".to_owned(),
            repo_id: format!("repo-{id}"),
            name: id.to_owned(),
            mode: RepoMode::Write,
            path: path.to_owned(),
            branch: None,
            base_ref: None,
            primary: false,
        }
    }

    #[test]
    fn paths_are_placed_in_the_repo_with_the_longest_matching_root() {
        let repos = [
            repo("outer", "/work/outer"),
            repo("inner", "/work/outer/vendor/inner"),
            repo("other", "/work/other"),
        ];
        let at = |location| locate(&repos, location);
        assert_eq!(
            at("/work/outer/src/lib.rs"),
            (Some("outer".to_owned()), "src/lib.rs".to_owned())
        );
        assert_eq!(
            at("/work/outer/vendor/inner/a/b.txt"),
            (Some("inner".to_owned()), "a/b.txt".to_owned())
        );
        assert_eq!(
            at("/work/other/x"),
            (Some("other".to_owned()), "x".to_owned())
        );
        // A sibling whose name merely starts like a root is not inside it.
        assert_eq!(at("/work/outer2/x"), (None, "/work/outer2/x".to_owned()));
        assert_eq!(at("/etc/hosts"), (None, "/etc/hosts".to_owned()));
        assert_eq!(at("relative/x"), (None, "relative/x".to_owned()));
        // A root is a directory, never a changed file of its own repo.
        assert_eq!(at("/work/other"), (None, "/work/other".to_owned()));
        assert_eq!(
            at("/work/outer/vendor/inner"),
            (Some("outer".to_owned()), "vendor/inner".to_owned())
        );
        assert_eq!(locate(&[], "/a"), (None, "/a".to_owned()));
    }

    #[test]
    fn the_primary_repo_is_the_flagged_one_else_the_first() {
        let mut repos = vec![repo("a", "/a"), repo("b", "/b")];
        assert_eq!(primary(&repos).unwrap().id, "a");
        repos[1].primary = true;
        assert_eq!(primary(&repos).unwrap().id, "b");
        assert!(primary(&[]).is_none());
    }
}
