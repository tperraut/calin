//! Path safety: the only way a path from outside becomes a path on disk.

use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// Turns a path relative to `root` into an absolute path that is guaranteed to
/// be inside `root`, with every symlink on the way resolved.
///
/// `rel` uses `/` separators; empty names the root itself. Empty and `.`
/// segments are dropped. The path is refused with [`Error::InvalidPath`] when it
///
/// - is absolute or contains a `..` segment,
/// - has a component named `.git` (in any letter case), or reaches a `.git`
///   directory through a symlink,
/// - goes through a symlink that leads outside `root` or cannot be resolved.
///
/// The path does not have to exist. Its deepest existing ancestor is resolved
/// and checked, and the remaining segments are appended as they are, so a new
/// file in a new directory is fine.
///
/// This checks the filesystem as it is now. A process that swaps a directory
/// for a symlink afterwards can still redirect a later read or write.
pub fn resolve_path(root: &Path, rel: &str) -> Result<PathBuf> {
    Ok(resolve(root, rel)?.abs)
}

/// A path that passed [`resolve_path`].
pub(crate) struct Resolved {
    /// The canonical root.
    pub(crate) root: PathBuf,
    /// Inside `root`. See the constructor for whether the last component is resolved.
    pub(crate) abs: PathBuf,
    /// The request, cleaned: `/`-separated, no empty or `.` segments, `""` for the root.
    pub(crate) rel: String,
}

/// [`resolve_path`], keeping the canonical root and the cleaned relative path.
pub(crate) fn resolve(root: &Path, rel: &str) -> Result<Resolved> {
    let root = fs::canonicalize(root).map_err(|e| Error::io(root, e))?;
    let parts = clean(rel)?;
    let abs = contain(&root, &parts, rel)?;
    Ok(Resolved {
        root,
        abs,
        rel: parts.join("/"),
    })
}

/// Like [`resolve`], but the last component is left as it is: when it is a
/// symlink, `abs` names the link, not its target. Its parent directory went
/// through the full check. Callers must not open `abs` without looking at it
/// with `symlink_metadata` first.
pub(crate) fn resolve_no_follow(root: &Path, rel: &str) -> Result<Resolved> {
    let root = fs::canonicalize(root).map_err(|e| Error::io(root, e))?;
    let parts = clean(rel)?;
    let Some((name, parent)) = parts.split_last() else {
        return Err(Error::invalid_path(rel, "names the root, not a file"));
    };
    let abs = contain(&root, parent, rel)?.join(name);
    Ok(Resolved {
        root,
        abs,
        rel: parts.join("/"),
    })
}

/// The lexical rules: splits `rel` into its segments or refuses it.
pub(crate) fn clean(rel: &str) -> Result<Vec<&str>> {
    if rel.contains('\0') {
        return Err(Error::invalid_path(rel, "contains a NUL byte"));
    }
    if rel.starts_with('/') || Path::new(rel).is_absolute() {
        return Err(Error::invalid_path(rel, "is absolute"));
    }
    let mut parts = Vec::new();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(Error::invalid_path(rel, "contains a `..` segment")),
            _ if is_dot_git(part) => {
                return Err(Error::invalid_path(rel, "`.git` is off limits"));
            }
            _ => parts.push(part),
        }
    }
    Ok(parts)
}

/// Case-insensitive because macOS and Windows filesystems are.
pub(crate) fn is_dot_git(name: &str) -> bool {
    name.eq_ignore_ascii_case(".git")
}

/// The filesystem rules: resolves the deepest existing prefix of `parts`
/// under the canonical `root` and checks where it really is.
fn contain(root: &Path, parts: &[&str], rel: &str) -> Result<PathBuf> {
    for existing in (0..=parts.len()).rev() {
        let mut candidate = root.to_path_buf();
        candidate.extend(&parts[..existing]);
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {}
            // Not there yet (or below a plain file): try one level up.
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                continue;
            }
            Err(e) => return Err(Error::io(&candidate, e)),
        }
        // The entry exists, so a failure here is a dangling or looping
        // symlink. Its target is unknown, so it cannot be trusted.
        let real = fs::canonicalize(&candidate).map_err(|_| {
            Error::invalid_path(rel, "goes through a symlink that does not resolve")
        })?;
        let inside = real
            .strip_prefix(root)
            .map_err(|_| Error::invalid_path(rel, "leads outside the root through a symlink"))?;
        let reaches_git = inside
            .components()
            .any(|c| matches!(c, Component::Normal(name) if name.to_str().is_some_and(is_dot_git)));
        if reaches_git {
            return Err(Error::invalid_path(
                rel,
                "leads into `.git` through a symlink",
            ));
        }
        let mut abs = real;
        abs.extend(&parts[existing..]);
        return Ok(abs);
    }
    // Only reached when the root vanished after it was canonicalized.
    Err(Error::NotFound(root.display().to_string()))
}

