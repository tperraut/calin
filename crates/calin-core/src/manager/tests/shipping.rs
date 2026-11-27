//! Delivery: what a ship commits, pushes and opens through a stand-in for
//! `gh`, and how the pull requests are watched afterwards. Remotes are bare
//! repositories on disk; nothing here reaches a network or the real `gh`.

use calin_proto::{
    CheckState, FromIssue, IssueRef, PullRequest, PullRequestState, ReviewState, ShipRequest,
};

use super::*;
use crate::testing::write_executable;

/// A stand-in for GitHub's CLI. Pull requests are folders of files next to
/// it, one folder per repository, which it finds like `gh` does: from the
/// `origin` remote of the directory it runs in. Every invocation is logged
/// as the repository, a tab and the arguments. Files beside it steer it:
/// `signed-out`, `hold` (wait while it exists) and `fail-<verb>-<repo>`.
/// Issues are JSON files, `issues/<repo>/<number>`, shown as they are.
/// Titles and bodies may hold quotes, backslashes and newlines, no tabs.
const FAKE_GH: &str = r#"#!/bin/sh
root=$(dirname "$0")
if [ "$1" = auth ]; then
  printf '\t%s\n' "$*" >> "$root/log"
  if [ -f "$root/signed-out" ]; then
    echo "You are not logged into any GitHub hosts." >&2
    exit 1
  fi
  exit 0
fi
remote=$(git config --get remote.origin.url) || { echo "no git remotes found" >&2; exit 1; }
repo=$(basename "$remote" .git)
printf '%s\t%s\n' "$repo" "$*" >> "$root/log"
while [ -f "$root/hold" ]; do sleep 0.05; done
if [ -f "$root/fail-$2-$repo" ]; then
  echo "HTTP 502: Bad Gateway" >&2
  exit 1
fi
prs="$root/prs/$repo"
mkdir -p "$prs"
esc() {
  sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' "$1" | awk 'BEGIN { ORS = "" } NR > 1 { print "\\n" } { print }'
}
show() {
  printf '{"number":%s,"url":"%s","title":"%s","state":"%s","isDraft":%s,' \
    "$(cat "$1/number")" "$(cat "$1/url")" "$(esc "$1/title")" "$(cat "$1/state")" "$(cat "$1/draft")"
  printf '"statusCheckRollup":%s,"reviewDecision":"%s","body":"%s","headRefName":"%s","isCrossRepository":false}' \
    "$(cat "$1/checks")" "$(cat "$1/review")" "$(esc "$1/body")" "$(cat "$1/head")"
}
case "$1 $2" in
"pr list")
  head=
  for arg in "$@"; do case "$arg" in --head=*) head=${arg#--head=} ;; esac; done
  printf '['
  sep=
  for dir in "$prs"/*/; do
    [ -d "$dir" ] || continue
    [ "$(cat "$dir/head")" = "$head" ] || continue
    [ "$(cat "$dir/state")" = OPEN ] || continue
    printf '%s' "$sep"
    show "$dir"
    sep=,
  done
  printf ']\n'
  ;;
"pr create")
  number=$(($(ls "$prs" | wc -l) + 1))
  dir="$prs/$number"
  mkdir "$dir"
  draft=false
  for arg in "$@"; do
    case "$arg" in
      --base=*) printf '%s' "${arg#--base=}" > "$dir/base" ;;
      --head=*) printf '%s' "${arg#--head=}" > "$dir/head" ;;
      --title=*) printf '%s' "${arg#--title=}" > "$dir/title" ;;
      --draft) draft=true ;;
    esac
  done
  cat > "$dir/body"
  echo "$number" > "$dir/number"
  echo "https://github.invalid/acme/$repo/pull/$number" > "$dir/url"
  echo OPEN > "$dir/state"
  echo "$draft" > "$dir/draft"
  echo '[]' > "$dir/checks"
  : > "$dir/review"
  cat "$dir/url"
  ;;
"pr view")
  dir="$prs/${3##*/}"
  if [ ! -d "$dir" ]; then echo "GraphQL: Could not resolve to a PullRequest." >&2; exit 1; fi
  show "$dir"
  ;;
"pr edit")
  dir="$prs/$3"
  if [ ! -d "$dir" ]; then echo "GraphQL: Could not resolve to a PullRequest." >&2; exit 1; fi
  cat > "$dir/body"
  ;;
"issue view")
  file="$root/issues/$repo/$3"
  if [ ! -f "$file" ]; then
    echo "GraphQL: Could not resolve to an issue or pull request with the number of $3." >&2
    exit 1
  fi
  cat "$file"
  ;;
*)
  echo "unexpected: $*" >&2
  exit 2
  ;;
esac
"#;

/// No tick of the watcher after the one at startup: the test looks itself.
const NEVER: Duration = Duration::from_secs(3600);

struct FakeGh {
    dir: TempDir,
}

impl FakeGh {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        write_executable(&dir.path().join("gh"), FAKE_GH);
        Self { dir }
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("gh")
    }

    /// Every invocation so far: the repository, a tab, the arguments.
    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// How many invocations began like `start`.
    fn calls(&self, start: &str) -> usize {
        let log = self.log();
        log.iter().filter(|line| line.starts_with(start)).count()
    }

    fn pull_request(&self, repo: &str, number: u32) -> PathBuf {
        self.dir.path().join(format!("prs/{repo}/{number}"))
    }

    /// How many pull requests the repository has, open or not.
    fn count(&self, repo: &str) -> usize {
        std::fs::read_dir(self.dir.path().join("prs").join(repo)).map_or(0, Iterator::count)
    }

    fn read(&self, repo: &str, number: u32, field: &str) -> String {
        let text = std::fs::read_to_string(self.pull_request(repo, number).join(field)).unwrap();
        text.strip_suffix('\n').unwrap_or(&text).to_owned()
    }

    /// Changes a pull request the way GitHub would, in one step: the fake
    /// may be reading it.
    fn write(&self, repo: &str, number: u32, field: &str, value: &str) {
        let dir = self.pull_request(repo, number);
        std::fs::create_dir_all(&dir).unwrap();
        let draft = self
            .dir
            .path()
            .join(format!("draft-{repo}-{number}-{field}"));
        std::fs::write(&draft, value).unwrap();
        std::fs::rename(draft, dir.join(field)).unwrap();
    }

    /// A pull request somebody opened without Calin.
    fn open_by_hand(&self, repo: &str, number: u32, head: &str, state: &str) {
        let url = format!("https://github.invalid/acme/{repo}/pull/{number}");
        for (field, value) in [
            ("number", number.to_string().as_str()),
            ("url", url.as_str()),
            ("title", "Opened by hand"),
            ("body", "Opened by hand.\n"),
            ("head", head),
            ("base", "main"),
            ("draft", "false"),
            ("checks", "[]"),
            ("review", ""),
            // Last: without it the fake does not list the pull request.
            ("state", state),
        ] {
            self.write(repo, number, field, value);
        }
    }

    /// An open issue of `repo`, as `gh issue view` shows it.
    fn issue(&self, repo: &str, number: u32, title: &str, body: &str) {
        let dir = self.dir.path().join("issues").join(repo);
        std::fs::create_dir_all(&dir).unwrap();
        let issue = serde_json::json!({
            "number": number,
            "title": title,
            "state": "OPEN",
            "labels": [{"name": "bug"}],
            "author": {"login": "octocat"},
            "updatedAt": "2026-11-20T08:30:00Z",
            "url": format!("https://github.com/acme/{repo}/issues/{number}"),
            "body": body,
        });
        std::fs::write(dir.join(number.to_string()), issue.to_string()).unwrap();
    }

    fn switch(&self, flag: &str, on: bool) {
        let path = self.dir.path().join(flag);
        if on {
            std::fs::write(path, "").unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }
}

