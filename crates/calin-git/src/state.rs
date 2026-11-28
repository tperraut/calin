//! What a file is on disk right now, reduced to a value that changes whenever
//! its content, kind or executable bit does: how the orchestrator notices
//! changes nobody reported.

use std::fs::{self, File, OpenOptions};
use std::ops::ControlFlow;
use std::path::Path;

use sha2::{Digest as _, Sha256};

use crate::cmd::work_dir;
use crate::content::{hex, read_chunks};
use crate::error::{Error, Result};
use crate::path::{metadata_no_follow, resolve_no_follow};

/// A path's state on disk. Two equal values mean the file did not change in
/// any way git would record.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FileState {
    /// Nothing at the path.
    Absent,
    /// A regular file: lowercase hex SHA-256 of its bytes, and whether git
    /// would store it as executable.
    File { sha256: String, executable: bool },
    /// A symbolic link, by its target, which git stores as its content.
    Link(String),
    /// A directory (a nested repository), a pipe, a socket, or a path that
    /// could not be read or that leaves the root. Never opened.
    Other,
}

/// The state of each of `paths` (relative to `dir`, `/`-separated) in order.
///
/// Every file is read whole and hashed: no stat cache can be fooled by a
/// write that keeps the size and lands in the same timestamp tick. A link is
/// never followed and nothing but a regular file is opened, so a link to
/// `/dev/zero` or a pipe in the tree cannot hang the caller.
///
/// Only a `dir` that is not a directory fails the call; a path that cannot
/// be read has the state [`FileState::Other`].
pub fn file_states(dir: &Path, paths: &[&str]) -> Result<Vec<FileState>> {
    let dir = work_dir(dir)?;
    Ok(paths.iter().map(|path| file_state(&dir, path)).collect())
}

fn file_state(dir: &Path, path: &str) -> FileState {
    let Ok(at) = resolve_no_follow(dir, path) else {
        return FileState::Other;
    };
    match metadata_no_follow(&at.abs) {
        Ok(Some(meta)) => state_of(&at.abs, &meta),
        Ok(None) => FileState::Absent,
        Err(_) => FileState::Other,
    }
}

/// The state of what `meta` says is at `path`.
fn state_of(path: &Path, meta: &fs::Metadata) -> FileState {
    if meta.is_symlink() {
        return fs::read_link(path).map_or(FileState::Other, |target| {
            FileState::Link(target.to_string_lossy().into_owned())
        });
    }
    if !meta.is_file() {
        return FileState::Other;
    }
    match hash_file(path) {
        Ok(Some((sha256, executable))) => FileState::File { sha256, executable },
        Ok(None) => FileState::Other,
        Err(Error::NotFound(_)) => FileState::Absent,
        Err(_) => FileState::Other,
    }
}

/// Hash and executable bit of a regular file, or `None` when the path turned
/// into something else since it was looked at.
fn hash_file(path: &Path) -> Result<Option<(String, bool)>> {
    let mut file = open_no_follow(path)?;
    // Looked at again through the open file: the path may have been swapped.
    let meta = file.metadata().map_err(|e| Error::io(path, e))?;
    if !meta.is_file() {
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    read_chunks(&mut file, |chunk| {
        hasher.update(chunk);
        ControlFlow::Continue(())
    })
    .map_err(|e| Error::io(path, e))?;
    Ok(Some((hex(hasher.finalize().as_slice()), executable(&meta))))
}

/// Opens without following a final link and without blocking on a pipe that
/// replaced the file after it was looked at.
#[cfg(unix)]
fn open_no_follow(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits() as i32)
        .open(path)
        .map_err(|e| Error::io(path, e))
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|e| Error::io(path, e))
}

/// Git records one bit: whether the owner may execute the file.
#[cfg(unix)]
fn executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::process::Command;

    use super::*;
    use crate::testing::{Sandbox, write};

    fn state(dir: &Path, path: &str) -> FileState {
        file_states(dir, &[path]).unwrap().remove(0)
    }

    #[test]
    fn a_file_is_known_by_its_content_and_its_executable_bit() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("d");
        write(&dir, "a.txt", "one\n");
        let first = state(&dir, "a.txt");
        let FileState::File { sha256, executable } = &first else {
            panic!("a file, not {first:?}");
        };
        // SHA-256 of "one\n".
        assert_eq!(
            sha256,
            "2c8b08da5ce60398e1f19af0e5dccc744df274b826abe585eaba68c525434806"
        );
        assert!(!executable);
        // Same length, same line count, other bytes.
        write(&dir, "a.txt", "two\n");
        let second = state(&dir, "a.txt");
        assert_ne!(first, second);
        write(&dir, "a.txt", "one\n");
        assert_eq!(state(&dir, "a.txt"), first);
        let path = dir.join("a.txt");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            state(&dir, "a.txt"),
            FileState::File {
                executable: true,
                ..
            }
        ));
    }

    #[test]
    fn links_pipes_and_directories_are_never_opened() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("d");
        symlink("/dev/zero", dir.join("zero")).unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        let made = Command::new("mkfifo")
            .arg(dir.join("pipe"))
            .status()
            .unwrap();
        assert!(made.success());
        let states = file_states(
            &dir,
            &["zero", "sub", "pipe", "missing", "sub/missing", "../x"],
        )
        .unwrap();
        assert_eq!(
            states,
            [
                FileState::Link("/dev/zero".to_owned()),
                FileState::Other,
                FileState::Other,
                FileState::Absent,
                FileState::Absent,
                FileState::Other,
            ]
        );
    }

    #[test]
    fn a_file_in_an_unreadable_directory_is_other() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("d");
        write(&dir, "locked/f.txt", "f\n");
        fs::set_permissions(dir.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let states = file_states(&dir, &["locked/f.txt"]);
        fs::set_permissions(dir.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(states.unwrap(), [FileState::Other]);
    }

    #[test]
    fn a_directory_that_is_gone_fails_the_call() {
        let sandbox = Sandbox::new();
        let dir = sandbox.path().join("gone");
        assert!(file_states(&dir, &["a"]).is_err());
    }
}