/// An absolute path with its existing part canonicalized, for comparing
/// locations that may not exist yet.
pub(crate) fn absolutize(path: &Path) -> Result<PathBuf> {
    let display = || path.display().to_string();
    if !path.is_absolute() {
        return Err(Error::invalid_path(display(), "must be absolute"));
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(Error::invalid_path(display(), "contains a `..` segment"));
    }
    for ancestor in path.ancestors() {
        if let Ok(real) = fs::canonicalize(ancestor) {
            // `ancestors` only yields prefixes of `path`, so this cannot fail.
            let rest = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
            return Ok(real.join(rest));
        }
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::testing::Sandbox;

    fn invalid(root: &Path, rel: &str) {
        match resolve_path(root, rel) {
            Err(Error::InvalidPath { .. }) => {}
            other => panic!("{rel:?} should be an invalid path, got {other:?}"),
        }
    }

    #[test]
    fn lexical_rules() {
        let sandbox = Sandbox::new();
        let root = sandbox.repo("repo");
        for rel in [
            "..",
            "../x",
            "a/../b",
            "a/..",
            "/etc/passwd",
            "/",
            ".git",
            ".git/config",
            "a/.git/x",
            ".GIT/config",
            "a\0b",
        ] {
            invalid(&root, rel);
        }
    }

    #[test]
    fn accepts_plain_paths_and_normalizes_them() {
        let sandbox = Sandbox::new();
        let root = sandbox.repo("repo");
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::write(root.join("src/deep/a.rs"), "x").unwrap();

        assert_eq!(resolve_path(&root, "").unwrap(), root);
        assert_eq!(resolve_path(&root, ".").unwrap(), root);
        assert_eq!(
            resolve_path(&root, "src/deep/a.rs").unwrap(),
            root.join("src/deep/a.rs")
        );
        assert_eq!(resolve(&root, "./src//deep/").unwrap().rel, "src/deep");
        // Names that merely look like `.git` or `..` are ordinary names.
        assert_eq!(
            resolve_path(&root, ".gitignore").unwrap(),
            root.join(".gitignore")
        );
        assert_eq!(
            resolve_path(&root, "a..b/...").unwrap(),
            root.join("a..b/...")
        );
    }

    #[test]
    fn a_path_that_does_not_exist_yet_is_fine() {
        let sandbox = Sandbox::new();
        let root = sandbox.repo("repo");
        fs::create_dir(root.join("src")).unwrap();
        assert_eq!(
            resolve_path(&root, "src/new/deeper/file.txt").unwrap(),
            root.join("src/new/deeper/file.txt")
        );
        // Below a plain file: nothing to resolve, the caller's read or write fails.
        assert_eq!(
            resolve_path(&root, "README.md/x").unwrap(),
            root.join("README.md/x")
        );
    }

    #[test]
    fn symlinks_out_of_the_root_are_refused() {
        let sandbox = Sandbox::new();
        let root = sandbox.repo("repo");
        let outside = sandbox.dir("outside");
        fs::write(outside.join("secret"), "s").unwrap();
        symlink(outside.join("secret"), root.join("leak")).unwrap();
        symlink(&outside, root.join("out")).unwrap();
        symlink("../outside/secret", root.join("relative-leak")).unwrap();

        invalid(&root, "leak");
        invalid(&root, "relative-leak");
        invalid(&root, "out");
        invalid(&root, "out/secret");
        // The parent escapes even though the file itself does not exist.
        invalid(&root, "out/new/file.txt");
    }

    #[test]
    fn symlinks_inside_the_root_are_followed() {
        let sandbox = Sandbox::new();
        let root = sandbox.repo("repo");
        fs::create_dir(root.join("real")).unwrap();
        fs::write(root.join("real/f"), "x").unwrap();
        symlink("real", root.join("alias")).unwrap();
        symlink("real/f", root.join("f-link")).unwrap();

        assert_eq!(resolve_path(&root, "alias/f").unwrap(), root.join("real/f"));
        assert_eq!(resolve_path(&root, "f-link").unwrap(), root.join("real/f"));
        assert_eq!(
            resolve_path(&root, "alias/new.txt").unwrap(),
            root.join("real/new.txt")
        );
        // The no-follow form keeps the link itself but still resolves its parent.
        assert_eq!(
            resolve_no_follow(&root, "f-link").unwrap().abs,
            root.join("f-link")
        );
        assert_eq!(
            resolve_no_follow(&root, "alias/f").unwrap().abs,
            root.join("real/f")
        );
    }

    #[test]
    fn dangling_and_git_bound_symlinks_are_refused() {
        let sandbox = Sandbox::new();
        let root = sandbox.repo("repo");
        // A dangling link could be created outside the root by a write.
        symlink(sandbox.path().join("nowhere"), root.join("dangling")).unwrap();
        symlink("loop-b", root.join("loop-a")).unwrap();
        symlink("loop-a", root.join("loop-b")).unwrap();
        symlink(".git", root.join("sneaky")).unwrap();

        invalid(&root, "dangling");
        invalid(&root, "dangling/x");
        invalid(&root, "loop-a");
        invalid(&root, "sneaky");
        invalid(&root, "sneaky/config");
    }

    #[test]
    fn the_root_may_itself_be_reached_through_a_symlink() {
        let sandbox = Sandbox::new();
        let root = sandbox.repo("repo");
        let alias = sandbox.path().join("alias");
        symlink(&root, &alias).unwrap();
        assert_eq!(
            resolve_path(&alias, "README.md").unwrap(),
            root.join("README.md")
        );
        assert!(matches!(
            resolve_path(&sandbox.path().join("missing"), "x"),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn no_follow_needs_a_file_name() {
        let sandbox = Sandbox::new();
        let root = sandbox.repo("repo");
        assert!(matches!(
            resolve_no_follow(&root, ""),
            Err(Error::InvalidPath { .. })
        ));
        let outside = sandbox.dir("outside");
        symlink(&outside, root.join("out")).unwrap();
        assert!(matches!(
            resolve_no_follow(&root, "out/file"),
            Err(Error::InvalidPath { .. })
        ));
    }
}
