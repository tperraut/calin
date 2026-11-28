//! `calin home init`: scaffolds a home repo (design spec, sections 3 and 13).
//!
//! It writes files and never overwrites one: what already exists is kept,
//! byte for byte, so running it twice is safe. It runs `git init` only in a
//! folder that is not a repository yet, and never commits.

mod repos_yaml;

use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, anyhow, bail};

pub use repos_yaml::Found;

use crate::absolute;

const REPOS_FILE: &str = ".calin/repos.yaml";
const LINK_SCRIPT: &str = "scripts/link-claude.sh";

/// What the command was asked to do.
pub struct Options {
    /// Where the home repo is, or will be. Relative to the current directory.
    pub dir: PathBuf,
    /// Directories whose direct subfolders are looked at for clones.
    pub scan: Vec<PathBuf>,
    /// The git program.
    pub git: OsString,
}

/// What the command did.
#[derive(Debug, Default)]
pub struct Report {
    /// The home repo, canonical.
    pub root: PathBuf,
    /// True when `git init` ran.
    pub initialized: bool,
    /// Paths relative to `root`, in the order they were written.
    pub created: Vec<String>,
    pub kept: Vec<String>,
    /// The scan directories, canonical.
    pub scan_roots: Vec<PathBuf>,
    /// The repos written to a new `repos.yaml`.
    pub repos: Vec<Found>,
    /// Things worth saying that are not errors.
    pub notes: Vec<String>,
}

/// One file of the scaffold.
struct File {
    path: String,
    content: String,
    executable: bool,
}

impl File {
    fn new(path: &str, content: &str) -> Self {
        Self {
            path: path.to_owned(),
            content: content.to_owned(),
            executable: false,
        }
    }
}

/// Blocking. Scaffolds the home repo `options.dir`.
pub fn init(options: &Options) -> Result<Report> {
    let dir = absolute(&options.dir)?;
    let before = Before::look(&dir)?;
    let scan_roots = scan_roots(&options.scan)?;
    let (root, initialized) = prepare(&dir, before, !scan_roots.is_empty(), &options.git)?;
    let mut report = Report {
        root,
        initialized,
        scan_roots,
        ..Report::default()
    };
    let repos_text = repos_text(&mut report, before.has_repos_file)?;
    write_scaffold(&repos_text, &mut report)?;
    Ok(report)
}

/// What is at the home repo's path before anything is written.
#[derive(Clone, Copy)]
struct Before {
    exists: bool,
    is_repo: bool,
    has_repos_file: bool,
}

impl Before {
    fn look(dir: &Path) -> Result<Self> {
        let exists = folder_exists(dir)?;
        let has = |path: &str| exists && fs::symlink_metadata(dir.join(path)).is_ok();
        Ok(Self {
            exists,
            is_repo: has(".git"),
            has_repos_file: has(REPOS_FILE),
        })
    }

    /// git runs to make the repo, or to read the clones a scan finds.
    fn needs_git(self, scan: bool) -> bool {
        !self.is_repo || (scan && !self.has_repos_file)
    }
}

/// False when nothing is at `dir`; an error when a file is.
fn folder_exists(dir: &Path) -> Result<bool> {
    match fs::metadata(dir) {
        Ok(meta) if !meta.is_dir() => bail!("{} is a file, not a directory.", dir.display()),
        Ok(_) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(io_sentence(&err, "read", dir)),
    }
}

/// The scan directories, canonical and each once.
fn scan_roots(scan: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut scan_roots = Vec::new();
    for path in scan {
        let path = absolute(path)?;
        let real = fs::canonicalize(&path)
            .ok()
            .filter(|real| real.is_dir())
            .ok_or_else(|| {
                anyhow!(
                    "The scan directory {} does not exist or is not a directory.",
                    path.display()
                )
            })?;
        if !scan_roots.contains(&real) {
            scan_roots.push(real);
        }
    }
    Ok(scan_roots)
}

/// Makes `dir` a git repo if it is not one. Returns it canonical, and
/// whether `git init` ran.
fn prepare(dir: &Path, before: Before, scan: bool, git: &OsString) -> Result<(PathBuf, bool)> {
    // Everything that can be refused is checked before anything is written.
    if before.needs_git(scan) {
        check_git(git)?;
    }
    if !before.exists {
        fs::create_dir_all(dir).map_err(|err| io_sentence(&err, "create", dir))?;
    }
    let root = fs::canonicalize(dir).map_err(|err| io_sentence(&err, "read", dir))?;
    if !before.is_repo {
        git_init(git, &root)?;
    }
    Ok((root, !before.is_repo))
}

