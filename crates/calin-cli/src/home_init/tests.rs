use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Output;

use serde::Deserialize;
use tempfile::TempDir;

use super::repos_yaml::{Guess, guess, scalar, without_secret};
use super::*;

const WORKFLOWS: [&str; 2] = [
    ".calin/workflows/implement-and-check.yaml",
    ".calin/workflows/implement-review-ship.yaml",
];

/// Everything a fresh scaffold writes, in order.
const ALL: [&str; 8] = [
    REPOS_FILE,
    WORKFLOWS[0],
    WORKFLOWS[1],
    LINK_SCRIPT,
    "README.md",
    "skills/.gitkeep",
    "agents/.gitkeep",
    "commands/.gitkeep",
];

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            // A commit may start maintenance in the background, whose lock
            // file would show up in a snapshot taken after it.
            "-c",
            "maintenance.auto=false",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A clone at `dir` with one commit and, if given, an `origin` remote.
fn clone_at(dir: &Path, origin: Option<&str>) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "--quiet"]);
    fs::write(dir.join("README"), "x\n").unwrap();
    git(dir, &["add", "README"]);
    git(dir, &["commit", "--quiet", "-m", "first"]);
    if let Some(url) = origin {
        git(dir, &["remote", "add", "origin", url]);
    }
    dir.to_path_buf()
}

fn options(dir: &Path, scan: &[&Path]) -> Options {
    Options {
        dir: dir.to_path_buf(),
        scan: scan.iter().map(|path| path.to_path_buf()).collect(),
        git: "git".into(),
    }
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).unwrap()
}

