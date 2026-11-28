//! Who answers a permission request: the policy at once, or the user.
//!
//! An agent only works without being asked in the directories it was started
//! with. A repo attached later is outside them, so the agent asks about every
//! file in it; the policy answers those requests from the session's repo set
//! and permission mode (design spec, section 4, "Permissions").

use std::path::{Component, Path, PathBuf};

use calin_agents::ToolAccess;
use calin_proto::{PermissionMode, RepoMode, SessionRepo};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allow,
    /// With what to tell the agent.
    Deny(String),
    /// The user decides.
    Ask,
}

/// What a request reaches for, placed in the session's repo set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// One of Calin's own tools.
    Orchestrator,
    /// A file tool: for each path it names, the index of the repo that holds
    /// it, or `None` when no repo of the session does.
    Files {
        write: bool,
        repos: Vec<Option<usize>>,
        guarded: bool,
    },
    Other,
}

/// Decides one request.
///
/// - Calin's own tools are always allowed: they do nothing the session could
///   not already do.
/// - A file tool whose every path is inside the session's repos reads freely
///   and writes when the mode accepts edits. Writing to a repo attached
///   `read` is refused in every mode.
/// - Everything else is as before repos could be attached mid-session:
///   `autonomous` allows, the other modes ask.
pub(crate) fn decide(mode: PermissionMode, target: &Target, repos: &[SessionRepo]) -> Verdict {
    if let Target::Files {
        write,
        repos: placed,
        guarded,
    } = target
        && let Some(verdict) = files_verdict(mode, *write, placed, *guarded, repos)
    {
        return verdict;
    }
    match (target, mode) {
        (Target::Orchestrator, _) | (_, PermissionMode::Autonomous) => Verdict::Allow,
        _ => Verdict::Ask,
    }
}

/// The verdict on a file tool from where its paths are, when that settles
/// it: a write to a `read` repo, or paths all inside the session's repos.
fn files_verdict(
    mode: PermissionMode,
    write: bool,
    placed: &[Option<usize>],
    guarded: bool,
    repos: &[SessionRepo],
) -> Option<Verdict> {
    let held: Vec<&SessionRepo> = placed
        .iter()
        .flatten()
        .filter_map(|i| repos.get(*i))
        .collect();
    if let Some(read_only) = held
        .iter()
        .find(|repo| write && repo.mode == RepoMode::Read)
    {
        return Some(Verdict::Deny(format!(
            "{} is attached to this session read-only: its files cannot be changed. If the \
             change is needed, tell the user so they can attach it in write mode.",
            read_only.name
        )));
    }
    let all_inside = !placed.is_empty() && held.len() == placed.len();
    // What is inside `.git` is the repository itself, not one of its
    // files: Calin's own file routes never write there either.
    let may_write = !guarded && mode != PermissionMode::Ask;
    (all_inside && (!write || may_write)).then_some(Verdict::Allow)
}

/// Blocking: resolves links on disk. Places what a request reaches for.
pub(crate) fn place(access: &ToolAccess, repos: &[SessionRepo]) -> Target {
    let (paths, write) = match access {
        ToolAccess::Orchestrator => return Target::Orchestrator,
        ToolAccess::Files { paths, write } if !paths.is_empty() => (paths, *write),
        ToolAccess::Files { .. } | ToolAccess::Other => return Target::Other,
    };
    let roots: Vec<PathBuf> = repos
        .iter()
        .map(|repo| real(Path::new(&repo.path)))
        .collect();
    let real_paths: Vec<PathBuf> = paths.iter().map(|path| real(path)).collect();
    Target::Files {
        write,
        repos: real_paths.iter().map(|path| holder(path, &roots)).collect(),
        guarded: real_paths.iter().chain(paths).any(|path| in_git_dir(path)),
    }
}

/// The index of the root that holds `path`: the longest one that is a prefix
/// of it, so a repo nested in another one wins. The root itself is inside.
fn holder(path: &Path, roots: &[PathBuf]) -> Option<usize> {
    roots
        .iter()
        .enumerate()
        .filter(|(_, root)| path.starts_with(root))
        .max_by_key(|(_, root)| root.components().count())
        .map(|(index, _)| index)
}

/// The path with every link in it resolved, which is where a write through it
/// really lands. A file that does not exist yet is resolved up to its
/// deepest existing folder; `..` is taken out by name, as the runner does.
fn real(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other),
        }
    }
    for ancestor in clean.ancestors() {
        if let Ok(resolved) = std::fs::canonicalize(ancestor) {
            // `ancestors` only yields prefixes of `clean`, so this cannot fail.
            let rest = clean.strip_prefix(ancestor).unwrap_or(Path::new(""));
            return resolved.join(rest);
        }
    }
    clean
}

