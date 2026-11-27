//! Changes an agent made through its shell: found at the end of its turn by
//! comparing each repo's change list with the one Calin last knew.

use super::*;
use crate::manager::unreported::MAX_CHANGES_PER_TURN;

/// A session on the puppet runner, its agent started and its first turn running.
struct Shell {
    manager: SessionManager,
    _dir: TempDir,
    clones: TempDir,
    id: String,
    io: AgentIo,
    lead: String,
}

/// A clone `name` with `files` committed on top of its first commit.
async fn clone_with(
    manager: &SessionManager,
    parent: &Path,
    name: &str,
    files: &[(&str, &str)],
) -> Repo {
    let repo = registered(manager, parent, name).await;
    if !files.is_empty() {
        let dir = Path::new(&repo.path);
        for (path, content) in files {
            write_file(dir, path, content);
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "files"]);
    }
    repo
}

/// Starts a session on `repos`, made by `make` from the manager and a
/// folder for clones, and waits for its agent.
async fn shell_with<F, Fut>(dry_run: bool, make: F) -> Shell
where
    F: FnOnce(SessionManager, PathBuf) -> Fut,
    Fut: Future<Output = Vec<AttachRepoRequest>>,
{
    let dir = TempDir::new().unwrap();
    let (manager, mut starts) = manager_in(&dir, dry_run).await;
    let clones = TempDir::new().unwrap();
    let repos = make(manager.clone(), clones.path().to_owned()).await;
    let mut wanted = request("work", Some("puppet"));
    wanted.repos = repos;
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let (_params, io) = within(starts.recv()).await.unwrap();
    let spawned = events_until(&manager, &id, 0, |event| label(event) == "agent_spawned").await;
    let lead = lead_id(&spawned);
    Shell {
        manager,
        _dir: dir,
        clones,
        id,
        io,
        lead,
    }
}

/// A session with one `write` repo `api` whose base has `files`.
async fn shell(files: &'static [(&'static str, &'static str)]) -> Shell {
    shell_with(false, |manager, clones| async move {
        let api = clone_with(&manager, &clones, "api", files).await;
        vec![attach(&api, None)]
    })
    .await
}

impl Shell {
    async fn repo(&self, name: &str) -> SessionRepo {
        let repos = self.manager.get(&self.id).await.unwrap().repos;
        repos.into_iter().find(|repo| repo.name == name).unwrap()
    }

    async fn dir(&self, name: &str) -> PathBuf {
        PathBuf::from(self.repo(name).await.path)
    }

    async fn head(&self) -> u64 {
        self.manager.get(&self.id).await.unwrap().session.head_seq
    }

    /// Ends the running turn; returns its events from the agent's idle status
    /// to the session's review.
    async fn end_turn(&self) -> Vec<Event> {
        let after = self.head().await;
        emit(&self.io, RunnerOutput::TurnEnded).await;
        let events = events_until(
            &self.manager,
            &self.id,
            after,
            is_status(SessionStatus::Review),
        )
        .await;
        let idle = events
            .iter()
            .position(|event| label(event) == "agent_status:idle")
            .expect("a turn ends with the agent idle");
        events[idle..].to_vec()
    }

    /// Sends a message and waits until the agent has it: its turn has begun.
    async fn next_turn(&mut self) {
        self.manager.send_message(&self.id, "more").await.unwrap();
        match within(self.io.inputs.recv()).await {
            Some(RunnerInput::UserMessage(_)) => {}
            other => panic!("expected a message for the agent, got {other:?}"),
        }
    }

    /// Reports a completed edit of `path` in `repo` by a tool call, and
    /// waits until it is on record.
    async fn tool_edit(&self, id: &str, repo: &str, path: &str) {
        let after = self.head().await;
        let location = self.dir(repo).await.join(path);
        let item = tool(
            id,
            ToolKind::Edit,
            ItemStatus::Completed,
            &[location.to_str().unwrap()],
        );
        emit(
            &self.io,
            RunnerOutput::Event(EventBody::ItemCompleted { item }),
        )
        .await;
        events_until(&self.manager, &self.id, after, |event| {
            matches!(event.body, EventBody::FileChanged { .. })
        })
        .await;
    }
}

/// The `file_changed` events among `events`, as (repo id, path, op).
fn changes(events: &[Event]) -> Vec<(Option<String>, String, FileOp)> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::FileChanged {
                session_repo_id,
                path,
                op,
            } => Some((session_repo_id.clone(), path.clone(), *op)),
            _ => None,
        })
        .collect()
}