struct Delivery {
    manager: SessionManager,
    starts: Starts,
    gh: FakeGh,
    home: TempDir,
    clones: TempDir,
    remotes: TempDir,
}

fn shipping_config(gh: &FakeGh, poll: Duration) -> ManagerConfig {
    ManagerConfig {
        gh_bin: gh.bin(),
        pull_request_poll: poll,
        ..config()
    }
}

async fn delivery() -> Delivery {
    delivery_with(false, NEVER).await
}

async fn delivery_with(dry_run: bool, poll: Duration) -> Delivery {
    let gh = FakeGh::new();
    let home = TempDir::new().unwrap();
    let (manager, starts) = manager_with(&home, dry_run, shipping_config(&gh, poll)).await;
    Delivery {
        manager,
        starts,
        gh,
        home,
        clones: TempDir::new().unwrap(),
        remotes: TempDir::new().unwrap(),
    }
}

impl Delivery {
    fn remote(&self, name: &str) -> PathBuf {
        self.remotes.path().join(format!("{name}.git"))
    }

    /// A registered clone `name` whose `origin` is a bare repository.
    async fn repo(&self, name: &str) -> Repo {
        let remote = self.remote(name);
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        let clone = git_repo(self.clones.path(), name);
        git(
            &clone,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&clone, &["push", "-q", "-u", "origin", "main"]);
        git(&clone, &["remote", "set-head", "origin", "main"]);
        // The library commits as this machine would: with its identity, its
        // hooks and its signing. The clones bring their own, so that the
        // tests do not depend on where they run.
        for (key, value) in [
            ("user.name", "Calin Test"),
            ("user.email", "test@calin.invalid"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git(&clone, &["config", key, value]);
        }
        let path = clone.to_str().unwrap().to_owned();
        let request = RegisterRepoRequest { path, name: None };
        self.manager.register_repo(request).await.unwrap()
    }

    /// [`Self::repo`], with an `origin` on GitHub as far as anyone asks:
    /// git sends its pushes to the bare repository all the same.
    async fn github_repo(&self, name: &str) -> Repo {
        let repo = self.repo(name).await;
        let github = format!("https://github.com/acme/{name}.git");
        let clone = Path::new(&repo.path);
        git(clone, &["remote", "set-url", "origin", &github]);
        let rewrite = format!("url.{}.insteadOf", self.remote(name).display());
        git(clone, &["config", &rewrite, &github]);
        repo
    }

    /// The commit `branch` is at on the remote of `name`, if it is there.
    fn remote_tip(&self, name: &str, branch: &str) -> Option<String> {
        let full = format!("refs/heads/{branch}");
        let tip = git(
            &self.remote(name),
            &["for-each-ref", "--format=%(objectname)", &full],
        );
        Some(tip).filter(|tip| !tip.is_empty())
    }

    /// A session of the mock agent on `repos`, at its review. The mock
    /// leaves an uncommitted note in the first repo.
    async fn reviewed(&self, repos: Vec<AttachRepoRequest>) -> SessionDetail {
        let mut wanted = request("Add the thing", None);
        wanted.repos = repos;
        let created = self.manager.create_session(wanted).await.unwrap();
        let id = created.session.id;
        events_until(&self.manager, &id, 0, is_status(SessionStatus::Review)).await;
        self.manager.get(&id).await.unwrap()
    }

    async fn head(&self, id: &str) -> u64 {
        self.manager.get(id).await.unwrap().session.head_seq
    }

    /// Ships, and returns what was appended up to the first event `last` matches.
    async fn ship(
        &self,
        id: &str,
        request: ShipRequest,
        last: impl Fn(&Event) -> bool,
    ) -> Vec<Event> {
        let head = self.head(id).await;
        self.manager.ship(id, request).await.unwrap();
        events_until(&self.manager, id, head, last).await
    }

    /// Sends a message and returns what was appended up to the next review.
    /// A ship in progress is over by then: the turn waits for it.
    async fn turn(&self, id: &str, text: &str) -> Vec<Event> {
        let head = self.head(id).await;
        self.manager.send_message(id, text).await.unwrap();
        events_until(&self.manager, id, head, is_status(SessionStatus::Review)).await
    }

    /// Looks at the pull requests now and returns what that appended.
    async fn look(&self, id: &str) -> Vec<Event> {
        let head = self.head(id).await;
        within(self.manager.poll_pull_requests(id)).await.unwrap();
        let now = self.head(id).await;
        if now == head {
            return Vec::new();
        }
        events_until(&self.manager, id, head, |event| event.seq == now).await
    }
}

fn titled(title: &str, body: &str) -> ShipRequest {
    ShipRequest {
        title: Some(title.to_owned()),
        body: Some(body.to_owned()),
        draft: None,
    }
}

fn is_error(event: &Event) -> bool {
    matches!(event.body, EventBody::Error { .. })
}

fn is_shipping() -> impl Fn(&Event) -> bool {
    is_status(SessionStatus::Shipping)
}

fn url(repo: &str, number: u32) -> String {
    format!("https://github.invalid/acme/{repo}/pull/{number}")
}

fn opened(repo: &str, number: u32) -> String {
    format!("pull_request_opened:{}", url(repo, number))
}

fn updated(repo: &str, number: u32) -> String {
    format!("pull_request_updated:{}", url(repo, number))
}

/// The value an event carries, when it is about a pull request.
fn carried(event: &Event) -> &PullRequest {
    match &event.body {
        EventBody::PullRequestOpened { pull_request }
        | EventBody::PullRequestUpdated { pull_request } => pull_request,
        other => panic!("not a pull request event: {other:?}"),
    }
}

fn subjects(dir: &str) -> Vec<String> {
    let log = git(Path::new(dir), &["log", "--format=%s"]);
    log.lines().map(str::to_owned).collect()
}

#[tokio::test]
async fn shipping_commits_pushes_and_opens_one_linked_pull_request_per_changed_repo() {
    let d = delivery().await;
    let api = d.repo("api").await;
    let web = d.repo("web").await;
    let docs = d.repo("docs").await;
    let tools = d.repo("tools").await;
    let detail = d
        .reviewed(vec![
            attach(&api, None),
            attach(&web, None),
            attach(&docs, None),
            attach(&tools, Some(RepoMode::Live)),
        ])
        .await;
    let id = detail.session.id.clone();
    let branch = detail.session.branch.clone().unwrap();
    let [api_at, web_at, docs_at, tools_at] = detail.repos.as_slice() else {
        panic!("expected four repos, got {:?}", detail.repos);
    };
    // api holds the mock's uncommitted note. web gets a commit made by
    // hand: a clean tree whose branch is ahead of its base. docs stays as
    // it was. tools, edited in place, is dirty and none of Calin's business.
    write_file(Path::new(&web_at.path), "src/page.txt", "page\n");
    git(Path::new(&web_at.path), &["add", "-A"]);
    git(Path::new(&web_at.path), &["commit", "-q", "-m", "By hand"]);
    write_file(Path::new(&tools_at.path), "scratch.txt", "mine\n");
    let tools_head = git(Path::new(&tools_at.path), &["rev-parse", "HEAD"]);

    let request = titled(
        "Add the thing",
        "What it does.\n\nWith \"quotes\" and a \\.",
    );
    let events = d.ship(&id, request, is_shipping()).await;
    assert_eq!(
        labels(&events),
        [
            opened("api", 1),
            opened("web", 1),
            updated("api", 1),
            updated("web", 1),
            "session_status:shipping".to_owned(),
        ]
    );
    // The orchestrator did this, caused by no event.
    assert!(
        events
            .iter()
            .all(|event| event.agent_id.is_none() && event.parent_id.is_none())
    );

    // Exactly two pull requests, on record in the order of the repo set.
    let shipped = d.manager.get(&id).await.unwrap();
    assert_eq!(shipped.session.status, SessionStatus::Shipping);
    let on_record = |at: &SessionRepo, repo: &str, updated_at: i64| PullRequest {
        session_repo_id: at.id.clone(),
        number: 1,
        url: url(repo, 1),
        title: "Add the thing".to_owned(),
        state: PullRequestState::Draft,
        checks: CheckState::None,
        review: ReviewState::None,
        updated_at,
    };
    assert_eq!(
        shipped.pull_requests,
        [
            on_record(api_at, "api", shipped.pull_requests[0].updated_at),
            on_record(web_at, "web", shipped.pull_requests[1].updated_at),
        ]
    );
    assert_eq!(carried(&events[0]).session_repo_id, api_at.id);
    assert!(carried(&events[2]).updated_at >= carried(&events[0]).updated_at);
    assert_eq!((d.gh.count("api"), d.gh.count("web")), (1, 1));
    assert_eq!((d.gh.count("docs"), d.gh.count("tools")), (0, 0));

    // Against the default branch, from the session's branch, as drafts.
    for repo in ["api", "web"] {
        assert_eq!(d.gh.read(repo, 1, "base"), "main");
        assert_eq!(d.gh.read(repo, 1, "head"), branch);
        assert_eq!(d.gh.read(repo, 1, "title"), "Add the thing");
        assert_eq!(d.gh.read(repo, 1, "draft"), "true");
    }
    // Each description has the user's text, then the other pull request,
    // between Calin's markers.
    for (repo, other) in [("api", "web"), ("web", "api")] {
        let body = d.gh.read(repo, 1, "body");
        assert!(body.starts_with("<!-- calin:begin"), "{body}");
        assert!(body.ends_with("<!-- calin:end -->"), "{body}");
        let text = "What it does.\n\nWith \"quotes\" and a \\.\n\n<!-- calin:related -->\n";
        assert!(body.contains(text), "{body}");
        let link = format!(
            "### Related pull requests\n\n- {other}: {}\n",
            url(other, 1)
        );
        assert!(body.contains(&link), "{body}");
        assert!(!body.contains(&format!("- {repo}: ")), "{body}");
    }

    // The uncommitted work is committed with the title and names the session.
    let api_dir = Path::new(&api_at.path);
    assert_eq!(
        git(api_dir, &["log", "-1", "--format=%B"]),
        format!("Add the thing\n\nCalin-Session: {id}")
    );
    assert_eq!(
        git(api_dir, &["show", "--name-only", "--format=", "HEAD"]),
        "calin-mock-notes.md"
    );
    assert_eq!(git(api_dir, &["status", "--porcelain"]), "");
    assert_eq!(subjects(&api_at.path), ["Add the thing", "initial"]);
    // What was already committed is pushed as it is.
    assert_eq!(subjects(&web_at.path), ["By hand", "initial"]);

    // The branches are on the remotes, at the commits of the worktrees.
    for at in [api_at, web_at] {
        let head = git(Path::new(&at.path), &["rev-parse", "HEAD"]);
        assert_eq!(d.remote_tip(&at.name, &branch), Some(head));
    }
    // The unchanged repo and the one edited in place were left alone.
    assert_eq!(d.remote_tip("docs", &branch), None);
    assert_eq!(
        git(Path::new(&docs_at.path), &["rev-parse", "HEAD"]),
        docs_at.base_ref.clone().unwrap()
    );
    assert_eq!(d.remote_tip("tools", &branch), None);
    assert_eq!(d.remote_tip("tools", "main"), Some(tools_head.clone()));
    let tools_dir = Path::new(&tools_at.path);
    assert_eq!(git(tools_dir, &["rev-parse", "HEAD"]), tools_head);
    assert_eq!(git(tools_dir, &["status", "--porcelain"]), "?? scratch.txt");
    for repo in ["api", "web", "docs", "tools"] {
        assert_eq!(d.remote_tip(repo, "main"), detail_base(&shipped, repo));
    }

    // What `gh` was asked, in order; never about the other two repos.
    let log = d.gh.log();
    let asked: Vec<&str> = log
        .iter()
        .map(|line| {
            let (repo, args) = line.split_once('\t').unwrap();
            let verb: Vec<&str> = args.split(' ').take(2).collect();
            match (repo, verb.as_slice()) {
                ("", ["auth", "status"]) => "auth status",
                ("api", ["pr", "list"]) => "api list",
                ("api", ["pr", "create"]) => "api create",
                ("api", ["pr", "view"]) => "api view",
                ("api", ["pr", "edit"]) => "api edit",
                ("web", ["pr", "list"]) => "web list",
                ("web", ["pr", "create"]) => "web create",
                ("web", ["pr", "view"]) => "web view",
                ("web", ["pr", "edit"]) => "web edit",
                _ => panic!("unexpected invocation: {line}"),
            }
        })
        .collect();
    assert_eq!(
        asked,
        [
            "auth status",
            "api list",
            "api create",
            "api view",
            "web list",
            "web create",
            "web view",
            "api edit",
            "web edit",
        ]
    );
    let created = format!(
        "api\tpr create --base=main --head={branch} --title=Add the thing --body-file=- --draft"
    );
    assert!(log.contains(&created), "{log:?}");
}

/// The commit the session started from in `repo`, which is where its
/// remote's `main` must still be.
fn detail_base(detail: &SessionDetail, repo: &str) -> Option<String> {
    let at = detail.repos.iter().find(|at| at.name == repo).unwrap();
    at.base_ref.clone()
}

#[tokio::test]
async fn a_repo_that_fails_is_reported_with_its_step_and_the_others_carry_on() {
    let d = delivery().await;
    let api = d.repo("api").await;
    let web = d.repo("web").await;
    let docs = d.repo("docs").await;
    let cli = d.repo("cli").await;
    let detail = d
        .reviewed(vec![
            attach(&api, None),
            attach(&web, None),
            attach(&docs, None),
            attach(&cli, None),
        ])
        .await;
    let id = detail.session.id.clone();
    let branch = detail.session.branch.clone().unwrap();
    let [_, web_at, docs_at, cli_at] = detail.repos.as_slice() else {
        panic!("expected four repos, got {:?}", detail.repos);
    };
    for at in [web_at, docs_at, cli_at] {
        write_file(Path::new(&at.path), "change.txt", "changed\n");
    }
    // web's remote is gone, docs has none, and somebody switched cli's
    // worktree to another branch.
    let web_remote = d.remote("web");
    let gone = d.remotes.path().join("gone.git");
    git(
        Path::new(&web.path),
        &["remote", "set-url", "origin", gone.to_str().unwrap()],
    );
    git(Path::new(&docs.path), &["remote", "remove", "origin"]);
    git(
        Path::new(&cli_at.path),
        &["switch", "-q", "-c", "elsewhere"],
    );

    let events = d
        .ship(&id, titled("Add the thing", "Body."), is_shipping())
        .await;
    let labels = labels(&events);
    assert_eq!(labels.len(), 5, "{labels:?}");
    assert_eq!(labels[0], opened("api", 1));
    assert!(
        labels[1].starts_with("error:could not ship web: push failed: "),
        "{}",
        labels[1]
    );
    assert!(
        labels[2].starts_with("error:could not ship docs: push failed: ")
            && labels[2].contains("no `origin` remote"),
        "{}",
        labels[2]
    );
    assert!(
        labels[3].starts_with("error:could not ship cli: commit failed: ")
            && labels[3].contains("elsewhere"),
        "{}",
        labels[3]
    );
    assert_eq!(labels[4], "session_status:shipping");

    // One pull request exists, so the session is shipping; nothing was
    // opened for the others, and with no sibling there is nothing to link.
    let shipped = d.manager.get(&id).await.unwrap();
    assert_eq!(shipped.session.status, SessionStatus::Shipping);
    assert_eq!(shipped.pull_requests.len(), 1);
    assert_eq!(shipped.agents[0].status, AgentStatus::Idle);
    assert_eq!(d.gh.calls("web\t") + d.gh.calls("docs\t"), 0);
    assert_eq!(d.gh.calls("cli\t") + d.gh.calls("api\tpr edit"), 0);
    assert!(!d.gh.read("api", 1, "body").contains("Related"));
    // web's work was committed before its push failed; cli's was not touched.
    assert_eq!(subjects(&web_at.path), ["Add the thing", "initial"]);
    assert_eq!(
        git(Path::new(&cli_at.path), &["status", "--porcelain"]),
        "?? change.txt"
    );

    // With web's remote back, shipping again opens its pull request and
    // links the two. The others fail as before.
    git(
        Path::new(&web.path),
        &["remote", "set-url", "origin", web_remote.to_str().unwrap()],
    );
    let last = updated("web", 1);
    let events = d
        .ship(&id, titled("Add the thing", "Body."), |event| {
            label(event) == last
        })
        .await;
    let labels = super::labels(&events);
    assert_eq!(labels.len(), 5, "{labels:?}");
    assert_eq!(labels[0], opened("web", 1));
    assert!(labels[1].starts_with("error:could not ship docs: "));
    assert!(labels[2].starts_with("error:could not ship cli: "));
    assert_eq!(labels[3..], [updated("api", 1), updated("web", 1)]);
    assert!(d.remote_tip("web", &branch).is_some());
    assert!(
        d.gh.read("api", 1, "body")
            .contains(&format!("- web: {}", url("web", 1)))
    );
    assert_eq!(d.manager.get(&id).await.unwrap().pull_requests.len(), 2);
}

#[tokio::test]
async fn a_commit_or_a_push_that_hangs_times_out_and_the_others_carry_on() {
    let gh = FakeGh::new();
    let home = TempDir::new().unwrap();
    let limit = Duration::from_millis(500);
    let config = ManagerConfig {
        commit_timeout: limit,
        push_timeout: limit,
        ..shipping_config(&gh, NEVER)
    };
    let (manager, starts) = manager_with(&home, false, config).await;
    let d = Delivery {
        manager,
        starts,
        gh,
        home,
        clones: TempDir::new().unwrap(),
        remotes: TempDir::new().unwrap(),
    };
    let api = d.repo("api").await;
    let web = d.repo("web").await;
    let docs = d.repo("docs").await;
    // web's commit hook waits for ever; docs' remote never answers.
    let hooks = d.clones.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    write_executable(&hooks.join("pre-commit"), "#!/bin/sh\nexec sleep 30\n");
    let hooks = hooks.to_str().unwrap();
    git(Path::new(&web.path), &["config", "core.hooksPath", hooks]);
    let silent = d.clones.path().join("silent-receive-pack");
    write_executable(&silent, "#!/bin/sh\nexec sleep 30\n");
    let receivepack = silent.to_str().unwrap();
    let docs_clone = Path::new(&docs.path);
    git(
        docs_clone,
        &["config", "remote.origin.receivepack", receivepack],
    );
    let detail = d
        .reviewed(vec![
            attach(&api, None),
            attach(&web, None),
            attach(&docs, None),
        ])
        .await;
    let id = detail.session.id.clone();
    let branch = detail.session.branch.clone().unwrap();
    for at in &detail.repos[1..] {
        write_file(Path::new(&at.path), "change.txt", "changed\n");
    }

    let started = std::time::Instant::now();
    let events = d
        .ship(&id, titled("Add the thing", "Body."), is_shipping())
        .await;
    assert!(started.elapsed() < Duration::from_secs(10));
    let labels = labels(&events);
    assert_eq!(labels.len(), 4, "{labels:?}");
    assert_eq!(labels[0], opened("api", 1));
    assert!(
        labels[1].starts_with("error:could not ship web: commit failed: ")
            && labels[1].ends_with("timed out after 500 ms"),
        "{}",
        labels[1]
    );
    assert!(
        labels[2].starts_with("error:could not ship docs: push failed: `git push")
            && labels[2].ends_with("timed out after 500 ms"),
        "{}",
        labels[2]
    );
    assert_eq!(labels[3], "session_status:shipping");
    assert!(d.remote_tip("api", &branch).is_some());
    assert!(d.remote_tip("docs", &branch).is_none());
    // docs' commit was made before its push hung; web's was not.
    assert_eq!(
        subjects(&detail.repos[2].path),
        ["Add the thing", "initial"]
    );
    assert_eq!(subjects(&detail.repos[1].path), ["initial"]);
}

#[tokio::test]
async fn with_nothing_to_ship_the_session_stays_in_review() {
    let mut d = delivery().await;
    let api = d.repo("api").await;
    let tools = d.repo("tools").await;
    // The puppet changes nothing; the only change is in a repo edited in place.
    let mut wanted = request("Look around", Some("puppet"));
    wanted.repos = vec![attach(&api, None), attach(&tools, Some(RepoMode::Live))];
    let id = d.manager.create_session(wanted).await.unwrap().session.id;
    let (_params, io) = within(d.starts.recv()).await.unwrap();
    emit(&io, RunnerOutput::TurnEnded).await;
    events_until(&d.manager, &id, 0, is_status(SessionStatus::Review)).await;
    write_file(Path::new(&tools.path), "scratch.txt", "mine\n");

    let events = d.ship(&id, ShipRequest::default(), is_error).await;
    let labels = labels(&events);
    assert_eq!(labels.len(), 1, "{labels:?}");
    assert!(labels[0].starts_with("error:nothing to ship"), "{labels:?}");
    // A turn after it shows that nothing else followed.
    let after = d.head(&id).await;
    d.manager.send_message(&id, "go on").await.unwrap();
    let next = events_until(&d.manager, &id, after, is_status(SessionStatus::Running)).await;
    assert_eq!(
        super::labels(&next),
        ["user_message:go on", "session_status:running"]
    );
    let detail = d.manager.get(&id).await.unwrap();
    assert!(detail.pull_requests.is_empty());
    assert_eq!(d.gh.log(), ["\tauth status --active"]);
    assert_eq!(
        git(Path::new(&tools.path), &["status", "--porcelain"]),
        "?? scratch.txt"
    );
}

#[tokio::test]
async fn shipping_is_refused_in_the_wrong_state_and_when_github_cannot_be_used() {
    let mut d = delivery().await;
    let api = d.repo("api").await;
    assert!(matches!(
        d.manager
            .ship("01ARZ3NDEKTSV4RRFFQ69G5FAV", ShipRequest::default())
            .await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        d.manager
            .refresh_pull_requests("01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .await,
        Err(Error::NotFound(_))
    ));

    // While the agent works there is nothing to ship yet.
    let mut wanted = request("Work", Some("puppet"));
    wanted.repos = vec![attach(&api, None)];
    let id = d.manager.create_session(wanted).await.unwrap().session.id;
    let (_params, mut io) = within(d.starts.recv()).await.unwrap();
    events_until(&d.manager, &id, 0, is_status(SessionStatus::Running)).await;
    let refused = d.manager.ship(&id, ShipRequest::default()).await;
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains("it is running")),
        "{refused:?}"
    );
    assert!(matches!(
        d.manager.refresh_pull_requests(&id).await,
        Err(Error::Conflict(_))
    ));
    emit(&io, RunnerOutput::TurnEnded).await;
    events_until(&d.manager, &id, 0, is_status(SessionStatus::Review)).await;
    let head = d.head(&id).await;

