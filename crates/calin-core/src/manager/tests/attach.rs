//! Repos that join a session after it started, by the user or by the agent,
//! and what follows from that: the path policy, the setup command, the
//! agent's token for Calin's tools and what the agent is told.

use calin_agents::{DirGrant, PermissionAsk, ToolAccess};
use calin_proto::Settings;

use super::*;

const TOOLS: &str = "http://127.0.0.1:4680/api/v1/mcp";

/// A session on the puppet runner with `api` attached as its primary repo,
/// its agent started and its first turn still running.
struct Running {
    manager: SessionManager,
    starts: Starts,
    _dir: TempDir,
    clones: TempDir,
    id: String,
    params: StartParams,
    io: AgentIo,
    api: Repo,
}

async fn running(mode: PermissionMode) -> Running {
    let Fixture {
        manager,
        mut starts,
        dir,
    } = fixture().await;
    manager.set_tool_endpoint(TOOLS);
    let clones = TempDir::new().unwrap();
    let api = registered(&manager, clones.path(), "api").await;
    let mut wanted = request_in(mode, "start", Some("puppet"));
    wanted.repos = vec![attach(&api, None)];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let (params, io) = within(starts.recv()).await.unwrap();
    Running {
        manager,
        starts,
        _dir: dir,
        clones,
        id,
        params,
        io,
        api,
    }
}

impl Running {
    async fn repo(&self, name: &str) -> Repo {
        registered(&self.manager, self.clones.path(), name).await
    }

    async fn attach(&self, repo: &Repo, mode: Option<RepoMode>) -> Result<SessionRepo> {
        within(self.manager.attach_repo(&self.id, attach(repo, mode))).await
    }

    async fn head_seq(&self) -> u64 {
        self.manager.get(&self.id).await.unwrap().session.head_seq
    }

    async fn status(&self) -> SessionStatus {
        self.manager.get(&self.id).await.unwrap().session.status
    }

    /// Ends the running turn and waits for the session to settle in review.
    async fn end_turn(&self) {
        let after = self.head_seq().await;
        emit(&self.io, RunnerOutput::TurnEnded).await;
        events_until(
            &self.manager,
            &self.id,
            after,
            is_status(SessionStatus::Review),
        )
        .await;
    }

    async fn input(&mut self) -> RunnerInput {
        within(self.io.inputs.recv())
            .await
            .expect("the agent was dropped")
    }

    /// Lets the agent exit between turns and waits until that is on record.
    async fn exit_agent(&self) {
        exit(&self.manager, &self.id, &self.io).await;
    }

    /// The events appended after `after`, once the log is quiet: every
    /// command of these tests is answered only after its events are written.
    async fn events_after(&self, after: u64) -> Vec<Event> {
        let subscription = self
            .manager
            .store()
            .subscribe(&self.id, after)
            .await
            .unwrap();
        subscription.backlog
    }
}

async fn exit(manager: &SessionManager, id: &str, io: &AgentIo) {
    let after = manager.get(id).await.unwrap().session.head_seq;
    emit(io, RunnerOutput::Exited(Exit::Ok)).await;
    events_until(manager, id, after, |event| {
        matches!(
            event.body,
            EventBody::AgentStatus {
                status: AgentStatus::Done
            }
        )
    })
    .await;
}

fn file_ask(id: &str, write: bool, paths: &[String]) -> RunnerOutput {
    RunnerOutput::PermissionRequest(PermissionAsk {
        request: PermissionRequest {
            id: id.to_owned(),
            tool_name: if write { "Edit" } else { "Read" }.to_owned(),
            tool_kind: if write {
                ToolKind::Edit
            } else {
                ToolKind::Read
            },
            title: paths.join(" "),
            input: serde_json::json!({ "file_path": paths.first() }),
            item_id: None,
        },
        access: ToolAccess::Files {
            paths: paths.iter().map(PathBuf::from).collect(),
            write,
        },
        subagent: None,
    })
}

fn decision(id: &str, decision: PermissionDecision, grants: Vec<DirGrant>) -> RunnerInput {
    RunnerInput::PermissionDecision {
        request_id: id.to_owned(),
        decision,
        message: None,
        grants,
    }
}

fn grant(repo: &SessionRepo) -> DirGrant {
    DirGrant {
        dir: PathBuf::from(&repo.path),
        write: repo.mode != RepoMode::Read,
    }
}

fn token_of(params: &StartParams) -> String {
    let endpoint = params.tool_endpoint.as_deref().expect("no tool endpoint");
    endpoint
        .strip_prefix(&format!("{TOOLS}/"))
        .expect("the endpoint is the base and the token")
        .to_owned()
}

