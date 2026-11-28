//! What a work directory changed against a base commit, and both sides of one file.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read as _};
use std::path::Path;

use calin_proto::{ChangeStatus, ChangedFile, FileDiff};

use crate::cmd::{Git, require_commit, work_dir};
use crate::content::{decode, read_capped};
use crate::error::{Error, Result};
use crate::path::{clean, metadata_no_follow, resolve_no_follow};

/// Git looks for a NUL in this many leading bytes to call a file binary.
/// Untracked files are judged the same way so both kinds agree.
const BINARY_SNIFF: usize = 8000;

/// Every path that differs between the commit `base` and the files on disk in
/// `dir`, sorted by path.
///
/// One list covers commits made on top of `base`, staged changes, unstaged
/// changes and untracked files that are not ignored. A path appears once, with
/// line counts for its whole difference against `base`. Renames of tracked
/// files are detected; a file moved without telling git shows as a deletion
/// plus an untracked file.
///
/// An untracked file has status `untracked` and `added` set to its line count.
/// Binary files have zero counts. An untracked directory that is itself a git
/// repository is left out: it is not a file.
///
/// `dir` must be the root of a work tree; `base` is any revision.
/// [`Error::NotFound`] when `base` names no commit.
pub fn changes(dir: &Path, base: &str) -> Result<Vec<ChangedFile>> {
    let dir = work_dir(dir)?;
    let base = require_commit(&dir, base)?;

    // Base against the work tree, in one pass: `--raw` gives the status of
    // each path and `--numstat` its line counts. The plumbing command, not
    // `git diff`: that one rewrites the index to refresh it, whatever
    // `GIT_OPTIONAL_LOCKS` says, and an agent's `git add` at the same moment
    // would fail on `index.lock`.
    let diff = Git::read(&dir)
        .args([
            "diff-index",
            "--raw",
            "--numstat",
            "-z",
            "--no-abbrev",
            "--find-renames",
            "--no-ext-diff",
            "--no-textconv",
            &base,
            "--",
        ])
        .run()?;
    let mut tracked = parse_diff(&diff)?;
    drop_unchanged(&dir, &mut tracked)?;

    let untracked = Git::read(&dir)
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .run()?;
    let mut files = Vec::new();
    for path in untracked.split(|b| *b == 0) {
        let path = String::from_utf8_lossy(path).into_owned();
        // A trailing slash marks a nested repository.
        if path.is_empty() || path.ends_with('/') {
            continue;
        }
        match tracked
            .get(&path)
            .map(|t| (t.file.status, t.old_blob.clone()))
        {
            // In the base and on disk, but dropped from the index (`git rm
            // --cached`). The diff calls it deleted; against the base it is
            // modified, or not changed at all.
            Some((ChangeStatus::Deleted, old_blob)) => {
                match diff_blob_with_file(&dir, &old_blob, &path)? {
                    Some(file) => tracked.insert(path, Tracked::settled(file, old_blob)),
                    None => tracked.remove(&path),
                };
            }
            // Already reported by the diff.
            Some(_) => {}
            None => {
                if let Some((added, binary)) = untracked_stats(&dir.join(&path)) {
                    files.push(ChangedFile {
                        path,
                        old_path: None,
                        status: ChangeStatus::Untracked,
                        added,
                        removed: 0,
                        binary,
                    });
                }
            }
        }
    }

    files.extend(tracked.into_values().map(|t| t.file));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Both sides of one changed file: its content at `base` and on disk.
///
/// `path` is the file's current path, and `old_path` the one it had at `base`
/// when it was renamed; pass both from the [`ChangedFile`] that [`changes`]
/// returned. Both go through the path-safety rules.
///
/// - A side that does not exist is absent: `old` for a new file, `new` for a
///   deleted one. [`Error::NotFound`] when neither exists.
/// - A side with a NUL byte or invalid UTF-8 sets `binary`.
/// - A side larger than `max_bytes` sets `too_large`.
/// - When either flag is set, both sides are absent.
///
/// A symlink is never followed: its side is the target path, as git stores
/// it. A submodule counts as binary.
pub fn file_diff(
    dir: &Path,
    base: &str,
    path: &str,
    old_path: Option<&str>,
    max_bytes: u64,
) -> Result<FileDiff> {
    let dir = work_dir(dir)?;
    let base = require_commit(&dir, base)?;
    let on_disk = resolve_no_follow(&dir, path)?;
    let old_rel = match old_path {
        Some(old) => clean(old)?.join("/"),
        None => on_disk.rel.clone(),
    };

    let old = base_side(&dir, &base, &old_rel, max_bytes)?;
    let new = disk_side(&on_disk.abs, max_bytes)?;
    if matches!((&old, &new), (Side::Absent, Side::Absent)) {
        return Err(Error::NotFound(format!(
            "{path} exists neither at the base nor on disk"
        )));
    }

    let too_large = matches!(old, Side::TooLarge) || matches!(new, Side::TooLarge);
    let binary = matches!(old, Side::Binary) || matches!(new, Side::Binary);
    let shown = !too_large && !binary;
    Ok(FileDiff {
        path: on_disk.rel,
        old_path: old_path.map(|_| old_rel),
        old: old.text().filter(|_| shown),
        new: new.text().filter(|_| shown),
        binary,
        too_large,
    })
}

/// One side of a [`FileDiff`].
enum Side {
    Absent,
    Text(String),
    Binary,
    TooLarge,
}

impl Side {
    fn of(bytes: Vec<u8>) -> Self {
        decode(bytes).map_or(Self::Binary, Self::Text)
    }

    fn text(self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// The content of `rel` in the tree of `base`.
fn base_side(dir: &Path, base: &str, rel: &str, max_bytes: u64) -> Result<Side> {
    if rel.is_empty() {
        return Ok(Side::Absent);
    }
    let entry = Git::read(dir)
        .literal_pathspecs()
        .args([
            "ls-tree",
            "-z",
            "--format=%(objecttype) %(objectname) %(objectsize)",
            base,
            "--",
            rel,
        ])
        .run()?;
    let entry = String::from_utf8_lossy(&entry);
    let mut fields = entry.trim_end_matches('\0').split(' ');
    match (fields.next(), fields.next(), fields.next()) {
        (Some("blob"), Some(id), Some(size)) => {
            // An unreadable size can only mean a blob too big to count.
            if size.trim().parse::<u64>().map_or(true, |n| n > max_bytes) {
                return Ok(Side::TooLarge);
            }
            let bytes = Git::read(dir).args(["cat-file", "blob", id]).run()?;
            Ok(Side::of(bytes))
        }
        // A submodule: a commit of another repository, not content.
        (Some("commit"), ..) => Ok(Side::Binary),
        // Not in the base, or a directory there.
        _ => Ok(Side::Absent),
    }
}

/// The content at `path`, whose last component has not been resolved.
fn disk_side(path: &Path, max_bytes: u64) -> Result<Side> {
    let Some(meta) = metadata_no_follow(path)? else {
        return Ok(Side::Absent);
    };
    if meta.is_symlink() {
        // What git stores for a link, and it never reads through it.
        let target = fs::read_link(path).map_err(|e| Error::io(path, e))?;
        return Ok(target
            .into_os_string()
            .into_string()
            .map_or(Side::Binary, Side::Text));
    }
    if meta.is_dir() {
        return Ok(Side::Absent);
    }
    if !meta.is_file() {
        // A socket or a pipe: nothing to show, and opening one could block.
        return Ok(Side::Binary);
    }
    if meta.len() > max_bytes {
        return Ok(Side::TooLarge);
    }
    file_side(path, max_bytes)
}

/// The content of the regular file at `path`.
fn file_side(path: &Path, max_bytes: u64) -> Result<Side> {
    match read_capped(path, max_bytes) {
        Ok(Some(bytes)) => Ok(Side::of(bytes)),
        // It grew past the limit between the two looks.
        Ok(None) => Ok(Side::TooLarge),
        Err(Error::NotFound(_)) => Ok(Side::Absent),
        Err(e) => Err(e),
    }
}

/// A path from the diff, with the id of its blob at the base.
struct Tracked {
    file: ChangedFile,
    old_blob: String,
    /// Git listed the path as modified without reading it: its index entry is
    /// out of date, which a mere `touch` causes. The content may be identical.
    unverified: bool,
    /// Line counts were reported.
    counted: bool,
}

impl Tracked {
    /// An entry whose content is known to differ.
    fn settled(file: ChangedFile, old_blob: String) -> Self {
        Self {
            file,
            old_blob,
            unverified: false,
            counted: true,
        }
    }
}

/// Parses `git diff-index --raw --numstat -z`: all raw records, then all numstat records.
fn parse_diff(out: &[u8]) -> Result<BTreeMap<String, Tracked>> {
    let malformed = || Error::Git {
        command: "git diff-index --raw --numstat -z".into(),
        stderr: "unexpected output".into(),
    };
    let mut files: BTreeMap<String, Tracked> = BTreeMap::new();
    let mut fields = out
        .split(|b| *b == 0)
        .map(|field| String::from_utf8_lossy(field).into_owned());
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        if let Some(raw) = field.strip_prefix(':') {
            // `<old mode> <new mode> <old id> <new id> <status><score>`, then
            // the path, or the old and the new path for a rename or a copy.
            let raw: Vec<&str> = raw.split(' ').collect();
            let &[old_mode, new_mode, old_blob, new_blob, status] = raw.as_slice() else {
                return Err(malformed());
            };
            let code = status.chars().next().ok_or_else(malformed)?;
            let first = fields.next().ok_or_else(malformed)?;
            let (status, old_path, path) = match code {
                'R' => (
                    ChangeStatus::Renamed,
                    Some(first),
                    fields.next().ok_or_else(malformed)?,
                ),
                // Copies are not asked for; should git report one, it is a new file.
                'C' => (
                    ChangeStatus::Added,
                    None,
                    fields.next().ok_or_else(malformed)?,
                ),
                'A' => (ChangeStatus::Added, None, first),
                'D' => (ChangeStatus::Deleted, None, first),
                // Content, mode or type changed, or the path is in a merge conflict.
                _ => (ChangeStatus::Modified, None, first),
            };
            let file = ChangedFile {
                path: path.clone(),
                old_path,
                status,
                added: 0,
                removed: 0,
                binary: false,
            };
            let tracked = Tracked {
                file,
                old_blob: old_blob.to_owned(),
                // An all-zero id stands for "whatever is on disk".
                unverified: code == 'M'
                    && old_mode == new_mode
                    && new_blob.bytes().all(|b| b == b'0'),
                counted: false,
            };
            files.insert(path, tracked);
        } else {
            // `<added>\t<removed>\t<path>`, or with an empty path followed by
            // the old and the new path. Binary files have `-` for both counts.
            let mut parts = field.splitn(3, '\t');
            let added = parts.next().ok_or_else(malformed)?;
            let removed = parts.next().ok_or_else(malformed)?;
            let path = match parts.next().ok_or_else(malformed)? {
                "" => {
                    fields.next().ok_or_else(malformed)?;
                    fields.next().ok_or_else(malformed)?
                }
                path => path.to_owned(),
            };
            let Some(tracked) = files.get_mut(&path) else {
                return Err(malformed());
            };
            tracked.counted = true;
            match (added.parse(), removed.parse()) {
                (Ok(added), Ok(removed)) => {
                    tracked.file.added = added;
                    tracked.file.removed = removed;
                }
                _ => tracked.file.binary = true,
            }
        }
    }
    Ok(files)
}

/// Removes the entries that only look modified. This is the work `git diff`
/// would do by refreshing the index, done without writing anything.
fn drop_unchanged(dir: &Path, tracked: &mut BTreeMap<String, Tracked>) -> Result<()> {
    // Git reports no counts for a modified text file in which no line changed.
    tracked.retain(|_, t| t.counted || !t.unverified);
    // A binary file gets `-` for counts either way; only its hash can tell.
    let suspects: Vec<(String, String)> = tracked
        .iter()
        .filter(|(_, t)| t.unverified && t.file.binary)
        .map(|(path, t)| (path.clone(), t.old_blob.clone()))
        .collect();
    for path in same_as_blob(dir, &suspects)? {
        tracked.remove(&path);
    }
    Ok(())
}

/// The paths among `(path, blob id)` whose file on disk hashes to that blob.
fn same_as_blob(dir: &Path, pairs: &[(String, String)]) -> Result<Vec<String>> {
    let mut same = Vec::new();
    // Batches keep the command line far below the system's limit.
    for batch in pairs.chunks(64) {
        let done = Git::read(dir)
            .args(["hash-object", "--"])
            .args(batch.iter().map(|(path, _)| path))
            .exec()?;
        // One vanished file fails the batch. Nothing is known then, so its
        // paths stay listed as modified.
        if !done.status.success() {
            continue;
        }
        let ids = String::from_utf8_lossy(&done.stdout);
        for ((path, blob), id) in batch.iter().zip(ids.lines()) {
            if id == blob {
                same.push(path.clone());
            }
        }
    }
    Ok(same)
}

/// Compares a blob with a file on disk that git does not track. `None` when
/// they are identical.
fn diff_blob_with_file(dir: &Path, blob: &str, path: &str) -> Result<Option<ChangedFile>> {
    let out = Git::read(dir)
        .literal_pathspecs()
        .args([
            // Keeps this porcelain command from refreshing, and so locking, the index.
            "-c",
            "diff.autoRefreshIndex=false",
            "diff",
            "--numstat",
            "-z",
            "--no-ext-diff",
            "--no-textconv",
            blob,
            "--",
            path,
        ])
        .run()?;
    let out = String::from_utf8_lossy(&out);
    let mut counts = out.splitn(3, '\t');
    // No record at all: a text file without a changed line.
    let (Some(added), Some(removed)) = (counts.next(), counts.next()) else {
        return Ok(None);
    };
    let counts = added.parse().ok().zip(removed.parse().ok());
    let pair = [(path.to_owned(), blob.to_owned())];
    if counts.is_none() && !same_as_blob(dir, &pair)?.is_empty() {
        return Ok(None);
    }
    let (added, removed) = counts.unwrap_or((0, 0));
    Ok(Some(ChangedFile {
        path: path.to_owned(),
        old_path: None,
        status: ChangeStatus::Modified,
        added,
        removed,
        binary: counts.is_none(),
    }))
}

/// Line count and binary flag of an untracked file. `None` for what is not a
/// file, or vanished since git listed it.
fn untracked_stats(path: &Path) -> Option<(u32, bool)> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.is_symlink() {
        // Git would store the target path: one line, and nothing to follow.
        return Some((1, false));
    }
    if !meta.is_file() {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; 64 * 1024];
    let (mut seen, mut lines, mut last) = (0usize, 0u32, b'\n');
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        let chunk = &buf[..n];
        if seen < BINARY_SNIFF {
            let sniffed = &chunk[..n.min(BINARY_SNIFF - seen)];
            if sniffed.contains(&0) {
                return Some((0, true));
            }
        }
        seen = seen.saturating_add(n);
        let newlines = chunk.iter().filter(|b| **b == b'\n').count();
        lines = lines.saturating_add(u32::try_from(newlines).unwrap_or(u32::MAX));
        last = chunk[n - 1];
    }
    // A last line without a newline is still a line.
    if last != b'\n' {
        lines = lines.saturating_add(1);
    }
    Some((lines, false))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::testing::{Sandbox, commit_all, git, write};
    use crate::{create_worktree, head_commit};

    const MIB: u64 = 1024 * 1024;

    /// A clone with `files` committed, and a session worktree of it.
    /// Returns the worktree and its base.
    fn session(sandbox: &Sandbox, files: &[(&str, &str)]) -> (PathBuf, String) {
        let repo = sandbox.repo("repo");
        for (path, content) in files {
            write(&repo, path, content);
        }
        if !files.is_empty() {
            commit_all(&repo, "base");
        }
        let wt = create_worktree(&repo, &sandbox.path().join("wt"), "calin/s", None).unwrap();
        (wt.path, wt.base_ref)
    }

    fn change(path: &str, status: ChangeStatus, added: u32, removed: u32) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            old_path: None,
            status,
            added,
            removed,
            binary: false,
        }
    }

    fn assert_changes(dir: &Path, base: &str, mut expected: Vec<ChangedFile>) {
        let actual = changes(dir, base).unwrap();
        assert!(
            actual.windows(2).all(|pair| pair[0].path < pair[1].path),
            "not sorted by path, or a path appears twice: {actual:#?}"
        );
        expected.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(actual, expected);
    }

    #[test]
    fn a_fresh_worktree_has_no_changes() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(&sandbox, &[("a.txt", "a\n")]);
        assert_changes(&wt, &base, vec![]);
    }

    #[test]
    fn every_kind_of_change_in_one_list() {
        use ChangeStatus::{Added, Deleted, Modified, Renamed, Untracked};

        let sandbox = Sandbox::new();
        let ten_lines = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let (wt, base) = session(
            &sandbox,
            &[
                (".gitignore", "*.log\nbuild/\n"),
                ("committed.txt", "one\ntwo\nthree\n"),
                ("unstaged.txt", "a\nb\n"),
                ("deleted.txt", "d1\nd2\n"),
                ("committed-delete.txt", "gone\n"),
                ("old name.txt", ten_lines),
                ("both.txt", "1\n2\n3\n"),
                ("same.txt", "untouched\n"),
                ("line\nbreak.txt", "x\n"),
                ("naïve café.txt", "y\n"),
            ],
        );

        // Committed on the session's branch.
        write(&wt, "committed.txt", "one\nTWO\nthree\nfour\n");
        write(&wt, "both.txt", "1\n2\n3\n4\n");
        write(&wt, "with \"quotes\".txt", "q\n");
        fs::remove_file(wt.join("committed-delete.txt")).unwrap();
        commit_all(&wt, "session commit");
        // Staged.
        write(&wt, "staged.txt", "s1\ns2\n");
        write(&wt, "image.bin", [0u8, 159, 146, 150, 0, 1]);
        git(&wt, &["add", "staged.txt", "image.bin"]);
        fs::create_dir(wt.join("dir with space")).unwrap();
        git(&wt, &["mv", "old name.txt", "dir with space/new name.txt"]);
        // Unstaged.
        write(&wt, "unstaged.txt", "a\nb\nc\n");
        write(&wt, "both.txt", "0\n1\n2\n3\n4\n5\n");
        write(&wt, "line\nbreak.txt", "x\nmore\n");
        write(&wt, "naïve café.txt", "Y\n");
        fs::remove_file(wt.join("deleted.txt")).unwrap();
        // Untracked, and ignored.
        write(&wt, "untracked.txt", "u1\nu2\nu3");
        write(&wt, "new dir/日本語.txt", "u\n");
        write(&wt, "new\nline.txt", "n\n");
        write(&wt, "empty.txt", "");
        write(&wt, "untracked.bin", [1u8, 0, 2]);
        write(&wt, "debug.log", "ignored\n");
        write(&wt, "build/out.txt", "ignored\n");

        assert_changes(
            &wt,
            &base,
            vec![
                change("committed.txt", Modified, 2, 1),
                change("committed-delete.txt", Deleted, 0, 1),
                change("with \"quotes\".txt", Added, 1, 0),
                change("staged.txt", Added, 2, 0),
                ChangedFile {
                    binary: true,
                    ..change("image.bin", Added, 0, 0)
                },
                ChangedFile {
                    old_path: Some("old name.txt".into()),
                    ..change("dir with space/new name.txt", Renamed, 0, 0)
                },
                change("unstaged.txt", Modified, 1, 0),
                // Committed, then edited again: one entry, totals against the base.
                change("both.txt", Modified, 3, 0),
                change("line\nbreak.txt", Modified, 1, 0),
                change("naïve café.txt", Modified, 1, 1),
                change("deleted.txt", Deleted, 0, 2),
                change("untracked.txt", Untracked, 3, 0),
                change("new dir/日本語.txt", Untracked, 1, 0),
                change("new\nline.txt", Untracked, 1, 0),
                change("empty.txt", Untracked, 0, 0),
                ChangedFile {
                    binary: true,
                    ..change("untracked.bin", Untracked, 0, 0)
                },
            ],
        );
    }

    #[test]
    fn a_renamed_and_edited_file_keeps_its_old_path_and_counts() {
        let sandbox = Sandbox::new();
        let body = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n";
        let (wt, base) = session(&sandbox, &[("src/before.rs", body)]);
        git(&wt, &["mv", "src/before.rs", "src/after.rs"]);
        write(&wt, "src/after.rs", format!("{body}l11\n"));
        commit_all(&wt, "rename");
        write(&wt, "src/after.rs", format!("{body}l11\nl12\n"));

        assert_changes(
            &wt,
            &base,
            vec![ChangedFile {
                old_path: Some("src/before.rs".into()),
                ..change("src/after.rs", ChangeStatus::Renamed, 2, 0)
            }],
        );
    }

    #[test]
    fn a_file_dropped_from_the_index_is_compared_with_the_base() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(&sandbox, &[("kept.txt", "k\n"), ("edited.txt", "e\n")]);
        // Both are still on disk; git lists them as deleted and as untracked.
        git(&wt, &["rm", "-q", "--cached", "kept.txt", "edited.txt"]);
        write(&wt, "edited.txt", "e\ne2\n");

        assert_changes(
            &wt,
            &base,
            vec![change("edited.txt", ChangeStatus::Modified, 1, 0)],
        );
    }

    #[test]
    fn untracked_links_and_nested_repositories() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(&sandbox, &[]);
        let outside = sandbox.dir("outside");
        write(&outside, "secret.txt", "1\n2\n3\n4\n5\n");
        symlink(outside.join("secret.txt"), wt.join("link")).unwrap();
        symlink(&outside, wt.join("dir-link")).unwrap();
        // Something an agent cloned into the worktree: not a file of this repo.
        let nested = wt.join("vendor/dep");
        fs::create_dir_all(&nested).unwrap();
        git(&nested, &["init", "-q"]);
        write(&nested, "inner.txt", "i\n");

        // A link counts as its one-line target, never as what it points to.
        assert_changes(
            &wt,
            &base,
            vec![
                change("link", ChangeStatus::Untracked, 1, 0),
                change("dir-link", ChangeStatus::Untracked, 1, 0),
            ],
        );
    }

    #[test]
    fn a_clone_edited_in_place_is_measured_from_its_head() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let base = head_commit(&repo).unwrap();
        write(&repo, "README.md", "# readme\nmore\n");
        write(&repo, "notes.txt", "n\n");
        assert_changes(
            &repo,
            &base,
            vec![
                change("README.md", ChangeStatus::Modified, 1, 0),
                change("notes.txt", ChangeStatus::Untracked, 1, 0),
            ],
        );
        // Committing moves HEAD, not the base the caller stored.
        commit_all(&repo, "live commit");
        assert_changes(
            &repo,
            &base,
            vec![
                change("README.md", ChangeStatus::Modified, 1, 0),
                change("notes.txt", ChangeStatus::Added, 1, 0),
            ],
        );
    }

    /// Gives `path` an old timestamp, so that its index entry no longer
    /// matches it and git cannot tell without reading whether it changed.
    fn age(path: &Path) {
        let then = SystemTime::now() - Duration::from_secs(3600);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(then)
            .unwrap();
    }

    #[test]
    fn touched_files_are_not_changes_and_the_index_is_never_written() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(
            &sandbox,
            &[
                ("edited.txt", "a\n"),
                ("touched.txt", "b\n"),
                ("touched.bin", "\0\x01\x02"),
                ("edited.bin", "\0\x01\x02"),
                ("script.sh", "#!/bin/sh\n"),
                ("uncached.bin", "\0\x03"),
                ("uncached-edited.bin", "\0\x04"),
            ],
        );
        write(&wt, "edited.txt", "changed\n");
        write(&wt, "edited.bin", "\0\x01\x03");
        fs::set_permissions(wt.join("script.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        git(
            &wt,
            &[
                "rm",
                "-q",
                "--cached",
                "uncached.bin",
                "uncached-edited.bin",
            ],
        );
        write(&wt, "uncached-edited.bin", "\0\x05");
        for path in ["edited.txt", "touched.txt", "touched.bin", "edited.bin"] {
            age(&wt.join(path));
        }
        let index = PathBuf::from(git(&wt, &["rev-parse", "--absolute-git-dir"])).join("index");
        let before = fs::read(&index).unwrap();
        let stamp = fs::metadata(&index).unwrap().modified().unwrap();

        let binary = |path| ChangedFile {
            binary: true,
            ..change(path, ChangeStatus::Modified, 0, 0)
        };
        assert_changes(
            &wt,
            &base,
            vec![
                change("edited.txt", ChangeStatus::Modified, 1, 1),
                binary("edited.bin"),
                // A mode change alone is a change, with no lines in it.
                change("script.sh", ChangeStatus::Modified, 0, 0),
                binary("uncached-edited.bin"),
            ],
        );
        file_diff(&wt, &base, "edited.txt", None, MIB).unwrap();
        crate::list_dir(&wt, "").unwrap();

        // Calin must never take `index.lock`: an agent's own git would fail on it.
        assert!(
            fs::read(&index).unwrap() == before,
            "the index was rewritten"
        );
        assert_eq!(fs::metadata(&index).unwrap().modified().unwrap(), stamp);
    }

    #[test]
    fn names_that_look_like_git_syntax_are_plain_names() {
        let sandbox = Sandbox::new();
        // Globs, pathspec magic and options, next to the file each could be mistaken for.
        let names = [
            "a.txt",
            "*.txt",
            ":(top)a.txt",
            ":x",
            "-n.txt",
            "--output=x",
            "[ab].txt",
            "back\\slash.txt",
        ];
        let v1: Vec<(&str, String)> = names.iter().map(|n| (*n, format!("{n} v1\n"))).collect();
        let mut files: Vec<(&str, &str)> = v1.iter().map(|(n, c)| (*n, c.as_str())).collect();
        files.extend([(":touched.bin", "\0\x01"), ("-edited.bin", "\0\x02")]);
        let (wt, base) = session(&sandbox, &files);

        for name in names {
            write(&wt, name, format!("{name} v2\n"));
        }
        age(&wt.join(":touched.bin"));
        write(&wt, "-edited.bin", "\0\x03");
        // Dropped from the index but still on disk: takes the blob-against-file path.
        git(
            &wt,
            &["update-index", "--force-remove", "--", ":x", "*.txt"],
        );

        let mut expected: Vec<ChangedFile> = names
            .iter()
            .map(|name| change(name, ChangeStatus::Modified, 1, 1))
            .collect();
        expected.push(ChangedFile {
            binary: true,
            ..change("-edited.bin", ChangeStatus::Modified, 0, 0)
        });
        assert_changes(&wt, &base, expected);

        for name in names {
            let d = diff(&wt, &base, name);
            assert_eq!(d.old, Some(format!("{name} v1\n")), "{name}");
            assert_eq!(d.new, Some(format!("{name} v2\n")), "{name}");
        }
    }

    #[test]
    fn a_submodule_is_one_entry_and_not_text() {
        let sandbox = Sandbox::new();
        let other = sandbox.repo("other");
        let (wt, _) = session(&sandbox, &[("a.txt", "a\n")]);
        git(
            &wt,
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
        let base = commit_all(&wt, "add submodule");
        assert_changes(&wt, &base, vec![]);

        // New commits inside the submodule move the one entry that stands for it.
        write(&wt.join("sub"), "inner.txt", "i\n");
        commit_all(&wt.join("sub"), "inner");
        assert_changes(
            &wt,
            &base,
            vec![change("sub", ChangeStatus::Modified, 1, 1)],
        );
        let d = diff(&wt, &base, "sub");
        assert!(d.binary);
        assert_eq!((d.old, d.new), (None, None));
    }

    #[test]
    fn bad_base_and_bad_directory() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(&sandbox, &[("src/a.txt", "a\n")]);
        assert!(matches!(changes(&wt, "nope"), Err(Error::NotFound(_))));
        assert!(matches!(
            changes(&wt, "--output=x"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            changes(&wt.join("src"), &base),
            Err(Error::NotARepository { .. })
        ));
        assert!(matches!(
            changes(&wt.join("missing"), &base),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            file_diff(&wt, "nope", "src/a.txt", None, MIB),
            Err(Error::NotFound(_))
        ));
    }

    fn diff(dir: &Path, base: &str, path: &str) -> FileDiff {
        file_diff(dir, base, path, None, MIB).unwrap()
    }

    #[test]
    fn diff_of_added_modified_and_deleted_files() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(
            &sandbox,
            &[
                ("mod.txt", "old\n"),
                ("del.txt", "bye\n"),
                ("both.txt", "1\n"),
            ],
        );
        write(&wt, "mod.txt", "new\n");
        write(&wt, "untracked/new file.txt", "fresh\n");
        write(&wt, "staged.txt", "staged\n");
        git(&wt, &["add", "staged.txt"]);
        fs::remove_file(wt.join("del.txt")).unwrap();
        write(&wt, "both.txt", "2\n");
        commit_all(&wt, "session commit");
        write(&wt, "both.txt", "3\n");

        let modified = diff(&wt, &base, "mod.txt");
        assert_eq!(
            modified,
            FileDiff {
                path: "mod.txt".into(),
                old_path: None,
                old: Some("old\n".into()),
                new: Some("new\n".into()),
                binary: false,
                too_large: false,
            }
        );

        let added = diff(&wt, &base, "untracked/new file.txt");
        assert_eq!((added.old, added.new), (None, Some("fresh\n".into())));
        let staged = diff(&wt, &base, "staged.txt");
        assert_eq!((staged.old, staged.new), (None, Some("staged\n".into())));

        let deleted = diff(&wt, &base, "del.txt");
        assert_eq!((deleted.old, deleted.new), (Some("bye\n".into()), None));
        assert!(!deleted.binary && !deleted.too_large);

        // The old side is the base, not the session's own intermediate commit.
        let both = diff(&wt, &base, "both.txt");
        assert_eq!(
            (both.old, both.new),
            (Some("1\n".into()), Some("3\n".into()))
        );

        // The path is cleaned the same way as everywhere else.
        assert_eq!(diff(&wt, &base, "./mod.txt").path, "mod.txt");
    }

    #[test]
    fn diff_of_a_renamed_file_reads_the_old_path_at_the_base() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(&sandbox, &[("old name.txt", "1\n2\n3\n4\n5\n6\n")]);
        git(&wt, &["mv", "old name.txt", "new\nname.txt"]);
        write(&wt, "new\nname.txt", "1\n2\n3\n4\n5\n6\n7\n");

        let listed = changes(&wt, &base).unwrap();
        assert_eq!(listed.len(), 1);
        let renamed = file_diff(
            &wt,
            &base,
            &listed[0].path,
            listed[0].old_path.as_deref(),
            MIB,
        )
        .unwrap();
        assert_eq!(
            renamed,
            FileDiff {
                path: "new\nname.txt".into(),
                old_path: Some("old name.txt".into()),
                old: Some("1\n2\n3\n4\n5\n6\n".into()),
                new: Some("1\n2\n3\n4\n5\n6\n7\n".into()),
                binary: false,
                too_large: false,
            }
        );
        // Without the old path it looks like a new file.
        assert_eq!(diff(&wt, &base, "new\nname.txt").old, None);
    }

    #[test]
    fn diff_of_binary_content_has_no_sides() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(&sandbox, &[("was-text.txt", "text\n")]);
        write(&wt, "was-text.txt", [b'a', 0, b'b']);
        write(&wt, "latin1.txt", [b'c', b'a', b'f', 0xe9]);
        write(&wt, "was-binary.dat", [0u8, 1, 2]);
        commit_all(&wt, "binary");
        let binary_base = head_commit(&wt).unwrap();
        write(&wt, "was-binary.dat", "now text\n");

        for (base, path) in [
            (&base, "was-text.txt"),
            // Invalid UTF-8 without a NUL byte is binary too.
            (&base, "latin1.txt"),
            (&binary_base, "was-binary.dat"),
        ] {
            let d = diff(&wt, base, path);
            assert!(d.binary && !d.too_large, "{path}");
            assert_eq!((d.old, d.new), (None, None), "{path}");
        }
    }

    #[test]
    fn diff_of_a_file_over_the_limit_has_no_sides() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(
            &sandbox,
            &[("grows.txt", "small\n"), ("shrinks.txt", "0123456789\n")],
        );
        write(&wt, "grows.txt", "0123456789\n");
        write(&wt, "shrinks.txt", "small\n");

        for path in ["grows.txt", "shrinks.txt"] {
            let d = file_diff(&wt, &base, path, None, 10).unwrap();
            assert!(d.too_large && !d.binary, "{path}");
            assert_eq!((d.old, d.new), (None, None), "{path}");
            // Exactly at the limit is fine.
            let d = file_diff(&wt, &base, path, None, 11).unwrap();
            assert!(!d.too_large && d.old.is_some() && d.new.is_some(), "{path}");
        }
    }

    #[test]
    fn diff_never_follows_a_symlink() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(&sandbox, &[("target.txt", "content\n")]);
        let outside = sandbox.dir("outside");
        write(&outside, "secret.txt", "secret\n");
        symlink("target.txt", wt.join("inside-link")).unwrap();
        symlink(outside.join("secret.txt"), wt.join("outside-link")).unwrap();
        symlink(&outside, wt.join("outside-dir")).unwrap();
        commit_all(&wt, "links");
        let with_links = head_commit(&wt).unwrap();

        // A link is its target path, on both sides, as git stores it.
        let inside = diff(&wt, &base, "inside-link");
        assert_eq!((inside.old, inside.new), (None, Some("target.txt".into())));
        let outside_link = diff(&wt, &with_links, "outside-link");
        let target = outside.join("secret.txt").to_str().unwrap().to_owned();
        assert_eq!(outside_link.old, Some(target.clone()));
        assert_eq!(outside_link.new, Some(target));

        // A parent directory that leads outside is refused outright.
        assert!(matches!(
            file_diff(&wt, &base, "outside-dir/secret.txt", None, MIB),
            Err(Error::InvalidPath { .. })
        ));
    }

    #[test]
    fn a_socket_is_not_a_change_and_its_diff_is_binary() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(&sandbox, &[("a.txt", "a\n")]);
        let _socket = UnixListener::bind(wt.join("socket")).unwrap();

        assert_changes(&wt, &base, vec![]);
        let d = diff(&wt, &base, "socket");
        assert!(d.binary && !d.too_large);
        assert_eq!((d.old, d.new), (None, None));
    }

    #[test]
    fn diff_of_a_file_in_an_unreadable_directory_is_an_io_error() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(&sandbox, &[("a.txt", "a\n")]);
        write(&wt, "locked/f.txt", "f\n");
        fs::set_permissions(wt.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let d = file_diff(&wt, &base, "locked/f.txt", None, MIB);
        fs::set_permissions(wt.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(d, Err(Error::Io { .. })), "{d:?}");
    }

    #[test]
    fn diff_rejects_unsafe_and_unknown_paths() {
        let sandbox = Sandbox::new();
        let (wt, base) = session(&sandbox, &[("src/a.txt", "a\n")]);
        for path in [
            "../repo/README.md",
            "/etc/passwd",
            ".git/config",
            "src/.git/x",
            "",
        ] {
            assert!(
                matches!(
                    file_diff(&wt, &base, path, None, MIB),
                    Err(Error::InvalidPath { .. })
                ),
                "{path:?}"
            );
        }
        assert!(matches!(
            file_diff(&wt, &base, "src/a.txt", Some("../x"), MIB),
            Err(Error::InvalidPath { .. })
        ));
        // Neither side exists; a directory is not a file either.
        for path in ["nope.txt", "src", "src/a.txt/below"] {
            assert!(
                matches!(
                    file_diff(&wt, &base, path, None, MIB),
                    Err(Error::NotFound(_))
                ),
                "{path:?}"
            );
        }
    }
}