    // In review: not with a title of several lines, and not without GitHub.
    let two_lines = titled("One\nTwo", "");
    assert!(matches!(
        d.manager.ship(&id, two_lines).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        d.manager.refresh_pull_requests(&id).await,
        Err(Error::Conflict(_))
    ));
    d.gh.switch("signed-out", true);
    let refused = d.manager.ship(&id, ShipRequest::default()).await;
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains("not signed in")),
        "{refused:?}"
    );
    let problem = d.manager.github_problem().await.unwrap();
    assert!(problem.contains("not signed in"), "{problem}");
    assert_eq!(d.head(&id).await, head);
    assert_eq!(d.gh.calls("api\t"), 0);

    // Signing in is noticed by the next ship, not a minute later.
    d.gh.switch("signed-out", false);
    let events = d.ship(&id, ShipRequest::default(), is_error).await;
    assert!(label(&events[0]).starts_with("error:nothing to ship"));
    assert_eq!(d.manager.github_problem().await, None);
    // The answer is remembered: health does not run `gh` every time.
    let asked = d.gh.calls("\tauth status");
    assert_eq!(d.manager.github_problem().await, None);
    assert_eq!(d.gh.calls("\tauth status"), asked);

    // A session that is over is not shipped.
    let stopping = tokio::spawn({
        let (manager, id) = (d.manager.clone(), id.clone());
        async move { manager.stop(&id).await }
    });
    assert_eq!(within(io.inputs.recv()).await, Some(RunnerInput::Stop));
    emit(&io, RunnerOutput::Exited(Exit::Ok)).await;
    within(stopping).await.unwrap().unwrap();
    let refused = d.manager.ship(&id, ShipRequest::default()).await;
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains("it is cancelled")),
        "{refused:?}"
    );

    // Without `gh` at all, the reason says so.
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let id = manager
        .create_session(request("no gh here", None))
        .await
        .unwrap()
        .session
        .id;
    events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    let refused = manager.ship(&id, ShipRequest::default()).await;
    assert!(
        matches!(&refused, Err(Error::Conflict(why)) if why.contains("was not found")),
        "{refused:?}"
    );
    assert!(manager.github_problem().await.is_some());
}

