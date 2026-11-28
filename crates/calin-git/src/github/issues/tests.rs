use serde_json::{Value, json};

use super::super::tests::Fake;
use super::*;
use crate::testing::git;

/// The fake `gh` and a clone whose `origin` is `remote`, where it runs.
fn fake_with_remote(remote: Option<&str>) -> (Fake, std::path::PathBuf) {
    let fake = Fake::new();
    let repo = fake.sandbox.repo("api");
    if let Some(remote) = remote {
        git(&repo, &["remote", "add", "origin", remote]);
    }
    (fake, repo)
}

fn fake() -> (Fake, std::path::PathBuf) {
    fake_with_remote(Some("https://github.com/acme/api.git"))
}

fn issue(overrides: Value) -> Value {
    let mut issue = json!({
        "number": 12,
        "title": "Login fails on mobile",
        "state": "OPEN",
        "labels": [{"id": "L1", "name": "bug", "color": "d73a4a", "description": ""}],
        "author": {"id": "U1", "is_bot": false, "login": "octocat", "name": "Mona"},
        "updatedAt": "2026-11-20T08:30:00Z",
        "url": "https://github.com/acme/api/issues/12",
        "body": "Steps:\n1. open\n2. tap",
        "somethingNew": true,
    });
    for (key, value) in overrides.as_object().unwrap() {
        issue[key] = value.clone();
    }
    issue
}

fn query() -> IssueQuery {
    IssueQuery {
        state: IssueState::Open,
        search: None,
        label: None,
        limit: 50,
    }
}

#[test]
fn issues_are_listed_with_gh_and_mapped_field_by_field() {
    let (fake, repo) = fake();
    let listed = json!([
        issue(
            json!({"number": 3, "labels": [], "author": null, "body": null,
                     "url": "https://github.com/acme/api/issues/3"})
        ),
        issue(json!({})),
    ]);
    fake.set("out-issue-list", listed.to_string());
    let issues = fake.github().list_issues(&repo, &query()).unwrap();
    assert_eq!(
        issues,
        [
            IssueDetail {
                issue: Issue {
                    number: 12,
                    title: "Login fails on mobile".into(),
                    state: IssueState::Open,
                    labels: vec!["bug".into()],
                    author: Some("octocat".into()),
                    updated_at: 1_795_163_400_000,
                    url: "https://github.com/acme/api/issues/12".into(),
                },
                body: "Steps:\n1. open\n2. tap".into(),
            },
            IssueDetail {
                issue: Issue {
                    number: 3,
                    title: "Login fails on mobile".into(),
                    state: IssueState::Open,
                    labels: vec![],
                    author: None,
                    updated_at: 1_795_163_400_000,
                    url: "https://github.com/acme/api/issues/3".into(),
                },
                body: String::new(),
            },
        ]
    );
    assert_eq!(
        fake.args("issue-list"),
        [
            "issue",
            "list",
            "--state=open",
            "--limit=50",
            &format!("--json={ISSUE_FIELDS}"),
        ]
    );
    // Run in the clone, without prompts.
    let canonical = std::fs::canonicalize(&repo).unwrap();
    assert_eq!(
        fake.read("env-issue-list").trim(),
        format!("1|1|{}", canonical.display())
    );
}

#[test]
fn the_state_the_search_and_the_label_are_passed_as_flags() {
    let (fake, repo) = fake();
    fake.set("out-issue-list", "[]");
    let wanted = IssueQuery {
        state: IssueState::Closed,
        search: Some("  --crash on start ".into()),
        label: Some("agent".into()),
        limit: 0,
    };
    assert_eq!(fake.github().list_issues(&repo, &wanted).unwrap(), []);
    assert_eq!(
        fake.args("issue-list"),
        [
            "issue",
            "list",
            "--state=closed",
            "--limit=1",
            &format!("--json={ISSUE_FIELDS}"),
            "--search=--crash on start",
            "--label=agent",
        ]
    );
    // Blank filters are no filters.
    std::fs::remove_file(fake.bin.with_file_name("args-issue-list")).unwrap();
    let blank = IssueQuery {
        search: Some("   ".into()),
        label: Some(String::new()),
        ..query()
    };
    fake.github().list_issues(&repo, &blank).unwrap();
    assert_eq!(fake.args("issue-list").len(), 5);
}