/// Every path under `dir` with what it holds: file bytes, a link's target,
/// or nothing for a folder. Links are not followed.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = path.strip_prefix(base).unwrap().to_path_buf();
            let kind = entry.file_type().unwrap();
            if kind.is_symlink() {
                let target = fs::read_link(&path).unwrap();
                out.insert(rel, Some(target.into_os_string().into_encoded_bytes()));
            } else if kind.is_dir() {
                out.insert(rel, None);
                walk(base, &path, out);
            } else {
                out.insert(rel, Some(fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

// The shape `calin-core`'s `repos/declared.rs` reads, which is crate-private
// there: a mapping of names to optional repos, and of set names to lists.
#[derive(Deserialize, Default)]
struct RawFile {
    #[serde(default)]
    repos: Option<serde_yaml_ng::Mapping>,
    #[serde(default)]
    sets: Option<serde_yaml_ng::Mapping>,
}

#[derive(Deserialize, Default, Debug, PartialEq)]
struct RawRepo {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    setup: Option<String>,
    #[serde(default)]
    checks: Option<Vec<String>>,
}

/// The repos a `repos.yaml` declares, in its order, as Calin reads them.
fn declared(text: &str) -> Vec<(String, RawRepo)> {
    let file: Option<RawFile> = serde_yaml_ng::from_str(text).unwrap();
    let file = file.unwrap_or_default();
    if let Some(sets) = file.sets {
        for (_, members) in sets {
            let _: Option<Vec<String>> = serde_yaml_ng::from_value(members).unwrap();
        }
    }
    file.repos
        .unwrap_or_default()
        .into_iter()
        .map(|(name, repo)| {
            let name: String = serde_yaml_ng::from_value(name).unwrap();
            // The checks `declared.rs` applies to a name.
            assert!(!name.is_empty() && name != "." && name != "..");
            assert!(!name.contains(['/', '\\', '\0']), "{name}");
            let repo: Option<RawRepo> = serde_yaml_ng::from_value(repo).unwrap();
            (name, repo.unwrap_or_default())
        })
        .collect()
}

/// The file with every commented `setup:`, `checks:` and `sets:` line taken
/// out of its comment, as the owner would.
fn uncommented(text: &str) -> String {
    text.lines()
        .map(|line| {
            for key in ["# setup:", "# checks:"] {
                if let Some(at) = line.find(key) {
                    return format!("{}{}", &line[..at], &line[at + 2..]);
                }
            }
            if line == "# sets:" || line.starts_with("#   main:") {
                return line[2..].to_owned();
            }
            line.to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_workflows_parse(root: &Path) {
    for path in WORKFLOWS {
        let id = Path::new(path).file_stem().unwrap().to_str().unwrap();
        let yaml = read(root, path);
        if let Err(problems) = calin_workflow::parse(id, &yaml) {
            panic!("{path} does not validate:\n{problems}");
        }
    }
}

#[test]
fn an_empty_folder_becomes_a_git_repo_with_every_file() {
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("home");
    let report = init(&options(&dir, &[])).unwrap();

    assert!(report.initialized);
    assert_eq!(report.root, fs::canonicalize(&dir).unwrap());
    assert_eq!(report.created, ALL);
    assert!(report.kept.is_empty(), "{:?}", report.kept);
    assert_eq!(git(&dir, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    // Nothing was committed.
    let head = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .unwrap();
    assert!(!head.status.success());

    assert_workflows_parse(&dir);
    let repos = read(&dir, REPOS_FILE);
    assert!(declared(&repos).is_empty(), "{repos}");
    // The commented example is itself a valid declaration.
    let example: String = repos
        .lines()
        .map(|line| line.replacen("  # ", "  ", 1))
        .collect::<Vec<_>>()
        .join("\n");
    let example = declared(&example);
    assert_eq!(
        example
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["api", "web"]
    );
    assert_eq!(example[0].1.checks, Some(vec!["cargo test".to_owned()]));

    let script = dir.join(LINK_SCRIPT);
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o755
    );
    for folder in ["skills", "agents", "commands"] {
        assert_eq!(read(&dir, &format!("{folder}/.gitkeep")), "");
    }
    assert!(read(&dir, "README.md").contains("Settings"));
}

#[test]
fn an_existing_repo_keeps_its_history_and_its_files_byte_for_byte() {
    let parent = TempDir::new().unwrap();
    let dir = clone_at(&parent.path().join("home"), None);
    fs::create_dir_all(dir.join(".calin/workflows")).unwrap();
    fs::write(dir.join(WORKFLOWS[0]), "mine: true\n").unwrap();
    fs::create_dir(dir.join("skills")).unwrap();
    fs::write(dir.join("skills/tool.md"), "a skill\n").unwrap();
    let head = git(&dir, &["rev-parse", "HEAD"]);
    let before = snapshot(&dir);

    let report = init(&options(&dir, &[])).unwrap();

    assert!(!report.initialized);
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        report.created,
        [
            REPOS_FILE,
            WORKFLOWS[1],
            LINK_SCRIPT,
            "agents/.gitkeep",
            "commands/.gitkeep"
        ]
    );
    // `clone_at` committed a `README`: any README counts, whatever its case or extension.
    assert_eq!(report.kept, ["README", WORKFLOWS[0], "skills/"]);
    let after = snapshot(&dir);
    for (path, content) in &before {
        // `.git` holds an index git may refresh; the work tree must not move.
        if path.starts_with(".git") {
            continue;
        }
        assert_eq!(after.get(path), Some(content), "{}", path.display());
    }
    assert!(!dir.join("README.md").exists());
    assert!(!dir.join("skills/.gitkeep").exists());
}

#[test]
fn a_second_run_keeps_everything() {
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("home");
    init(&options(&dir, &[])).unwrap();
    let before = snapshot(&dir);

    let report = init(&options(&dir, &[])).unwrap();

    assert!(report.created.is_empty(), "{:?}", report.created);
    assert!(!report.initialized);
    assert_eq!(
        report.kept,
        [
            "README.md",
            REPOS_FILE,
            WORKFLOWS[0],
            WORKFLOWS[1],
            LINK_SCRIPT,
            "skills/",
            "agents/",
            "commands/"
        ]
    );
    assert_eq!(snapshot(&dir), before);
    let text = summary(&report, "http://127.0.0.1:4680");
    assert!(text.contains("Kept as they were:"), "{text}");
    assert!(!text.contains("Created:"), "{text}");
}

#[test]
fn scanning_lists_clones_with_their_origin_and_renames_a_clash() {
    let parent = TempDir::new().unwrap();
    let code = parent.path().join("code");
    let more = parent.path().join("more");
    clone_at(&code.join("api"), Some("git@github.com:you/api.git"));
    clone_at(&code.join("notes"), None);
    clone_at(
        &code.join("web"),
        Some("https://user:secret@github.com/you/web.git"),
    );
    clone_at(&code.join(".hidden"), Some("git@github.com:you/hidden.git"));
    fs::create_dir_all(code.join("plain-folder")).unwrap();
    fs::write(code.join("file.txt"), "not a folder\n").unwrap();
    // Two levels down: not looked at.
    clone_at(
        &code.join("group/deep"),
        Some("git@github.com:you/deep.git"),
    );
    clone_at(&more.join("api"), Some("git@gitlab.com:else/api.git"));
    clone_at(&more.join("lonely"), None);
    // The home repo sits under a scan root and is not listed.
    let home = code.join("home");
    fs::create_dir_all(&home).unwrap();
    let before = snapshot(parent.path());

    let report = init(&options(&home, &[&code, &more, &code])).unwrap();

    let names: Vec<&str> = report.repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["api", "notes", "web", "api-2", "lonely"]);
    assert_eq!(report.scan_roots.len(), 2, "a repeated root counts once");
    let repos = read(&home, REPOS_FILE);
    let found = declared(&repos);
    let urls: Vec<(&str, Option<&str>)> = found
        .iter()
        .map(|(name, repo)| (name.as_str(), repo.url.as_deref()))
        .collect();
    assert_eq!(
        urls,
        [
            ("api", Some("git@github.com:you/api.git")),
            ("notes", None),
            ("web", Some("https://github.com/you/web.git")),
            ("api-2", Some("git@gitlab.com:else/api.git")),
            ("lonely", None),
        ]
    );
    assert!(!repos.contains("secret"), "{repos}");
    assert!(
        repos.contains("renamed because an earlier repo is named api"),
        "{repos}"
    );
    assert!(
        repos.contains("No origin remote: found by its folder name"),
        "{repos}"
    );
    // Suggestions stay commented out until the owner takes them.
    assert!(found.iter().all(|(_, repo)| repo.checks.is_none()));
    let sets = uncommented(&repos);
    let file: RawFile = serde_yaml_ng::from_str(&sets).unwrap();
    assert_eq!(file.sets.unwrap().len(), 1);

    // Nothing outside the home repo changed.
    let after = snapshot(parent.path());
    let outside = |map: &BTreeMap<PathBuf, Option<Vec<u8>>>| -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        map.iter()
            .filter(|(path, _)| !path.starts_with("code/home"))
            .map(|(path, content)| (path.clone(), content.clone()))
            .collect()
    };
    assert_eq!(outside(&after), outside(&before));

    let text = summary(&report, "http://127.0.0.1:4680");
    assert!(text.contains(".calin/repos.yaml (5 repos)"), "{text}");
    let body = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("-d '"))
        .and_then(|line| line.strip_suffix('\''))
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    let root = |dir: &Path| fs::canonicalize(dir).unwrap().to_str().unwrap().to_owned();
    assert_eq!(
        body,
        serde_json::json!({
            "home_repo": root(&home),
            "scan_roots": [root(&code), root(&more)],
        })
    );
}

#[test]
fn guessed_setup_and_checks_are_comments_that_parse_once_uncommented() {
    let parent = TempDir::new().unwrap();
    let code = parent.path().join("code");
    let rust = clone_at(&code.join("rust"), Some("git@github.com:you/rust.git"));
    fs::write(rust.join("Cargo.toml"), "[package]\n").unwrap();
    let web = clone_at(&code.join("web"), Some("git@github.com:you/web.git"));
    fs::write(web.join("package.json"), r#"{"scripts":{"test":"vitest"}}"#).unwrap();
    fs::write(web.join("bun.lock"), "").unwrap();
    clone_at(&code.join("bare"), None);
    let home = parent.path().join("home");

    init(&options(&home, &[&code])).unwrap();

    let repos = read(&home, REPOS_FILE);
    assert!(repos.contains("    # Guessed from Cargo.toml:\n    # setup: \"cargo fetch\"\n    # checks: [\"cargo test\"]\n"), "{repos}");
    let taken = declared(&uncommented(&repos));
    let by_name: BTreeMap<&str, &RawRepo> = taken
        .iter()
        .map(|(name, repo)| (name.as_str(), repo))
        .collect();
    assert_eq!(
        *by_name["rust"],
        RawRepo {
            url: Some("git@github.com:you/rust.git".to_owned()),
            setup: Some("cargo fetch".to_owned()),
            checks: Some(vec!["cargo test".to_owned()]),
        }
    );
    assert_eq!(by_name["web"].setup.as_deref(), Some("bun install"));
    assert_eq!(by_name["web"].checks, Some(vec!["bun run test".to_owned()]));
    assert_eq!(*by_name["bare"], RawRepo::default());
}

#[test]
fn each_kind_of_project_gets_its_own_guess() {
    fn with(files: &[(&str, &str)]) -> Guess {
        let dir = TempDir::new().unwrap();
        for (name, content) in files {
            fs::write(dir.path().join(name), content).unwrap();
        }
        guess(dir.path())
    }
    let checks = |files: &[(&str, &str)]| with(files).checks;
    let test_script = r#"{"scripts":{"test":"jest"}}"#;

    assert_eq!(with(&[]), Guess::default());
    assert_eq!(
        with(&[("Cargo.toml", "")]),
        Guess {
            from: vec!["Cargo.toml".to_owned()],
            setup: Some("cargo fetch".to_owned()),
            checks: vec!["cargo test".to_owned()],
        }
    );
    for (lockfile, setup, test) in [
        ("bun.lockb", "bun install", "bun run test"),
        ("bun.lock", "bun install", "bun run test"),
        ("pnpm-lock.yaml", "pnpm install", "pnpm test"),
        ("yarn.lock", "yarn install", "yarn test"),
        ("package-lock.json", "npm ci", "npm test"),
    ] {
        let guess = with(&[("package.json", test_script), (lockfile, "")]);
        assert_eq!(guess.setup.as_deref(), Some(setup), "{lockfile}");
        assert_eq!(guess.checks, [test], "{lockfile}");
    }
    assert_eq!(checks(&[("package.json", test_script)]), ["npm test"]);
    // No test script, npm's placeholder, or a file that is not JSON: no guess.
    assert!(checks(&[("package.json", r#"{"scripts":{"build":"tsc"}}"#)]).is_empty());
    let placeholder = r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#;
    assert!(checks(&[("package.json", placeholder)]).is_empty());
    assert!(checks(&[("package.json", "{ not json")]).is_empty());

    assert_eq!(checks(&[("pyproject.toml", "")]), ["pytest"]);
    assert_eq!(checks(&[("pytest.ini", "")]), ["pytest"]);
    assert_eq!(with(&[("pytest.ini", "")]).setup, None);
    assert_eq!(
        checks(&[("pyproject.toml", ""), ("pytest.ini", "")]),
        ["pytest"]
    );
    assert_eq!(checks(&[("go.mod", "module x\n")]), ["go test ./..."]);
    assert_eq!(
        checks(&[("Cargo.toml", ""), ("go.mod", "")]),
        ["cargo test", "go test ./..."]
    );

    let makefile = "all: build\n\ncheck: lint test\n\t$(MAKE) x\ntest:\n\tcargo test\n";
    assert_eq!(checks(&[("Makefile", makefile)]), ["make check"]);
    assert_eq!(checks(&[("Makefile", "build test: x\n")]), ["make test"]);
    // A variable, a recipe line or a comment is not a target.
    let not_targets = "check := yes\n\tcheck: x\n# check: y\nall:\n";
    assert!(checks(&[("Makefile", not_targets)]).is_empty());
    // A Makefile target wins over the language's commands, whose setup stays.
    let both = with(&[("Makefile", "check:\n"), ("Cargo.toml", "")]);
    assert_eq!(both.checks, ["make check"]);
    assert_eq!(both.setup.as_deref(), Some("cargo fetch"));
    assert_eq!(both.from, ["Makefile", "Cargo.toml"]);
}

#[test]
fn urls_lose_their_secrets_and_scalars_are_quoted_when_needed() {
    for (url, written) in [
        ("git@github.com:you/api.git", "git@github.com:you/api.git"),
        ("https://github.com/you/api", "https://github.com/you/api"),
        (
            "https://user:token@github.com/you/api.git",
            "https://github.com/you/api.git",
        ),
        (
            "https://token@github.com/you/api",
            "https://github.com/you/api",
        ),
        (
            "ssh://git@host:22/you/api.git",
            "ssh://git@host:22/you/api.git",
        ),
        ("ssh://git:pw@host/you/api.git", "ssh://host/you/api.git"),
        ("/srv/git/api.git", "/srv/git/api.git"),
        (" https://h/x \n", "https://h/x"),
    ] {
        assert_eq!(without_secret(url), written, "{url}");
    }
    for plain in ["api", "web-app", "a_b.c", "x1"] {
        assert_eq!(scalar(plain), plain);
    }
    for quoted in [
        "yes", "No", "true", "null", "1api", "a b", "a:b", "#x", "", "é",
    ] {
        let text = scalar(quoted);
        assert!(text.starts_with('"'), "{quoted}: {text}");
        let back: String = serde_yaml_ng::from_str(&text).unwrap();
        assert_eq!(back, quoted);
    }
}

#[test]
fn an_unusual_folder_name_is_quoted_in_the_list() {
    let parent = TempDir::new().unwrap();
    let code = parent.path().join("code");
    clone_at(&code.join("on"), None);
    clone_at(&code.join("my repo: v2"), Some("git@h:x/y.git"));
    let home = parent.path().join("home");
    init(&options(&home, &[&code])).unwrap();
    let names: Vec<String> = declared(&read(&home, REPOS_FILE))
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["my repo: v2", "on"]);
}

#[test]
fn an_existing_repo_list_is_kept_and_the_scan_is_not_used() {
    let parent = TempDir::new().unwrap();
    let code = parent.path().join("code");
    clone_at(&code.join("api"), None);
    let home = parent.path().join("home");
    fs::create_dir_all(home.join(".calin")).unwrap();
    fs::write(home.join(REPOS_FILE), "repos: {}\n").unwrap();

    let report = init(&options(&home, &[&code])).unwrap();

    assert_eq!(read(&home, REPOS_FILE), "repos: {}\n");
    assert!(report.kept.contains(&REPOS_FILE.to_owned()));
    assert!(report.repos.is_empty());
    assert!(
        report.notes[0].contains("scan was not used"),
        "{:?}",
        report.notes
    );
}

#[test]
fn errors_are_plain_sentences() {
    let parent = TempDir::new().unwrap();
    let file = parent.path().join("file");
    fs::write(&file, "x").unwrap();
    let error = init(&options(&file, &[])).unwrap_err().to_string();
    assert!(error.ends_with("is a file, not a directory."), "{error}");

    let error = init(&options(
        &parent.path().join("h"),
        &[&parent.path().join("none")],
    ))
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("does not exist or is not a directory"),
        "{error}"
    );

    let error = init(&Options {
        git: parent.path().join("no-such-git").into_os_string(),
        ..options(&parent.path().join("h2"), &[])
    })
    .unwrap_err()
    .to_string();
    assert!(error.starts_with("git was not found."), "{error}");

    let locked = parent.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores permissions; the refusal can only be seen without it.
    if fs::write(locked.join("probe"), "").is_err() {
        let error = init(&options(&locked.join("home"), &[]))
            .unwrap_err()
            .to_string();
        assert!(error.ends_with(": permission denied."), "{error}");
        assert!(error.starts_with("Cannot create "), "{error}");
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn the_summary_lists_the_next_steps_in_order() {
    let parent = TempDir::new().unwrap();
    let dir = parent.path().join("it's home");
    let report = init(&options(&dir, &[])).unwrap();
    let text = summary(&report, "http://127.0.0.1:4680");
    let steps = [
        "Initialized a git repository",
        "Created:",
        "  .calin/repos.yaml (a commented example)",
        "Next steps:",
        "1. Review .calin/repos.yaml",
        "git add .calin scripts README.md skills agents commands",
        "3. Start Calin",
        "curl -X PUT http://127.0.0.1:4680/api/v1/settings",
        "4. Optionally",
        "scripts/link-claude.sh --dry-run",
    ];
    let mut from = 0;
    for step in steps {
        let at = text[from..]
            .find(step)
            .unwrap_or_else(|| panic!("{step:?} missing or out of order in:\n{text}"));
        from += at + step.len();
    }
    // The quote in the path survives the shell.
    assert!(text.contains(r"it'\''s home"), "{text}");
    // With no scan root given, the request names a placeholder, never an
    // empty list that would wipe the roots already set.
    assert!(
        text.contains(r#""scan_roots":["/path/to/your/clones"]"#),
        "{text}"
    );
}

// The link script, run with `sh` in a made-up HOME.

struct Linking {
    _dir: TempDir,
    home_repo: PathBuf,
    user_home: PathBuf,
}

impl Linking {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let home_repo = dir.path().join("home");
        init(&options(&home_repo, &[])).unwrap();
        let home_repo = fs::canonicalize(home_repo).unwrap();
        let user_home = dir.path().join("user");
        fs::create_dir(&user_home).unwrap();
        Self {
            _dir: dir,
            home_repo,
            user_home,
        }
    }

    fn config(&self) -> PathBuf {
        self.user_home.join(".claude")
    }

    fn run(&self, args: &[&str], config_dir: Option<&Path>) -> Output {
        let mut command = Command::new("sh");
        command
            .arg(self.home_repo.join(LINK_SCRIPT))
            .args(args)
            .env("HOME", &self.user_home)
            .env_remove("CLAUDE_CONFIG_DIR")
            .current_dir(&self.user_home);
        if let Some(dir) = config_dir {
            command.env("CLAUDE_CONFIG_DIR", dir);
        }
        command.output().unwrap()
    }

    fn lines(&self, args: &[&str]) -> Vec<String> {
        let output = self.run(args, None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn the_link_script_is_posix_sh_and_says_it_is_a_starting_point() {
    let linking = Linking::new();
    let script = linking.home_repo.join(LINK_SCRIPT);
    let check = Command::new("sh").arg("-n").arg(&script).output().unwrap();
    assert!(check.status.success(), "{check:?}");
    let text = fs::read_to_string(&script).unwrap();
    assert!(text.starts_with("#!/bin/sh\n"));
    assert!(text.contains("This is a starting point"), "{text}");
    // Some systems' sh is dash, which is stricter than bash: check with it too.
    if Command::new("dash").arg("-c").arg("true").output().is_ok() {
        let check = Command::new("dash")
            .arg("-n")
            .arg(&script)
            .output()
            .unwrap();
        assert!(check.status.success(), "{check:?}");
    }
    let output = linking.run(&["--force"], None);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn the_link_script_links_what_exists_and_skips_what_does_not() {
    let linking = Linking::new();
    let config = linking.config();

    let lines = linking.lines(&[]);

    // CLAUDE.md is not in the home repo: skipped without a word.
    assert_eq!(lines.len(), 3, "{lines:?}");
    for item in ["skills", "agents", "commands"] {
        let dest = config.join(item);
        assert_eq!(fs::read_link(&dest).unwrap(), linking.home_repo.join(item));
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("linked: ") && line.contains(&format!("/{item} -> ")))
        );
    }
    assert!(!config.join("CLAUDE.md").exists());

    // Again: nothing changes, and each item says so.
    let before = snapshot(&config);
    let lines = linking.lines(&[]);
    assert!(
        lines
            .iter()
            .all(|line| line.starts_with("linked already: ")),
        "{lines:?}"
    );
    assert_eq!(snapshot(&config), before);

    // CLAUDE.md appears in the home repo: it is linked too.
    fs::write(linking.home_repo.join("CLAUDE.md"), "rules\n").unwrap();
    linking.lines(&[]);
    assert_eq!(
        fs::read_to_string(config.join("CLAUDE.md")).unwrap(),
        "rules\n"
    );
}

#[test]
fn the_link_script_leaves_alone_what_it_did_not_make() {
    let linking = Linking::new();
    let config = linking.config();
    let elsewhere = linking.user_home.join("elsewhere");
    fs::create_dir_all(config.join("skills")).unwrap();
    fs::write(config.join("skills/mine.md"), "my skill\n").unwrap();
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, config.join("agents")).unwrap();
    let before = snapshot(&linking.user_home);

    let lines = linking.lines(&[]);

    assert_eq!(
        lines[0],
        format!(
            "left alone: {} exists and is not a link",
            config.join("skills").display()
        )
    );
    assert!(
        lines[1].starts_with("left alone: ") && lines[1].contains("is a link to"),
        "{lines:?}"
    );
    assert!(lines[2].starts_with("linked: "), "{lines:?}");
    assert_eq!(
        fs::read_to_string(config.join("skills/mine.md")).unwrap(),
        "my skill\n"
    );
    assert_eq!(fs::read_link(config.join("agents")).unwrap(), elsewhere);

    // Unlinking removes the one link into the home repo and nothing else.
    let lines = linking.lines(&["--unlink"]);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines[0].starts_with("left alone: "), "{lines:?}");
    assert!(lines[1].starts_with("left alone: "), "{lines:?}");
    assert!(lines[2].starts_with("unlinked: "), "{lines:?}");
    assert!(lines[3].starts_with("not linked: "), "{lines:?}");
    assert_eq!(snapshot(&linking.user_home), before);
    // The home repo's own folders are untouched by unlinking.
    assert!(linking.home_repo.join("commands/.gitkeep").exists());
}

#[test]
fn a_dry_run_changes_nothing() {
    let linking = Linking::new();
    let before = snapshot(&linking.user_home);
    let lines = linking.lines(&["--dry-run"]);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines.iter().all(|line| line.starts_with("would link: ")),
        "{lines:?}"
    );
    assert_eq!(snapshot(&linking.user_home), before);
    assert!(!linking.config().exists());

    linking.lines(&[]);
    let linked = snapshot(&linking.user_home);
    let lines = linking.lines(&["--unlink", "--dry-run"]);
    assert!(
        lines[..3]
            .iter()
            .all(|line| line.starts_with("would unlink: ")),
        "{lines:?}"
    );
    assert_eq!(snapshot(&linking.user_home), linked);
}

#[test]
fn the_link_script_honours_claude_config_dir() {
    let linking = Linking::new();
    let custom = linking.user_home.join("custom");
    let output = linking.run(&[], Some(&custom));
    assert!(output.status.success());
    assert!(fs::read_link(custom.join("skills")).is_ok());
    assert!(!linking.config().exists());
}

/// The first YAML block after `heading` in the user guide.
fn guide_yaml(heading: &str) -> &'static str {
    const GUIDE: &str = include_str!("../../../../docs/guide.md");
    let after = &GUIDE[GUIDE.find(heading).unwrap()..];
    let start = after.find("```yaml\n").unwrap() + "```yaml\n".len();
    &after[start..start + after[start..].find("```").unwrap()]
}

#[test]
fn the_examples_in_the_user_guide_are_valid() {
    if let Err(problems) = calin_workflow::parse("example", guide_yaml("### The format by example"))
    {
        panic!("the guide's workflow does not validate:\n{problems}");
    }

    // `checks` may be one string there, as `declared.rs` allows.
    #[derive(Deserialize)]
    #[serde(untagged)]
    #[allow(dead_code)]
    enum Checks {
        One(String),
        Many(Vec<String>),
    }
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Repo {
        url: Option<String>,
        setup: Option<String>,
        checks: Option<Checks>,
    }
    #[derive(Deserialize)]
    struct File {
        repos: serde_yaml_ng::Mapping,
        sets: BTreeMap<String, Vec<String>>,
    }
    let file: File = serde_yaml_ng::from_str(guide_yaml("### The repo list")).unwrap();
    let names: Vec<String> = file
        .repos
        .into_iter()
        .map(|(name, repo)| {
            let _: Repo = serde_yaml_ng::from_value(repo).unwrap();
            serde_yaml_ng::from_value(name).unwrap()
        })
        .collect();
    assert_eq!(names, ["api", "web", "notes"]);
    assert_eq!(file.sets["storefront"], ["api", "web"]);
}

fn found(name: &str) -> Found {
    Found {
        name: name.to_owned(),
        path: PathBuf::from("/code").join(name),
        url: None,
        renamed_from: None,
        guess: Guess::default(),
    }
}

#[test]
fn the_summary_says_how_many_repos_the_list_holds() {
    let note = |scan: &[&str], repos: &[&str]| {
        let report = Report {
            root: "/home/u/home".into(),
            created: vec![REPOS_FILE.to_owned()],
            scan_roots: scan.iter().map(PathBuf::from).collect(),
            repos: repos.iter().map(|name| found(name)).collect(),
            ..Report::default()
        };
        let text = summary(&report, "http://127.0.0.1:4680");
        let line = text.lines().find(|l| l.starts_with("  .calin/repos.yaml"));
        line.unwrap().to_owned()
    };
    assert_eq!(note(&[], &[]), "  .calin/repos.yaml (a commented example)");
    assert_eq!(
        note(&["/code"], &[]),
        "  .calin/repos.yaml (no clone found; a commented example)"
    );
    assert_eq!(note(&["/code"], &["api"]), "  .calin/repos.yaml (1 repo)");
    assert_eq!(
        note(&["/code"], &["api", "web"]),
        "  .calin/repos.yaml (2 repos)"
    );
}

#[test]
fn the_whole_summary() {
    let report = Report {
        root: "/home/u/home".into(),
        initialized: true,
        created: vec![
            REPOS_FILE.to_owned(),
            "README.md".to_owned(),
            "skills/.gitkeep".to_owned(),
        ],
        kept: vec![WORKFLOWS[0].to_owned(), "agents/".to_owned()],
        scan_roots: vec!["/code".into(), "/more code".into()],
        repos: vec![found("api")],
        notes: vec!["skipped /code/x: not a clone".to_owned()],
    };
    let text = summary(&report, "http://127.0.0.1:4680");
    assert_eq!(
        text,
        r#"Initialized a git repository in /home/u/home.
Home repo: /home/u/home

Created:
  .calin/repos.yaml (1 repo)
  README.md
  skills/.gitkeep

Kept as they were:
  .calin/workflows/implement-and-check.yaml
  agents/

Note: skipped /code/x: not a clone

Next steps:
  1. Review .calin/repos.yaml: names, URLs, and the commented setup and checks lines.
  2. Commit:
       cd /home/u/home
       git add .calin README.md skills
       git commit -m "Calin home repo"
  3. Start Calin (`calin serve`), open Settings, set these and save:
       Home repo:  /home/u/home
       Scan roots: /code, /more code
     Or, from this machine (this replaces all of Calin's settings):
       curl -X PUT http://127.0.0.1:4680/api/v1/settings -H 'Content-Type: application/json' \
         -d '{"home_repo":"/home/u/home","scan_roots":["/code","/more code"]}'
  4. Optionally, see how scripts/link-claude.sh would link skills/, agents/, commands/ and
     CLAUDE.md into ~/.claude:
       /home/u/home/scripts/link-claude.sh --dry-run
"#
    );
    let nothing_new = Report {
        root: "/home/u/home".into(),
        kept: vec![REPOS_FILE.to_owned()],
        ..Report::default()
    };
    let text = summary(&nothing_new, "http://127.0.0.1:4680");
    assert!(
        text.starts_with(
            "Home repo: /home/u/home\n\nKept as they were:\n  .calin/repos.yaml\n\nNext steps:\n"
        ),
        "{text}"
    );
    for line in [
        "  2. Commit any changes you made.",
        "       Scan roots: the folders that hold your clones",
    ] {
        assert!(text.lines().any(|l| l == line), "{line:?} not in\n{text}");
    }
}
