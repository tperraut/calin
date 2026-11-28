//! `.calin/repos.yaml` for a new home repo: the clones found under the scan
//! directories, with commented guesses at their `setup` and `checks`. The
//! shape is the one `calin-core`'s `repos/declared.rs` reads (design spec,
//! section 3).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// A clone found directly under a scan directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Its name in the file: the folder name, made unique.
    pub name: String,
    /// The folder, canonical.
    pub path: PathBuf,
    pub url: Option<String>,
    /// Why the name is not the folder's, when it is not.
    pub renamed_from: Option<String>,
    pub guess: Guess,
}

/// What the files at a repo's root suggest for `setup` and `checks`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Guess {
    /// The files the guess was made from, for the comment.
    pub from: Vec<String>,
    pub setup: Option<String>,
    pub checks: Vec<String>,
}

/// Blocking. The git clones directly under each of `roots`, by folder name
/// within a root, leaving out hidden folders and `home` itself. Folders that
/// cannot be described are left out and said why in `notes`.
pub fn scan(roots: &[PathBuf], home: &Path, notes: &mut Vec<String>) -> Result<Vec<Found>> {
    let mut found: Vec<Found> = Vec::new();
    for root in roots {
        for (folder, path) in folders(root, notes)? {
            if let Some(path) = new_clone(&path, home, &found) {
                add_clone(&folder, path, &mut found, notes);
            }
        }
    }
    Ok(found)
}

/// The folders directly under `root`, by name, leaving out hidden ones.
fn folders(root: &Path, notes: &mut Vec<String>) -> Result<Vec<(String, PathBuf)>> {
    let entries = fs::read_dir(root).with_context(|| format!("cannot read {}", root.display()))?;
    let mut folders: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.filter_map(|entry| entry.ok()) {
        // The entry's own type: a link to a folder is not followed.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            notes.push(format!(
                "skipped {}: its name is not valid UTF-8",
                entry.path().display()
            ));
            continue;
        };
        if !name.starts_with('.') {
            folders.push((name, entry.path()));
        }
    }
    folders.sort();
    Ok(folders)
}

/// The folder, canonical, when it looks like a clone that is neither the
/// home repo nor one found already.
fn new_clone(path: &Path, home: &Path, found: &[Found]) -> Option<PathBuf> {
    // `.git` is a directory in a clone and a file in a linked worktree.
    fs::symlink_metadata(path.join(".git")).ok()?;
    let path = fs::canonicalize(path).ok()?;
    let known = path == home || found.iter().any(|known| known.path == path);
    (!known).then_some(path)
}

/// Describes the clone at `path` under a unique name, or says in `notes`
/// why it cannot.
fn add_clone(folder: &str, path: PathBuf, found: &mut Vec<Found>, notes: &mut Vec<String>) {
    let clone = match calin_git::inspect_clone(&path) {
        Ok(clone) => clone,
        Err(err) => {
            notes.push(format!("skipped {}: {err}", path.display()));
            return;
        }
    };
    let (name, renamed_from) = unique_name(folder, found);
    found.push(Found {
        guess: guess(&path),
        url: clone.remote_url.as_deref().map(without_secret),
        name,
        renamed_from,
        path,
    });
}

/// The folder name, or `<folder>-2`, `-3`... when an earlier clone has it.
fn unique_name(folder: &str, found: &[Found]) -> (String, Option<String>) {
    let taken = |name: &str| found.iter().any(|known| known.name == name);
    if !taken(folder) {
        return (folder.to_owned(), None);
    }
    let mut count = 2;
    while taken(&format!("{folder}-{count}")) {
        count += 1;
    }
    (format!("{folder}-{count}"), Some(folder.to_owned()))
}

/// The URL as it can be committed: a password or token in an `http(s)` URL
/// (`https://user:token@host/...`) is dropped with its user. Calin matches
/// URLs without their user, so nothing is lost.
pub fn without_secret(url: &str) -> String {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((user, host))
            if scheme.eq_ignore_ascii_case("http")
                || scheme.eq_ignore_ascii_case("https")
                || user.contains(':') =>
        {
            format!("{scheme}://{host}{path}")
        }
        _ => url.to_owned(),
    }
}

