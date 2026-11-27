//! What an agent is told about where it runs: the addition to its system
//! prompt, and the note it gets when the user attaches a repo while it works.

use std::path::Path;

use calin_proto::{RepoMode, SessionRepo};

fn mode_name(mode: RepoMode) -> &'static str {
    match mode {
        RepoMode::Write => "write",
        RepoMode::Read => "read",
        RepoMode::Live => "live",
    }
}

/// The text added to the system prompt of an agent started in `cwd` with
/// `repos` attached. `tools` says whether the agent can reach Calin's tools;
/// without them there is nothing to say about attaching.
pub(crate) fn system_prompt(repos: &[SessionRepo], cwd: &Path, tools: bool) -> String {
    let mut text = format!(
        "# Calin\n\nYou are running inside Calin, an orchestrator that gives each session a set \
         of git repositories to work in. Your working directory is {}.\n\n",
        cwd.display()
    );
    if repos.is_empty() {
        text.push_str("No repository is attached to this session yet.\n");
    } else {
        text.push_str("Repositories attached to this session:\n");
        for repo in repos {
            let primary = if repo.primary { ", primary" } else { "" };
            let mode = mode_name(repo.mode);
            text.push_str(&format!(
                "- {} ({mode}{primary}) at {}\n",
                repo.name, repo.path
            ));
        }
        text.push_str(
            "\nModes: `write` is a private git worktree on this session's branch, yours to \
             edit. `live` is the user's own checkout, edited in place. `read` is for reading \
             only; edits there are refused.\n",
        );
    }
    if tools {
        text.push_str(
            "\nTo work in a repository that is not in this list, call the `attach_repo` tool \
             of the `calin` tool server with the repository's name. It returns the path to \
             work in. The `list_repos` tool shows every repository Calin knows. Do not look \
             for clones elsewhere on disk and do not clone a repository yourself: only \
             changes made in the attached paths are part of this session. In a repository \
             you have just attached, read and edit files with your file tools rather than \
             with shell commands: the file tools work there at once, while a shell command \
             may have to wait for the user's approval.\n",
        );
    }
    text
}

/// Tells the agent about a repo the user attached while it was running.
pub(crate) fn attach_note(repo: &SessionRepo) -> String {
    format!(
        "[Calin] The user attached the repository `{}` to this session in {} mode. It is at {}",
        repo.name,
        mode_name(repo.mode),
        repo.path
    )
}

/// The text handed to the agent for a user message: the notes that were
/// waiting, then the message as the user wrote it.
pub(crate) fn with_notes(notes: Vec<String>, text: String) -> String {
    if notes.is_empty() {
        return text;
    }
    format!("{}\n\n{text}", notes.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str, mode: RepoMode, primary: bool) -> SessionRepo {
        SessionRepo {
            id: name.to_owned(),
            session_id: "s".to_owned(),
            repo_id: name.to_owned(),
            name: name.to_owned(),
            mode,
            path: format!("/work/{name}"),
            branch: None,
            base_ref: None,
            primary,
        }
    }

    #[test]
    fn the_prompt_lists_every_repo_with_its_mode_and_names_the_tools() {
        let repos = [
            repo("home", RepoMode::Live, true),
            repo("api", RepoMode::Write, false),
            repo("docs", RepoMode::Read, false),
        ];
        let text = system_prompt(&repos, Path::new("/work/home"), true);
        assert!(text.contains("running inside Calin"));
        assert!(text.contains("Your working directory is /work/home."));
        assert!(text.contains("- home (live, primary) at /work/home\n"));
        assert!(text.contains("- api (write) at /work/api\n"));
        assert!(text.contains("- docs (read) at /work/docs\n"));
        assert!(text.contains("`attach_repo`") && text.contains("`list_repos`"));
        assert!(text.contains("file tools rather than with shell commands"));
        // Tool names as Calin serves them, not as one runner spells them.
        assert!(!text.contains("mcp__"));
    }

    #[test]
    fn without_repos_or_tools_the_prompt_says_only_what_holds() {
        let text = system_prompt(&[], Path::new("/data/work"), true);
        assert!(text.contains("No repository is attached to this session yet."));
        assert!(text.contains("`attach_repo`"));
        assert!(!text.contains("Modes:"));
        let text = system_prompt(
            &[repo("api", RepoMode::Write, true)],
            Path::new("/w"),
            false,
        );
        assert!(text.contains("- api (write, primary)"));
        assert!(!text.contains("attach_repo"));
    }

    #[test]
    fn notes_go_before_the_users_text_which_stays_whole() {
        let api = attach_note(&repo("api", RepoMode::Write, false));
        assert_eq!(
            api,
            "[Calin] The user attached the repository `api` to this session in write mode. \
             It is at /work/api"
        );
        let docs = attach_note(&repo("docs", RepoMode::Read, false));
        assert_eq!(with_notes(Vec::new(), "hello".to_owned()), "hello");
        assert_eq!(
            with_notes(vec![api.clone(), docs.clone()], "now fix\nit".to_owned()),
            format!("{api}\n{docs}\n\nnow fix\nit")
        );
    }
}
