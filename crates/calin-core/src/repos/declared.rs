//! The home repo's `.calin/repos.yaml`: the repos it declares and its named
//! sets (design spec, section 3, "Repo registry").

use std::fmt;
use std::marker::PhantomData;

use serde::Deserialize;
use serde::de::{DeserializeOwned, MapAccess, Visitor};

use super::check_name;

/// The file's path inside the home repo.
pub(crate) const HOME_FILE: &str = ".calin/repos.yaml";

/// A repo the home repo declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Declared {
    pub name: String,
    pub url: Option<String>,
    pub setup: Option<String>,
    pub checks: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HomeFile {
    /// In the file's order.
    pub repos: Vec<Declared>,
    /// Each set with the names it lists, both in the file's order.
    pub sets: Vec<(String, Vec<String>)>,
    /// Entries that were left out, and why.
    pub skipped: Vec<String>,
}

/// A YAML mapping with its entries in the order they were written, which a
/// map type would lose.
struct Ordered<T>(Vec<(String, T)>);

impl<T> Default for Ordered<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Ordered<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries<T>(PhantomData<T>);

        impl<'de, T: DeserializeOwned> Visitor<'de> for Entries<T> {
            type Value = Ordered<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a mapping of names")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, T>()? {
                    entries.push(entry);
                }
                Ok(Ordered(entries))
            }

            // `repos:` with nothing under it.
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(Ordered(Vec::new()))
            }
        }

        deserializer.deserialize_any(Entries(PhantomData))
    }
}

#[derive(Deserialize, Default)]
struct RawFile {
    #[serde(default)]
    repos: Ordered<Option<RawRepo>>,
    #[serde(default)]
    sets: Ordered<Option<Vec<String>>>,
}

/// Every field is optional and unknown keys are ignored, so a file written
/// for a newer Calin still reads.
#[derive(Deserialize, Default)]
struct RawRepo {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    setup: Option<String>,
    #[serde(default)]
    checks: Option<Checks>,
}

/// `checks: "cargo test"` is a list of one.
#[derive(Deserialize)]
#[serde(untagged)]
enum Checks {
    One(String),
    Many(Vec<String>),
}

/// Parses the file. The error is one line for the user.
pub(crate) fn parse(text: &str) -> Result<HomeFile, String> {
    // A file with nothing in it, or only comments, declares nothing.
    let raw: Option<RawFile> =
        serde_yaml_ng::from_str(text).map_err(|err| one_line(&err.to_string()))?;
    let raw = raw.unwrap_or_default();
    let mut file = HomeFile::default();
    for (name, repo) in raw.repos.0 {
        if let Err(err) = check_name(&name) {
            file.skipped.push(format!("repo {name:?}: {err}"));
            continue;
        }
        if file.repos.iter().any(|known| known.name == name) {
            file.skipped.push(format!("repo {name} is declared twice"));
            continue;
        }
        let repo = repo.unwrap_or_default();
        let filled = |value: Option<String>| {
            value
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let checks = match repo.checks {
            None => Vec::new(),
            Some(Checks::One(check)) => vec![check],
            Some(Checks::Many(checks)) => checks,
        };
        file.repos.push(Declared {
            name,
            url: filled(repo.url),
            setup: filled(repo.setup),
            checks: checks.into_iter().filter_map(|c| filled(Some(c))).collect(),
        });
    }
    for (name, members) in raw.sets.0 {
        if !file.sets.iter().any(|(known, _)| *known == name) {
            file.sets.push((name, members.unwrap_or_default()));
        }
    }
    Ok(file)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(name: &str, url: Option<&str>, setup: Option<&str>, checks: &[&str]) -> Declared {
        Declared {
            name: name.to_owned(),
            url: url.map(str::to_owned),
            setup: setup.map(str::to_owned),
            checks: checks.iter().map(|check| (*check).to_owned()).collect(),
        }
    }

    #[test]
    fn the_full_form_keeps_every_field_and_the_order_of_the_file() {
        let file = parse(
            r#"
# The repos of the storefront.
repos:
  web: { url: https://github.com/you/web.git }
  api: { url: git@github.com:you/api.git, checks: ["cargo test", "cargo clippy"], setup: "cargo fetch" }
  docs: {}
sets:
  storefront: [web, api]
  everything: [api, web, docs, gone]
"#,
        )
        .unwrap();
        assert_eq!(
            file.repos,
            [
                declared("web", Some("https://github.com/you/web.git"), None, &[]),
                declared(
                    "api",
                    Some("git@github.com:you/api.git"),
                    Some("cargo fetch"),
                    &["cargo test", "cargo clippy"]
                ),
                declared("docs", None, None, &[]),
            ]
        );
        assert_eq!(
            file.sets,
            [
                (
                    "storefront".to_owned(),
                    vec!["web".to_owned(), "api".to_owned()]
                ),
                (
                    "everything".to_owned(),
                    ["api", "web", "docs", "gone"].map(str::to_owned).to_vec()
                ),
            ]
        );
        assert!(file.skipped.is_empty());
    }

    #[test]
    fn minimal_and_empty_files_declare_what_they_can() {
        for empty in [
            "",
            "\n",
            "# nothing yet\n",
            "repos:\n",
            "repos: {}\nsets:\n",
            "{}",
        ] {
            assert_eq!(parse(empty), Ok(HomeFile::default()), "{empty:?}");
        }
        // The block form, an entry with no value, and a single check.
        let file = parse("repos:\n  api:\n    url: /srv/git/api\n    checks: make test\n  docs:\n")
            .unwrap();
        assert_eq!(
            file.repos,
            [
                declared("api", Some("/srv/git/api"), None, &["make test"]),
                declared("docs", None, None, &[]),
            ]
        );
        assert!(file.sets.is_empty());
        // Sets without repos: the names resolve against whatever is registered.
        let file = parse("sets:\n  pair: [a, b]\n  none:\n").unwrap();
        assert_eq!(
            file.sets,
            [
                ("pair".to_owned(), vec!["a".to_owned(), "b".to_owned()]),
                ("none".to_owned(), vec![]),
            ]
        );
    }

    #[test]
    fn unknown_keys_are_ignored_and_blank_values_are_absent() {
        let file = parse(
            r#"
version: 2
repos:
  api: { url: "  ", setup: "", checks: ["", "cargo test"], owner: platform, tier: 1 }
workflows: { nightly: {} }
"#,
        )
        .unwrap();
        assert_eq!(file.repos, [declared("api", None, None, &["cargo test"])]);
    }

    #[test]
    fn names_that_cannot_be_folders_and_repeats_are_skipped_with_a_reason() {
        let file = parse("repos:\n  a/b: {}\n  \"..\": {}\n  ok: {}\n").unwrap();
        assert_eq!(file.repos, [declared("ok", None, None, &[])]);
        assert_eq!(file.skipped.len(), 2);
        assert!(file.skipped[0].contains("a/b"), "{:?}", file.skipped);
    }

    #[test]
    fn what_does_not_parse_is_an_error_in_one_line() {
        for invalid in [
            "repos: [api, web]",
            "repos:\n  api: just a string\n",
            "repos:\n  api: { url: [1, 2] }\n",
            "repos: {api: {url: x}",
            "- a\n- b\n",
            "sets:\n  pair: { a: 1 }\n",
            "\tnot yaml: [",
        ] {
            let err = parse(invalid).expect_err(invalid);
            assert!(
                !err.is_empty() && !err.contains('\n'),
                "{invalid:?}: {err:?}"
            );
        }
    }
}
