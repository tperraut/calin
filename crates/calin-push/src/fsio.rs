//! Files that hold secrets: written whole and atomically, mode 0600.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

pub(crate) fn corrupt_path(path: &Path) -> PathBuf {
    with_suffix(path, ".corrupt")
}

fn temporary_path(path: &Path) -> PathBuf {
    with_suffix(path, ".tmp")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Replaces the file at `path` with `bytes`, so that a reader, or a restart
/// after a crash at any point, sees the old content or the new, never a mix.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = write_temporary(path, bytes)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    sync_directory(path);
    Ok(())
}

/// Creates the file at `path` with `bytes` only if no file is there, and
/// atomically: a concurrent creator, or a crash, never leaves a partial file.
/// Returns `false` when a file already existed (nothing was written).
pub(crate) fn create_atomically(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let temporary = write_temporary(path, bytes)?;
    // A hard link fails if the target exists, unlike a rename: the existing
    // key file, which subscriptions depend on, can never be replaced here.
    let linked = std::fs::hard_link(&temporary, path);
    let _ = std::fs::remove_file(&temporary);
    match linked {
        Ok(()) => {
            sync_directory(path);
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// Makes a rename durable. Not every filesystem can sync a directory, and
/// the content is already safe either way.
fn sync_directory(path: &Path) {
    if let Some(directory) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && let Ok(directory) = std::fs::File::open(directory)
    {
        let _ = directory.sync_all();
    }
}

/// Writes `bytes` to a new mode-0600 file next to `path` and syncs it.
pub(crate) fn write_temporary(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let temporary = temporary_path(path);
    // A leftover from a crash is removed, not reused: `create_new` then
    // guarantees a file this process created, with the mode asked for.
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(temporary)
}

/// Sets the file to mode 0600 if anyone but its owner can access it.
/// Returns whether it had to.
#[cfg(unix)]
pub(crate) fn tighten_permissions(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 == 0 {
        return Ok(false);
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(true)
}

#[cfg(not(unix))]
pub(crate) fn tighten_permissions(_path: &Path) -> io::Result<bool> {
    Ok(false)
}

#[cfg(all(test, unix))]
pub(crate) fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_whole_file_with_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.json");
        std::fs::write(&path, b"old content that is longer").unwrap();
        // A crash leftover is not reused.
        std::fs::write(dir.path().join("f.json.tmp"), b"junk").unwrap();
        write_atomically(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(mode(&path), 0o600);
        assert!(!dir.path().join("f.json.tmp").exists());
    }

    #[test]
    fn create_never_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k");
        assert!(create_atomically(&path, b"first").unwrap());
        assert_eq!(mode(&path), 0o600);
        assert!(!create_atomically(&path, b"second").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        assert!(!dir.path().join("k.tmp").exists());
    }

    #[test]
    fn permissions_are_tightened() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(tighten_permissions(&path).unwrap());
        assert_eq!(mode(&path), 0o600);
        assert!(!tighten_permissions(&path).unwrap());
    }
}