#[tokio::test]
async fn a_repo_attached_by_the_user_is_ready_at_once_and_the_agent_is_told_with_the_next_message()
{
    let mut session = running(PermissionMode::Autonomous).await;
    let web = session.repo("web").await;
    let docs = session.repo("docs").await;
    let web_head = git(Path::new(&web.path), &["rev-parse", "HEAD"]);
    let before = session.head_seq().await;

    // While the turn is running: a worktree on the session's branch.
    let attached = session.attach(&web, None).await.unwrap();
    let detail = session.manager.get(&session.id).await.unwrap();
    let branch = detail.session.branch.clone().unwrap();
    let worktree = session.params.cwd.parent().unwrap().join("web");
    assert_eq!(
        attached,
        SessionRepo {
            id: attached.id.clone(),
            session_id: session.id.clone(),
            repo_id: web.id.clone(),
            name: "web".to_owned(),
            mode: RepoMode::Write,
            path: worktree.to_str().unwrap().to_owned(),
            branch: Some(branch.clone()),
            base_ref: Some(web_head),
            // The agent already works somewhere.
            primary: false,
        }
    );
    assert_eq!(
        git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        branch
    );
    assert_eq!(detail.repos.len(), 2);
    assert_eq!(detail.repos[1], attached);
    assert_eq!(session.status().await, SessionStatus::Running);

    // One event, by nobody in particular: the user did it.
    let events = session.events_after(before).await;
    assert_eq!(labels(&events), ["repo_attached:web"]);
    assert_eq!(events[0].agent_id, None);
    assert_eq!(
        events[0].body,
        EventBody::RepoAttached {
            repo: attached.clone()
        }
    );

    // What the agent writes there shows in the session's changes.
    write_file(&worktree, "src/new.rs", "pub fn new() {}\n");
    let changes = session.manager.changes(&session.id).await.unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[1].session_repo_id, attached.id);
    assert_eq!(changes[1].files[0].path, "src/new.rs");

    // In review, in the other two modes: the clone itself.
    session.end_turn().await;
    let live = session.attach(&docs, Some(RepoMode::Live)).await.unwrap();
    assert_eq!(
        (live.mode, live.path.as_str(), &live.branch),
        (RepoMode::Live, docs.path.as_str(), &None)
    );
    let notes = session.repo("notes").await;
    let read = session.attach(&notes, Some(RepoMode::Read)).await.unwrap();
    assert_eq!(
        (read.mode, read.path.as_str()),
        (RepoMode::Read, notes.path.as_str())
    );
    assert!(!Path::new(&docs.path).join(".git/worktrees").exists());

    // The next message carries one note per repo, ahead of the user's text;
    // the transcript holds the text as the user wrote it.
    session
        .manager
        .send_message(&session.id, "go on\nplease")
        .await
        .unwrap();
    let RunnerInput::UserMessage(delivered) = session.input().await else {
        panic!("expected the message");
    };
    assert_eq!(
        delivered,
        format!(
            "[Calin] The user attached the repository `web` to this session in write mode. It is at {}\n\
             [Calin] The user attached the repository `docs` to this session in live mode. It is at {}\n\
             [Calin] The user attached the repository `notes` to this session in read mode. It is at {}\n\
             \ngo on\nplease",
            attached.path, docs.path, notes.path
        )
    );
    let events = session.events_after(before).await;
    let said: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::UserMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(said, ["go on\nplease"]);

    // Told once: the message after that is the user's text alone.
    session.end_turn().await;
    session
        .manager
        .send_message(&session.id, "and then?")
        .await
        .unwrap();
    assert_eq!(
        session.input().await,
        RunnerInput::UserMessage("and then?".to_owned())
    );
}

#[tokio::test]
async fn a_note_waits_for_a_message_that_was_queued_behind_the_turn() {
    let mut session = running(PermissionMode::Autonomous).await;
    let web = session.repo("web").await;
    // Queued first, then the repo: the message is delivered after both.
    session
        .manager
        .send_message(&session.id, "next")
        .await
        .unwrap();
    let attached = session.attach(&web, None).await.unwrap();
    emit(&session.io, RunnerOutput::TurnEnded).await;
    let RunnerInput::UserMessage(delivered) = session.input().await else {
        panic!("expected the queued message");
    };
    assert!(
        delivered.starts_with("[Calin] The user attached the repository `web`"),
        "{delivered}"
    );
    assert!(
        delivered.contains(&attached.path) && delivered.ends_with("\n\nnext"),
        "{delivered}"
    );
}

#[tokio::test]
async fn attaching_is_refused_outside_the_states_and_cases_the_contract_allows() {
    let mut session = running(PermissionMode::Ask).await;
    let web = session.repo("web").await;
    let docs = session.repo("docs").await;
    let conflict = |result: Result<SessionRepo>| matches!(result, Err(Error::Conflict(_)));

    // Unknown session, unknown repo, a repo that is already attached.
    let unknown = session
        .manager
        .attach_repo("01ARZ3NDEKTSV4RRFFQ69G5FAV", attach(&web, None));
    assert!(matches!(unknown.await, Err(Error::NotFound(_))));
    let mut nowhere = attach(&web, None);
    nowhere.repo_id = "nope".to_owned();
    let missing = session.manager.attach_repo(&session.id, nowhere).await;
    assert!(matches!(missing, Err(Error::Missing(_))), "{missing:?}");
    let api = session.api.clone();
    assert!(conflict(session.attach(&api, Some(RepoMode::Read)).await));

    // Waiting on a permission request is a state that takes repos.
    emit(&session.io, ask("p1", None)).await;
    waiting_request(&session.manager, &session.id, 0).await;
    session.attach(&web, None).await.unwrap();
    assert!(conflict(session.attach(&web, None).await));
    assert_eq!(session.status().await, SessionStatus::Waiting);

    // Cancelled, and failed: no.
    session.manager.stop(&session.id).await.unwrap();
    assert_eq!(session.status().await, SessionStatus::Cancelled);
    assert!(conflict(session.attach(&docs, None).await));
    session
        .manager
        .send_message(&session.id, "again")
        .await
        .unwrap();
    let (_, io) = within(session.starts.recv()).await.unwrap();
    emit(&io, RunnerOutput::Exited(Exit::Error("boom".to_owned()))).await;
    events_until(
        &session.manager,
        &session.id,
        0,
        is_status(SessionStatus::Failed),
    )
    .await;
    assert!(conflict(session.attach(&docs, None).await));
    assert_eq!(
        session.manager.get(&session.id).await.unwrap().repos.len(),
        2
    );
}