fn in_git_dir(path: &Path) -> bool {
    path.components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case(".git"))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const MODES: [PermissionMode; 3] = [
        PermissionMode::Ask,
        PermissionMode::AcceptEdits,
        PermissionMode::Autonomous,
    ];

    fn repo(name: &str, mode: RepoMode, path: &Path) -> SessionRepo {
        SessionRepo {
            id: name.to_owned(),
            session_id: "s".to_owned(),
            repo_id: format!("repo-{name}"),
            name: name.to_owned(),
            mode,
            path: path.to_str().unwrap().to_owned(),
            branch: None,
            base_ref: None,
            primary: false,
        }
    }

    /// A session with a `write`, a `live` and a `read` repo, and a folder
    /// that belongs to none of them.
    struct World {
        _dir: TempDir,
        root: PathBuf,
        repos: Vec<SessionRepo>,
    }

    fn world() -> World {
        let dir = TempDir::new().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for folder in ["api/src", "web", "docs/guide", "outside", "api/.git"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        std::fs::write(root.join("api/src/lib.rs"), "").unwrap();
        std::fs::write(root.join("docs/guide/intro.md"), "").unwrap();
        let repos = vec![
            repo("api", RepoMode::Write, &root.join("api")),
            repo("web", RepoMode::Live, &root.join("web")),
            repo("docs", RepoMode::Read, &root.join("docs")),
        ];
        World {
            _dir: dir,
            root,
            repos,
        }
    }

    impl World {
        fn verdict(&self, mode: PermissionMode, write: bool, paths: &[&str]) -> Verdict {
            let access = ToolAccess::Files {
                paths: paths.iter().map(|path| self.root.join(path)).collect(),
                write,
            };
            decide(mode, &place(&access, &self.repos), &self.repos)
        }
    }

    #[test]
    fn the_matrix_of_mode_access_and_place() {
        use PermissionMode::{AcceptEdits, Ask, Autonomous};
        use Verdict::{Allow, Ask as AskUser};
        let world = world();
        let denied = |verdict: Verdict| matches!(verdict, Verdict::Deny(_));

        // Inside a `write` or a `live` repo, existing file or not.
        for path in [
            "api/src/lib.rs",
            "api/src/new/made.rs",
            "web/index.html",
            "api",
        ] {
            for mode in MODES {
                assert_eq!(
                    world.verdict(mode, false, &[path]),
                    Allow,
                    "read {path} {mode:?}"
                );
            }
            assert_eq!(world.verdict(Ask, true, &[path]), AskUser, "{path}");
            assert_eq!(world.verdict(AcceptEdits, true, &[path]), Allow, "{path}");
            assert_eq!(world.verdict(Autonomous, true, &[path]), Allow, "{path}");
        }

        // Inside a `read` repo: read in every mode, written in none.
        for path in ["docs/guide/intro.md", "docs/new.md", "docs"] {
            for mode in MODES {
                assert_eq!(
                    world.verdict(mode, false, &[path]),
                    Allow,
                    "read {path} {mode:?}"
                );
                assert!(
                    denied(world.verdict(mode, true, &[path])),
                    "write {path} {mode:?}"
                );
            }
        }

        // Outside every repo: as before, the mode alone decides.
        for path in ["outside/notes.txt", "apiary/x", ""] {
            for write in [false, true] {
                assert_eq!(world.verdict(Ask, write, &[path]), AskUser, "{path}");
                assert_eq!(
                    world.verdict(AcceptEdits, write, &[path]),
                    AskUser,
                    "{path}"
                );
                assert_eq!(world.verdict(Autonomous, write, &[path]), Allow, "{path}");
            }
        }
    }

    #[test]
    fn every_path_of_a_call_must_be_inside_and_one_read_only_path_refuses_the_write() {
        use PermissionMode::{AcceptEdits, Autonomous};
        let world = world();
        let mixed = ["api/src/lib.rs", "outside/notes.txt"];
        assert_eq!(world.verdict(AcceptEdits, false, &mixed), Verdict::Ask);
        assert_eq!(world.verdict(AcceptEdits, true, &mixed), Verdict::Ask);
        assert_eq!(world.verdict(Autonomous, true, &mixed), Verdict::Allow);
        assert_eq!(
            world.verdict(AcceptEdits, true, &["api/a", "web/b"]),
            Verdict::Allow
        );
        for mode in MODES {
            let Verdict::Deny(message) =
                world.verdict(mode, true, &["api/a", "docs/b", "outside/c"])
            else {
                panic!("a write that touches a read-only repo must be refused in {mode:?}");
            };
            assert!(
                message.contains("docs") && message.contains("read-only"),
                "{message}"
            );
        }
    }

    #[test]
    fn the_inside_of_a_git_directory_is_never_written_by_the_repo_rule() {
        use PermissionMode::{AcceptEdits, Ask, Autonomous};
        let world = world();
        for path in [
            "api/.git/config",
            "api/.git/hooks/pre-commit",
            "api/sub/.git/config",
        ] {
            assert_eq!(
                world.verdict(AcceptEdits, true, &[path]),
                Verdict::Ask,
                "{path}"
            );
            assert_eq!(world.verdict(Ask, true, &[path]), Verdict::Ask, "{path}");
            // `autonomous` allowed everything before, and still does.
            assert_eq!(
                world.verdict(Autonomous, true, &[path]),
                Verdict::Allow,
                "{path}"
            );
            assert_eq!(
                world.verdict(AcceptEdits, false, &[path]),
                Verdict::Allow,
                "{path}"
            );
        }
        assert_eq!(
            world.verdict(AcceptEdits, true, &["api/.github/workflows/ci.yml"]),
            Verdict::Allow
        );
    }

    #[test]
    fn other_tools_follow_the_mode_and_calins_own_are_always_allowed() {
        let world = world();
        for mode in MODES {
            let own = place(&ToolAccess::Orchestrator, &world.repos);
            assert_eq!(decide(mode, &own, &world.repos), Verdict::Allow, "{mode:?}");
            let expected = match mode {
                PermissionMode::Autonomous => Verdict::Allow,
                _ => Verdict::Ask,
            };
            let other = place(&ToolAccess::Other, &world.repos);
            assert_eq!(decide(mode, &other, &world.repos), expected, "{mode:?}");
            // A file tool that names nothing is like any other tool.
            let nothing = ToolAccess::Files {
                paths: Vec::new(),
                write: false,
            };
            assert_eq!(place(&nothing, &world.repos), Target::Other);
            // And so is every file tool in a session without repos.
            let access = ToolAccess::Files {
                paths: vec![world.root.join("api/src/lib.rs")],
                write: false,
            };
            assert_eq!(
                decide(mode, &place(&access, &[]), &[]),
                expected,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_path_is_placed_in_the_repo_with_the_longest_matching_root() {
        let world = world();
        let mut repos = world.repos.clone();
        // A read-only repo nested in the write repo, and the reverse.
        std::fs::create_dir_all(world.root.join("api/vendor/lib")).unwrap();
        std::fs::create_dir_all(world.root.join("docs/examples/app")).unwrap();
        repos.push(repo(
            "lib",
            RepoMode::Read,
            &world.root.join("api/vendor/lib"),
        ));
        repos.push(repo(
            "app",
            RepoMode::Write,
            &world.root.join("docs/examples/app"),
        ));
        let placed = |path: &str| {
            let access = ToolAccess::Files {
                paths: vec![world.root.join(path)],
                write: true,
            };
            match place(&access, &repos) {
                Target::Files { repos: placed, .. } => placed[0].map(|i| repos[i].name.clone()),
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(placed("api/vendor/lib/x.rs").as_deref(), Some("lib"));
        assert_eq!(placed("api/vendor/other.rs").as_deref(), Some("api"));
        assert_eq!(placed("docs/examples/app/main.rs").as_deref(), Some("app"));
        assert_eq!(placed("docs/examples/readme.md").as_deref(), Some("docs"));
        // A sibling whose name merely starts like a root is not inside it.
        assert_eq!(placed("api-old/x"), None);
        // `..` is taken out before the path is placed.
        assert_eq!(placed("api/src/../../outside/x"), None);
        assert_eq!(placed("outside/../web/x").as_deref(), Some("web"));
    }

    #[cfg(unix)]
    #[test]
    fn links_are_resolved_before_a_path_is_placed() {
        use PermissionMode::AcceptEdits;
        let world = world();
        let link = |target: &str, at: &str| {
            std::os::unix::fs::symlink(world.root.join(target), world.root.join(at)).unwrap();
        };
        // Out of a write repo, into the read-only one, and into a write repo
        // from outside.
        link("outside", "api/escape");
        link("docs/guide", "api/guide");
        link("api/src", "outside/src");
        assert_eq!(
            world.verdict(AcceptEdits, true, &["api/escape/x.txt"]),
            Verdict::Ask
        );
        assert_eq!(
            world.verdict(AcceptEdits, false, &["api/escape/x.txt"]),
            Verdict::Ask
        );
        assert!(matches!(
            world.verdict(AcceptEdits, true, &["api/guide/intro.md"]),
            Verdict::Deny(_)
        ));
        assert_eq!(
            world.verdict(AcceptEdits, true, &["outside/src/lib.rs"]),
            Verdict::Allow
        );
        // A repo whose own path goes through a link still holds its files.
        link("web", "site");
        let mut repos = world.repos.clone();
        repos[1].path = world.root.join("site").to_str().unwrap().to_owned();
        let access = ToolAccess::Files {
            paths: vec![world.root.join("web/index.html")],
            write: true,
        };
        assert_eq!(
            decide(AcceptEdits, &place(&access, &repos), &repos),
            Verdict::Allow
        );
    }
}