#[tokio::test]
async fn one_ship_runs_at_a_time_and_a_message_waits_for_it() {
    let d = delivery().await;
    let api = d.repo("api").await;
    let detail = d.reviewed(vec![attach(&api, None)]).await;
    let id = detail.session.id.clone();
    let head = detail.session.head_seq;

    // `gh` does not answer until it is let go.
    d.gh.switch("hold", true);
    d.manager
        .ship(&id, titled("Add the thing", "Body."))
        .await
        .unwrap();
    let second = d.manager.ship(&id, titled("Again", "Body.")).await;
    assert!(
        matches!(&second, Err(Error::Conflict(why)) if why.contains("already being shipped")),
        "{second:?}"
    );
    // A look at the pull requests does not wait for the ship either.
    within(d.manager.poll_pull_requests(&id)).await.unwrap();
    d.manager.send_message(&id, "meanwhile").await.unwrap();
    d.gh.switch("hold", false);

    let events = events_until(&d.manager, &id, head, is_status(SessionStatus::Review)).await;
    let labels = labels(&events);
    assert_eq!(
        labels[..5],
        [
            "user_message:meanwhile".to_owned(),
            opened("api", 1),
            "session_status:shipping".to_owned(),
            "session_status:running".to_owned(),
            "agent_status:running".to_owned(),
        ]
    );
    assert_eq!(d.gh.count("api"), 1);
    assert_eq!(d.gh.read("api", 1, "title"), "Add the thing");
    // The turn found the worktree committed: its note is the only change since.
    assert_eq!(
        git(Path::new(&detail.repos[0].path), &["status", "--porcelain"]),
        "M calin-mock-notes.md"
    );
}

