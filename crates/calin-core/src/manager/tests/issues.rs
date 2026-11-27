//! Issues in dry run: made-up ones, so the flow works without GitHub
//! (integrations spec, section 2). With `gh`, see `shipping.rs`.

use calin_proto::{FromIssue, IssueRef, IssueState};

use super::*;

#[tokio::test]
async fn a_dry_run_lists_and_shows_sample_issues() {
    let dir = TempDir::new().unwrap();
    let clones = TempDir::new().unwrap();
    let (manager, _starts) = manager_in(&dir, true).await;
    let api = registered(&manager, clones.path(), "api").await;

    let open = manager
        .list_issues(&api.id, IssueState::Open, None)
        .await
        .unwrap();
    let numbers: Vec<u32> = open.iter().map(|issue| issue.number).collect();
    assert_eq!(numbers, [4, 3, 2]);
    assert!(
        open.iter()
            .all(|issue| issue.title.starts_with("Sample issue: "))
    );
    assert!(
        open[0]
            .url
            .starts_with("https://example.invalid/api/issues/")
    );
    let closed = manager
        .list_issues(&api.id, IssueState::Closed, None)
        .await
        .unwrap();
    assert_eq!(closed.len(), 1);
    let found = manager
        .list_issues(&api.id, IssueState::Open, Some("dark".to_owned()))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    let blank = manager
        .list_issues(&api.id, IssueState::Open, Some("  ".to_owned()))
        .await
        .unwrap();
    assert_eq!(blank.len(), 3);

    let detail = manager.issue(&api.id, 3).await.unwrap();
    assert_eq!(detail.issue, open[1]);
    assert!(detail.body.contains("dry run"));
    assert!(matches!(
        manager.issue(&api.id, 99).await,
        Err(Error::Missing(_))
    ));
    for unknown in [
        manager
            .list_issues("nope", IssueState::Open, None)
            .await
            .err(),
        manager.issue("nope", 1).await.err(),
    ] {
        assert!(matches!(unknown, Some(Error::Missing(_))), "{unknown:?}");
    }
}

#[tokio::test]
async fn a_dry_run_session_starts_from_a_sample_issue_and_keeps_it_after_a_reindex() {
    let dir = TempDir::new().unwrap();
    let clones = TempDir::new().unwrap();
    let (manager, _starts) = manager_in(&dir, true).await;
    let api = registered(&manager, clones.path(), "api").await;
    let web = registered(&manager, clones.path(), "web").await;
    let mut wanted = request("Make it work on phones", None);
    // The issue's repo is listed already, second and to read: it stays so.
    wanted.repos = vec![attach(&web, None), attach(&api, Some(RepoMode::Read))];
    wanted.issue = Some(FromIssue {
        repo_id: api.id.clone(),
        number: 4,
    });
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert!(labels(&events).contains(&"user_message:Make it work on phones".to_owned()));
    let detail = manager.get(&id).await.unwrap();
    let repos: Vec<(&str, RepoMode, bool)> = detail
        .repos
        .iter()
        .map(|repo| (repo.name.as_str(), repo.mode, repo.primary))
        .collect();
    assert_eq!(
        repos,
        [
            ("web", RepoMode::Write, true),
            ("api", RepoMode::Read, false)
        ]
    );
    let issue = IssueRef {
        repo_id: api.id.clone(),
        number: 4,
        title: "Sample issue: the login button does nothing on small screens".to_owned(),
        url: "https://example.invalid/api/issues/4".to_owned(),
    };
    assert_eq!(detail.session.issue.as_ref(), Some(&issue));

    // The index is a projection: rebuilt, it says the same.
    manager.shutdown().await;
    manager.store().close().await;
    std::fs::remove_file(Home::new(dir.path()).db_path()).unwrap();
    let (again, _starts) = manager_in(&dir, true).await;
    assert_eq!(again.get(&id).await.unwrap().session.issue, Some(issue));
}

#[tokio::test]
async fn an_unknown_sample_or_repo_is_a_bad_request() {
    let dir = TempDir::new().unwrap();
    let clones = TempDir::new().unwrap();
    let (manager, _starts) = manager_in(&dir, true).await;
    let api = registered(&manager, clones.path(), "api").await;
    for (repo_id, number) in [(api.id.as_str(), 99), ("nope", 1)] {
        let mut wanted = request("Fix it", None);
        wanted.issue = Some(FromIssue {
            repo_id: repo_id.to_owned(),
            number,
        });
        let err = manager.create_session(wanted).await.unwrap_err();
        assert!(
            matches!(err, Error::Invalid(_)),
            "{repo_id} #{number}: {err:?}"
        );
    }
    assert_eq!(manager.list().await.unwrap(), []);
}
