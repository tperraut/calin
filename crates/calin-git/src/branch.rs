//! The name of a session's branch.

const PREFIX: &str = "calin/";
const MAX_SLUG: usize = 40;
const ID_CHARS: usize = 6;

/// The branch a session works on, in every repo it is attached to:
/// `calin/<slug>-<last 6 characters of the id, lowercased>`.
///
/// The slug is the title reduced to lowercase ASCII letters and digits, with
/// every run of anything else turned into one hyphen, cut to 40 characters.
/// A title with nothing usable gives `session`. Only letters and digits of the
/// id are used, so the result is a valid ref name whatever the input.
pub fn session_branch_name(title: &str, session_id: &str) -> String {
    let mut slug = String::with_capacity(MAX_SLUG);
    for c in title.chars() {
        if slug.len() == MAX_SLUG {
            break;
        }
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = match slug.trim_end_matches('-') {
        "" => "session",
        slug => slug,
    };

    let id: Vec<char> = session_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let suffix: String = id[id.len().saturating_sub(ID_CHARS)..].iter().collect();

    if suffix.is_empty() {
        format!("{PREFIX}{slug}")
    } else {
        format!("{PREFIX}{slug}-{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sandbox, git_ok};

    const ID: &str = "01KB2M5S7Q9ZC4X8HVTPA3DWEN";

    #[test]
    fn slugs() {
        let name = |title| session_branch_name(title, ID);
        assert_eq!(name("Fix the login bug"), "calin/fix-the-login-bug-a3dwen");
        assert_eq!(name("  Fix -- the   bug!! "), "calin/fix-the-bug-a3dwen");
        assert_eq!(name(""), "calin/session-a3dwen");
        assert_eq!(name("?!... ///"), "calin/session-a3dwen");
        assert_eq!(
            name("Ünïcödé café ☕ naïve"),
            "calin/n-c-d-caf-na-ve-a3dwen"
        );
        assert_eq!(name("日本語のタイトル"), "calin/session-a3dwen");
        assert_eq!(name("feat/api: add v2"), "calin/feat-api-add-v2-a3dwen");
        assert_eq!(name("...hidden.lock"), "calin/hidden-lock-a3dwen");
    }

    #[test]
    fn slug_is_at_most_40_characters_and_never_ends_with_a_hyphen() {
        let long = session_branch_name(&"abcdefghi ".repeat(20), ID);
        let slug = long
            .strip_prefix("calin/")
            .and_then(|rest| rest.strip_suffix("-a3dwen"))
            .unwrap();
        // Cut at 40 lands on a hyphen here, which is then dropped.
        assert_eq!(slug, "abcdefghi-abcdefghi-abcdefghi-abcdefghi");
        assert_eq!(session_branch_name(&"x".repeat(200), ID).len(), 6 + 40 + 7);
    }

    #[test]
    fn id_suffix() {
        assert_eq!(session_branch_name("t", "AbC"), "calin/t-abc");
        assert_eq!(session_branch_name("t", ""), "calin/t");
        assert_eq!(session_branch_name("t", "../~^:x y?"), "calin/t-xy");
        assert_eq!(
            session_branch_name("t", "7c9e6679-7425-40de-944b-e07fc1f90ae7"),
            "calin/t-f90ae7"
        );
    }

    #[test]
    fn every_name_is_a_valid_ref() {
        let sandbox = Sandbox::new();
        let titles = [
            "",
            " ",
            "?!...",
            ".",
            "..",
            ".hidden",
            "...leading dots",
            "trailing.lock",
            "a/b/c",
            "/leading/slash/",
            "back\\slash",
            "-leading-hyphen",
            "@{-1}",
            "HEAD",
            "tilde~caret^colon:",
            "tab\tand\nnewline",
            "Ünïcödé",
            "日本語",
            "emoji 🚀 launch",
            "a..b",
            "x.lock",
            &"very long title ".repeat(30),
        ];
        for title in titles {
            for id in [ID, "", "-", "a/b..c"] {
                let name = session_branch_name(title, id);
                assert!(
                    git_ok(sandbox.path(), &["check-ref-format", "--branch", &name]),
                    "{title:?} / {id:?} gave the invalid branch name {name:?}"
                );
                let slug = name.strip_prefix("calin/").unwrap();
                assert!(
                    slug.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                    "{name:?}"
                );
                assert!(!slug.contains("--") && !slug.starts_with('-') && !slug.ends_with('-'));
            }
        }
    }
}