fn change(repo: &SessionRepo, path: &str, op: FileOp) -> (Option<String>, String, FileOp) {
    (Some(repo.id.clone()), path.to_owned(), op)
}

#[tokio::test]
async fn a_shell_edit_of_a_tracked_file_is_reported_once_by_the_lead_caused_by_its_idle() {
    let s = shell(&[("src/lib.rs", "fn a() {}\n")]).await;
    let api = s.repo("api").await;
    write_file(Path::new(&api.path), "src/lib.rs", "fn a() {}\nfn b() {}\n");
    let events = s.end_turn().await;

    assert_eq!(
        labels(&events),
        [
            "agent_status:idle",
            "file_changed:edit",
            "session_status:review"
        ]
    );
    let (idle, changed) = (&events[0], &events[1]);
    assert_eq!(changes(&events), [change(&api, "src/lib.rs", FileOp::Edit)]);
    assert_eq!(changed.agent_id.as_deref(), Some(s.lead.as_str()));
    assert_eq!(changed.parent_id.as_ref(), Some(&idle.id));
    assert_eq!(idle.agent_id.as_deref(), Some(s.lead.as_str()));
}

#[tokio::test]
async fn creating_deleting_and_renaming_are_told_apart() {
    let s = shell(&[("a.txt", "a\n"), ("b.txt", "b\nb\nb\n")]).await;
    let api = s.repo("api").await;
    let dir = Path::new(&api.path);
    write_file(dir, "new.txt", "new\n");
    std::fs::remove_file(dir.join("a.txt")).unwrap();
    git(dir, &["mv", "b.txt", "c.txt"]);
    let events = s.end_turn().await;
    assert_eq!(
        changes(&events),
        [
            change(&api, "a.txt", FileOp::Delete),
            change(&api, "c.txt", FileOp::Rename),
            change(&api, "new.txt", FileOp::Create),
        ]
    );
}

#[tokio::test]
async fn an_edit_that_keeps_the_line_counts_is_found_and_a_quiet_turn_reports_nothing() {
    let mut s = shell(&[("conf.txt", "x = 1\n")]).await;
    let api = s.repo("api").await;
    let dir = Path::new(&api.path);
    write_file(dir, "conf.txt", "x = 2\n");
    write_file(dir, "notes.txt", "one\n");
    let first = s.end_turn().await;
    assert_eq!(
        changes(&first),
        [
            change(&api, "conf.txt", FileOp::Edit),
            change(&api, "notes.txt", FileOp::Create),
        ]
    );

    // Same size, same counts against the base, other content.
    s.next_turn().await;
    write_file(dir, "conf.txt", "x = 3\n");
    write_file(dir, "notes.txt", "two\n");
    let second = s.end_turn().await;
    assert_eq!(
        changes(&second),
        [
            change(&api, "conf.txt", FileOp::Edit),
            change(&api, "notes.txt", FileOp::Edit),
        ]
    );

    // Nothing changed: nothing to say. Staging is not a change of a file.
    s.next_turn().await;
    git(dir, &["add", "-A"]);
    assert_eq!(
        labels(&s.end_turn().await),
        ["agent_status:idle", "session_status:review"]
    );

    // Another file: only that one.
    s.next_turn().await;
    write_file(dir, "other.txt", "other\n");
    assert_eq!(
        changes(&s.end_turn().await),
        [change(&api, "other.txt", FileOp::Create)]
    );
}

#[tokio::test]
async fn a_file_a_tool_call_reported_is_reported_again_only_if_the_shell_changed_it_after() {
    let s = shell(&[]).await;
    let api = s.repo("api").await;
    let dir = Path::new(&api.path);
    write_file(dir, "kept.txt", "by the tool\n");
    s.tool_edit("e1", "api", "kept.txt").await;
    write_file(dir, "again.txt", "by the tool\n");
    s.tool_edit("e2", "api", "again.txt").await;
    // The shell, after the call: same line count, other bytes.
    write_file(dir, "again.txt", "by the shell\n");
    let events = s.end_turn().await;
    assert_eq!(
        labels(&events),
        [
            "agent_status:idle",
            "file_changed:create",
            "session_status:review"
        ]
    );
    assert_eq!(
        changes(&events),
        [change(&api, "again.txt", FileOp::Create)]
    );
    assert_eq!(events[1].parent_id.as_ref(), Some(&events[0].id));
    assert_eq!(events[1].agent_id.as_deref(), Some(s.lead.as_str()));
}