#[tokio::test]
async fn a_repo_without_a_clone_cannot_be_attached_or_started_with() {
    let session = running(PermissionMode::Autonomous).await;
    let home = git_repo(session.clones.path(), "home");
    write_file(
        &home,
        ".calin/repos.yaml",
        "repos:\n  far: { url: git@github.com:you/far.git }\n",
    );
    let settings = Settings {
        home_repo: Some(home.to_str().unwrap().to_owned()),
        ..Settings::default()
    };
    session.manager.update_settings(settings).await.unwrap();
    let repos = session.manager.list_repos().await;
    let far = repos.iter().find(|repo| repo.name == "far").unwrap();
    assert!(!far.available);

    let refused = session.attach(far, None).await;
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    let mut wanted = request("needs far", Some("puppet"));
    wanted.repos = vec![attach(far, None)];
    let refused = session.manager.create_session(wanted).await;
    assert!(matches!(refused, Err(Error::Conflict(_))));
    // The home repo itself attaches like any other clone.
    let attached = session
        .attach(&repos[0], Some(RepoMode::Live))
        .await
        .unwrap();
    assert_eq!(attached.path, home.to_str().unwrap());
}

#[tokio::test]
async fn an_attach_whose_worktree_cannot_be_made_fails_alone_and_the_next_goes_on() {
    let session = running(PermissionMode::Autonomous).await;
    let web = session.repo("web").await;
    let docs = session.repo("docs").await;
    let before = session.head_seq().await;
    // Something that is not a worktree is where web's would go.
    let work_dir = Home::new(session._dir.path()).work_dir(&session.id);
    std::fs::write(work_dir.join("web"), "in the way").unwrap();

    let failed = session.attach(&web, None).await;
    assert!(failed.is_err(), "{failed:?}");
    let docs = session.attach(&docs, None).await.unwrap();
    assert_eq!(docs.name, "docs");
    let events = session.events_after(before).await;
    assert_eq!(labels(&events), ["repo_attached:docs"]);
    assert_eq!(session.status().await, SessionStatus::Running);
}

#[tokio::test]
async fn two_attaches_at_once_are_made_one_after_the_other() {
    let session = running(PermissionMode::Autonomous).await;
    let web = session.repo("web").await;
    let docs = session.repo("docs").await;
    let before = session.head_seq().await;
    let (first, second, again) = tokio::join!(
        session.attach(&web, None),
        session.attach(&docs, None),
        session.attach(&web, None),
    );
    // The same repo twice: one of the two requests wins.
    assert_eq!(first.is_ok() as u8 + again.is_ok() as u8, 1);
    assert!(second.is_ok());
    let events = session.events_after(before).await;
    assert_eq!(events.len(), 2, "{:?}", labels(&events));
    assert_eq!(
        session.manager.get(&session.id).await.unwrap().repos.len(),
        3
    );
}

#[tokio::test]
async fn an_agent_is_started_with_a_token_of_its_own_that_dies_with_it() {
    let mut session = running(PermissionMode::Autonomous).await;
    let token = token_of(&session.params);
    assert!(token.len() >= 32 && token.bytes().all(|b| b.is_ascii_hexdigit()));
    let detail = session.manager.get(&session.id).await.unwrap();
    let lead = detail.agents[0].id.clone();
    let agent = session.manager.agent_for_token(&token).unwrap();
    assert_eq!(
        (agent.session_id.as_str(), agent.agent_id.as_str()),
        (session.id.as_str(), lead.as_str())
    );
    assert_eq!(session.manager.agent_for_token("nope"), None);
    assert_eq!(session.manager.agent_for_token(""), None);

    // The prompt addition names the repos and the tools.
    let prompt = session.params.system_prompt.clone().unwrap();
    assert!(prompt.contains("running inside Calin"), "{prompt}");
    assert!(prompt.contains(&format!(
        "- api (write, primary) at {}",
        session.params.cwd.display()
    )));
    assert!(prompt.contains("`attach_repo`"));

    // The token outlives turns, not the agent.
    session.end_turn().await;
    assert!(session.manager.agent_for_token(&token).is_some());
    session.exit_agent().await;
    assert_eq!(session.manager.agent_for_token(&token), None);

    // The same agent started again has another token.
    session
        .manager
        .send_message(&session.id, "again")
        .await
        .unwrap();
    let (params, io) = within(session.starts.recv()).await.unwrap();
    let second = token_of(&params);
    assert_ne!(second, token);
    assert_eq!(
        session.manager.agent_for_token(&second).unwrap().agent_id,
        lead
    );
    assert_eq!(session.manager.agent_for_token(&token), None);

    // Stopped, failed or shut down: revoked each time.
    session.manager.stop(&session.id).await.unwrap();
    assert_eq!(session.manager.agent_for_token(&second), None);
    drop(io);
    session
        .manager
        .send_message(&session.id, "third")
        .await
        .unwrap();
    let (params, io) = within(session.starts.recv()).await.unwrap();
    let third = token_of(&params);
    emit(&io, RunnerOutput::Exited(Exit::Error("boom".to_owned()))).await;
    events_until(
        &session.manager,
        &session.id,
        0,
        is_status(SessionStatus::Failed),
    )
    .await;
    assert_eq!(session.manager.agent_for_token(&third), None);
    session
        .manager
        .send_message(&session.id, "fourth")
        .await
        .unwrap();
    let (params, _io) = within(session.starts.recv()).await.unwrap();
    let fourth = token_of(&params);
    session.manager.shutdown().await;
    assert_eq!(session.manager.agent_for_token(&fourth), None);
}