#[tokio::test]
async fn stopping_during_a_ship_keeps_the_pull_request_it_opened_on_record() {
    let d = delivery().await;
    let api = d.repo("api").await;
    let web = d.repo("web").await;
    let detail = d
        .reviewed(vec![attach(&api, None), attach(&web, None)])
        .await;
    let id = detail.session.id.clone();
    let head = detail.session.head_seq;
    write_file(Path::new(&detail.repos[1].path), "page.txt", "page\n");

    d.gh.switch("hold", true);
    d.manager
        .ship(&id, titled("Add the thing", "Body."))
        .await
        .unwrap();
    let stopping = tokio::spawn({
        let (manager, id) = (d.manager.clone(), id.clone());
        async move { manager.stop(&id).await }
    });
    // Long enough for the stop to find the ship at its first call to `gh`.
    tokio::time::sleep(Duration::from_millis(200)).await;
    d.gh.switch("hold", false);
    within(stopping).await.unwrap().unwrap();

    let events = events_until(&d.manager, &id, head, is_status(SessionStatus::Cancelled)).await;
    let labels = labels(&events);
    // What `gh` was already doing is on record; the ship went no further.
    assert!(labels.contains(&opened("api", 1)), "{labels:?}");
    assert_eq!(d.gh.count("api"), 1);
    let detail = d.manager.get(&id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Cancelled);
    assert_eq!(
        detail.pull_requests.len(),
        d.gh.count("api") + d.gh.count("web")
    );
}

