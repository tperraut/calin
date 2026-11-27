use std::time::Duration;

use tempfile::TempDir;

use super::*;
use crate::home::Home;
use crate::testing::{git, git_repo, write_file};

async fn registry(dir: &TempDir) -> RepoRegistry {
    RepoRegistry::load(Home::new(dir.path()).repos_path()).await
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

/// A machine with a home repo, a folder of clones to scan and a clone root,
/// all in one temporary directory next to the data directory.
struct Machine {
    data: TempDir,
    _work: TempDir,
    /// Canonical root of everything but the data directory.
    root: PathBuf,
    home: PathBuf,
    registry: Arc<RepoRegistry>,
}

impl Machine {
    async fn new() -> Self {
        let data = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let root = std::fs::canonicalize(work.path()).unwrap();
        let home = git_repo(&root, "home");
        std::fs::create_dir(root.join("code")).unwrap();
        let registry = Arc::new(registry(&data).await);
        Self {
            data,
            _work: work,
            root,
            home,
            registry,
        }
    }

    fn settings(&self) -> Settings {
        Settings {
            home_repo: Some(text(&self.home)),
            scan_roots: vec![text(&self.root.join("code"))],
            clone_root: Some(text(&self.root.join("clones"))),
            max_parallel: None,
        }
    }

    /// Writes the home repo's file and rescans.
    async fn declare(&self, yaml: &str) -> Vec<Repo> {
        write_file(&self.home, HOME_FILE, yaml);
        self.rescan().await
    }

    async fn rescan(&self) -> Vec<Repo> {
        self.registry.refresh(&self.settings()).await.unwrap();
        self.registry.list().await
    }

    /// A clone under the scan root whose remote is `url`.
    fn clone_of(&self, rel: &str, url: &str) -> PathBuf {
        let path = self.root.join("code").join(rel);
        let repo = git_repo(
            path.parent().unwrap(),
            path.file_name().unwrap().to_str().unwrap(),
        );
        git(&repo, &["remote", "add", "origin", url]);
        repo
    }

    /// A new process: the registry read again from the same file.
    async fn restarted(&self) -> RepoRegistry {
        let registry = registry(&self.data).await;
        registry.refresh(&self.settings()).await.unwrap();
        registry
    }
}

fn named<'a>(repos: &'a [Repo], name: &str) -> &'a Repo {
    repos
        .iter()
        .find(|repo| repo.name == name)
        .unwrap_or_else(|| panic!("no repo {name} in {repos:?}"))
}

fn names(repos: &[Repo]) -> Vec<&str> {
    repos.iter().map(|repo| repo.name.as_str()).collect()
}

