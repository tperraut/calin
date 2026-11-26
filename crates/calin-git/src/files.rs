//! The file browser and editor: list a directory, read a file, write a file.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use calin_proto::{EntryKind, FileContent, TreeEntry};

use crate::cmd::Git;
use crate::content::{decode, hash, read_hashed};
use crate::error::{Error, Result};
use crate::path::{is_dot_git, resolve, resolve_no_follow};

/// One level of the directory `path` inside the work directory `dir`.
///
/// `path` is relative to `dir`; empty lists the root. Directories come first,
/// then everything else, each group sorted by name without regard to case.
/// `.git` is never listed. Symlinks are reported as such and not followed.
/// `ignored` is what `git check-ignore` says, asked once for the whole
/// listing; where git cannot say (inside a submodule), nothing is marked.
///
/// [`Error::NotFound`] when the directory does not exist,
/// [`Error::InvalidPath`] when `path` is not a directory.
pub fn list_dir(dir: &Path, path: &str) -> Result<Vec<TreeEntry>> {
    let at = resolve(dir, path)?;
    let not_a_dir = || Error::invalid_path(path, "is not a directory");
    let read = fs::read_dir(&at.abs).map_err(|e| match e.kind() {
        ErrorKind::NotADirectory => not_a_dir(),
        _ => Error::io(&at.abs, e),
    })?;

    // Git refuses to look beyond a symlink, so it is asked about the real
    // location, which differs from `at.rel` when that goes through a link.
    // `resolve` guarantees the prefix, and with it that this cannot fail.
    let real_rel = at.abs.strip_prefix(&at.root).unwrap_or(Path::new(""));

    let mut entries = Vec::new();
    let mut queries = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| Error::io(&at.abs, e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_dot_git(&name) {
            continue;
        }
        // Neither call follows a symlink.
        let file_type = entry.file_type().map_err(|e| Error::io(&entry.path(), e))?;
        let (kind, size) = if file_type.is_symlink() {
            (EntryKind::Symlink, None)
        } else if file_type.is_dir() {
            (EntryKind::Dir, None)
        } else {
            // Gone already when an agent deletes it under us; then there is no size.
            (EntryKind::File, entry.metadata().ok().map(|m| m.len()))
        };
        // The `./` keeps a name that starts with `:` from being read as pathspec magic.
        queries.push(format!("./{}", real_rel.join(&name).to_string_lossy()));
        entries.push(TreeEntry {
            path: if at.rel.is_empty() {
                name.clone()
            } else {
                format!("{}/{name}", at.rel)
            },
            name,
            kind,
            size,
            ignored: false,
        });
    }

    let ignored = ignored_paths(&at.root, &queries)?;
    for (entry, query) in entries.iter_mut().zip(&queries) {
        entry.ignored = ignored.contains(query);
    }
    entries.sort_by_cached_key(|e| {
        (
            e.kind != EntryKind::Dir,
            e.name.to_lowercase(),
            e.name.clone(),
        )
    });
    Ok(entries)
}