#[test]
fn one_issue_is_viewed_with_its_body() {
    let (fake, repo) = fake();
    fake.set(
        "out-issue-view",
        issue(json!({"state": "CLOSED"})).to_string(),
    );
    let detail = fake.github().issue(&repo, 12).unwrap();
    assert_eq!(detail.issue.state, IssueState::Closed);
    assert_eq!(detail.body, "Steps:\n1. open\n2. tap");
    assert_eq!(
        fake.args("issue-view"),
        ["issue", "view", "12", &format!("--json={ISSUE_FIELDS}")]
    );

    // A number that names nothing.
    fake.set("code-issue-view", "1");
    fake.set(
        "err-issue-view",
        "GraphQL: Could not resolve to an issue or pull request with the number of 99. (repository.issue)",
    );
    let err = fake.github().issue(&repo, 99).unwrap_err();
    assert!(
        matches!(&err, Error::NotFound(what) if what == "issue #99"),
        "{err:?}"
    );

    // A pull request is not an issue; neither is another number.
    std::fs::remove_file(fake.bin.with_file_name("code-issue-view")).unwrap();
    let pull = issue(json!({"url": "https://github.com/acme/api/pull/12"}));
    fake.set("out-issue-view", pull.to_string());
    assert!(matches!(
        fake.github().issue(&repo, 12),
        Err(Error::NotFound(_))
    ));
    fake.set("out-issue-view", issue(json!({"number": 13})).to_string());
    assert!(matches!(
        fake.github().issue(&repo, 12),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn output_that_is_not_the_issue_json_asked_for_is_an_error() {
    let (fake, repo) = fake();
    let without = |field: &str| {
        let mut issue = issue(json!({}));
        issue.as_object_mut().unwrap().remove(field);
        issue
    };
    let cases = [
        String::new(),
        "not json".to_owned(),
        "{\"number\": 12".to_owned(),
        "[]".to_owned(),
        without("title").to_string(),
        without("labels").to_string(),
        without("updatedAt").to_string(),
        without("url").to_string(),
        issue(json!({"state": "MERGED"})).to_string(),
        issue(json!({"number": "12"})).to_string(),
        issue(json!({"labels": ["bug"]})).to_string(),
        issue(json!({"author": "octocat"})).to_string(),
        issue(json!({"updatedAt": "yesterday"})).to_string(),
        issue(json!({"updatedAt": 1_795_163_400})).to_string(),
    ];
    for output in cases {
        fake.set("out-issue-view", &output);
        let err = fake.github().issue(&repo, 12).unwrap_err();
        assert!(
            matches!(&err, Error::Git { stderr, .. } if stderr.contains("not what was asked")),
            "{output:?} gave {err:?}"
        );
    }
    // A list must be a list of issues.
    fake.set("out-issue-list", issue(json!({})).to_string());
    assert!(fake.github().list_issues(&repo, &query()).is_err());
    fake.set(
        "out-issue-list",
        json!([issue(json!({"state": 1}))]).to_string(),
    );
    assert!(fake.github().list_issues(&repo, &query()).is_err());
}

#[test]
fn a_repository_that_is_not_on_github_is_refused_without_running_gh() {
    for remote in [
        None,
        Some("https://gitlab.com/acme/api.git"),
        Some("git@bitbucket.org:acme/api.git"),
        Some("/srv/git/api.git"),
    ] {
        let (fake, repo) = fake_with_remote(remote);
        let github = fake.github();
        for err in [
            github.list_issues(&repo, &query()).unwrap_err(),
            github.issue(&repo, 1).unwrap_err(),
            github.list_open_pull_requests(&repo, 10).unwrap_err(),
            github.pull_request_item(&repo, 1).unwrap_err(),
        ] {
            assert!(
                matches!(&err, Error::Conflict(reason) if reason.contains("origin")),
                "{remote:?}: {err:?}"
            );
        }
        assert_eq!(fake.log(), Vec::<String>::new(), "{remote:?}");
    }
    // Not a directory at all.
    let (fake, _) = fake();
    let gone = fake.sandbox.path().join("gone");
    assert!(matches!(
        fake.github().issue(&gone, 1),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn a_failing_gh_is_an_error_with_what_it_said() {
    let (fake, repo) = fake();
    fake.set("code-issue-list", "1");
    fake.set("err-issue-list", "HTTP 502: Bad Gateway");
    let err = fake.github().list_issues(&repo, &query()).unwrap_err();
    assert!(
        matches!(&err, Error::Git { stderr, .. } if stderr == "HTTP 502: Bad Gateway"),
        "{err:?}"
    );
    // Any other failure of a view is not a missing issue.
    fake.set("code-issue-view", "4");
    let err = fake.github().issue(&repo, 1).unwrap_err();
    assert!(matches!(&err, Error::Git { .. }), "{err:?}");

    let missing = GitHub::new(fake.sandbox.path().join("nope/gh"));
    assert!(matches!(
        missing.list_issues(&repo, &query()),
        Err(Error::Io { .. })
    ));
}

#[test]
fn open_pull_requests_are_listed_and_viewed() {
    let (fake, repo) = fake();
    let pull = |number: u32| {
        json!({
            "number": number,
            "title": "Bump the parser",
            "body": null,
            "url": format!("https://github.com/acme/api/pull/{number}"),
            "author": {"login": "dependabot[bot]"},
            "updatedAt": "2026-11-20T08:30:00.250Z",
        })
    };
    fake.set("out-pr-list", json!([pull(4), pull(9)]).to_string());
    let items = fake.github().list_open_pull_requests(&repo, 30).unwrap();
    let numbers: Vec<u32> = items.iter().map(|item| item.number).collect();
    assert_eq!(numbers, [9, 4]);
    assert_eq!(
        items[0],
        PullRequestItem {
            number: 9,
            title: "Bump the parser".into(),
            body: String::new(),
            url: "https://github.com/acme/api/pull/9".into(),
            author: Some("dependabot[bot]".into()),
            updated_at: 1_795_163_400_250,
        }
    );
    assert_eq!(
        fake.args("pr-list"),
        [
            "pr",
            "list",
            "--state=open",
            "--limit=30",
            &format!("--json={PULL_REQUEST_FIELDS}"),
        ]
    );
    // No limit still asks for one.
    std::fs::remove_file(fake.bin.with_file_name("args-pr-list")).unwrap();
    fake.github().list_open_pull_requests(&repo, 0).unwrap();
    assert_eq!(fake.args("pr-list")[3], "--limit=1");

    fake.set("out-pr-view", pull(4).to_string());
    assert_eq!(fake.github().pull_request_item(&repo, 4).unwrap().number, 4);
    assert_eq!(
        fake.args("pr-view"),
        ["pr", "view", "4", &format!("--json={PULL_REQUEST_FIELDS}")]
    );
    fake.set("code-pr-view", "1");
    fake.set(
        "err-pr-view",
        "GraphQL: Could not resolve to a PullRequest with the number of 5.",
    );
    assert!(matches!(
        fake.github().pull_request_item(&repo, 5),
        Err(Error::NotFound(_))
    ));
    fake.set("out-pr-list", json!([{"number": 1}]).to_string());
    assert!(fake.github().list_open_pull_requests(&repo, 30).is_err());
}

#[test]
fn github_remotes_are_told_from_others_in_every_url_form() {
    for url in [
        "https://github.com/acme/api.git",
        "https://github.com/acme/api",
        "http://github.com/acme/api",
        "https://user:token@github.com/acme/api.git",
        "ssh://git@github.com/acme/api.git",
        "ssh://git@github.com:22/acme/api.git",
        "git@github.com:acme/api.git",
        "GitHub.com:acme/api",
        "git://github.com/acme/api.git",
        "https://github.example.com/acme/api.git",
        "git@acme.ghe.com:acme/api.git",
        "  https://www.github.com/acme/api  ",
    ] {
        assert!(is_github_url(url), "{url}");
    }
    for url in [
        "https://gitlab.com/acme/api.git",
        "git@gitlab.com:acme/api.git",
        "https://notgithub.com/acme/api",
        "https://github.com.evil.example/acme/api",
        "https://github./acme/api",
        "file:///srv/git/api.git",
        "/srv/git/api.git",
        "../api",
        "./github.com:acme/api",
        "",
    ] {
        assert!(!is_github_url(url), "{url}");
    }
}