/// Waits for a clone that was started to end, one way or the other.
async fn cloned(registry: &RepoRegistry, repo_id: &str) -> Repo {
    for _ in 0..2000 {
        let repo = registry.get(repo_id).await.unwrap();
        if !repo.cloning {
            return repo;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the clone never ended");
}

#[tokio::test]
async fn repos_are_registered_listed_by_name_and_persisted() {
    let dir = TempDir::new().unwrap();
    let repos = TempDir::new().unwrap();
    let zeta = git_repo(repos.path(), "zeta");
    let alpha = git_repo(repos.path(), "alpha");
    let registry = registry(&dir).await;
    assert!(registry.list().await.is_empty());

    let first = registry
        .register(zeta.to_str().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(
        first,
        Repo {
            id: first.id.clone(),
            name: "zeta".to_owned(),
            path: text(&zeta),
            default_branch: Some("main".to_owned()),
            remote_url: None,
            available: true,
            declared: false,
            home: false,
            url: None,
            setup: None,
            checks: Vec::new(),
            cloning: false,
            problem: None,
        }
    );
    let second = registry
        .register(alpha.to_str().unwrap(), Some("  api "))
        .await
        .unwrap();
    assert_eq!(second.name, "api");
    assert_ne!(first.id, second.id);
    assert_eq!(registry.list().await, [second.clone(), first.clone()]);
    assert_eq!(registry.get(&first.id).await, Some(first.clone()));
    assert_eq!(registry.get("nope").await, None);
    assert!(registry.sets().await.is_empty());

    // A new process reads the same registry from the file.
    let reloaded = self::registry(&dir).await;
    assert_eq!(reloaded.list().await, [second, first]);
    assert!(!dir.path().join("repos.json.tmp").exists());
}

#[tokio::test]
async fn what_is_not_a_clone_root_is_invalid() {
    let dir = TempDir::new().unwrap();
    let repos = TempDir::new().unwrap();
    let repo = git_repo(repos.path(), "repo");
    write_file(&repo, "src/lib.rs", "");
    let registry = registry(&dir).await;
    let plain = repos.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    for path in [
        repo.join("src"),
        repo.join(".git"),
        repo.join("missing"),
        plain,
        PathBuf::from("relative/path"),
        PathBuf::new(),
    ] {
        let result = registry.register(path.to_str().unwrap(), None).await;
        assert!(matches!(result, Err(Error::Invalid(_))), "{path:?}");
    }
    assert!(registry.list().await.is_empty());
    assert!(!dir.path().join("repos.json").exists());
}

#[tokio::test]
async fn names_must_be_unique_and_usable_as_a_folder() {
    let dir = TempDir::new().unwrap();
    let repos = TempDir::new().unwrap();
    let one = git_repo(repos.path(), "one");
    let two = git_repo(repos.path(), "two");
    let registry = registry(&dir).await;
    let one_path = one.to_str().unwrap();
    let two_path = two.to_str().unwrap();

    for name in [".", "..", "a/b", "a\\b", "/abs"] {
        let result = registry.register(one_path, Some(name)).await;
        assert!(matches!(result, Err(Error::Invalid(_))), "{name}");
    }
    // A blank name falls back to the folder name.
    let repo = registry.register(one_path, Some("  ")).await.unwrap();
    assert_eq!(repo.name, "one");

    let taken = registry.register(two_path, Some("one")).await;
    assert!(matches!(taken, Err(Error::Conflict(_))));
    // The same clone twice, whatever the name and however the path is spelled.
    let again = registry.register(one_path, Some("other")).await;
    assert!(matches!(again, Err(Error::Conflict(_))));
    let dotted = format!("{}/../one", one_path);
    let again = registry.register(&dotted, Some("other")).await;
    assert!(matches!(again, Err(Error::Conflict(_))));
    assert_eq!(registry.list().await.len(), 1);
}

#[tokio::test]
async fn a_corrupt_or_unreadable_file_is_an_empty_registry() {
    let dir = TempDir::new().unwrap();
    let repos = TempDir::new().unwrap();
    let repo = git_repo(repos.path(), "repo");
    let path = Home::new(dir.path()).repos_path();
    for garbage in ["", "{\"version\": 1, \"repos\": [{\"id\"", "[1, 2]", "null"] {
        std::fs::write(&path, garbage).unwrap();
        assert!(registry(&dir).await.list().await.is_empty(), "{garbage}");
    }
    // A directory where the file should be cannot be read at all.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(registry(&dir).await.list().await.is_empty());
    std::fs::remove_dir(&path).unwrap();

    // Registering replaces the garbage with a good file.
    std::fs::write(&path, "garbage").unwrap();
    let registry = self::registry(&dir).await;
    registry
        .register(repo.to_str().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(self::registry(&dir).await.list().await.len(), 1);

    // A file written before repos could be declared still reads.
    let old = format!(
        "{{\"version\": 1, \"repos\": [{{\"id\": \"01OLD\", \"name\": \"old\", \"path\": {:?}, \
         \"default_branch\": \"main\", \"remote_url\": null}}]}}",
        text(&repo)
    );
    std::fs::write(&path, old).unwrap();
    let listed = self::registry(&dir).await.list().await;
    assert_eq!(names(&listed), ["old"]);
    assert_eq!(listed[0].id, "01OLD");
    assert!(listed[0].available);
}

#[tokio::test]
async fn reservations_check_the_ids_and_block_removal() {
    let dir = TempDir::new().unwrap();
    let repos = TempDir::new().unwrap();
    let store = Store::open(Home::new(dir.path())).await.unwrap();
    let registry = registry(&dir).await;
    let one = registry
        .register(git_repo(repos.path(), "one").to_str().unwrap(), None)
        .await
        .unwrap();
    let two = registry
        .register(git_repo(repos.path(), "two").to_str().unwrap(), None)
        .await
        .unwrap();

    let unknown = registry.reserve(&[&one.id, "nope"]).await;
    assert!(matches!(unknown.map(|_| ()), Err(Error::Invalid(_))));
    let twice = registry.reserve(&[&one.id, &two.id, &one.id]).await;
    assert!(matches!(twice.map(|_| ()), Err(Error::Invalid(_))));

    let held = registry.reserve(&[&one.id]).await.unwrap();
    assert!(matches!(
        registry.remove(&one.id, &store).await,
        Err(Error::Conflict(_))
    ));
    registry.remove(&two.id, &store).await.unwrap();
    drop(held);
    registry.remove(&one.id, &store).await.unwrap();
    assert!(matches!(
        registry.remove(&one.id, &store).await,
        Err(Error::Missing(_))
    ));
    assert!(self::registry(&dir).await.list().await.is_empty());
}

#[tokio::test]
async fn declared_repos_are_matched_to_clones_by_their_url() {
    let machine = Machine::new().await;
    // Each clone spells its remote differently from the declaration.
    let api = machine.clone_of("api-checkout", "ssh://git@github.com/you/api");
    let web = machine.clone_of("github.com/you/web", "https://GitHub.com/you/web.git/");
    // Too deep to be found, and one whose remote is another repository.
    machine.clone_of("a/b/c/deep", "git@github.com:you/deep.git");
    machine.clone_of("fork", "git@github.com:someone-else/docs.git");

    let repos = machine
        .declare(
            r#"
repos:
  web: { url: https://github.com/you/web }
  api: { url: git@github.com:you/api.git, checks: ["cargo test"], setup: "cargo fetch" }
  deep: { url: git@github.com:you/deep.git }
  docs: { url: git@github.com:you/docs.git }
  notes: {}
"#,
        )
        .await;
    // The home repo first, then by name. Clones nothing declares are not listed.
    assert_eq!(
        names(&repos),
        ["home", "api", "deep", "docs", "notes", "web"]
    );

    let home = &repos[0];
    assert!(home.home && home.available && !home.declared);
    assert_eq!(home.path, text(&machine.home));
    assert_eq!(home.default_branch.as_deref(), Some("main"));
    assert_eq!(home.problem, None);

    assert_eq!(
        *named(&repos, "api"),
        Repo {
            id: named(&repos, "api").id.clone(),
            name: "api".to_owned(),
            path: text(&api),
            default_branch: Some("main".to_owned()),
            remote_url: Some("ssh://git@github.com/you/api".to_owned()),
            available: true,
            declared: true,
            home: false,
            url: Some("git@github.com:you/api.git".to_owned()),
            setup: Some("cargo fetch".to_owned()),
            checks: vec!["cargo test".to_owned()],
            cloning: false,
            problem: None,
        }
    );
    assert_eq!(named(&repos, "web").path, text(&web));
    for missing in ["deep", "docs", "notes"] {
        let repo = named(&repos, missing);
        assert!(!repo.available && repo.declared, "{missing}");
        assert_eq!(repo.path, "");
        assert_eq!((&repo.default_branch, &repo.remote_url), (&None, &None));
    }
    assert_eq!(named(&repos, "notes").url, None);

    // A repo with no clone cannot be attached or reserved.
    let reserved = machine.registry.reserve(&[&named(&repos, "docs").id]).await;
    assert!(matches!(reserved.map(|_| ()), Err(Error::Conflict(_))));
    let reserved = machine
        .registry
        .reserve(&[&home.id, &named(&repos, "api").id])
        .await;
    assert!(reserved.is_ok());
}

#[tokio::test]
async fn a_repo_declared_without_a_url_is_the_folder_of_its_name() {
    let machine = Machine::new().await;
    // Directly under the scan root, and one level too deep to count.
    let notes = git_repo(&machine.root.join("code"), "notes");
    git_repo(&machine.root.join("code/group"), "docs");
    let elsewhere = git_repo(&machine.root.join("elsewhere"), "wiki");

    let repos = machine
        .declare("repos:\n  notes: {}\n  docs: {}\n  wiki: {}\n")
        .await;
    assert_eq!(named(&repos, "notes").path, text(&notes));
    assert!(named(&repos, "notes").available);
    assert!(!named(&repos, "docs").available);
    assert!(!named(&repos, "wiki").available);

    // Registering a clone by hand under the declared name says where it is,
    // and the repo keeps the id it was listed with.
    let before = named(&repos, "wiki").id.clone();
    let registered = machine
        .registry
        .register(&text(&elsewhere), None)
        .await
        .unwrap();
    assert_eq!(registered.id, before);
    assert!(registered.available && registered.declared);
    assert_eq!(registered.path, text(&elsewhere));
    assert_eq!(
        names(&machine.registry.list().await),
        ["home", "docs", "notes", "wiki"]
    );
}

#[tokio::test]
async fn a_hand_registered_repo_is_merged_with_the_declaration_of_its_name() {
    let machine = Machine::new().await;
    let store = Store::open(Home::new(machine.data.path())).await.unwrap();
    // Registered first, from a place the scan does not look at, while a
    // clone with the declared remote also sits under the scan root.
    let mine = git_repo(&machine.root.join("elsewhere"), "api");
    let registered = machine.registry.register(&text(&mine), None).await.unwrap();
    machine.clone_of("api", "git@github.com:you/api.git");
    let plain = git_repo(&machine.root.join("elsewhere"), "scratch");
    let scratch = machine
        .registry
        .register(&text(&plain), None)
        .await
        .unwrap();

    let yaml = "repos:\n  api: { url: git@github.com:you/api.git, setup: make deps, checks: [make test] }\n";
    let repos = machine.declare(yaml).await;
    assert_eq!(names(&repos), ["home", "api", "scratch"]);
    let api = named(&repos, "api");
    // One entry: the hand-registered clone, with what the declaration adds.
    assert_eq!(api.id, registered.id);
    assert_eq!(api.path, text(&mine));
    assert!(api.declared && api.available);
    assert_eq!(api.url.as_deref(), Some("git@github.com:you/api.git"));
    assert_eq!(api.setup.as_deref(), Some("make deps"));
    assert_eq!(api.checks, ["make test"]);
    assert!(!named(&repos, "scratch").declared);

    // A declared repo stays for as long as it is declared; so does the home repo.
    let refused = machine.registry.remove(&api.id, &store).await;
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    let refused = machine.registry.remove(&repos[0].id, &store).await;
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    machine.registry.remove(&scratch.id, &store).await.unwrap();

    // Once the home repo stops declaring it, it is a plain registered repo again.
    let repos = machine.declare("repos: {}\n").await;
    assert_eq!(names(&repos), ["home", "api"]);
    let api = named(&repos, "api");
    assert_eq!(api.id, registered.id);
    assert!(!api.declared);
    assert_eq!((&api.url, &api.setup, api.checks.len()), (&None, &None, 0));
    machine.registry.remove(&api.id, &store).await.unwrap();
}

#[tokio::test]
async fn ids_are_stable_across_rescans_and_restarts() {
    let machine = Machine::new().await;
    let yaml = "repos:\n  api: { url: git@github.com:you/api.git }\n  docs: {}\n";
    let first = machine.declare(yaml).await;
    let ids = |repos: &[Repo]| -> Vec<(String, String)> {
        repos
            .iter()
            .map(|r| (r.name.clone(), r.id.clone()))
            .collect()
    };
    assert_eq!(first.len(), 3);
    let distinct: HashSet<&String> = first.iter().map(|repo| &repo.id).collect();
    assert_eq!(distinct.len(), 3);

    // A rescan, and a clone that turns up: the same repos under the same ids.
    assert_eq!(ids(&machine.rescan().await), ids(&first));
    machine.clone_of("api", "https://github.com/you/api");
    let found = machine.rescan().await;
    assert!(named(&found, "api").available);
    assert_eq!(ids(&found), ids(&first));

    // A restart reads the ids back.
    let restarted = machine.restarted().await;
    assert_eq!(ids(&restarted.list().await), ids(&first));

    // A repo that is dropped and declared again is the repo it was.
    let without = machine.declare("repos:\n  docs: {}\n").await;
    assert_eq!(names(&without), ["home", "docs"]);
    assert_eq!(ids(&machine.declare(yaml).await), ids(&first));

    // The home repo keeps its id when it is registered by hand, and when
    // that registration is removed again.
    let store = Store::open(Home::new(machine.data.path())).await.unwrap();
    let by_hand = machine
        .registry
        .register(&text(&machine.home), None)
        .await
        .unwrap();
    assert_eq!(by_hand.id, first[0].id);
    assert!(by_hand.home);
    assert_eq!(ids(&machine.registry.list().await), ids(&first));
    machine.registry.remove(&by_hand.id, &store).await.unwrap();
    assert_eq!(ids(&machine.registry.list().await), ids(&first));
    assert_eq!(ids(&machine.restarted().await.list().await), ids(&first));
}

#[tokio::test]
async fn a_missing_or_invalid_file_leaves_the_registered_repos_and_says_why() {
    let machine = Machine::new().await;
    let plain = git_repo(&machine.root.join("elsewhere"), "scratch");
    machine
        .registry
        .register(&text(&plain), None)
        .await
        .unwrap();

    // No home repo at all: only what was registered.
    machine
        .registry
        .refresh(&Settings::default())
        .await
        .unwrap();
    assert_eq!(names(&machine.registry.list().await), ["scratch"]);

    // A home repo without the file.
    let repos = machine.rescan().await;
    assert_eq!(names(&repos), ["home", "scratch"]);
    let problem = repos[0].problem.as_deref().unwrap();
    assert!(problem.contains("has no .calin/repos.yaml"), "{problem}");
    assert!(repos[0].available);

    // A file that does not parse, then one with an entry that cannot be used.
    let repos = machine.declare("repos: [api, web\n").await;
    assert_eq!(names(&repos), ["home", "scratch"]);
    let problem = repos[0].problem.as_deref().unwrap();
    assert!(problem.contains("does not parse"), "{problem}");
    assert!(machine.registry.sets().await.is_empty());

    let repos = machine.declare("repos:\n  a/b: {}\n  ok: {}\n").await;
    assert_eq!(names(&repos), ["home", "ok", "scratch"]);
    assert!(repos[0].problem.as_deref().unwrap().contains("a/b"));

    // Fixed, the problem is gone.
    let repos = machine.declare("repos:\n  ok: {}\n").await;
    assert_eq!(repos[0].problem, None);

    // A home repo that is no work tree any more is still listed, as unusable.
    std::fs::remove_dir_all(machine.home.join(".git")).unwrap();
    let repos = machine.rescan().await;
    assert_eq!(names(&repos), ["home", "scratch"]);
    assert!(!repos[0].available && repos[0].home);
    assert!(repos[0].problem.is_some());
}

#[tokio::test]
async fn the_home_repo_merges_with_its_own_entry_and_never_takes_a_name_in_use() {
    let machine = Machine::new().await;
    // A registered repo that already goes by the home repo's folder name.
    let other = git_repo(&machine.root.join("elsewhere"), "home");
    machine
        .registry
        .register(&text(&other), None)
        .await
        .unwrap();
    let repos = machine.declare("repos:\n  home-home: {}\n").await;
    assert_eq!(names(&repos), ["home-home-2", "home", "home-home"]);
    assert!(repos[0].home);
    assert_eq!(repos[0].path, text(&machine.home));
    // The name is taken by the home repo now, for any other clone.
    let other = text(&machine.clone_of("x", "u"));
    let taken = machine.registry.register(&other, Some("home-home-2")).await;
    assert!(matches!(taken, Err(Error::Conflict(_))));

    // A home repo that declares itself is one entry, declared and home.
    let machine = Machine::new().await;
    git(
        &machine.home,
        &["remote", "add", "origin", "git@github.com:you/home.git"],
    );
    let mut settings = machine.settings();
    settings.scan_roots = vec![text(&machine.root)];
    write_file(
        &machine.home,
        HOME_FILE,
        "repos:\n  base: { url: git@github.com:you/home.git }\n",
    );
    machine.registry.refresh(&settings).await.unwrap();
    let repos = machine.registry.list().await;
    assert_eq!(names(&repos), ["base"]);
    assert!(repos[0].home && repos[0].declared && repos[0].available);
}

#[tokio::test]
async fn sets_keep_the_home_repos_order_and_drop_unknown_names() {
    let machine = Machine::new().await;
    let plain = git_repo(&machine.root.join("elsewhere"), "scratch");
    let scratch = machine
        .registry
        .register(&text(&plain), None)
        .await
        .unwrap();
    let repos = machine
        .declare(
            r#"
repos:
  api: {}
  web: {}
sets:
  storefront: [web, api, web]
  all: [scratch, api, gone, web]
  empty: []
"#,
        )
        .await;
    let id = |name: &str| named(&repos, name).id.clone();
    assert_eq!(
        machine.registry.sets().await,
        [
            RepoSet {
                name: "storefront".to_owned(),
                repo_ids: vec![id("web"), id("api")],
            },
            // A set may name a repo that is only registered by hand.
            RepoSet {
                name: "all".to_owned(),
                repo_ids: vec![scratch.id.clone(), id("api"), id("web")],
            },
            RepoSet {
                name: "empty".to_owned(),
                repo_ids: Vec::new(),
            },
        ]
    );
    assert!(machine.declare("repos: {}\n").await.len() == 2);
    assert!(machine.registry.sets().await.is_empty());
}

#[tokio::test]
async fn a_declared_repo_is_cloned_into_the_clone_root() {
    let machine = Machine::new().await;
    let upstream = git_repo(&machine.root.join("upstream"), "api");
    let head = git(&upstream, &["rev-parse", "HEAD"]);
    let plain = git_repo(&machine.root.join("elsewhere"), "scratch");
    let scratch = machine
        .registry
        .register(&text(&plain), None)
        .await
        .unwrap();
    let yaml = format!(
        "repos:\n  api: {{ url: {} }}\n  here: {{ url: git@github.com:you/here.git }}\n  docs: {{}}\n",
        upstream.display()
    );
    machine.clone_of("here", "git@github.com:you/here.git");
    let repos = machine.declare(&yaml).await;
    let api = named(&repos, "api").clone();
    assert!(!api.available && !api.cloning);

    // Only a declared repo without a clone and with a URL can be cloned.
    let settings = machine.settings();
    let registry = &machine.registry;
    let clone = async |id: String| registry.start_clone(&id, settings.clone()).await;
    assert!(matches!(
        clone("nope".to_owned()).await,
        Err(Error::Missing(_))
    ));
    for id in [
        &scratch.id,
        &named(&repos, "here").id,
        &named(&repos, "docs").id,
        &repos[0].id,
    ] {
        let refused = clone(id.clone()).await;
        assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    }

    clone(api.id.clone()).await.unwrap();
    let cloned = cloned(&machine.registry, &api.id).await;
    let dest = machine.root.join("clones/api");
    assert_eq!(
        cloned,
        Repo {
            path: text(&dest),
            default_branch: Some("main".to_owned()),
            remote_url: Some(text(&upstream)),
            available: true,
            ..api.clone()
        }
    );
    assert_eq!(git(&dest, &["rev-parse", "HEAD"]), head);
    // It has a clone now: there is nothing left to clone.
    assert!(matches!(
        clone(api.id.clone()).await,
        Err(Error::Conflict(_))
    ));
    // A restart finds the clone again, under the same id.
    assert_eq!(machine.restarted().await.get(&api.id).await, Some(cloned));
}

#[tokio::test]
async fn a_failed_clone_leaves_its_reason_and_no_directory() {
    let machine = Machine::new().await;
    let missing = machine.root.join("upstream/gone");
    let repos = machine
        .declare(&format!(
            "repos:\n  api: {{ url: {} }}\n",
            missing.display()
        ))
        .await;
    let api = named(&repos, "api").clone();
    let settings = machine.settings();

    machine
        .registry
        .start_clone(&api.id, settings.clone())
        .await
        .unwrap();
    let failed = cloned(&machine.registry, &api.id).await;
    assert!(!failed.available && !failed.cloning);
    let problem = failed.problem.clone().unwrap();
    assert!(problem.contains("does not exist"), "{problem}");
    // git's own words, trimmed, without the command line around them.
    assert!(
        problem.starts_with("fatal:") && !problem.contains("git clone"),
        "{problem}"
    );
    assert_eq!(problem, problem.trim());
    assert!(!machine.root.join("clones/api").exists());
    // The reason stays across a rescan, until the repo has a clone.
    assert_eq!(named(&machine.rescan().await, "api").problem, Some(problem));

    // Something in the way is a failure too, and is left alone.
    write_file(&machine.root.join("clones/api"), "mine.txt", "keep");
    machine
        .registry
        .start_clone(&api.id, settings.clone())
        .await
        .unwrap();
    let failed = cloned(&machine.registry, &api.id).await;
    assert!(failed.problem.unwrap().contains("already exists"));
    assert!(machine.root.join("clones/api/mine.txt").is_file());

    // Once the upstream exists and the way is clear, a retry succeeds and
    // the old reason is gone.
    std::fs::remove_dir_all(machine.root.join("clones/api")).unwrap();
    git_repo(&machine.root.join("upstream"), "gone");
    machine
        .registry
        .start_clone(&api.id, settings)
        .await
        .unwrap();
    let done = cloned(&machine.registry, &api.id).await;
    assert!(done.available);
    assert_eq!(done.problem, None);
}

#[tokio::test]
async fn a_repo_reports_cloning_while_its_clone_runs_and_takes_no_second_one() {
    let machine = Machine::new().await;
    let repos = machine
        .declare("repos:\n  api: { url: git@github.com:you/api.git }\n")
        .await;
    let api = named(&repos, "api").clone();
    // As `start_clone` leaves the state while git runs.
    machine
        .registry
        .state
        .lock()
        .await
        .cloning
        .insert("api".to_owned());
    let listed = machine.registry.get(&api.id).await.unwrap();
    assert!(listed.cloning && !listed.available);
    let second = machine
        .registry
        .start_clone(&api.id, machine.settings())
        .await;
    assert!(matches!(second, Err(Error::Conflict(_))), "{second:?}");
    // A rescan in the meantime changes nothing about it.
    assert!(named(&machine.rescan().await, "api").cloning);
}