#[tokio::test]
async fn shipping_again_pushes_new_commits_and_reuses_the_pull_requests() {
    let d = delivery().await;
    let api = d.repo("api").await;
    let web = d.repo("web").await;
    let detail = d
        .reviewed(vec![attach(&api, None), attach(&web, None)])
        .await;
    let id = detail.session.id.clone();
    let branch = detail.session.branch.clone().unwrap();
    let (api_at, web_at) = (&detail.repos[0], &detail.repos[1]);
    write_file(Path::new(&web_at.path), "page.txt", "page\n");
    d.ship(&id, titled("Add the thing", "What it does."), is_shipping())
        .await;
    let first_tip = d.remote_tip("api", &branch).unwrap();

    // A message while shipping sends the agent back in, as in review.
    let turn = d.turn(&id, "one more thing").await;
    let turn = labels(&turn);
    assert_eq!(
        turn[..3],
        [
            "user_message:one more thing",
            "session_status:running",
            "agent_status:running",
        ]
    );
    assert_eq!(
        turn[turn.len() - 2..],
        ["agent_status:idle", "session_status:review"]
    );
    let detail = d.manager.get(&id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Review);
    assert_eq!(detail.pull_requests.len(), 2);
    // Out of `shipping`, the pull requests are not watched.
    assert!(matches!(
        d.manager.refresh_pull_requests(&id).await,
        Err(Error::Conflict(_))
    ));
    let viewed = d.gh.calls("api\tpr view");
    assert!(d.look(&id).await.is_empty());
    assert_eq!(d.gh.calls("api\tpr view"), viewed);

    // Meanwhile somebody wrote below Calin's block on GitHub.
    let body = d.gh.read("api", 1, "body");
    let by_hand = format!("{body}\n\nReviewer note.\n");
    d.gh.write("api", 1, "body", &by_hand);

    // The second ship commits the agent's new work and pushes it to the
    // pull requests there are. Both descriptions get the new text.
    let events = d
        .ship(&id, titled("Second pass", "Now with more."), is_shipping())
        .await;
    assert_eq!(
        labels(&events),
        [
            updated("api", 1),
            updated("web", 1),
            "session_status:shipping".to_owned(),
        ]
    );
    assert_eq!((d.gh.count("api"), d.gh.count("web")), (1, 1));
    assert_eq!(
        d.gh.calls("api\tpr create") + d.gh.calls("web\tpr create"),
        2
    );
    assert_eq!(
        subjects(&api_at.path),
        ["Second pass", "Add the thing", "initial"]
    );
    let second_tip = git(Path::new(&api_at.path), &["rev-parse", "HEAD"]);
    assert_ne!(second_tip, first_tip);
    assert_eq!(d.remote_tip("api", &branch), Some(second_tip.clone()));
    let body = d.gh.read("api", 1, "body");
    assert!(body.contains("\nNow with more.\n"), "{body}");
    assert!(!body.contains("What it does."), "{body}");
    assert!(
        body.contains(&format!("- web: {}", url("web", 1))),
        "{body}"
    );
    assert!(
        body.ends_with("<!-- calin:end -->\n\nReviewer note."),
        "{body}"
    );
    assert_eq!(body.matches("<!-- calin:begin").count(), 1);
    assert!(d.gh.read("web", 1, "body").contains("\nNow with more.\n"));
    // A pull request keeps the title it was opened with.
    assert_eq!(carried(&events[0]).title, "Add the thing");

    // Shipping with nothing new and no text changes nothing at all.
    let head = d.head(&id).await;
    d.manager.ship(&id, ShipRequest::default()).await.unwrap();
    d.manager.send_message(&id, "and another").await.unwrap();
    let quiet = events_until(&d.manager, &id, head, is_status(SessionStatus::Running)).await;
    assert_eq!(
        labels(&quiet),
        ["user_message:and another", "session_status:running"]
    );
    events_until(&d.manager, &id, head, is_status(SessionStatus::Review)).await;
    assert_eq!(d.gh.read("api", 1, "body"), body);
    assert_eq!(d.remote_tip("api", &branch), Some(second_tip));

    // Somebody else pushes to the branch. The next push is rejected, and
    // is not forced: their commit stays where it is.
    let other = d.clones.path().join("someone-elses-api");
    git(
        d.clones.path(),
        &[
            "clone",
            "-q",
            d.remote("api").to_str().unwrap(),
            "someone-elses-api",
        ],
    );
    git(&other, &["switch", "-q", &branch]);
    write_file(&other, "theirs.txt", "theirs\n");
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-q", "-m", "Theirs"]);
    git(&other, &["push", "-q", "origin", &branch]);
    let theirs = git(&other, &["rev-parse", "HEAD"]);

    let events = d.ship(&id, ShipRequest::default(), is_shipping()).await;
    let labels = labels(&events);
    assert_eq!(labels.len(), 2, "{labels:?}");
    assert!(
        labels[0].starts_with("error:could not ship api: push failed: ")
            && labels[0].contains("rejected"),
        "{}",
        labels[0]
    );
    assert_eq!(labels[1], "session_status:shipping");
    assert_eq!(d.remote_tip("api", &branch), Some(theirs));
    // The work is committed locally, under the session's title this time.
    assert_eq!(subjects(&api_at.path)[0], "Add the thing");
}

#[tokio::test]
async fn an_open_pull_request_of_the_branch_is_reused() {
    let d = delivery().await;
    let api = d.repo("api").await;
    let detail = d.reviewed(vec![attach(&api, None)]).await;
    let id = detail.session.id.clone();
    let branch = detail.session.branch.clone().unwrap();
    // One for another branch, one for this branch that was closed, and the
    // open one somebody made for this branch.
    d.gh.open_by_hand("api", 1, "calin/something-else", "OPEN");
    d.gh.open_by_hand("api", 2, &branch, "CLOSED");
    d.gh.open_by_hand("api", 3, &branch, "OPEN");

    let events = d
        .ship(&id, titled("Add the thing", "From Calin."), is_shipping())
        .await;
    assert_eq!(
        labels(&events),
        [
            opened("api", 3),
            updated("api", 3),
            "session_status:shipping".to_owned(),
        ]
    );
    let found = carried(&events[0]);
    assert_eq!(found.number, 3);
    assert_eq!(found.title, "Opened by hand");
    assert_eq!(found.state, PullRequestState::Open);
    assert_eq!(d.gh.calls("api\tpr create"), 0);
    assert_eq!(d.gh.count("api"), 3);
    // Its description keeps what was there, below Calin's block.
    let body = d.gh.read("api", 3, "body");
    assert!(body.starts_with("<!-- calin:begin"), "{body}");
    assert!(
        body.ends_with("From Calin.\n<!-- calin:end -->\n\nOpened by hand."),
        "{body}"
    );
    assert_eq!(d.gh.read("api", 2, "body"), "Opened by hand.");
    assert_eq!(
        d.remote_tip("api", &branch),
        Some(git(
            Path::new(&detail.repos[0].path),
            &["rev-parse", "HEAD"]
        ))
    );
}

