//! Settings, the repo catalog, attaching mid-session and the tool endpoint.

use std::sync::Arc;

use calin_agents::{AgentHandle, AgentIo, Runner, RunnerError, RunnerOutput, StartParams};
use calin_proto::{RepoSet, SessionRepo, Settings};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::*;

/// Runs git-less text into a file under `dir`.
fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

async fn put(app: &Router, path: &str, body: Value) -> Reply {
    send_json(app, Method::PUT, path, body).await
}

/// A machine: a home repo, a folder that is scanned for clones, and a folder
/// of clones that are only found when registered by hand.
struct Machine {
    app: Router,
    state: AppState,
    home: PathBuf,
    code: PathBuf,
    elsewhere: PathBuf,
    root: PathBuf,
    _data: TempDir,
    _work: TempDir,
}

async fn machine() -> Machine {
    let data = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap();
    let home = git_repo(&root, "home");
    let (code, elsewhere) = (root.join("code"), root.join("elsewhere"));
    std::fs::create_dir_all(&code).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let state = state(&data, true).await;
    Machine {
        app: router(state.clone()),
        state,
        home,
        code,
        elsewhere,
        root,
        _data: data,
        _work: work,
    }
}

impl Machine {
    fn settings(&self) -> Value {
        json!({
            "home_repo": self.home,
            "scan_roots": [self.code],
            "clone_root": self.root.join("clones"),
        })
    }

    /// Writes the home repo's file and points the settings at this machine.
    async fn configure(&self, yaml: &str) -> Vec<Repo> {
        write(&self.home, ".calin/repos.yaml", yaml);
        let reply = put(&self.app, "/api/v1/settings", self.settings()).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        get(&self.app, "/api/v1/repos").await.json()
    }

    /// A clone under the scan root whose remote is `url`.
    fn clone_of(&self, name: &str, url: &str) -> PathBuf {
        let repo = git_repo(&self.code, name);
        git(&repo, &["remote", "add", "origin", url]);
        repo
    }
}

fn named<'a>(repos: &'a [Repo], name: &str) -> &'a Repo {
    repos
        .iter()
        .find(|repo| repo.name == name)
        .unwrap_or_else(|| panic!("no repo {name}"))
}

fn names(repos: &[Repo]) -> Vec<&str> {
    repos.iter().map(|repo| repo.name.as_str()).collect()
}

#[tokio::test]
async fn settings_have_defaults_and_are_replaced_whole() {
    let data = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let first = state(&data, true).await;
    let app = router(first.clone());
    let reply = get(&app, "/api/v1/settings").await;
    assert_eq!(reply.status, StatusCode::OK);
    let defaults: Settings = reply.json();
    // The clone root is always filled in: the data directory's `repos`.
    assert_eq!(
        defaults,
        Settings {
            home_repo: None,
            scan_roots: Vec::new(),
            clone_root: Some(data.path().join("repos").to_str().unwrap().to_owned()),
        }
    );

    let home = git_repo(work.path(), "home");
    let code = std::fs::canonicalize(work.path()).unwrap();
    let wanted =
        json!({ "home_repo": home, "scan_roots": [code], "clone_root": code.join("clones") });
    let reply = put(&app, "/api/v1/settings", wanted).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let stored: Settings = reply.json();
    assert_eq!(stored.home_repo.as_deref(), home.to_str());
    assert_eq!(stored.scan_roots, [code.to_str().unwrap()]);
    assert_eq!(stored.clone_root.as_deref(), code.join("clones").to_str());
    assert_eq!(
        get(&app, "/api/v1/settings").await.json::<Settings>(),
        stored
    );
    // A restart reads them back.
    first.manager().store().close().await;
    let again = router(state_reopened(&data).await);
    assert_eq!(
        get(&again, "/api/v1/settings").await.json::<Settings>(),
        stored
    );

    // A field left out is a field unset: the body is the whole settings.
    let reply = put(&again, "/api/v1/settings", json!({ "scan_roots": [code] })).await;
    let partial: Settings = reply.json();
    assert_eq!(partial.home_repo, None);
    assert_eq!(partial.clone_root, defaults.clone_root);
    let reply = put(&again, "/api/v1/settings", json!({})).await;
    assert_eq!(reply.json::<Settings>(), defaults);
}

/// The same data directory in a second manager, as after a restart. The
/// store of the first one must be closed: a data directory has one owner.
async fn state_reopened(dir: &TempDir) -> AppState {
    let store = Store::open(Home::new(dir.path())).await.unwrap();
    let registry = RunnerRegistry::new(RegistryConfig {
        dry_run: true,
        ..RegistryConfig::default()
    });
    let manager = SessionManager::start_with(store, registry, config())
        .await
        .unwrap();
    AppState::new(manager).with_assets(Assets::empty())
}