#[tokio::test]
async fn without_a_known_address_agents_start_without_tools() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    manager
        .create_session(request("no server", Some("puppet")))
        .await
        .unwrap();
    let (params, _io) = within(starts.recv()).await.unwrap();
    assert_eq!(params.tool_endpoint, None);
    let prompt = params.system_prompt.unwrap();
    assert!(prompt.contains("No repository is attached to this session yet."));
    assert!(!prompt.contains("attach_repo"));
}

#[tokio::test]
async fn an_agent_attaches_a_repo_through_its_token_and_gets_the_path() {
    let mut session = running(PermissionMode::Autonomous).await;
    let web = session.repo("web").await;
    session.repo("docs").await;
    let agent = session
        .manager
        .agent_for_token(&token_of(&session.params))
        .unwrap();
    let before = session.head_seq().await;

    // What it can see: every repo, with how it takes part in the session.
    let seen = session.manager.agent_repos(&agent).await.unwrap();
    let summary: Vec<(&str, Option<RepoMode>)> = seen
        .iter()
        .map(|entry| {
            (
                entry.repo.name.as_str(),
                entry.attached.as_ref().map(|a| a.mode),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("api", Some(RepoMode::Write)),
            ("docs", None),
            ("web", None)
        ]
    );

    let attached = within(session.manager.agent_attach_repo(&agent, "web", None))
        .await
        .unwrap();
    assert_eq!(attached.repo_id, web.id);
    assert_eq!(attached.mode, RepoMode::Write);
    assert!(Path::new(&attached.path).join("README.md").is_file());
    let events = session.events_after(before).await;
    assert_eq!(labels(&events), ["repo_attached:web"]);
    // The agent asked: the event is its own, caused by the turn it is in.
    assert_eq!(events[0].agent_id.as_deref(), Some(agent.agent_id.as_str()));
    let prompt = events_until(&session.manager, &session.id, 0, |event| {
        matches!(event.body, EventBody::UserMessage { .. })
    })
    .await;
    assert_eq!(
        events[0].parent_id.as_ref(),
        Some(&prompt.last().unwrap().id)
    );

    // Refusals the agent can act on.
    let attach = |name: &'static str, mode| session.manager.agent_attach_repo(&agent, name, mode);
    let unknown = attach("nope", None).await;
    assert!(
        matches!(&unknown, Err(Error::Missing(message)) if message.contains("nope")),
        "{unknown:?}"
    );
    let again = attach("web", None).await;
    assert!(
        matches!(&again, Err(Error::Conflict(message)) if message.contains(&attached.path)),
        "{again:?}"
    );
    let live = attach("docs", Some(RepoMode::Live)).await;
    assert!(matches!(live, Err(Error::Invalid(_))), "{live:?}");
    let read = attach("docs", Some(RepoMode::Read)).await.unwrap();
    assert_eq!(read.mode, RepoMode::Read);

    // The tool's result told the agent: no note with the next message.
    session.end_turn().await;
    session
        .manager
        .send_message(&session.id, "thanks")
        .await
        .unwrap();
    assert_eq!(
        session.input().await,
        RunnerInput::UserMessage("thanks".to_owned())
    );
}

#[tokio::test]
async fn in_accept_edits_the_policy_answers_for_the_sessions_repos_and_opens_each_once() {
    let mut session = running(PermissionMode::AcceptEdits).await;
    let web = session.repo("web").await;
    let docs = session.repo("docs").await;
    let web = session.attach(&web, None).await.unwrap();
    let docs = session.attach(&docs, Some(RepoMode::Read)).await.unwrap();
    let before = session.head_seq().await;
    let inside = |repo: &SessionRepo, rel: &str| vec![format!("{}/{rel}", repo.path)];
    let allow = PermissionDecision::Allow;

    // The first write in a repo attached after the start opens it.
    emit(
        &session.io,
        file_ask("w1", true, &inside(&web, "src/new.rs")),
    )
    .await;
    assert_eq!(
        session.input().await,
        decision("w1", allow, vec![grant(&web)])
    );
    // After that the policy still answers, with nothing left to open.
    emit(
        &session.io,
        file_ask("w2", true, &inside(&web, "README.md")),
    )
    .await;
    assert_eq!(session.input().await, decision("w2", allow, Vec::new()));
    // A read-only repo is opened for reading only, by its first read.
    emit(
        &session.io,
        file_ask("r1", false, &inside(&docs, "README.md")),
    )
    .await;
    let read_only = DirGrant {
        dir: PathBuf::from(&docs.path),
        write: false,
    };
    assert_eq!(
        session.input().await,
        decision("r1", allow, vec![read_only])
    );
    // Writing there is refused, with words for the agent.
    emit(
        &session.io,
        file_ask("w3", true, &inside(&docs, "README.md")),
    )
    .await;
    let RunnerInput::PermissionDecision {
        request_id,
        decision: refused,
        message,
        grants,
    } = session.input().await
    else {
        panic!("expected a decision");
    };
    assert_eq!(
        (request_id.as_str(), refused),
        ("w3", PermissionDecision::Deny)
    );
    assert!(
        message.as_deref().unwrap().contains("read-only"),
        "{message:?}"
    );
    assert!(grants.is_empty());
    // The repo the agent was started in needs no opening.
    let own = vec![
        session
            .params
            .cwd
            .join("src/lib.rs")
            .to_str()
            .unwrap()
            .to_owned(),
    ];
    emit(&session.io, file_ask("w4", true, &own)).await;
    assert_eq!(session.input().await, decision("w4", allow, Vec::new()));
    // Calin's own tools are allowed in every mode.
    let own_tool = PermissionAsk {
        request: PermissionRequest {
            id: "t1".to_owned(),
            tool_name: "mcp__calin__attach_repo".to_owned(),
            tool_kind: ToolKind::Other,
            title: "attach_repo".to_owned(),
            input: serde_json::json!({ "name": "web" }),
            item_id: None,
        },
        access: ToolAccess::Orchestrator,
        subagent: None,
    };
    emit(&session.io, RunnerOutput::PermissionRequest(own_tool)).await;
    assert_eq!(session.input().await, decision("t1", allow, Vec::new()));

    // Every one of these is on record as the policy's decision.
    let events = session.events_after(before).await;
    assert_eq!(
        labels(&events),
        [
            "permission_requested:Edit",
            "permission_resolved:allow:policy",
            "permission_requested:Edit",
            "permission_resolved:allow:policy",
            "permission_requested:Read",
            "permission_resolved:allow:policy",
            "permission_requested:Edit",
            "permission_resolved:deny:policy",
            "permission_requested:Edit",
            "permission_resolved:allow:policy",
            "permission_requested:mcp__calin__attach_repo",
            "permission_resolved:allow:policy",
        ]
    );
    assert!(matches!(
        &events[7].body,
        EventBody::PermissionResolved { message: Some(message), .. } if message.contains("docs")
    ));
    assert_eq!(events[1].parent_id.as_ref(), Some(&events[0].id));
    assert_eq!(session.status().await, SessionStatus::Running);

    // Outside every repo, and for a command, the user is still asked.
    emit(
        &session.io,
        file_ask("o1", true, &["/etc/hosts".to_owned()]),
    )
    .await;
    let (_, _, request) = waiting_request(&session.manager, &session.id, before).await;
    assert_eq!(request.id, "o1");
    emit(&session.io, ask("b1", None)).await;
    // On record before o1 is answered: the driver takes output and commands
    // in either order, and b1 is what keeps the session waiting.
    events_until(&session.manager, &session.id, before, |event| {
        matches!(&event.body, EventBody::PermissionRequested { request } if request.id == "b1")
    })
    .await;
    session
        .manager
        .resolve_permission(&session.id, "o1", PermissionDecision::Deny, None)
        .await
        .unwrap();
    assert!(
        matches!(session.input().await, RunnerInput::PermissionDecision { request_id, .. } if request_id == "o1")
    );
    assert_eq!(session.status().await, SessionStatus::Waiting);
}

#[tokio::test]
async fn in_ask_mode_reads_are_free_writes_wait_and_the_users_allow_opens_the_repo() {
    let mut session = running(PermissionMode::Ask).await;
    let web = session.repo("web").await;
    let docs = session.repo("docs").await;
    let web = session.attach(&web, None).await.unwrap();
    let docs = session.attach(&docs, Some(RepoMode::Read)).await.unwrap();
    let before = session.head_seq().await;
    let inside = |repo: &SessionRepo, rel: &str| vec![format!("{}/{rel}", repo.path)];

    // A write inside a write repo waits for the user, as every write does.
    emit(&session.io, file_ask("w1", true, &inside(&web, "a.txt"))).await;
    let (_, _, request) = waiting_request(&session.manager, &session.id, before).await;
    assert_eq!(request.id, "w1");
    session
        .manager
        .resolve_permission(&session.id, "w1", PermissionDecision::Allow, None)
        .await
        .unwrap();
    // The user's allow is the first one for that repo, so it opens it.
    assert_eq!(
        session.input().await,
        decision("w1", PermissionDecision::Allow, vec![grant(&web)])
    );
    // A read anywhere in the session's repos is allowed without asking.
    emit(&session.io, file_ask("r1", false, &inside(&web, "a.txt"))).await;
    assert_eq!(
        session.input().await,
        decision("r1", PermissionDecision::Allow, Vec::new())
    );
    emit(
        &session.io,
        file_ask("r2", false, &inside(&docs, "README.md")),
    )
    .await;
    assert_eq!(
        session.input().await,
        decision("r2", PermissionDecision::Allow, vec![grant(&docs)])
    );
    // A write to the read-only repo never reaches the user.
    emit(
        &session.io,
        file_ask("w2", true, &inside(&docs, "README.md")),
    )
    .await;
    assert!(matches!(
        session.input().await,
        RunnerInput::PermissionDecision {
            decision: PermissionDecision::Deny,
            message: Some(_),
            ..
        }
    ));
    assert_eq!(session.status().await, SessionStatus::Running);
    // A user's denial opens nothing.
    let api = session.repo("extra").await;
    let extra = session.attach(&api, None).await.unwrap();
    let after = session.head_seq().await;
    emit(&session.io, file_ask("w3", true, &inside(&extra, "a.txt"))).await;
    waiting_request(&session.manager, &session.id, after).await;
    session
        .manager
        .resolve_permission(
            &session.id,
            "w3",
            PermissionDecision::Deny,
            Some("no".to_owned()),
        )
        .await
        .unwrap();
    assert_eq!(
        session.input().await,
        RunnerInput::PermissionDecision {
            request_id: "w3".to_owned(),
            decision: PermissionDecision::Deny,
            message: Some("no".to_owned()),
            grants: Vec::new(),
        }
    );
}

#[tokio::test]
async fn in_autonomous_mode_only_a_write_to_a_read_only_repo_is_refused() {
    let mut session = running(PermissionMode::Autonomous).await;
    let docs = session.repo("docs").await;
    let docs = session.attach(&docs, Some(RepoMode::Read)).await.unwrap();
    emit(
        &session.io,
        file_ask("w1", true, &[format!("{}/x.md", docs.path)]),
    )
    .await;
    assert!(matches!(
        session.input().await,
        RunnerInput::PermissionDecision {
            decision: PermissionDecision::Deny,
            message: Some(_),
            ..
        }
    ));
    emit(
        &session.io,
        file_ask("w2", true, &["/etc/hosts".to_owned()]),
    )
    .await;
    assert_eq!(
        session.input().await,
        decision("w2", PermissionDecision::Allow, Vec::new())
    );
    emit(&session.io, ask("b1", None)).await;
    assert_eq!(
        session.input().await,
        decision("b1", PermissionDecision::Allow, Vec::new())
    );
}

#[tokio::test]
async fn a_restarted_agent_is_given_the_repos_attached_since_except_the_read_only_ones() {
    let mut session = running(PermissionMode::AcceptEdits).await;
    let web = session.repo("web").await;
    let docs = session.repo("docs").await;
    let web = session.attach(&web, None).await.unwrap();
    let docs = session.attach(&docs, Some(RepoMode::Read)).await.unwrap();
    assert!(session.params.add_dirs.is_empty());
    session.end_turn().await;
    session.exit_agent().await;
    session
        .manager
        .send_message(&session.id, "again")
        .await
        .unwrap();
    let (params, io) = within(session.starts.recv()).await.unwrap();
    session.io = io;

    // Same working directory; the write repo is a directory of its own now.
    assert_eq!(params.cwd, session.params.cwd);
    assert_eq!(params.add_dirs, [PathBuf::from(&web.path)]);
    let prompt = params.system_prompt.unwrap();
    assert!(
        prompt.contains(&format!("- web (write) at {}", web.path)),
        "{prompt}"
    );
    assert!(
        prompt.contains(&format!("- docs (read) at {}", docs.path)),
        "{prompt}"
    );
    // The agent starts over knowing its repos: the note is not sent as well.
    assert_eq!(params.prompt, "again");

    // Nothing to open for the repo it was started with; the read-only one
    // is still opened by its first read.
    emit(
        &session.io,
        file_ask("w1", true, &[format!("{}/a.txt", web.path)]),
    )
    .await;
    assert_eq!(
        session.input().await,
        decision("w1", PermissionDecision::Allow, Vec::new())
    );
    emit(
        &session.io,
        file_ask("r1", false, &[format!("{}/README.md", docs.path)]),
    )
    .await;
    assert_eq!(
        session.input().await,
        decision("r1", PermissionDecision::Allow, vec![grant(&docs)])
    );
}

#[tokio::test]
async fn a_session_started_without_repos_keeps_its_working_directory_when_one_is_attached() {
    let Fixture {
        manager,
        mut starts,
        dir,
    } = fixture().await;
    let clones = TempDir::new().unwrap();
    let web = registered(&manager, clones.path(), "web").await;
    let id = manager
        .create_session(request("no repos", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (params, io) = within(starts.recv()).await.unwrap();
    let work_dir = Home::new(dir.path()).work_dir(&id);
    assert_eq!(params.cwd, work_dir);
    let attached = within(manager.attach_repo(&id, attach(&web, None)))
        .await
        .unwrap();
    assert!(!attached.primary);
    assert_eq!(
        Path::new(&attached.path),
        work_dir.join("web").canonicalize().unwrap()
    );

    // A restart must find the conversation where it was left.
    emit(&io, RunnerOutput::TurnEnded).await;
    events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    exit(&manager, &id, &io).await;
    manager.send_message(&id, "again").await.unwrap();
    let (params, _io) = within(starts.recv()).await.unwrap();
    assert_eq!(params.cwd, work_dir);
    assert_eq!(params.add_dirs, [PathBuf::from(&attached.path)]);
    assert_eq!(manager.terminal_dir(&id, None).await.unwrap(), work_dir);
}

#[tokio::test]
async fn an_agent_that_works_in_a_read_only_repo_asks_about_every_edit() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let clones = TempDir::new().unwrap();
    let docs = registered(&manager, clones.path(), "docs").await;
    let api = registered(&manager, clones.path(), "api").await;
    let mut wanted = request_in(PermissionMode::AcceptEdits, "read it", Some("puppet"));
    wanted.repos = vec![attach(&docs, Some(RepoMode::Read)), attach(&api, None)];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let (params, mut io) = within(starts.recv()).await.unwrap();
    // Started where it would otherwise edit without asking: it must ask.
    assert_eq!(params.cwd, PathBuf::from(&docs.path));
    assert_eq!(params.permission_mode, PermissionMode::Ask);
    let detail = manager.get(&id).await.unwrap();
    assert_eq!(detail.session.permission_mode, PermissionMode::AcceptEdits);
    assert_eq!(params.add_dirs, [PathBuf::from(&detail.repos[1].path)]);

    // The session still accepts edits where they are allowed, and refuses
    // them in the read-only repo.
    emit(
        &io,
        file_ask("w1", true, &[format!("{}/a.rs", detail.repos[1].path)]),
    )
    .await;
    assert_eq!(
        within(io.inputs.recv()).await,
        Some(decision("w1", PermissionDecision::Allow, Vec::new()))
    );
    emit(
        &io,
        file_ask("w2", true, &[format!("{}/README.md", docs.path)]),
    )
    .await;
    assert!(matches!(
        within(io.inputs.recv()).await,
        Some(RunnerInput::PermissionDecision {
            decision: PermissionDecision::Deny,
            message: Some(_),
            ..
        })
    ));
    emit(
        &io,
        file_ask("r1", false, &[format!("{}/README.md", docs.path)]),
    )
    .await;
    assert_eq!(
        within(io.inputs.recv()).await,
        Some(decision("r1", PermissionDecision::Allow, Vec::new()))
    );
}

#[tokio::test]
async fn a_repo_is_attached_in_review_after_the_agent_has_exited() {
    let mut session = running(PermissionMode::Autonomous).await;
    let web = session.repo("web").await;
    session.end_turn().await;
    session.exit_agent().await;
    let attached = session.attach(&web, None).await.unwrap();
    assert!(Path::new(&attached.path).join("README.md").is_file());
    assert_eq!(session.status().await, SessionStatus::Review);
    // No agent to tell: the next start names the repo in its prompt.
    session
        .manager
        .send_message(&session.id, "use it")
        .await
        .unwrap();
    let (params, _io) = within(session.starts.recv()).await.unwrap();
    assert_eq!(params.prompt, "use it");
    assert_eq!(params.add_dirs, [PathBuf::from(&attached.path)]);
    assert!(params.system_prompt.unwrap().contains("- web (write) at"));
}

/// A manager whose home repo declares `api` with `setup`, registered by hand
/// so that it has a clone, and with a setup limit short enough to hit.
async fn with_setup(setup: &str) -> (Fixture, TempDir, Repo) {
    with_setup_limit(setup, Duration::from_millis(400)).await
}

async fn with_setup_limit(setup: &str, setup_limit: Duration) -> (Fixture, TempDir, Repo) {
    let dir = TempDir::new().unwrap();
    let config = ManagerConfig {
        setup_limit,
        ..config()
    };
    let (manager, starts) = manager_with(&dir, false, config).await;
    let clones = TempDir::new().unwrap();
    let home = git_repo(clones.path(), "home");
    let yaml = format!("repos:\n  api: {{ setup: {setup:?} }}\n  plain: {{}}\n");
    write_file(&home, ".calin/repos.yaml", &yaml);
    registered(&manager, clones.path(), "api").await;
    registered(&manager, clones.path(), "plain").await;
    let settings = Settings {
        home_repo: Some(home.to_str().unwrap().to_owned()),
        ..Settings::default()
    };
    manager.update_settings(settings).await.unwrap();
    let repos = manager.list_repos().await;
    let api = repos
        .iter()
        .find(|repo| repo.name == "api")
        .unwrap()
        .clone();
    assert_eq!(api.setup.as_deref(), Some(setup));
    let fixture = Fixture {
        dir,
        manager,
        starts,
    };
    (fixture, clones, api)
}

fn setup_items(events: &[Event]) -> Vec<&Item> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ItemStarted { item } | EventBody::ItemCompleted { item } => Some(item),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_setup_command_runs_in_a_new_worktree_before_the_agent_starts() {
    let command = "echo fetching; echo ready > setup.txt; echo warned >&2";
    let (fixture, _clones, api) = with_setup(command).await;
    let Fixture {
        manager,
        mut starts,
        ..
    } = fixture;
    let mut wanted = request("with setup", Some("puppet"));
    wanted.repos = vec![attach(&api, None)];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let (params, _io) = within(starts.recv()).await.unwrap();
    // The agent starts in a worktree that is set up.
    assert_eq!(
        std::fs::read_to_string(params.cwd.join("setup.txt")).unwrap(),
        "ready\n"
    );

    let events = events_until(&manager, &id, 0, |event| {
        matches!(event.body, EventBody::AgentSpawned { .. })
    })
    .await;
    assert_eq!(
        labels(&events),
        [
            "session_status:queued",
            "user_message:with setup",
            "session_status:preparing",
            "repo_attached:api",
            "item_started",
            "item_completed",
            "session_status:running",
            "agent_spawned",
        ]
    );
    // A tool call nobody but the orchestrator made, caused by the prompt.
    for event in &events[4..6] {
        assert_eq!(event.agent_id, None);
        assert_eq!(event.parent_id.as_ref(), Some(&events[1].id));
    }
    let [started, completed] = setup_items(&events)[..] else {
        panic!("expected the setup item twice");
    };
    assert_eq!(started.id, completed.id);
    assert_eq!(started.status, ItemStatus::Running);
    assert_eq!(
        completed.content,
        ItemContent::Tool {
            name: "setup".to_owned(),
            tool_kind: ToolKind::Execute,
            title: command.to_owned(),
            input: serde_json::json!({ "command": command, "repo": "api" }),
            output: Some("fetching\nwarned\n".to_owned()),
            locations: Vec::new(),
        }
    );
    assert_eq!(completed.status, ItemStatus::Completed);
}

#[tokio::test]
async fn a_failing_setup_is_an_error_in_the_transcript_and_the_session_goes_on() {
    let (fixture, _clones, api) = with_setup("echo no network >&2; exit 7").await;
    let Fixture {
        manager,
        mut starts,
        ..
    } = fixture;
    let mut wanted = request("broken setup", Some("puppet"));
    wanted.repos = vec![attach(&api, None)];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let (_params, io) = within(starts.recv()).await.unwrap();
    emit(&io, RunnerOutput::TurnEnded).await;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&events)[3..8],
        [
            "repo_attached:api",
            "item_started",
            "item_completed",
            "error:the setup command of api failed (exit status: 7)",
            "session_status:running",
        ]
    );
    let completed = setup_items(&events)[1];
    assert_eq!(completed.status, ItemStatus::Failed);
    assert!(matches!(
        &completed.content,
        ItemContent::Tool { output: Some(output), .. } if output == "no network\n"
    ));
    assert_eq!(events[6].agent_id, None);
}