/// The text of a new `repos.yaml`, from the scan. An existing one is kept,
/// so the scan is not used.
fn repos_text(report: &mut Report, has_repos_file: bool) -> Result<String> {
    if has_repos_file {
        if !report.scan_roots.is_empty() {
            report.notes.push(format!(
                "{REPOS_FILE} already exists, so the scan was not used; nothing in it changed."
            ));
        }
        return Ok(String::new());
    }
    report.repos = repos_yaml::scan(&report.scan_roots, &report.root, &mut report.notes)?;
    Ok(repos_yaml::render(&report.repos, &report.scan_roots))
}

/// Writes every file and folder of the scaffold that is not there yet.
fn write_scaffold(repos_text: &str, report: &mut Report) -> Result<()> {
    let root = report.root.clone();
    let mut files = vec![
        File::new(REPOS_FILE, repos_text),
        File::new(
            ".calin/workflows/implement-and-check.yaml",
            IMPLEMENT_AND_CHECK,
        ),
        File::new(
            ".calin/workflows/implement-review-ship.yaml",
            IMPLEMENT_REVIEW_SHIP,
        ),
        File {
            executable: true,
            ..File::new(LINK_SCRIPT, LINK_CLAUDE)
        },
    ];
    match existing_readme(&root)? {
        Some(name) => report.kept.push(name),
        None => files.push(File::new("README.md", README)),
    }
    for file in &files {
        write_new(&root, file, report)?;
    }
    for folder in ["skills", "agents", "commands"] {
        write_folder(&root, folder, report)?;
    }
    Ok(())
}

/// An empty folder, with a `.gitkeep` so git keeps it, unless something is
/// already at its path.
fn write_folder(root: &Path, folder: &str, report: &mut Report) -> Result<()> {
    let path = root.join(folder);
    if fs::symlink_metadata(&path).is_ok() {
        report.kept.push(format!("{folder}/"));
        return Ok(());
    }
    fs::create_dir(&path).map_err(|err| io_sentence(&err, "create", &path))?;
    write_new(root, &File::new(&format!("{folder}/.gitkeep"), ""), report)
}

/// Writes the file unless something is already at its path, which is then
/// reported as kept. `create_new` makes the check and the write one step.
fn write_new(root: &Path, file: &File, report: &mut Report) -> Result<()> {
    let path = root.join(&file.path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| io_sentence(&err, "create", parent))?;
    }
    let mode = if file.executable { 0o755 } else { 0o644 };
    let mut out = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&path)
    {
        Ok(out) => out,
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            report.kept.push(file.path.clone());
            return Ok(());
        }
        Err(err) => return Err(io_sentence(&err, "write", &path)),
    };
    out.write_all(file.content.as_bytes())
        .map_err(|err| io_sentence(&err, "write", &path))?;
    report.created.push(file.path.clone());
    Ok(())
}

/// The name of a README the folder already has, in any case or extension.
fn existing_readme(root: &Path) -> Result<Option<String>> {
    let entries = fs::read_dir(root).map_err(|err| io_sentence(&err, "read", root))?;
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| name.to_ascii_lowercase().starts_with("readme"))
        .collect();
    names.sort();
    Ok(names.into_iter().next())
}

fn check_git(git: &OsString) -> Result<()> {
    match Command::new(git).arg("--version").output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(_) => bail!(
            "{} --version failed; check your git installation.",
            git.to_string_lossy()
        ),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            bail!("git was not found. Install git, or put it on PATH, and run this again.")
        }
        Err(err) => bail!("git cannot be run: {err}."),
    }
}