#[tokio::test]
async fn invalid_settings_are_400_and_change_nothing() {
    let machine = machine().await;
    let before: Vec<Repo> = machine.configure("repos:\n  api: {}\n").await;
    let stored: Settings = get(&machine.app, "/api/v1/settings").await.json();
    let file = machine.root.join("file.txt");
    std::fs::write(&file, "x").unwrap();
    for body in [
        // The home repo must be the root of a git work tree.
        json!({ "home_repo": machine.code }),
        json!({ "home_repo": machine.home.join("src") }),
        json!({ "home_repo": machine.root.join("missing") }),
        json!({ "home_repo": "relative/home" }),
        // Every scan root must be an existing directory.
        json!({ "home_repo": machine.home, "scan_roots": [machine.code, machine.root.join("missing")] }),
        json!({ "scan_roots": [file] }),
        json!({ "scan_roots": ["relative"] }),
        json!({ "clone_root": "relative" }),
        // Not settings at all.
        json!({ "scan_roots": "one" }),
        json!({ "home_repo": 7 }),
        json!([]),
    ] {
        put(&machine.app, "/api/v1/settings", body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    let broken = request(Method::PUT, "/api/v1/settings")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from("{\"home_repo\""))
        .unwrap();
    call(&machine.app, broken)
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    assert_eq!(
        get(&machine.app, "/api/v1/settings")
            .await
            .json::<Settings>(),
        stored
    );
    assert_eq!(
        get(&machine.app, "/api/v1/repos").await.json::<Vec<Repo>>(),
        before
    );
    post(&machine.app, "/api/v1/settings", json!({}))
        .await
        .assert_error(StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn the_repo_list_is_the_home_repo_what_it_declares_and_what_was_registered() {
    let machine = machine().await;
    let api = machine.clone_of("api", "ssh://git@github.com/you/api");
    let scratch = register(&machine.app, &git_repo(&machine.elsewhere, "scratch")).await;
    // Before any settings: only what was registered by hand.
    assert_eq!(
        names(&get(&machine.app, "/api/v1/repos").await.json::<Vec<Repo>>()),
        ["scratch"]
    );
    assert!(
        get(&machine.app, "/api/v1/repo-sets")
            .await
            .json::<Vec<RepoSet>>()
            .is_empty()
    );

    // Setting the home repo reads its file and scans.
    let repos = machine
        .configure(
            r#"
repos:
  web: { url: https://github.com/you/web.git }
  api: { url: git@github.com:you/api.git, checks: ["cargo test"], setup: "cargo fetch" }
sets:
  storefront: [web, api, gone]
  tools: [scratch]
"#,
        )
        .await;
    assert_eq!(names(&repos), ["home", "api", "scratch", "web"]);
    assert!(repos[0].home && repos[0].available);
    assert_eq!(repos[0].problem, None);
    let listed_api = named(&repos, "api");
    assert!(listed_api.declared && listed_api.available);
    assert_eq!(listed_api.path, api.to_str().unwrap());
    assert_eq!(
        listed_api.url.as_deref(),
        Some("git@github.com:you/api.git")
    );
    assert_eq!(listed_api.setup.as_deref(), Some("cargo fetch"));
    assert_eq!(listed_api.checks, ["cargo test"]);
    let web = named(&repos, "web");
    assert!(web.declared && !web.available && !web.cloning);
    assert_eq!(web.path, "");
    assert_eq!(*named(&repos, "scratch"), scratch);

    // Sets, in the home repo's order, without the names that are no repo.
    let reply = get(&machine.app, "/api/v1/repo-sets").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json::<Vec<RepoSet>>(),
        [
            RepoSet {
                name: "storefront".to_owned(),
                repo_ids: vec![web.id.clone(), listed_api.id.clone()],
            },
            RepoSet {
                name: "tools".to_owned(),
                repo_ids: vec![scratch.id.clone()],
            },
        ]
    );

    // A declared repo cannot be deleted, nor the home repo; a repo with no
    // clone cannot start a session.
    for id in [&listed_api.id, &web.id, &repos[0].id] {
        delete(&machine.app, &format!("/api/v1/repos/{id}"))
            .await
            .assert_error(StatusCode::CONFLICT);
    }
    let body = json!({ "prompt": "needs web", "repos": [{ "repo_id": web.id }] });
    post(&machine.app, "/api/v1/sessions", body)
        .await
        .assert_error(StatusCode::CONFLICT);

    // Refresh picks up a new clone and a changed file, under the same ids.
    machine.clone_of("web", "git@github.com:you/web.git");
    write(
        &machine.home,
        ".calin/repos.yaml",
        "repos:\n  web: { url: https://github.com/you/web.git }\n",
    );
    let unchanged: Vec<Repo> = get(&machine.app, "/api/v1/repos").await.json();
    assert_eq!(unchanged, repos);
    let reply = post_empty(&machine.app, "/api/v1/repos/refresh").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let refreshed: Vec<Repo> = reply.json();
    assert_eq!(names(&refreshed), ["home", "scratch", "web"]);
    assert!(named(&refreshed, "web").available);
    assert_eq!(named(&refreshed, "web").id, web.id);
    assert_eq!(
        get(&machine.app, "/api/v1/repos").await.json::<Vec<Repo>>(),
        refreshed
    );
    assert!(
        get(&machine.app, "/api/v1/repo-sets")
            .await
            .json::<Vec<RepoSet>>()
            .is_empty()
    );
}

#[tokio::test]
async fn a_broken_repos_file_never_fails_a_request() {
    let machine = machine().await;
    let scratch = register(&machine.app, &git_repo(&machine.elsewhere, "scratch")).await;
    // No file at all, then one that does not parse.
    let reply = put(&machine.app, "/api/v1/settings", machine.settings()).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    for yaml in [None, Some("repos: [api\n"), Some("repos:\n  api: 7\n")] {
        if let Some(yaml) = yaml {
            write(&machine.home, ".calin/repos.yaml", yaml);
        }
        let reply = post_empty(&machine.app, "/api/v1/repos/refresh").await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        let repos: Vec<Repo> = reply.json();
        assert_eq!(names(&repos), ["home", "scratch"], "{yaml:?}");
        assert!(repos[0].home);
        assert!(
            repos[0]
                .problem
                .as_deref()
                .is_some_and(|why| why.contains("repos.yaml"))
        );
        assert_eq!(repos[1], scratch);
        assert!(
            get(&machine.app, "/api/v1/repo-sets")
                .await
                .json::<Vec<RepoSet>>()
                .is_empty()
        );
    }
}

/// Polls the repo list until `name` is no longer being cloned.
async fn cloned(app: &Router, name: &str) -> Repo {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let repos: Vec<Repo> = get(app, "/api/v1/repos").await.json();
            let repo = named(&repos, name).clone();
            if !repo.cloning {
                return repo;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the clone never ended")
}

#[tokio::test]
async fn a_declared_repo_is_cloned_in_the_background() {
    let machine = machine().await;
    let upstream = git_repo(&machine.root.join("upstream"), "api");
    let missing = machine.root.join("upstream/gone");
    let scratch = register(&machine.app, &git_repo(&machine.elsewhere, "scratch")).await;
    machine.clone_of("here", "git@github.com:you/here.git");
    let yaml = format!(
        "repos:\n  api: {{ url: \"file://{}\" }}\n  gone: {{ url: {} }}\n  \
         here: {{ url: git@github.com:you/here.git }}\n  docs: {{}}\n",
        upstream.display(),
        missing.display()
    );
    let repos = machine.configure(&yaml).await;
    let clone = |name: &str| format!("/api/v1/repos/{}/clone", named(&repos, name).id);

    // 404 for an unknown repo; 409 unless it is declared, has a URL and no clone.
    post_empty(&machine.app, "/api/v1/repos/nope/clone")
        .await
        .assert_error(StatusCode::NOT_FOUND);
    for path in [
        clone("here"),
        clone("docs"),
        clone("home"),
        format!("/api/v1/repos/{}/clone", scratch.id),
    ] {
        post_empty(&machine.app, &path)
            .await
            .assert_error(StatusCode::CONFLICT);
    }

    let reply = post_empty(&machine.app, &clone("api")).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    assert!(reply.body.is_empty());
    let api = cloned(&machine.app, "api").await;
    assert!(api.available, "{api:?}");
    assert_eq!(api.problem, None);
    assert_eq!(api.id, named(&repos, "api").id);
    let dest = machine.root.join("clones/api");
    assert_eq!(api.path, dest.to_str().unwrap());
    assert!(dest.join("README.md").is_file());
    post_empty(&machine.app, &clone("api"))
        .await
        .assert_error(StatusCode::CONFLICT);

    // A clone that fails says why and leaves no directory.
    let reply = post_empty(&machine.app, &clone("gone")).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    let gone = cloned(&machine.app, "gone").await;
    assert!(!gone.available);
    assert!(
        gone.problem
            .as_deref()
            .is_some_and(|why| why.contains("does not exist")),
        "{gone:?}"
    );
    assert!(!machine.root.join("clones/gone").exists());

    // The clone can start a session like any other repo.
    let created = create_with(
        &machine.app,
        json!({ "prompt": "use it", "permission_mode": "autonomous", "repos": [{ "repo_id": api.id }] }),
    )
    .await;
    let detail = wait_for(&machine.app, &created.session.id, SessionStatus::Review).await;
    assert_eq!(detail.repos[0].name, "api");
}

#[tokio::test]
async fn a_repo_is_attached_mid_session_and_shows_in_the_changes() {
    let machine = machine().await;
    let api = register(&machine.app, &git_repo(&machine.elsewhere, "api")).await;
    let web = register(&machine.app, &git_repo(&machine.elsewhere, "web")).await;
    let docs = register(&machine.app, &git_repo(&machine.elsewhere, "docs")).await;
    let far = machine
        .configure("repos:\n  far: { url: git@github.com:you/far.git }\n")
        .await;
    let far = named(&far, "far").clone();
    let created = create_with(
        &machine.app,
        json!({ "prompt": "start", "permission_mode": "autonomous", "repos": [{ "repo_id": api.id }] }),
    )
    .await;
    let id = created.session.id;
    wait_for(&machine.app, &id, SessionStatus::Review).await;
    let path = format!("/api/v1/sessions/{id}/repos");

    let reply = post(&machine.app, &path, json!({ "repo_id": web.id })).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let attached: SessionRepo = reply.json();
    assert_eq!(
        (attached.name.as_str(), attached.mode),
        ("web", RepoMode::Write)
    );
    assert!(!attached.primary);
    assert!(attached.branch.is_some());
    // Ready when the response arrives: a worktree to write in.
    let worktree = PathBuf::from(&attached.path);
    assert!(worktree.join("README.md").is_file());
    std::fs::write(
        worktree.join("src/lib.rs"),
        "pub fn two() -> u8 {\n    2\n}\n",
    )
    .unwrap();
    std::fs::write(worktree.join("NEW.md"), "new\n").unwrap();

    let detail: SessionDetail = get(&machine.app, &format!("/api/v1/sessions/{id}"))
        .await
        .json();
    assert_eq!(detail.repos.len(), 2);
    assert_eq!(detail.repos[1], attached);
    let changes: Vec<RepoChanges> = get(&machine.app, &format!("/api/v1/sessions/{id}/changes"))
        .await
        .json();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[1].session_repo_id, attached.id);
    assert_eq!(changes[1].name, "web");
    let changed: Vec<(&str, ChangeStatus)> = changes[1]
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.status))
        .collect();
    assert_eq!(
        changed,
        [
            ("NEW.md", ChangeStatus::Untracked),
            ("src/lib.rs", ChangeStatus::Modified)
        ]
    );
    // The file routes reach the new repo too.
    let file = format!("{path}/{}/file?path=NEW.md", attached.id);
    assert_eq!(
        get(&machine.app, &file)
            .await
            .json::<FileContent>()
            .text
            .as_deref(),
        Some("new\n")
    );

    // The event is the user's: no agent on it.
    let events = events_until(
        &machine.state,
        &id,
        |event| matches!(&event.body, EventBody::RepoAttached { repo } if repo.name == "web"),
    )
    .await;
    assert_eq!(events.last().unwrap().agent_id, None);

    // The other modes, by name.
    let reply = post(
        &machine.app,
        &path,
        json!({ "repo_id": docs.id, "mode": "read" }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let read: SessionRepo = reply.json();
    assert_eq!(
        (read.mode, read.path.as_str(), &read.branch),
        (RepoMode::Read, docs.path.as_str(), &None)
    );

    // 404: unknown session or repo. 400: not a request. 409: attached
    // already, or no clone.
    let unknown = format!("/api/v1/sessions/{UNKNOWN_SESSION}/repos");
    post(&machine.app, &unknown, json!({ "repo_id": web.id }))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    post(&machine.app, &path, json!({ "repo_id": "nope" }))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    for body in [
        json!({}),
        json!({ "repo_id": web.id, "mode": "sideways" }),
        json!([]),
    ] {
        post(&machine.app, &path, body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    for body in [
        json!({ "repo_id": web.id }),
        json!({ "repo_id": api.id, "mode": "read" }),
        json!({ "repo_id": far.id }),
    ] {
        post(&machine.app, &path, body)
            .await
            .assert_error(StatusCode::CONFLICT);
    }
    // 409 once the session takes no repos any more.
    let stop = format!("/api/v1/sessions/{id}/stop");
    assert_eq!(
        post_empty(&machine.app, &stop).await.status,
        StatusCode::ACCEPTED
    );
    post(
        &machine.app,
        &path,
        json!({ "repo_id": machine_home(&machine).await.id }),
    )
    .await
    .assert_error(StatusCode::CONFLICT);
    let detail: SessionDetail = get(&machine.app, &format!("/api/v1/sessions/{id}"))
        .await
        .json();
    assert_eq!(detail.repos.len(), 3);
}

async fn machine_home(machine: &Machine) -> Repo {
    let repos: Vec<Repo> = get(&machine.app, "/api/v1/repos").await.json();
    repos[0].clone()
}

/// A runner that hands the test the agent's end of every start, so the test
/// sees what an agent is started with and plays its part.
struct Capture {
    starts: mpsc::UnboundedSender<(StartParams, AgentIo)>,
}

impl Runner for Capture {
    fn id(&self) -> &str {
        "capture"
    }

    fn start(&self, params: StartParams) -> Result<AgentHandle, RunnerError> {
        let (handle, io) = AgentHandle::channel();
        self.starts
            .send((params, io))
            .map_err(|_| RunnerError::Start("the test is over".to_owned()))?;
        Ok(handle)
    }
}

pub(super) type Starts = mpsc::UnboundedReceiver<(StartParams, AgentIo)>;

async fn capturing_state(dir: &TempDir) -> (AppState, Starts) {
    let store = Store::open(Home::new(dir.path())).await.unwrap();
    let mut registry = RunnerRegistry::new(RegistryConfig {
        mock: MockConfig {
            delay: Duration::ZERO,
        },
        ..RegistryConfig::default()
    });
    let (starts, started) = mpsc::unbounded_channel();
    registry.register(Arc::new(Capture { starts }));
    let manager = SessionManager::start_with(store, registry, config())
        .await
        .unwrap();
    (AppState::new(manager).with_assets(Assets::empty()), started)
}

/// A session whose agent the test plays, with `api` attached, and the
/// agent's tool endpoint as a path of the router.
pub(super) struct Agent {
    pub(super) app: Router,
    pub(super) state: AppState,
    pub(super) session_id: String,
    pub(super) agent_id: String,
    /// `/api/v1/mcp/<token>`.
    pub(super) endpoint: String,
    pub(super) params: StartParams,
    pub(super) io: AgentIo,
    web: Repo,
    /// The agents started after this one: those of the sessions it dispatches.
    pub(super) starts: Starts,
    _dirs: (TempDir, TempDir),
}

/// The path of the tool endpoint an agent was started with.
pub(super) fn endpoint_of(params: &StartParams) -> String {
    let url = params
        .tool_endpoint
        .clone()
        .expect("the agent has a tool endpoint");
    let endpoint = url
        .strip_prefix(&format!("http://{LOCAL}"))
        .unwrap()
        .to_owned();
    assert!(endpoint.starts_with("/api/v1/mcp/"), "{endpoint}");
    endpoint
}

/// One JSON-RPC request to a tool endpoint; the response's body.
pub(super) async fn rpc(
    app: &Router,
    endpoint: &str,
    id: Value,
    method: &str,
    params: Value,
) -> Value {
    let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let reply = post(app, endpoint, body).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.content_type(), "application/json");
    let answer: Value = reply.json();
    assert_eq!(answer["jsonrpc"], "2.0");
    assert_eq!(answer["id"], id);
    answer
}

/// Calls a tool at an endpoint; its text and whether it is an error.
pub(super) async fn call_tool(
    app: &Router,
    endpoint: &str,
    name: &str,
    arguments: Value,
) -> (String, bool) {
    let params = json!({
        "name": name, "arguments": arguments,
        // What a real client adds; nothing here depends on it.
        "_meta": { "progressToken": 7 },
    });
    let answer = rpc(app, endpoint, json!(3), "tools/call", params).await;
    let result = &answer["result"];
    assert!(answer.get("error").is_none(), "{answer}");
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    (
        content[0]["text"].as_str().unwrap().to_owned(),
        result["isError"].as_bool().unwrap(),
    )
}

pub(super) async fn agent() -> Agent {
    let data = TempDir::new().unwrap();
    let clones = TempDir::new().unwrap();
    let (state, mut starts) = capturing_state(&data).await;
    // A server that never bound has no address; the tests give it one.
    state
        .manager()
        .set_tool_endpoint(format!("http://{LOCAL}/api/v1/mcp"));
    let app = router(state.clone());
    let api = register(&app, &git_repo(clones.path(), "api")).await;
    let web = register(&app, &git_repo(clones.path(), "web")).await;
    register(&app, &git_repo(clones.path(), "docs")).await;
    let created = create_with(
        &app,
        json!({
            "prompt": "work", "runner": "capture", "permission_mode": "autonomous",
            "repos": [{ "repo_id": api.id }],
        }),
    )
    .await;
    let (params, io) = tokio::time::timeout(TIMEOUT, starts.recv())
        .await
        .unwrap()
        .unwrap();
    let endpoint = endpoint_of(&params);
    let detail = wait_for(&app, &created.session.id, SessionStatus::Running).await;
    Agent {
        app,
        state,
        session_id: created.session.id,
        agent_id: detail.agents[0].id.clone(),
        endpoint,
        params,
        io,
        web,
        starts,
        _dirs: (data, clones),
    }
}

impl Agent {
    /// One JSON-RPC request to the agent's endpoint; the response's body.
    pub(super) async fn rpc(&self, id: Value, method: &str, params: Value) -> Value {
        rpc(&self.app, &self.endpoint, id, method, params).await
    }

    /// Calls a tool; its text and whether it is an error.
    pub(super) async fn tool(&self, name: &str, arguments: Value) -> (String, bool) {
        call_tool(&self.app, &self.endpoint, name, arguments).await
    }
}

#[tokio::test]
async fn the_tool_endpoint_speaks_the_handshake_a_client_expects() {
    let agent = agent().await;

    // Clients probe for a newer protocol first, with a string id.
    let probe = agent
        .rpc(
            json!("server-discover-probe-1"),
            "server/discover",
            json!({}),
        )
        .await;
    assert_eq!(probe["error"]["code"], -32601);
    assert!(probe.get("result").is_none());

    let init = agent
        .rpc(
            json!(0),
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": { "roots": { "listChanged": true } },
                "clientInfo": { "name": "claude-code", "version": "2.1.332" },
            }),
        )
        .await;
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-11-25");
    assert_eq!(result["serverInfo"]["name"], "calin");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(result["capabilities"]["tools"].is_object());
    assert!(
        result["instructions"]
            .as_str()
            .unwrap()
            .contains("attach_repo")
    );
    // An older revision is spoken as asked; an unknown one gets the newest.
    for (asked, answered) in [("2025-03-26", "2025-03-26"), ("2099-01-01", "2025-11-25")] {
        let init = agent
            .rpc(json!(1), "initialize", json!({ "protocolVersion": asked }))
            .await;
        assert_eq!(init["result"]["protocolVersion"], answered);
    }
    let init = agent.rpc(json!(1), "initialize", Value::Null).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");

    // Notifications are acknowledged and never answered.
    for method in [
        "notifications/initialized",
        "notifications/cancelled",
        "anything/else",
    ] {
        let body = json!({ "jsonrpc": "2.0", "method": method });
        let reply = post(&agent.app, &agent.endpoint, body).await;
        assert_eq!(reply.status, StatusCode::ACCEPTED, "{method}");
        assert!(reply.body.is_empty(), "{method}");
    }
    // So is a response to a request the server never made.
    let reply = post(
        &agent.app,
        &agent.endpoint,
        json!({ "jsonrpc": "2.0", "id": 9, "result": {} }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);

    assert_eq!(
        agent.rpc(json!(2), "ping", Value::Null).await["result"],
        json!({})
    );

    let listed = agent.rpc(json!(3), "tools/list", Value::Null).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "list_repos",
            "attach_repo",
            "dispatch",
            "send_message",
            "report",
            "ask_user"
        ]
    );
    for tool in tools {
        assert!(tool["description"].as_str().unwrap().len() > 40);
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }
    let attach = &tools[1]["inputSchema"];
    assert_eq!(attach["required"], json!(["name"]));
    assert_eq!(attach["properties"]["name"]["type"], "string");
    assert_eq!(
        attach["properties"]["mode"]["enum"],
        json!(["write", "read"])
    );

    // Unknown methods and unknown tools are protocol errors, by id.
    let unknown = agent.rpc(json!(4), "resources/list", Value::Null).await;
    assert_eq!(unknown["error"]["code"], -32601);
    assert!(
        unknown["error"]["message"]
            .as_str()
            .unwrap()
            .contains("resources/list")
    );
    let no_tool = agent
        .rpc(json!(5), "tools/call", json!({ "name": "merge_branch" }))
        .await;
    assert_eq!(no_tool["error"]["code"], -32602);

    // What is not one JSON-RPC message is a 400 that says so.
    for (body, code) in [
        ("{\"jsonrpc\"", -32700),
        ("[]", -32600),
        ("7", -32600),
        ("{}", -32600),
    ] {
        let request = json_post(&agent.endpoint).body(Body::from(body)).unwrap();
        let reply = call(&agent.app, request).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
        let answer: Value = reply.json();
        assert_eq!(answer["error"]["code"], code, "{body}");
        assert_eq!(answer["id"], Value::Null);
    }
    // The stream a client may ask for does not exist.
    get(&agent.app, &agent.endpoint)
        .await
        .assert_error(StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn an_agent_lists_and_attaches_repos_through_its_tools() {
    let agent = agent().await;
    let (listed, is_error) = agent.tool("list_repos", json!({})).await;
    assert!(!is_error);
    let cwd = agent.params.cwd.display();
    assert_eq!(
        listed,
        format!(
            "Repositories Calin knows:\n\
             - api: attached to this session (write, primary) at {cwd}\n\
             - docs: not attached; call attach_repo to work in it\n\
             - web: not attached; call attach_repo to work in it\n"
        )
    );
    // A client may leave the arguments out altogether.
    let bare = agent
        .rpc(json!(1), "tools/call", json!({ "name": "list_repos" }))
        .await;
    assert_eq!(bare["result"]["isError"], false);

    let (text, is_error) = agent.tool("attach_repo", json!({ "name": "web" })).await;
    assert!(!is_error, "{text}");
    let detail: SessionDetail = get(
        &agent.app,
        &format!("/api/v1/sessions/{}", agent.session_id),
    )
    .await
    .json();
    let attached = &detail.repos[1];
    assert_eq!(
        (attached.repo_id.as_str(), attached.mode),
        (agent.web.id.as_str(), RepoMode::Write)
    );
    // The result carries the path the agent can now work in.
    assert!(
        text.contains(&format!("Work in it at: {}", attached.path)),
        "{text}"
    );
    assert!(Path::new(&attached.path).join("README.md").is_file());

    // The event says which agent asked.
    let events = events_until(
        &agent.state,
        &agent.session_id,
        |event| matches!(&event.body, EventBody::RepoAttached { repo } if repo.name == "web"),
    )
    .await;
    assert_eq!(
        events.last().unwrap().agent_id.as_deref(),
        Some(agent.agent_id.as_str())
    );
    let (listed, _) = agent.tool("list_repos", json!({})).await;
    assert!(listed.contains(&format!(
        "- web: attached to this session (write) at {}",
        attached.path
    )));

    let (text, is_error) = agent
        .tool("attach_repo", json!({ "name": "docs", "mode": "read" }))
        .await;
    assert!(!is_error);
    assert!(text.starts_with("Attached docs read-only."), "{text}");

    // Failures are results the model can act on, not transport errors.
    for (arguments, said) in [
        (
            json!({ "name": "nope" }),
            "No repository is named nope. Call list_repos",
        ),
        (
            json!({ "name": "web" }),
            "web is already attached to this session, at",
        ),
        (
            json!({ "name": "api", "mode": "read" }),
            "api is already attached",
        ),
        (
            json!({ "name": "web", "mode": "live" }),
            "`mode` must be \"write\" or \"read\"",
        ),
        (json!({ "name": "  " }), "needs the repository's name"),
        (json!({}), "needs the repository's name"),
        (Value::Null, "needs the repository's name"),
    ] {
        let (text, is_error) = agent.tool("attach_repo", arguments.clone()).await;
        assert!(is_error, "{arguments}: {text}");
        assert!(text.contains(said), "{arguments}: {text}");
    }
    let detail: SessionDetail = get(
        &agent.app,
        &format!("/api/v1/sessions/{}", agent.session_id),
    )
    .await
    .json();
    assert_eq!(detail.repos.len(), 3);
}

#[tokio::test]
async fn a_repo_without_a_clone_is_a_tool_error_that_says_what_to_do() {
    let data = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let (state, mut starts) = capturing_state(&data).await;
    state
        .manager()
        .set_tool_endpoint(format!("http://{LOCAL}/api/v1/mcp"));
    let app = router(state);
    let home = git_repo(work.path(), "home");
    write(
        &home,
        ".calin/repos.yaml",
        "repos:\n  far: { url: git@github.com:you/far.git }\n",
    );
    let reply = put(&app, "/api/v1/settings", json!({ "home_repo": home })).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    create_with(&app, json!({ "prompt": "work", "runner": "capture" })).await;
    let (params, _io) = tokio::time::timeout(TIMEOUT, starts.recv())
        .await
        .unwrap()
        .unwrap();
    let endpoint = params.tool_endpoint.unwrap();
    let endpoint = endpoint.strip_prefix(&format!("http://{LOCAL}")).unwrap();
    let call = |name: &str, arguments: Value| {
        let params = json!({ "name": name, "arguments": arguments });
        post(
            &app,
            endpoint,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params }),
        )
    };
    let listed: Value = call("list_repos", json!({})).await.json();
    let text = listed["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("- far: not cloned on this machine"), "{text}");
    assert!(text.contains("- home: not attached"), "{text}");
    let refused: Value = call("attach_repo", json!({ "name": "far" })).await.json();
    assert_eq!(refused["result"]["isError"], true);
    let text = refused["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("far has no clone on this machine yet"),
        "{text}"
    );
}

#[tokio::test]
async fn a_token_opens_one_agents_endpoint_for_as_long_as_the_agent_runs() {
    let agent = agent().await;
    let body = || json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    // Not a token, and nearly this agent's token.
    let mut near = agent.endpoint.clone();
    let last = if near.ends_with('0') { '1' } else { '0' };
    near.pop();
    near.push(last);
    for path in ["/api/v1/mcp/nope", "/api/v1/mcp/", near.as_str()] {
        let reply = post(&agent.app, path, body()).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }
    // The guard stands in front of it like in front of every route.
    let foreign = json_post(&agent.endpoint)
        .header(ORIGIN, "https://example.com")
        .body(Body::from(body().to_string()))
        .unwrap();
    call(&agent.app, foreign)
        .await
        .assert_error(StatusCode::FORBIDDEN);
    let rebound = Request::builder()
        .method(Method::POST)
        .uri(&agent.endpoint)
        .header(HOST, "example.com")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body().to_string()))
        .unwrap();
    call(&agent.app, rebound)
        .await
        .assert_error(StatusCode::FORBIDDEN);
    assert_eq!(
        post(&agent.app, &agent.endpoint, body()).await.status,
        StatusCode::OK
    );

    // Another session's agent has another token, for its own session.
    let other = create_with(
        &agent.app,
        json!({ "prompt": "other", "permission_mode": "autonomous" }),
    )
    .await;
    wait_for(&agent.app, &other.session.id, SessionStatus::Review).await;
    let detail: SessionDetail = get(
        &agent.app,
        &format!("/api/v1/sessions/{}", other.session.id),
    )
    .await
    .json();
    assert!(detail.repos.is_empty());

    // Once the agent has exited, its token opens nothing.
    agent
        .io
        .outputs
        .send(RunnerOutput::TurnEnded)
        .await
        .unwrap();
    wait_for(&agent.app, &agent.session_id, SessionStatus::Review).await;
    assert_eq!(
        post(&agent.app, &agent.endpoint, body()).await.status,
        StatusCode::OK
    );
    let stop = format!("/api/v1/sessions/{}/stop", agent.session_id);
    assert_eq!(
        post_empty(&agent.app, &stop).await.status,
        StatusCode::ACCEPTED
    );
    let reply = post(&agent.app, &agent.endpoint, body()).await;
    reply.assert_error(StatusCode::NOT_FOUND);
}