/// Which of `paths` (relative to `root`) git ignores, in one process. The
/// answer repeats each ignored path exactly as it was asked.
fn ignored_paths(root: &Path, paths: &[String]) -> Result<HashSet<String>> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    let done = Git::read(root)
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(input)
        .exec()?;
    match done.status.code() {
        Some(0) => {}
        Some(1) => return Ok(HashSet::new()),
        // Git will not answer for paths inside a submodule. The listing is
        // still worth showing, with nothing marked.
        _ if done.stderr.contains("is in submodule") => return Ok(HashSet::new()),
        _ => return Err(done.into_error()),
    }
    Ok(done
        .stdout
        .split(|b| *b == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

/// Reads the file `path` inside the work directory `dir`.
///
/// `hash` (lowercase hex SHA-256 of the raw bytes) and `size` are always set.
/// `text` is absent when the file is larger than `max_bytes` (`too_large`) or
/// is not text (`binary`: a NUL byte or invalid UTF-8). A symlink that stays
/// inside `dir` is followed.
///
/// [`Error::NotFound`] when there is no such file, [`Error::InvalidPath`]
/// when `path` is unsafe or names something that is not a regular file.
pub fn read_file(dir: &Path, path: &str, max_bytes: u64) -> Result<FileContent> {
    let at = resolve(dir, path)?;
    let meta = fs::metadata(&at.abs).map_err(|e| match e.kind() {
        // A parent that is a file: the path names nothing.
        ErrorKind::NotADirectory => Error::NotFound(at.abs.display().to_string()),
        _ => Error::io(&at.abs, e),
    })?;
    if meta.is_dir() {
        return Err(Error::invalid_path(path, "is a directory"));
    }
    if !meta.is_file() {
        // Opening a pipe or a device could block for ever.
        return Err(Error::invalid_path(path, "is not a regular file"));
    }

    let read = read_hashed(&at.abs, max_bytes)?;
    let too_large = read.bytes.is_none();
    let text = read.bytes.and_then(decode);
    Ok(FileContent {
        path: at.rel,
        binary: !too_large && text.is_none(),
        text,
        too_large,
        size: read.size,
        hash: read.hash,
    })
}

/// Writes `text` to the file `path` inside the work directory `dir` and
/// returns what [`read_file`] would now return.
///
/// `expected_hash` is the `hash` of the content the edit started from:
///
/// - given, the file must still have that content. If it changed or is gone,
///   the write is refused with [`Error::Conflict`].
/// - absent, the file is being created and must not exist yet, else
///   [`Error::Conflict`].
///
/// Nothing is changed when the write is refused. Otherwise the content goes
/// to a temporary file in the same directory that is then renamed over the
/// target, so a reader sees the old content or the new, never a mix. Missing
/// parent directories are created, and an existing file keeps its permission
/// bits.
///
/// The check and the rename are not one atomic step: an agent writing the
/// same file in between loses its write.
///
/// [`Error::InvalidPath`] when `path` is unsafe, is a symlink (wherever it
/// points) or names something that is not a regular file.
pub fn write_file(
    dir: &Path,
    path: &str,
    text: &str,
    expected_hash: Option<&str>,
) -> Result<FileContent> {
    // The last component stays unresolved: a link there is refused, not followed.
    let at = resolve_no_follow(dir, path)?;
    let existing = match fs::symlink_metadata(&at.abs) {
        Ok(meta) => Some(meta),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => None,
        Err(e) => return Err(Error::io(&at.abs, e)),
    };
    if let Some(meta) = &existing {
        if meta.is_symlink() {
            return Err(Error::invalid_path(path, "is a symlink"));
        }
        if meta.is_dir() {
            return Err(Error::invalid_path(path, "is a directory"));
        }
        if !meta.is_file() {
            return Err(Error::invalid_path(path, "is not a regular file"));
        }
    }
    match (expected_hash, &existing) {
        (None, None) => {}
        (None, Some(_)) => {
            return Err(Error::Conflict(format!("{} already exists", at.rel)));
        }
        (Some(_), None) => {
            return Err(Error::Conflict(format!("{} no longer exists", at.rel)));
        }
        (Some(expected), Some(_)) => {
            // Only the hash is needed, so keep none of the content.
            let current = read_hashed(&at.abs, 0)?.hash;
            if !current.eq_ignore_ascii_case(expected) {
                return Err(Error::Conflict(format!(
                    "{} changed since it was read",
                    at.rel
                )));
            }
        }
    }

    // `resolve_no_follow` appended a file name, so there is a parent.
    let parent = at.abs.parent().unwrap_or(&at.root);
    fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    let (temp, file) = create_temp(parent)?;
    let written = fill(&temp, file, text.as_bytes(), existing.as_ref())
        .and_then(|()| fs::rename(&temp, &at.abs).map_err(|e| Error::io(&at.abs, e)));
    if let Err(e) = written {
        // Best effort: the write already failed, and that is the error to report.
        let _ = fs::remove_file(&temp);
        return Err(e);
    }

    let binary = text.contains('\0');
    Ok(FileContent {
        path: at.rel,
        text: (!binary).then(|| text.to_owned()),
        binary,
        too_large: false,
        size: text.len() as u64,
        hash: hash(text.as_bytes()),
    })
}

/// Distinguishes the temporary files of concurrent writes in one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new, empty file in `parent`. Created the ordinary way rather than with
/// owner-only permissions, so a new file gets the mode the umask gives.
fn create_temp(parent: &Path) -> Result<(PathBuf, File)> {
    let mut last = None;
    // A name can only be taken by a leftover of a crashed process with the same pid.
    for _ in 0..32 {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".calin-write-{}-{n}.tmp", process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(Error::io(&temp, e)),
        }
    }
    Err(Error::Io {
        context: format!("creating a temporary file in {}", parent.display()),
        source: last.unwrap_or_else(|| ErrorKind::AlreadyExists.into()),
    })
}

/// Writes the content and makes the temporary file look like the one it replaces.
fn fill(temp: &Path, mut file: File, bytes: &[u8], existing: Option<&fs::Metadata>) -> Result<()> {
    let io = |e| Error::io(temp, e);
    file.write_all(bytes).map_err(io)?;
    if let Some(meta) = existing {
        file.set_permissions(meta.permissions()).map_err(io)?;
    }
    // On disk before the rename, so a crash cannot leave an empty file behind.
    file.sync_all().map_err(io)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;
    use crate::testing::{Sandbox, commit_all, git, write};

    const MIB: u64 = 1024 * 1024;
    /// SHA-256 of `hello\n`.
    const HELLO_HASH: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";

    fn names(entries: &[TreeEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    fn entry<'a>(entries: &'a [TreeEntry], name: &str) -> &'a TreeEntry {
        entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no entry named {name:?}"))
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    /// Everything in `dir`, to prove that nothing was created or left behind.
    fn dir_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn lists_directories_first_then_files_ignoring_case() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, ".gitignore", "*.log\nbuild/\n");
        write(&repo, "Zeta/z.txt", "");
        write(&repo, "alpha/a.txt", "");
        write(&repo, "build/out.o", "");
        write(&repo, "Beta.txt", "12345");
        write(&repo, "apple.txt", "");
        write(&repo, "debug.log", "log\n");
        write(&repo, "naïve file.txt", "");
        symlink("Beta.txt", repo.join("cherry-link")).unwrap();
        symlink("alpha", repo.join("alpha-link")).unwrap();
        commit_all(&repo, "files");

        let root = list_dir(&repo, "").unwrap();
        assert_eq!(
            names(&root),
            [
                "alpha",
                "build",
                "Zeta",
                ".gitignore",
                "alpha-link",
                "apple.txt",
                "Beta.txt",
                "cherry-link",
                "debug.log",
                "naïve file.txt",
                "README.md",
            ]
        );
        assert_eq!(
            *entry(&root, "Beta.txt"),
            TreeEntry {
                name: "Beta.txt".into(),
                path: "Beta.txt".into(),
                kind: EntryKind::File,
                size: Some(5),
                ignored: false,
            }
        );
        assert_eq!(
            *entry(&root, "alpha"),
            TreeEntry {
                name: "alpha".into(),
                path: "alpha".into(),
                kind: EntryKind::Dir,
                size: None,
                ignored: false,
            }
        );
        // Links are not followed, whether they point to a file or a directory.
        for link in ["cherry-link", "alpha-link"] {
            let link = entry(&root, link);
            assert_eq!((link.kind, link.size), (EntryKind::Symlink, None));
        }
        let ignored: Vec<&str> = root
            .iter()
            .filter(|e| e.ignored)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(ignored, ["build", "debug.log"]);
        // The same listing whichever way the root is spelled.
        assert_eq!(list_dir(&repo, ".").unwrap(), root);
    }

    #[test]
    fn lists_a_nested_directory_with_paths_from_the_root() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, "src/deep/mod.rs", "mod x;\n");
        write(&repo, "src/deep/x.rs", "");
        write(&repo, "src/deep/.gitignore", "*.tmp\n");
        write(&repo, "src/deep/scratch.tmp", "");
        write(&repo, "src/deep/inner/leaf.txt", "");
        // A repository an agent cloned inside: its `.git` is hidden as well.
        write(&repo, "src/deep/.git/config", "");

        let listing = list_dir(&repo, "src/deep").unwrap();
        assert_eq!(
            names(&listing),
            ["inner", ".gitignore", "mod.rs", "scratch.tmp", "x.rs"]
        );
        assert_eq!(entry(&listing, "mod.rs").path, "src/deep/mod.rs");
        assert_eq!(entry(&listing, "inner").path, "src/deep/inner");
        assert_eq!(entry(&listing, "mod.rs").size, Some(7));
        assert!(entry(&listing, "scratch.tmp").ignored);
        assert!(!entry(&listing, "x.rs").ignored);
        assert_eq!(list_dir(&repo, "src//deep/./").unwrap(), listing);

        let root = list_dir(&repo, "").unwrap();
        assert_eq!(names(&root), ["src", "README.md"]);
        assert_eq!(list_dir(&repo, "src/deep/inner").unwrap().len(), 1);
    }

    #[test]
    fn everything_below_an_ignored_directory_is_ignored() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, ".gitignore", "node_modules/\n");
        write(&repo, "node_modules/pkg/index.js", "");
        write(&repo, "node_modules/.bin/tool", "");
        let listing = list_dir(&repo, "node_modules").unwrap();
        assert_eq!(names(&listing), [".bin", "pkg"]);
        assert!(listing.iter().all(|e| e.ignored));
        assert_eq!(list_dir(&repo, "node_modules/pkg").unwrap().len(), 1);
    }

    #[test]
    fn lists_through_a_symlink_that_stays_inside() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, ".gitignore", "*.log\n");
        write(&repo, "real/a.txt", "");
        write(&repo, "real/b.log", "");
        symlink("real", repo.join("alias")).unwrap();

        let listing = list_dir(&repo, "alias").unwrap();
        assert_eq!(names(&listing), ["a.txt", "b.log"]);
        // Paths keep the name that was asked for.
        assert_eq!(entry(&listing, "a.txt").path, "alias/a.txt");
        assert!(entry(&listing, "b.log").ignored);
        assert!(!entry(&listing, "a.txt").ignored);
    }

    #[test]
    fn odd_names_are_listed_and_checked_like_any_other() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, ".gitignore", "*.log\n");
        let odd = [
            ":magic.log",
            ":!excluded.log",
            ":(top)README.md",
            "-option.log",
            "*.txt",
            "new\nline.log",
            "with space.txt",
            "tab\there.txt",
            "日本語.log",
        ];
        // At the root and one level down: git reads a leading `:` as pathspec
        // magic, which only the first component of a path can trigger.
        for dir in ["", "odd/"] {
            for name in odd {
                write(&repo, &format!("{dir}{name}"), "x");
            }
            let listing = list_dir(&repo, dir).unwrap();
            for name in odd {
                let e = entry(&listing, name);
                assert_eq!(e.ignored, name.ends_with(".log"), "{dir}{name}");
                assert_eq!(e.path, format!("{dir}{name}"));
                assert_eq!(
                    read_file(&repo, &e.path, MIB).unwrap().text.as_deref(),
                    Some("x")
                );
            }
        }
    }

    #[test]
    fn a_submodule_is_listed_without_ignore_information() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let other = sandbox.repo("other");
        git(
            &repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "--quiet",
                "add",
                other.to_str().unwrap(),
                "sub",
            ],
        );
        write(&repo, ".gitignore", "*.log\n");
        write(&repo, "sub/debug.log", "");

        let root = list_dir(&repo, "").unwrap();
        assert_eq!(
            names(&root),
            ["sub", ".gitignore", ".gitmodules", "README.md"]
        );
        // Git refuses to answer for paths inside a submodule; the listing still works.
        let inside = list_dir(&repo, "sub").unwrap();
        assert_eq!(names(&inside), ["debug.log", "README.md"]);
        assert!(inside.iter().all(|e| !e.ignored));
    }

    #[test]
    fn listing_errors() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let outside = sandbox.dir("outside");
        symlink(&outside, repo.join("out")).unwrap();
        fs::create_dir(repo.join("empty")).unwrap();

        assert!(matches!(
            list_dir(&repo, "missing"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            list_dir(&repo, "missing/deeper"),
            Err(Error::NotFound(_))
        ));
        for path in [
            "README.md",
            "..",
            "../outside",
            ".git",
            ".git/refs",
            "out",
            "/",
        ] {
            assert!(
                matches!(list_dir(&repo, path), Err(Error::InvalidPath { .. })),
                "{path:?}"
            );
        }
        assert_eq!(list_dir(&repo, "empty").unwrap(), []);
        // Not a repository: there is no ignore information to give.
        assert!(matches!(
            list_dir(&outside, ""),
            Ok(entries) if entries.is_empty()
        ));
        write(&outside, "file.txt", "");
        assert!(matches!(
            list_dir(&outside, ""),
            Err(Error::NotARepository { .. })
        ));
    }

    #[test]
    fn reads_text_with_size_and_hash() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, "src/hello.txt", "hello\n");
        assert_eq!(
            read_file(&repo, "src/hello.txt", MIB).unwrap(),
            FileContent {
                path: "src/hello.txt".into(),
                text: Some("hello\n".into()),
                binary: false,
                too_large: false,
                size: 6,
                hash: HELLO_HASH.into(),
            }
        );
        assert_eq!(
            read_file(&repo, "./src//hello.txt", MIB).unwrap().path,
            "src/hello.txt"
        );

        write(&repo, "empty.txt", "");
        let empty = read_file(&repo, "empty.txt", MIB).unwrap();
        assert_eq!((empty.text.as_deref(), empty.size), (Some(""), 0));
        assert_eq!(
            empty.hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        write(&repo, "unicode.txt", "héllo wörld ✓\n");
        let unicode = read_file(&repo, "unicode.txt", MIB).unwrap();
        assert_eq!(unicode.text.as_deref(), Some("héllo wörld ✓\n"));
    }

    #[test]
    fn binary_and_large_files_have_no_text_but_a_hash() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, "nul.bin", [b'a', 0, b'b']);
        write(&repo, "latin1.txt", [b'c', b'a', b'f', 0xe9]);
        for path in ["nul.bin", "latin1.txt"] {
            let content = read_file(&repo, path, MIB).unwrap();
            assert!(content.binary && !content.too_large, "{path}");
            assert_eq!(content.text, None, "{path}");
            assert_eq!(content.hash.len(), 64);
        }
        assert_eq!(read_file(&repo, "nul.bin", MIB).unwrap().size, 3);

        // Bigger than one read buffer, to cover hashing across chunks.
        let big = "0123456789abcdef".repeat(10_000);
        write(&repo, "big.txt", &big);
        let whole = read_file(&repo, "big.txt", MIB).unwrap();
        assert_eq!(whole.text.as_deref(), Some(big.as_str()));
        let capped = read_file(&repo, "big.txt", 1000).unwrap();
        assert!(capped.too_large && !capped.binary);
        assert_eq!(capped.text, None);
        assert_eq!(capped.size, 160_000);
        assert_eq!(capped.hash, whole.hash);
        // Exactly at the limit still fits.
        assert!(read_file(&repo, "big.txt", 160_000).unwrap().text.is_some());
        assert!(read_file(&repo, "big.txt", 159_999).unwrap().too_large);
    }

    #[test]
    fn read_errors() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let outside = sandbox.dir("outside");
        write(&outside, "secret.txt", "secret\n");
        symlink(outside.join("secret.txt"), repo.join("leak")).unwrap();
        symlink(&outside, repo.join("out")).unwrap();
        symlink("README.md", repo.join("inside-link")).unwrap();
        fs::create_dir(repo.join("dir")).unwrap();

        for path in ["missing.txt", "missing/deeper.txt", "README.md/below"] {
            assert!(
                matches!(read_file(&repo, path, MIB), Err(Error::NotFound(_))),
                "{path:?}"
            );
        }
        for path in [
            "dir",
            "",
            "../outside/secret.txt",
            "/etc/hostname",
            ".git/config",
            ".git/HEAD",
            "dir/.git/x",
            "leak",
            "out/secret.txt",
        ] {
            assert!(
                matches!(read_file(&repo, path, MIB), Err(Error::InvalidPath { .. })),
                "{path:?}"
            );
        }
        // A link that stays inside reads as the file it points to.
        let linked = read_file(&repo, "inside-link", MIB).unwrap();
        assert_eq!(linked.text.as_deref(), Some("# readme\n"));
        assert_eq!(linked.path, "inside-link");
    }

    #[test]
    fn write_then_read_round_trips() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let created = write_file(&repo, "notes.txt", "hello\n", None).unwrap();
        assert_eq!(created.hash, HELLO_HASH);
        assert_eq!(created, read_file(&repo, "notes.txt", MIB).unwrap());

        let text = "second version ✓\nwith two lines\n";
        let saved = write_file(&repo, "notes.txt", text, Some(&created.hash)).unwrap();
        assert_eq!(saved.text.as_deref(), Some(text));
        assert_eq!(saved, read_file(&repo, "notes.txt", MIB).unwrap());
        assert_eq!(fs::read_to_string(repo.join("notes.txt")).unwrap(), text);

        // An uppercase hash is the same hash.
        let upper = saved.hash.to_uppercase();
        let emptied = write_file(&repo, "notes.txt", "", Some(&upper)).unwrap();
        assert_eq!((emptied.size, emptied.text.as_deref()), (0, Some("")));
        assert_eq!(emptied, read_file(&repo, "notes.txt", MIB).unwrap());

        // No temporary file is left next to it.
        assert_eq!(dir_names(&repo), [".git", "README.md", "notes.txt"]);
    }

    #[test]
    fn a_stale_hash_is_a_conflict_and_changes_nothing() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let read = read_file(&repo, "README.md", MIB).unwrap();
        // The agent edits the file after the editor loaded it.
        write(&repo, "README.md", "# changed by the agent\n");

        assert!(matches!(
            write_file(&repo, "README.md", "mine\n", Some(&read.hash)),
            Err(Error::Conflict(_))
        ));
        assert_eq!(
            fs::read_to_string(repo.join("README.md")).unwrap(),
            "# changed by the agent\n"
        );
        assert_eq!(dir_names(&repo), [".git", "README.md"]);

        // The agent deletes it: still a conflict, and it is not recreated.
        fs::remove_file(repo.join("README.md")).unwrap();
        assert!(matches!(
            write_file(&repo, "README.md", "mine\n", Some(&read.hash)),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            write_file(&repo, "gone/dir/file.txt", "mine\n", Some(&read.hash)),
            Err(Error::Conflict(_))
        ));
        assert_eq!(dir_names(&repo), [".git"]);
    }

    #[test]
    fn creating_over_an_existing_file_is_a_conflict() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        assert!(matches!(
            write_file(&repo, "README.md", "replaced\n", None),
            Err(Error::Conflict(_))
        ));
        assert_eq!(
            fs::read_to_string(repo.join("README.md")).unwrap(),
            "# readme\n"
        );
        assert_eq!(dir_names(&repo), [".git", "README.md"]);
    }

    #[test]
    fn creates_parent_directories() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let created = write_file(&repo, "a/b c/d/new file.txt", "deep\n", None).unwrap();
        assert_eq!(created.path, "a/b c/d/new file.txt");
        assert_eq!(
            fs::read_to_string(repo.join("a/b c/d/new file.txt")).unwrap(),
            "deep\n"
        );
        assert_eq!(dir_names(&repo.join("a/b c/d")), ["new file.txt"]);
    }

    #[test]
    fn keeps_the_permission_bits_of_the_file_it_replaces() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, "run.sh", "#!/bin/sh\n");
        write(&repo, "private.txt", "p\n");
        fs::set_permissions(repo.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(repo.join("private.txt"), fs::Permissions::from_mode(0o600)).unwrap();

        for (path, expected) in [("run.sh", 0o755), ("private.txt", 0o600)] {
            let hash = read_file(&repo, path, MIB).unwrap().hash;
            write_file(&repo, path, "#!/bin/sh\necho hi\n", Some(&hash)).unwrap();
            assert_eq!(mode(&repo.join(path)), expected, "{path}");
        }

        // A new file gets what the umask gives, like any file an editor creates.
        write(&repo, "reference.txt", "");
        write_file(&repo, "created.txt", "", None).unwrap();
        assert_eq!(
            mode(&repo.join("created.txt")),
            mode(&repo.join("reference.txt"))
        );
    }

    #[test]
    fn refuses_to_write_through_a_symlink() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let outside = sandbox.dir("outside");
        write(&outside, "target.txt", "outside\n");
        symlink(outside.join("target.txt"), repo.join("out-link")).unwrap();
        symlink("README.md", repo.join("in-link")).unwrap();
        symlink(outside.join("not-there.txt"), repo.join("dangling")).unwrap();
        symlink(&outside, repo.join("out-dir")).unwrap();

        let outside_hash = hash(b"outside\n");
        let readme_hash = hash(b"# readme\n");
        for (path, expected_hash) in [
            ("out-link", Some(outside_hash.as_str())),
            ("out-link", None),
            // Even a link that stays inside: saving must not replace the link
            // with a file, nor edit a file under another name.
            ("in-link", Some(readme_hash.as_str())),
            ("in-link", None),
            ("dangling", None),
            ("out-dir/target.txt", Some(outside_hash.as_str())),
            ("out-dir/new.txt", None),
            ("out-dir/new/deeper.txt", None),
        ] {
            assert!(
                matches!(
                    write_file(&repo, path, "overwritten\n", expected_hash),
                    Err(Error::InvalidPath { .. })
                ),
                "{path:?} with {expected_hash:?}"
            );
        }
        assert_eq!(dir_names(&outside), ["target.txt"]);
        assert_eq!(
            fs::read_to_string(outside.join("target.txt")).unwrap(),
            "outside\n"
        );
        assert_eq!(
            fs::read_to_string(repo.join("README.md")).unwrap(),
            "# readme\n"
        );
        for link in ["out-link", "in-link", "dangling", "out-dir"] {
            assert!(repo.join(link).is_symlink(), "{link}");
        }
    }

    #[test]
    fn write_rejects_unsafe_paths_and_non_files() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        fs::create_dir(repo.join("dir")).unwrap();
        for path in [
            "",
            ".",
            "dir",
            "../escape.txt",
            "/tmp/calin-git-test-escape.txt",
            ".git/hooks/pre-commit",
            ".git/config",
            "dir/.git/config",
            "dir/../../escape.txt",
        ] {
            assert!(
                matches!(
                    write_file(&repo, path, "x\n", None),
                    Err(Error::InvalidPath { .. })
                ),
                "{path:?}"
            );
        }
        assert!(!sandbox.path().join("escape.txt").exists());
        assert!(!repo.join(".git/hooks/pre-commit").exists());
        assert_eq!(dir_names(&repo.join("dir")), Vec::<String>::new());
    }

    #[test]
    fn a_legitimate_new_nested_path_passes_every_check() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, "real/keep.txt", "");
        symlink("real", repo.join("alias")).unwrap();
        // Through a link that stays inside, into directories that do not exist yet.
        write_file(&repo, "alias/new/deep/file.txt", "ok\n", None).unwrap();
        assert_eq!(
            fs::read_to_string(repo.join("real/new/deep/file.txt")).unwrap(),
            "ok\n"
        );
        assert_eq!(
            read_file(&repo, "alias/new/deep/file.txt", MIB)
                .unwrap()
                .text
                .as_deref(),
            Some("ok\n")
        );
    }
}