fn git_init(git: &OsString, root: &Path) -> Result<()> {
    let output = Command::new(git)
        .args(["init", "--quiet", "-b", "main"])
        .arg(root)
        .output()
        .context("git cannot be run")?;
    if !output.status.success() {
        bail!(
            "git init failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// An I/O error as a plain sentence: "Cannot create /x: permission denied."
fn io_sentence(err: &std::io::Error, verb: &str, path: &Path) -> anyhow::Error {
    let reason = match err.kind() {
        ErrorKind::PermissionDenied => "permission denied".to_owned(),
        ErrorKind::NotADirectory => "a part of the path is a file, not a directory".to_owned(),
        ErrorKind::ReadOnlyFilesystem => "the file system is read-only".to_owned(),
        _ => err.to_string(),
    };
    anyhow!("Cannot {verb} {}: {reason}.", path.display())
}

/// The text printed after a run: what was written and what to do next.
pub fn summary(report: &Report, server: &str) -> String {
    let mut out = String::new();
    let root = report.root.display();
    if report.initialized {
        out.push_str(&format!("Initialized a git repository in {root}.\n"));
    }
    out.push_str(&format!("Home repo: {root}\n"));
    out.push_str(&changes(report));
    out.push_str(&next_steps(report, server));
    out
}

/// What was created and kept, and the notes.
fn changes(report: &Report) -> String {
    let mut out = String::new();
    if !report.created.is_empty() {
        out.push_str("\nCreated:\n");
        for path in &report.created {
            out.push_str(&format!("  {path}"));
            if path == REPOS_FILE {
                out.push_str(&repos_count(report));
            }
            out.push('\n');
        }
    }
    if !report.kept.is_empty() {
        out.push_str("\nKept as they were:\n");
        for path in &report.kept {
            out.push_str(&format!("  {path}\n"));
        }
    }
    for note in &report.notes {
        out.push_str(&format!("\nNote: {note}\n"));
    }
    out
}

/// What a new `repos.yaml` holds, said after its path.
fn repos_count(report: &Report) -> String {
    match report.repos.len() {
        0 if report.scan_roots.is_empty() => " (a commented example)".to_owned(),
        0 => " (no clone found; a commented example)".to_owned(),
        1 => " (1 repo)".to_owned(),
        n => format!(" ({n} repos)"),
    }
}

fn next_steps(report: &Report, server: &str) -> String {
    let mut out = String::new();
    let root = report.root.display();
    let quoted_root = shell_quote(&report.root.to_string_lossy());
    let mut tops: Vec<&str> = report
        .created
        .iter()
        .map(|path| path.split('/').next().unwrap_or(path))
        .collect();
    tops.dedup();
    out.push_str("\nNext steps:\n");
    out.push_str(&format!(
        "  1. Review {REPOS_FILE}: names, URLs, and the commented setup and checks lines.\n"
    ));
    if tops.is_empty() {
        out.push_str("  2. Commit any changes you made.\n");
    } else {
        out.push_str(&format!(
            "  2. Commit:\n       cd {quoted_root}\n       git add {}\n       git commit -m \"Calin home repo\"\n",
            tops.join(" ")
        ));
    }
    let scan_text: Vec<String> = report
        .scan_roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    out.push_str(&format!(
        "  3. Start Calin (`calin serve`), open Settings, set these and save:\n       \
         Home repo:  {root}\n       \
         Scan roots: {}\n",
        if scan_text.is_empty() {
            "the folders that hold your clones".to_owned()
        } else {
            scan_text.join(", ")
        }
    ));
    // A placeholder rather than an empty list: the request replaces every
    // setting, and the server refuses a folder that does not exist.
    let scan_body = if scan_text.is_empty() {
        vec!["/path/to/your/clones".to_owned()]
    } else {
        scan_text
    };
    let body = serde_json::json!({
        "home_repo": report.root.to_string_lossy(),
        "scan_roots": scan_body,
    });
    out.push_str(&format!(
        "     Or, from this machine (this replaces all of Calin's settings):\n       \
         curl -X PUT {server}/api/v1/settings -H 'Content-Type: application/json' \\\n         \
         -d {}\n",
        shell_quote(&body.to_string())
    ));
    out.push_str(&format!(
        "  4. Optionally, see how {LINK_SCRIPT} would link skills/, agents/, commands/ and\n     \
         CLAUDE.md into ~/.claude:\n       {}/{LINK_SCRIPT} --dry-run\n",
        quoted_root
    ));
    out
}

/// `text` in single quotes for a POSIX shell.
fn shell_quote(text: &str) -> String {
    let safe = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | ':'));
    if safe {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

const IMPLEMENT_AND_CHECK: &str = include_str!("files/implement-and-check.yaml");
const IMPLEMENT_REVIEW_SHIP: &str = include_str!("files/implement-review-ship.yaml");
const LINK_CLAUDE: &str = include_str!("files/link-claude.sh");
const README: &str = include_str!("files/README.md");

#[cfg(test)]
mod tests;