#[tokio::test]
async fn pull_requests_are_recorded_when_they_change_and_the_session_ends_with_them() {
    let d = delivery().await;
    let api = d.repo("api").await;
    let web = d.repo("web").await;
    let detail = d
        .reviewed(vec![attach(&api, None), attach(&web, None)])
        .await;
    let id = detail.session.id.clone();
    let (api_at, web_at) = (&detail.repos[0], &detail.repos[1]);
    write_file(Path::new(&web_at.path), "page.txt", "page\n");
    d.ship(&id, titled("Add the thing", "Body."), is_shipping())
        .await;

    // Nothing changed: both are looked at and nothing is appended.
    let viewed = d.gh.calls("api\tpr view") + d.gh.calls("web\tpr view");
    assert!(d.look(&id).await.is_empty());
    assert_eq!(
        d.gh.calls("api\tpr view") + d.gh.calls("web\tpr view"),
        viewed + 2
    );

    // Ready for review, approved, checks green: one event with the whole value.
    d.gh.write("api", 1, "draft", "false");
    d.gh.write("api", 1, "review", "APPROVED");
    let passed = r#"[{"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"}]"#;
    d.gh.write("api", 1, "checks", passed);
    let before = d.manager.get(&id).await.unwrap().pull_requests[0].clone();
    let events = d.look(&id).await;
    assert_eq!(labels(&events), [updated("api", 1)]);
    let now = carried(&events[0]);
    assert_eq!(
        *now,
        PullRequest {
            state: PullRequestState::Open,
            checks: CheckState::Passing,
            review: ReviewState::Approved,
            updated_at: now.updated_at,
            ..before.clone()
        }
    );
    assert!(now.updated_at >= before.updated_at);
    assert!(events[0].agent_id.is_none() && events[0].parent_id.is_none());
    assert_eq!(d.manager.get(&id).await.unwrap().pull_requests[0], *now);
    // Seen once, recorded once.
    assert!(d.look(&id).await.is_empty());

    // The refresh route's way in: accepted, done in the background.
    let running = r#"[{"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":""}]"#;
    d.gh.write("web", 1, "checks", running);
    let head = d.head(&id).await;
    d.manager.refresh_pull_requests(&id).await.unwrap();
    let events = events_until(&d.manager, &id, head, |event| event.seq == head + 1).await;
    assert_eq!(labels(&events), [updated("web", 1)]);
    assert_eq!(carried(&events[0]).checks, CheckState::Pending);
    assert_eq!(carried(&events[0]).session_repo_id, web_at.id);

    // One merged, the other unreadable for now: the failure is nobody's
    // news and the session stays as it is.
    d.gh.write("api", 1, "state", "MERGED");
    d.gh.switch("fail-view-web", true);
    let events = d.look(&id).await;
    assert_eq!(labels(&events), [updated("api", 1)]);
    assert_eq!(carried(&events[0]).state, PullRequestState::Merged);
    assert_eq!(carried(&events[0]).session_repo_id, api_at.id);
    assert!(d.look(&id).await.is_empty());
    let detail = d.manager.get(&id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Shipping);
    // A merged pull request is not asked about again.
    let viewed = d.gh.calls("api\tpr view");
    d.gh.switch("fail-view-web", false);
    assert!(d.look(&id).await.is_empty());
    assert_eq!(d.gh.calls("api\tpr view"), viewed);

    // The last one is closed without a merge: all are over, so is the session.
    d.gh.write("web", 1, "state", "CLOSED");
    let events = d.look(&id).await;
    assert_eq!(
        labels(&events),
        [
            updated("web", 1),
            "agent_status:done".to_owned(),
            "session_status:done".to_owned(),
        ]
    );
    assert_eq!(carried(&events[0]).state, PullRequestState::Closed);
    let detail = d.manager.get(&id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Done);
    assert_eq!(detail.agents[0].status, AgentStatus::Done);
    let states: Vec<PullRequestState> = detail.pull_requests.iter().map(|pr| pr.state).collect();
    assert_eq!(states, [PullRequestState::Merged, PullRequestState::Closed]);

    // Done is done: nothing is watched, shipped or said any more.
    let asked = d.gh.log().len();
    assert!(d.look(&id).await.is_empty());
    assert_eq!(d.gh.log().len(), asked);
    for refused in [
        d.manager.ship(&id, ShipRequest::default()).await,
        d.manager.refresh_pull_requests(&id).await,
        d.manager.send_message(&id, "too late").await,
    ] {
        assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    }
}

#[tokio::test]
async fn shipping_sessions_are_watched_on_a_timer_and_again_after_a_restart() {
    let tick = Duration::from_millis(50);
    let d = delivery_with(false, tick).await;
    let api = d.repo("api").await;
    let detail = d.reviewed(vec![attach(&api, None)]).await;
    let id = detail.session.id.clone();
    let shipped = d
        .ship(&id, titled("Add the thing", "Body."), is_shipping())
        .await;
    let head = shipped.last().unwrap().seq;

    // Nobody asks: the change is noticed at a tick.
    d.gh.write("api", 1, "draft", "false");
    let events = events_until(&d.manager, &id, head, |event| event.seq == head + 1).await;
    assert_eq!(labels(&events), [updated("api", 1)]);
    assert_eq!(carried(&events[0]).state, PullRequestState::Open);

    // The server stops; the pull request is merged while it is down.
    let Delivery {
        manager,
        gh,
        home,
        clones: _clones,
        remotes: _remotes,
        ..
    } = d;
    manager.shutdown().await;
    manager.store().close().await;
    drop(manager);
    gh.write("api", 1, "state", "MERGED");

    let (manager, _starts) = manager_with(&home, false, shipping_config(&gh, tick)).await;
    let events = events_until(&manager, &id, head + 1, is_status(SessionStatus::Done)).await;
    let labels = labels(&events);
    assert_eq!(labels[0], updated("api", 1));
    assert_eq!(labels[labels.len() - 1], "session_status:done");
    assert_eq!(carried(&events[0]).state, PullRequestState::Merged);
    let detail = manager.get(&id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Done);
    assert_eq!(detail.pull_requests[0].state, PullRequestState::Merged);
    // Nothing is watched after that.
    tokio::time::sleep(tick * 4).await;
    let asked = gh.log().len();
    tokio::time::sleep(tick * 4).await;
    assert_eq!(gh.log().len(), asked);
    assert_eq!(
        manager.get(&id).await.unwrap().session.head_seq,
        detail.session.head_seq
    );
}