#[tokio::test]
async fn reverting_a_file_to_the_base_is_an_edit_and_removing_a_new_one_a_delete() {
    let mut s = shell(&[]).await;
    let api = s.repo("api").await;
    let dir = Path::new(&api.path);
    write_file(dir, "README.md", "# changed\n");
    write_file(dir, "scratch.txt", "scratch\n");
    assert_eq!(
        changes(&s.end_turn().await),
        [
            change(&api, "README.md", FileOp::Edit),
            change(&api, "scratch.txt", FileOp::Create),
        ]
    );
    s.next_turn().await;
    git(dir, &["checkout", "--", "README.md"]);
    std::fs::remove_file(dir.join("scratch.txt")).unwrap();
    assert_eq!(
        changes(&s.end_turn().await),
        [
            change(&api, "README.md", FileOp::Edit),
            change(&api, "scratch.txt", FileOp::Delete),
        ]
    );
}

#[tokio::test]
async fn the_users_saves_are_never_the_agents() {
    let mut s = shell(&[]).await;
    let api = s.repo("api").await;
    let save = |text: &str, hash: Option<String>| WriteFileRequest {
        text: text.to_owned(),
        expected_hash: hash,
    };
    // Mid-turn: the user's save is recorded as the user's, and the turn's
    // end does not see it again.
    let readme = s
        .manager
        .read_file(&s.id, &api.id, "README.md")
        .await
        .unwrap();
    s.manager
        .write_file(
            &s.id,
            &api.id,
            "README.md",
            save("# mine\n", Some(readme.hash)),
        )
        .await
        .unwrap();
    s.manager
        .write_file(&s.id, &api.id, "mine.txt", save("mine\n", None))
        .await
        .unwrap();
    let events = s.end_turn().await;
    assert_eq!(
        labels(&events),
        ["agent_status:idle", "session_status:review"]
    );

    // Between turns, likewise.
    let readme = s
        .manager
        .read_file(&s.id, &api.id, "README.md")
        .await
        .unwrap();
    s.manager
        .write_file(
            &s.id,
            &api.id,
            "README.md",
            save("# mine again\n", Some(readme.hash)),
        )
        .await
        .unwrap();
    s.next_turn().await;
    write_file(Path::new(&api.path), "theirs.txt", "theirs\n");
    let events = s.end_turn().await;
    assert_eq!(
        changes(&events),
        [change(&api, "theirs.txt", FileOp::Create)]
    );
    let all = s.manager.store().subscribe(&s.id, 0).await.unwrap().backlog;
    let by_user: Vec<_> = all
        .iter()
        .filter(|event| matches!(event.body, EventBody::FileChanged { .. }))
        .filter(|event| event.agent_id.is_none())
        .collect();
    assert_eq!(by_user.len(), 3);
}

#[tokio::test]
async fn each_repo_is_tracked_on_its_own_and_read_repos_are_not() {
    let s = shell_with(false, |manager, clones| async move {
        let api = clone_with(&manager, &clones, "api", &[]).await;
        let web = clone_with(&manager, &clones, "web", &[]).await;
        let docs = clone_with(&manager, &clones, "docs", &[]).await;
        // Already there when the agent starts: not its doing.
        write_file(Path::new(&web.path), "before.txt", "before\n");
        vec![
            attach(&api, None),
            attach(&web, Some(RepoMode::Live)),
            attach(&docs, Some(RepoMode::Read)),
        ]
    })
    .await;
    let (api, web, docs) = (
        s.repo("api").await,
        s.repo("web").await,
        s.repo("docs").await,
    );
    write_file(Path::new(&api.path), "README.md", "# api\n");
    write_file(Path::new(&web.path), "README.md", "# web\n");
    write_file(Path::new(&docs.path), "README.md", "# docs\n");
    let events = s.end_turn().await;
    assert_eq!(
        changes(&events),
        [
            change(&api, "README.md", FileOp::Edit),
            change(&web, "README.md", FileOp::Edit),
        ]
    );
}