/// One HTTP/1.1 POST over a real socket; the status and the body.
async fn http_post(url: &str, body: &str) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("an http URL");
    let (authority, path) = rest.split_once('/').unwrap();
    let mut stream = TcpStream::connect(authority).await.unwrap();
    let request = format!(
        "POST /{path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(TIMEOUT, stream.read_to_string(&mut response))
        .await
        .expect("no response")
        .unwrap();
    let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned());
    (status, body.unwrap_or_default())
}

#[tokio::test]
async fn a_bound_server_gives_agents_an_endpoint_at_its_own_address() {
    let data = TempDir::new().unwrap();
    let (state, mut starts) = capturing_state(&data).await;
    let server = Server::bind("127.0.0.1:0".parse().unwrap(), state.clone())
        .await
        .unwrap();
    let addr = server.local_addr().unwrap();
    let serving = tokio::spawn(server.run(std::future::pending()));
    let app = router(state);
    create_with(&app, json!({ "prompt": "work", "runner": "capture" })).await;
    let (params, _io) = tokio::time::timeout(TIMEOUT, starts.recv())
        .await
        .unwrap()
        .unwrap();

    // The port is the one that was bound, whatever it turned out to be.
    let endpoint = params.tool_endpoint.unwrap();
    assert!(
        endpoint.starts_with(&format!("http://{addr}/api/v1/mcp/")),
        "{endpoint}"
    );
    let (status, body) = http_post(
        &endpoint,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("attach_repo"), "{body}");
    let (status, _) = http_post(
        &format!("http://{addr}/api/v1/mcp/0000"),
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
    )
    .await;
    assert_eq!(status, 404);
    serving.abort();
}