/// Blocking. Reads the files at the root of the repo at `dir`.
pub fn guess(dir: &Path) -> Guess {
    let mut guess = Guess::default();
    // A Makefile target usually wraps the language's own commands, so it
    // stands alone when there is one.
    let make = make_target(dir);
    if let Some((makefile, target)) = make {
        guess.from.push(makefile.to_owned());
        guess.checks.push(format!("make {target}"));
    }
    let mut add = |from: &str, setup: Option<&str>, check: String| {
        guess.from.push(from.to_owned());
        if guess.setup.is_none() {
            guess.setup = setup.map(str::to_owned);
        }
        if make.is_none() {
            guess.checks.push(check);
        }
    };
    if dir.join("Cargo.toml").is_file() {
        add("Cargo.toml", Some("cargo fetch"), "cargo test".to_owned());
    }
    if let Some((setup, test)) = node_commands(dir) {
        add("package.json", Some(setup), test.to_owned());
    }
    if dir.join("go.mod").is_file() {
        add(
            "go.mod",
            Some("go mod download"),
            "go test ./...".to_owned(),
        );
    }
    for python in ["pyproject.toml", "pytest.ini"] {
        if dir.join(python).is_file() {
            add(python, None, "pytest".to_owned());
            break;
        }
    }
    guess
}

/// The first Makefile at `dir` with a `check` (else `test`) target, and
/// that target.
fn make_target(dir: &Path) -> Option<(&'static str, &'static str)> {
    ["Makefile", "makefile", "GNUmakefile"]
        .into_iter()
        .find_map(|makefile| {
            let text = fs::read_to_string(dir.join(makefile)).ok()?;
            ["check", "test"]
                .into_iter()
                .find(|target| has_make_target(&text, target))
                .map(|target| (makefile, target))
        })
}

/// True when a line of the Makefile starts a rule for `target`
/// (`check:` or `lint check: deps`), not a variable (`check := 1`).
fn has_make_target(text: &str, target: &str) -> bool {
    text.lines().any(|line| {
        if line.starts_with(['\t', ' ', '#']) {
            return false;
        }
        let Some((names, rest)) = line.split_once(':') else {
            return false;
        };
        !rest.starts_with('=') && names.split_whitespace().any(|name| name == target)
    })
}