#[tokio::test]
async fn a_repo_attached_mid_session_is_tracked_from_its_attach() {
    let s = shell(&[]).await;
    let web = registered(&s.manager, s.clones.path(), "web").await;
    write_file(Path::new(&web.path), "dirty.txt", "before the attach\n");
    let attached = within(
        s.manager
            .attach_repo(&s.id, attach(&web, Some(RepoMode::Live))),
    )
    .await
    .unwrap();
    write_file(Path::new(&attached.path), "later.txt", "after the attach\n");
    let events = s.end_turn().await;
    assert_eq!(
        changes(&events),
        [change(&attached, "later.txt", FileOp::Create)]
    );
}

#[tokio::test]
async fn a_turn_that_changes_too_many_files_reports_the_first_ones_and_says_how_many_more() {
    let s = shell(&[]).await;
    let api = s.repo("api").await;
    let extra = 3;
    for n in 0..MAX_CHANGES_PER_TURN + extra {
        write_file(Path::new(&api.path), &format!("gen/f{n:04}.txt"), "x\n");
    }
    let events = s.end_turn().await;
    let found = changes(&events);
    assert_eq!(found.len(), MAX_CHANGES_PER_TURN);
    assert_eq!(found[0].1, "gen/f0000.txt");
    assert_eq!(found[found.len() - 1].1, "gen/f0499.txt");
    let errors: Vec<&Event> = events
        .iter()
        .filter(|event| matches!(event.body, EventBody::Error { .. }))
        .collect();
    let [error] = errors.as_slice() else {
        panic!("expected one error, got {}", errors.len());
    };
    let EventBody::Error { message } = &error.body else {
        unreachable!();
    };
    assert!(
        message.starts_with("3 more files changed in api"),
        "{message}"
    );
    assert!(message.contains("Changes view"), "{message}");
    assert_eq!(error.agent_id.as_deref(), Some(s.lead.as_str()));
    assert_eq!(error.parent_id.as_ref(), Some(&events[0].id));
}

#[tokio::test]
async fn a_repo_that_cannot_be_read_is_skipped_and_the_turn_ends_as_usual() {
    let s = shell_with(false, |manager, clones| async move {
        let api = clone_with(&manager, &clones, "api", &[]).await;
        let web = clone_with(&manager, &clones, "web", &[]).await;
        vec![attach(&api, None), attach(&web, None)]
    })
    .await;
    let (api, web) = (s.repo("api").await, s.repo("web").await);
    std::fs::remove_dir_all(&api.path).unwrap();
    write_file(Path::new(&web.path), "still.txt", "here\n");
    let events = s.end_turn().await;
    assert_eq!(
        changes(&events),
        [change(&web, "still.txt", FileOp::Create)]
    );
    let detail = s.manager.get(&s.id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Review);
}

#[tokio::test]
async fn an_agent_that_exits_mid_turn_has_its_changes_found_too() {
    let s = shell(&[]).await;
    let api = s.repo("api").await;
    write_file(Path::new(&api.path), "last.txt", "last words\n");
    let after = s.head().await;
    emit(&s.io, RunnerOutput::Exited(Exit::Ok)).await;
    let events = events_until(&s.manager, &s.id, after, is_status(SessionStatus::Review)).await;
    let done = events
        .iter()
        .find(|event| label(event) == "agent_status:done")
        .unwrap();
    assert_eq!(changes(&events), [change(&api, "last.txt", FileOp::Create)]);
    let changed = events
        .iter()
        .find(|event| matches!(event.body, EventBody::FileChanged { .. }))
        .unwrap();
    assert_eq!(changed.parent_id.as_ref(), Some(&done.id));
}

#[tokio::test]
async fn the_mock_agents_edit_is_reported_once_per_turn() {
    let dir = TempDir::new().unwrap();
    let (manager, _starts) = manager_in(&dir, true).await;
    let clones = TempDir::new().unwrap();
    let api = registered(&manager, clones.path(), "api").await;
    let mut wanted = request("Make a note", None);
    wanted.repos = vec![attach(&api, None)];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let first = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    let repo = manager.get(&id).await.unwrap().repos.remove(0);
    let note = change(&repo, "calin-mock-notes.md", FileOp::Edit);
    assert_eq!(changes(&first), std::slice::from_ref(&note));

    let head = manager.get(&id).await.unwrap().session.head_seq;
    manager.send_message(&id, "Another").await.unwrap();
    let second = events_until(&manager, &id, head, is_status(SessionStatus::Review)).await;
    assert_eq!(changes(&second), [note]);
}