#[tokio::test]
async fn stopping_during_a_setup_cancels_it_and_keeps_its_end_on_record() {
    let (fixture, _clones, api) = with_setup_limit("sleep 30", TIMEOUT).await;
    let Fixture {
        manager,
        mut starts,
        ..
    } = fixture;
    let mut wanted = request("stopped setup", Some("puppet"));
    wanted.repos = vec![attach(&api, None)];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let before = events_until(&manager, &id, 0, |event| {
        matches!(event.body, EventBody::ItemStarted { .. })
    })
    .await;
    within(manager.stop(&id)).await.unwrap();
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Cancelled)).await;
    assert_eq!(
        labels(&events[before.len()..]),
        [
            "item_completed",
            "error:the setup command of api was cancelled",
            "session_status:cancelled",
        ]
    );
    assert_eq!(setup_items(&events)[1].status, ItemStatus::Failed);
    // No agent was ever started.
    assert!(starts.try_recv().is_err());
}

#[tokio::test]
async fn a_setup_that_runs_past_the_limit_is_stopped() {
    let (fixture, _clones, api) = with_setup("echo slow; sleep 30").await;
    let Fixture {
        manager,
        mut starts,
        ..
    } = fixture;
    let mut wanted = request("slow setup", Some("puppet"));
    wanted.repos = vec![attach(&api, None)];
    let started = std::time::Instant::now();
    let id = manager.create_session(wanted).await.unwrap().session.id;
    within(starts.recv()).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(15));
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Running)).await;
    assert_eq!(
        labels(&events)[5..7],
        [
            "item_completed",
            "error:the setup command of api was stopped: it ran longer than 400ms",
        ]
    );
    let completed = setup_items(&events)[1];
    assert_eq!(completed.status, ItemStatus::Failed);
    assert!(matches!(
        &completed.content,
        ItemContent::Tool { output: Some(output), .. } if output == "slow\n"
    ));
}