/// The install and test commands of a Node project whose `package.json` has
/// a real `test` script, for the package manager its lockfile names.
fn node_commands(dir: &Path) -> Option<(&'static str, &'static str)> {
    let text = fs::read_to_string(dir.join("package.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    let test = manifest.get("scripts")?.get("test")?.as_str()?;
    // What `npm init` writes: a script that only fails.
    if test.trim().is_empty() || test.contains("no test specified") {
        return None;
    }
    let has = |file: &&str| dir.join(file).is_file();
    let manager = NODE_MANAGERS
        .iter()
        .find(|(lockfiles, _)| lockfiles.iter().any(has));
    Some(manager.map_or(NPM_WITHOUT_LOCKFILE, |&(_, commands)| commands))
}

/// Each package manager's lockfiles and its install and test commands,
/// in the order they are looked for.
const NODE_MANAGERS: [(&[&str], (&str, &str)); 4] = [
    // `bun test` is Bun's own runner, not the script.
    (&["bun.lock", "bun.lockb"], ("bun install", "bun run test")),
    (&["pnpm-lock.yaml"], ("pnpm install", "pnpm test")),
    (&["yarn.lock"], ("yarn install", "yarn test")),
    (&["package-lock.json"], ("npm ci", "npm test")),
];
const NPM_WITHOUT_LOCKFILE: (&str, &str) = ("npm install", "npm test");

/// A scalar as YAML: plain when that cannot be misread, double-quoted
/// otherwise. A JSON string is a valid YAML double-quoted scalar.
pub fn scalar(text: &str) -> String {
    let plain = text.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !matches!(
            text.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n"
        );
    if plain { text.to_owned() } else { quoted(text) }
}

fn quoted(text: &str) -> String {
    // Serializing a string cannot fail.
    serde_json::to_string(text).unwrap_or_else(|_| format!("{text:?}"))
}

const HEADER: &str = "\
# The repos you work in, for Calin. Each is named once here: sessions, sets
# and workflows refer to it by that name, and the name is also its folder
# inside a session, so it must be unique and contain no `/`.
#
# On each machine Calin finds a repo's clone by its `url`: a clone whose
# `origin` remote is that URL, under the scan roots set in Calin's Settings.
# A repo with no `url` is found by its name instead: a folder of that name
# directly under a scan root. A repo that has a `url` but no clone yet can be
# cloned from Settings.
#
# Every field is optional:
#   url:    the repo's git URL
#   setup:  a shell command run in each new worktree before the agent starts,
#           for example \"cargo fetch\" or \"bun install\"
#   checks: shell commands that validate a change, run in order by a
#           workflow's `check` step, for example [\"cargo test\", \"cargo clippy\"]
";

/// The file's text.
pub fn render(found: &[Found], roots: &[PathBuf]) -> String {
    if found.is_empty() {
        return render_example(roots);
    }
    let mut out = String::from(HEADER);
    let _ = writeln!(
        out,
        "#\n# Written by `calin home init` from the clones under {}.\n\
         # The setup and checks lines are guesses from the files at each repo's\n\
         # root, left commented out: check them, then uncomment what you want.",
        list(roots)
    );
    out.push_str("\nrepos:\n");
    for repo in found {
        render_repo(&mut out, repo);
    }
    let example: Vec<String> = found.iter().take(3).map(|r| scalar(&r.name)).collect();
    let _ = write!(
        out,
        "\n# Named sets attach several repos in one step:\n\
         # sets:\n\
         #   main: [{}]\n",
        example.join(", ")
    );
    out
}

/// The file when no clone was found: commented examples only.
fn render_example(roots: &[PathBuf]) -> String {
    let mut out = String::from(HEADER);
    if !roots.is_empty() {
        let _ = writeln!(
            out,
            "#\n# No git clone was found directly under {}.",
            list(roots)
        );
    }
    out.push_str(
        "\nrepos:\n  \
         # api:\n  \
         #   url: \"git@github.com:you/api.git\"\n  \
         #   setup: \"cargo fetch\"\n  \
         #   checks: [\"cargo test\"]\n  \
         # web: { url: \"git@github.com:you/web.git\", checks: [\"bun run check\"] }\n",
    );
    out.push_str(
        "\n# Named sets attach several repos in one step:\n\
         # sets:\n\
         #   storefront: [api, web]\n",
    );
    out
}

/// One repo's entry: its name, its URL or why it has none, and the
/// commented guess.
fn render_repo(out: &mut String, repo: &Found) {
    let _ = writeln!(out, "  {}:", scalar(&repo.name));
    if let Some(folder) = &repo.renamed_from {
        let _ = writeln!(
            out,
            "    # {}: renamed because an earlier repo is named {folder}.",
            repo.path.display()
        );
    }
    match (&repo.url, &repo.renamed_from) {
        (Some(url), _) => {
            let _ = writeln!(out, "    url: {}", quoted(url));
        }
        (None, None) => {
            out.push_str(
                "    # No origin remote: found by its folder name under a scan root.\n    \
                 # Add `url:` to find it by its remote instead.\n",
            );
        }
        (None, Some(_)) => {
            let _ = writeln!(
                out,
                "    # It has no origin remote either, and a repo without `url:` is found\n    \
                 # by a folder of its own name: add a `url:` or rename the folder."
            );
        }
    }
    render_guess(out, &repo.guess);
}

fn render_guess(out: &mut String, guess: &Guess) {
    if guess.from.is_empty() {
        return;
    }
    let _ = writeln!(out, "    # Guessed from {}:", guess.from.join(", "));
    if let Some(setup) = &guess.setup {
        let _ = writeln!(out, "    # setup: {}", quoted(setup));
    }
    if !guess.checks.is_empty() {
        let checks: Vec<String> = guess.checks.iter().map(|check| quoted(check)).collect();
        let _ = writeln!(out, "    # checks: [{}]", checks.join(", "));
    }
}

fn list(roots: &[PathBuf]) -> String {
    let roots: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
    roots.join(", ")
}