#[tokio::test]
async fn a_dry_run_makes_up_pull_requests_and_touches_neither_github_nor_a_remote() {
    let d = delivery_with(true, Duration::from_millis(20)).await;
    let api = d.repo("api").await;
    let web = d.repo("web").await;
    let detail = d
        .reviewed(vec![attach(&api, None), attach(&web, None)])
        .await;
    assert_eq!(detail.session.runner, "mock");
    let id = detail.session.id.clone();
    let branch = detail.session.branch.clone().unwrap();
    let (api_at, web_at) = (&detail.repos[0], &detail.repos[1]);
    write_file(Path::new(&web_at.path), "page.txt", "page\n");
    assert_eq!(d.manager.github_problem().await, None);

    let made_up = |repo: &str, number: u32| format!("https://example.invalid/{repo}/pull/{number}");
    let request = ShipRequest {
        draft: Some(false),
        ..titled("Add the thing", "Body.")
    };
    let events = d.ship(&id, request, is_shipping()).await;
    assert_eq!(
        labels(&events),
        [
            format!("pull_request_opened:{}", made_up("api", 1)),
            format!("pull_request_opened:{}", made_up("web", 1)),
            format!("pull_request_updated:{}", made_up("api", 1)),
            format!("pull_request_updated:{}", made_up("web", 1)),
            "session_status:shipping".to_owned(),
        ]
    );
    let shipped = d.manager.get(&id).await.unwrap();
    assert_eq!(shipped.session.status, SessionStatus::Shipping);
    assert_eq!(shipped.pull_requests.len(), 2);
    for (pull_request, at) in shipped.pull_requests.iter().zip([api_at, web_at]) {
        assert_eq!(
            *pull_request,
            PullRequest {
                session_repo_id: at.id.clone(),
                number: 1,
                url: made_up(&at.name, 1),
                title: "Add the thing".to_owned(),
                state: PullRequestState::Draft,
                checks: CheckState::None,
                review: ReviewState::None,
                updated_at: pull_request.updated_at,
            }
        );
    }
    // The commits are real: they are local, and the demo shows them.
    assert_eq!(subjects(&api_at.path), ["Add the thing", "initial"]);
    assert_eq!(subjects(&web_at.path), ["Add the thing", "initial"]);
    // Nothing left the machine and `gh` was never run, not even to look.
    assert_eq!(d.remote_tip("api", &branch), None);
    assert_eq!(d.remote_tip("web", &branch), None);
    d.manager.refresh_pull_requests(&id).await.unwrap();
    assert!(d.look(&id).await.is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(d.gh.log(), [] as [&str; 0]);
    assert_eq!(d.head(&id).await, shipped.session.head_seq);

    // Another turn and another ship: the new commit counts as a change,
    // and the pull requests keep their numbers.
    d.turn(&id, "one more thing").await;
    let events = d.ship(&id, ShipRequest::default(), is_shipping()).await;
    assert_eq!(
        labels(&events),
        [
            format!("pull_request_updated:{}", made_up("api", 1)),
            "session_status:shipping".to_owned(),
        ]
    );
    assert_eq!(subjects(&api_at.path).len(), 3);

    // Numbers go up per repo: the next session's pull request for api is #2.
    let next = d.reviewed(vec![attach(&api, None)]).await;
    let events = d
        .ship(&next.session.id, ShipRequest::default(), is_shipping())
        .await;
    assert_eq!(
        labels(&events),
        [
            format!("pull_request_opened:{}", made_up("api", 2)),
            "session_status:shipping".to_owned(),
        ]
    );
    // Without a title, the session's own.
    assert_eq!(carried(&events[0]).title, next.session.title);
    assert_eq!(d.gh.log(), [] as [&str; 0]);
}

fn from_issue(repo: &Repo, number: u32) -> FromIssue {
    FromIssue {
        repo_id: repo.id.clone(),
        number,
    }
}

#[tokio::test]
async fn a_session_from_an_issue_hands_the_agent_its_text_and_keeps_the_users_own() {
    let mut d = delivery().await;
    let api = d.github_repo("api").await;
    d.gh.issue("api", 12, "Login fails", "Steps:\n1. open the app");
    let mut wanted = request("Fix it, tests first.", Some("puppet"));
    wanted.issue = Some(from_issue(&api, 12));
    let created = d.manager.create_session(wanted).await.unwrap();
    let issue = IssueRef {
        repo_id: api.id.clone(),
        number: 12,
        title: "Login fails".to_owned(),
        url: "https://github.com/acme/api/issues/12".to_owned(),
    };
    assert_eq!(created.session.issue.as_ref(), Some(&issue));
    assert_eq!(created.session.prompt, "Fix it, tests first.");
    let (params, _io) = within(d.starts.recv()).await.unwrap();
    assert_eq!(
        params.prompt,
        "Fix it, tests first.\n\n[Calin] This session works on GitHub issue #12: \
         https://github.com/acme/api/issues/12\n\n<github-issue>\n# Login fails\n\n\
         Steps:\n1. open the app\n</github-issue>"
    );
    // The repo of the issue is the primary one: the agent works there.
    let detail = d.manager.get(&created.session.id).await.unwrap();
    assert_eq!(detail.repos[0].repo_id, api.id);
    assert!(detail.repos[0].primary);
    assert_eq!(params.cwd, Path::new(&detail.repos[0].path));
    let events = events_until(&d.manager, &created.session.id, 0, |event| {
        matches!(event.body, EventBody::AgentSpawned { .. })
    })
    .await;
    assert!(labels(&events).contains(&"user_message:Fix it, tests first.".to_owned()));
    assert_eq!(d.gh.calls("api\tissue view 12"), 1);

    // What GitHub does not have is the request's fault; what keeps GitHub
    // from answering is the state's.
    let mut unknown = request("Fix it", Some("puppet"));
    unknown.issue = Some(from_issue(&api, 99));
    let err = d.manager.create_session(unknown.clone()).await.unwrap_err();
    assert!(
        matches!(&err, Error::Invalid(what) if what.contains("#99")),
        "{err:?}"
    );
    unknown.issue = Some(FromIssue {
        repo_id: "nope".to_owned(),
        number: 12,
    });
    let err = d.manager.create_session(unknown.clone()).await.unwrap_err();
    assert!(
        matches!(&err, Error::Invalid(what) if what.contains("nope")),
        "{err:?}"
    );
    // An issue can stand in for the prompt; a blank prompt alone is refused.
    unknown.issue = Some(from_issue(&api, 12));
    unknown.prompt = "  ".to_owned();
    let stood_in = d.manager.create_session(unknown.clone()).await.unwrap();
    assert_eq!(stood_in.session.prompt, "Work on issue #12: Login fails");
    assert_eq!(stood_in.session.title, "Work on issue #12: Login fails");
    unknown.issue = None;
    let err = d.manager.create_session(unknown).await.unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    // A repo that is not on GitHub has no issues.
    let local = d.repo("local").await;
    let mut there = request("Fix it", Some("puppet"));
    there.issue = Some(from_issue(&local, 1));
    let err = d.manager.create_session(there).await.unwrap_err();
    assert!(
        matches!(&err, Error::Conflict(why) if why.contains("not on GitHub")),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_issues_repo_closes_it_and_the_others_link_to_it_on_every_ship() {
    let d = delivery().await;
    let api = d.github_repo("api").await;
    let web = d.github_repo("web").await;
    d.gh.issue("api", 12, "Login fails", "Broken.");
    let mut wanted = request("Fix it", None);
    // The issue's repo is not listed: it comes first.
    wanted.repos = vec![attach(&web, None)];
    wanted.issue = Some(from_issue(&api, 12));
    let id = d.manager.create_session(wanted).await.unwrap().session.id;
    events_until(&d.manager, &id, 0, is_status(SessionStatus::Review)).await;
    let detail = d.manager.get(&id).await.unwrap();
    let names: Vec<&str> = detail.repos.iter().map(|repo| repo.name.as_str()).collect();
    assert_eq!(names, ["api", "web"]);
    write_file(Path::new(&detail.repos[1].path), "page.txt", "page\n");

    let closes = "<!-- calin:issue -->\nCloses #12\n";
    let link = "<!-- calin:issue -->\nIssue: https://github.com/acme/api/issues/12\n";
    d.ship(&id, titled("Fix login", "What it does."), is_shipping())
        .await;
    for (repo, line) in [("api", closes), ("web", link)] {
        let body = d.gh.read(repo, 1, "body");
        assert!(
            body.contains(&format!("What it does.\n\n{line}\n<!-- calin:related -->")),
            "{body}"
        );
        assert_eq!(body.matches("<!-- calin:issue -->").count(), 1, "{body}");
    }
    // Once more, after another turn, keeping the text: still one line each.
    d.turn(&id, "one more thing").await;
    let again = ShipRequest {
        title: None,
        body: None,
        draft: None,
    };
    d.ship(&id, again, is_shipping()).await;
    for (repo, line) in [("api", closes), ("web", link)] {
        let body = d.gh.read(repo, 1, "body");
        assert!(body.contains(&format!("What it does.\n\n{line}")), "{body}");
        assert_eq!(body.matches("<!-- calin:issue -->").count(), 1, "{body}");
        assert_eq!(
            body.matches("#12").count() + body.matches("issues/12").count(),
            1,
            "{body}"
        );
    }
}

#[tokio::test]
async fn without_a_usable_gh_a_session_cannot_start_from_an_issue() {
    let d = delivery().await;
    let api = d.github_repo("api").await;
    d.gh.issue("api", 12, "Login fails", "Broken.");
    d.gh.switch("signed-out", true);
    let mut wanted = request("Fix it", Some("puppet"));
    wanted.issue = Some(from_issue(&api, 12));
    let err = d.manager.create_session(wanted).await.unwrap_err();
    assert!(
        matches!(&err, Error::Conflict(why) if why.contains("not signed in")),
        "{err:?}"
    );
    let err = d
        .manager
        .list_issues(&api.id, calin_proto::IssueState::Open, None)
        .await;
    assert!(matches!(err, Err(Error::Conflict(_))), "{err:?}");
    assert_eq!(d.manager.list().await.unwrap(), []);
    assert_eq!(d.gh.calls("api\tissue"), 0);
}