#[tokio::test]
async fn an_attach_mid_session_returns_after_its_setup_and_only_a_worktree_gets_one() {
    let command = "pwd > where.txt";
    let (fixture, _clones, api) = with_setup(command).await;
    let Fixture {
        manager,
        mut starts,
        ..
    } = fixture;
    let repos = manager.list_repos().await;
    let plain = repos.iter().find(|repo| repo.name == "plain").unwrap();
    let home = &repos[0];
    let mut wanted = request("start plain", Some("puppet"));
    wanted.repos = vec![attach(plain, None)];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let (_params, _io) = within(starts.recv()).await.unwrap();
    let before = manager.get(&id).await.unwrap().session.head_seq;

    let attached = within(manager.attach_repo(&id, attach(&api, None)))
        .await
        .unwrap();
    // Ready means set up: the command has run where the agent will work.
    let ran_in = std::fs::read_to_string(Path::new(&attached.path).join("where.txt")).unwrap();
    assert_eq!(ran_in.trim(), attached.path);
    let events = manager
        .store()
        .subscribe(&id, before)
        .await
        .unwrap()
        .backlog;
    assert_eq!(
        labels(&events),
        ["repo_attached:api", "item_started", "item_completed"]
    );
    assert!(events.iter().all(|event| event.agent_id.is_none()));

    // The clone itself is the user's: nothing is run in it.
    let before = manager.get(&id).await.unwrap().session.head_seq;
    within(manager.attach_repo(&id, attach(home, Some(RepoMode::Live))))
        .await
        .unwrap();
    let events = manager
        .store()
        .subscribe(&id, before)
        .await
        .unwrap()
        .backlog;
    assert_eq!(labels(&events).len(), 1);
    assert!(!Path::new(&api.path).join("where.txt").exists());
}
