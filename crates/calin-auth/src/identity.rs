//! Who a signed-in user is, and the list of who may sign in (spec section 2).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::secret::log_safe;

/// Longest subject accepted, per OpenID Connect Core section 2.
const MAX_SUBJECT_BYTES: usize = 255;
/// Longest address accepted, per RFC 5321.
const MAX_EMAIL_BYTES: usize = 254;
const MAX_EMAIL_LOCAL_BYTES: usize = 64;
/// Bound on the allow-list, so matching on every request stays trivial.
const MAX_ALLOW_LIST_ENTRIES: usize = 1000;

/// The identity of a user: a verified email address, or a provider subject.
///
/// An email is held with its ASCII letters lowercased, so two spellings of
/// one address are one identity. Its `Display` form is the address itself or
/// `sub:<subject>`, the same forms `CALIN_ALLOWED_USERS` uses; an address can
/// never contain `:`, so the two cannot be confused.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "IdentityRepr", into = "IdentityRepr")]
pub struct Identity(Kind);

#[derive(Clone, PartialEq, Eq, Hash)]
enum Kind {
    Email(String),
    Subject(String),
}

/// Why a string cannot be an identity.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InvalidIdentity {
    /// Not something an email address can plausibly look like.
    #[error("not a plausible email address")]
    Email,
    /// Empty, longer than 255 bytes, or holding whitespace at an end or a
    /// control character.
    #[error("not a usable subject")]
    Subject,
}

impl Identity {
    /// An email identity. Refuses what is not plausibly an address.
    pub fn email(address: &str) -> Result<Self, InvalidIdentity> {
        if plausible_email(address) {
            Ok(Self(Kind::Email(address.to_ascii_lowercase())))
        } else {
            Err(InvalidIdentity::Email)
        }
    }

    /// A subject identity, compared exactly.
    pub fn subject(subject: &str) -> Result<Self, InvalidIdentity> {
        let usable = !subject.is_empty()
            && subject.len() <= MAX_SUBJECT_BYTES
            && subject.trim() == subject
            && !subject.chars().any(char::is_control);
        if usable {
            Ok(Self(Kind::Subject(subject.to_owned())))
        } else {
            Err(InvalidIdentity::Subject)
        }
    }

    /// The address, when this is an email identity.
    pub fn as_email(&self) -> Option<&str> {
        match &self.0 {
            Kind::Email(address) => Some(address),
            Kind::Subject(_) => None,
        }
    }

    /// The subject, when this is a subject identity.
    pub fn as_subject(&self) -> Option<&str> {
        match &self.0 {
            Kind::Email(_) => None,
            Kind::Subject(subject) => Some(subject),
        }
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Kind::Email(address) => f.write_str(address),
            Kind::Subject(subject) => write!(f, "sub:{subject}"),
        }
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Identity({})", log_safe(&self.to_string()))
    }
}

/// The stored form: tagged, so a subject that looks like an address stays a
/// subject when `auth.json` is read back.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
enum IdentityRepr {
    Email(String),
    Sub(String),
}

impl From<Identity> for IdentityRepr {
    fn from(identity: Identity) -> Self {
        match identity.0 {
            Kind::Email(address) => Self::Email(address),
            Kind::Subject(subject) => Self::Sub(subject),
        }
    }
}

impl TryFrom<IdentityRepr> for Identity {
    type Error = InvalidIdentity;

    fn try_from(repr: IdentityRepr) -> Result<Self, Self::Error> {
        match repr {
            IdentityRepr::Email(address) => Self::email(&address),
            IdentityRepr::Sub(subject) => Self::subject(&subject),
        }
    }
}

/// Whether `text` can plausibly be an email address: one `@`, a local part,
/// a dotted domain, and none of the characters that only appear in display
/// names, lists or quoted forms. Deliberately narrower than RFC 5322.
fn plausible_email(text: &str) -> bool {
    if text.len() > MAX_EMAIL_BYTES {
        return false;
    }
    let forbidden = |c: char| {
        c.is_whitespace()
            || c.is_control()
            || matches!(c, ',' | ';' | ':' | '<' | '>' | '(' | ')' | '[' | ']')
            // `*` is legal in an address but far more likely a wildcard
            // attempt, which this list does not support.
            || matches!(c, '"' | '\\' | '/' | '*')
    };
    if text.chars().any(forbidden) {
        return false;
    }
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    if local.is_empty() || local.len() > MAX_EMAIL_LOCAL_BYTES || domain.contains('@') {
        return false;
    }
    let label_ok = |label: &str| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_alphanumeric() || c == '-')
    };
    domain.contains('.') && domain.split('.').all(label_ok)
}

/// The identities that may sign in. Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowList {
    identities: Vec<Identity>,
}

/// One thing wrong with `CALIN_ALLOWED_USERS`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AllowListProblem {
    /// No entry at all: nobody could sign in.
    #[error("the allow-list is empty")]
    Empty,
    /// An entry that is neither an email address nor `sub:<subject>`.
    #[error("allow-list entry \"{entry}\" is {reason}")]
    InvalidEntry {
        /// The entry, made safe to log.
        entry: String,
        /// What is wrong with it.
        reason: InvalidIdentity,
    },
    /// More entries than this crate accepts.
    #[error("the allow-list has more than {MAX_ALLOW_LIST_ENTRIES} entries")]
    TooLong,
}

impl AllowList {
    /// Parses the comma-separated form of `CALIN_ALLOWED_USERS`.
    ///
    /// Entries are trimmed; empty ones (a trailing comma) are skipped; an
    /// entry starting with `sub:` is a subject, anything else must be an
    /// email address. Every problem is returned, not only the first.
    pub fn parse(text: &str) -> Result<Self, Vec<AllowListProblem>> {
        let mut identities: Vec<Identity> = Vec::new();
        let mut problems = Vec::new();
        for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let parsed = match entry.strip_prefix("sub:") {
                Some(subject) => Identity::subject(subject),
                None => Identity::email(entry),
            };
            match parsed {
                Ok(identity) if identities.contains(&identity) => {}
                Ok(identity) => identities.push(identity),
                Err(reason) => problems.push(AllowListProblem::InvalidEntry {
                    entry: log_safe(entry),
                    reason,
                }),
            }
        }
        if identities.is_empty() && problems.is_empty() {
            problems.push(AllowListProblem::Empty);
        }
        if identities.len() > MAX_ALLOW_LIST_ENTRIES {
            problems.push(AllowListProblem::TooLong);
        }
        if problems.is_empty() {
            Ok(Self { identities })
        } else {
            Err(problems)
        }
    }

    /// Whether `identity` may sign in: an email identity must equal an email
    /// entry (ASCII case already folded on both sides), a subject identity
    /// must equal a `sub:` entry byte for byte. The two kinds never match
    /// each other.
    pub fn allows(&self, identity: &Identity) -> bool {
        self.identities.contains(identity)
    }

    /// The entries, in the order they were written.
    pub fn identities(&self) -> &[Identity] {
        &self.identities
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(address: &str) -> Identity {
        Identity::email(address).unwrap()
    }

    fn subject(sub: &str) -> Identity {
        Identity::subject(sub).unwrap()
    }

    #[test]
    fn plausible_and_implausible_addresses() {
        for good in [
            "alice@example.com",
            "Alice.O'Neil+calin@Mail.Example.co.uk",
            "a@b.c",
            "user_name-1@sub-domain.example.org",
            "jos\u{e9}@exampl\u{e9}.fr",
        ] {
            assert!(Identity::email(good).is_ok(), "{good}");
        }
        let too_long = format!("{}@example.com", "a".repeat(250));
        let long_local = format!("{}@example.com", "a".repeat(65));
        for bad in [
            "",
            "alice",
            "@example.com",
            "alice@",
            "alice@localhost",
            "alice@@example.com",
            "alice@exa@mple.com",
            "alice@.example.com",
            "alice@example..com",
            "alice@example.com.",
            "alice@-example.com",
            "alice@example-.com",
            "alice@exam_ple.com",
            "alice smith@example.com",
            " alice@example.com",
            "alice@example.com\n",
            "alice@example.com\u{0}",
            "Alice <alice@example.com>",
            "\"alice\"@example.com",
            "alice@[127.0.0.1]",
            "sub:alice@example.com",
            "alice@example.com;bob@example.com",
            "alice@example.com/x",
            "alice\\@example.com",
            "*",
            "*@example.com",
            too_long.as_str(),
            long_local.as_str(),
        ] {
            assert_eq!(Identity::email(bad), Err(InvalidIdentity::Email), "{bad:?}");
        }
    }

    #[test]
    fn usable_and_unusable_subjects() {
        for good in [
            "auth0|5f3c",
            "248289761001",
            "a b",
            "alice@example.com",
            "x:y",
        ] {
            assert!(Identity::subject(good).is_ok(), "{good}");
        }
        let too_long = "s".repeat(256);
        for bad in ["", " lead", "trail ", "a\nb", "a\u{0}b", too_long.as_str()] {
            assert_eq!(
                Identity::subject(bad),
                Err(InvalidIdentity::Subject),
                "{bad:?}"
            );
        }
        assert!(Identity::subject(&"s".repeat(255)).is_ok());
    }

    #[test]
    fn email_case_is_folded_for_ascii_only() {
        assert_eq!(email("Alice@Example.COM"), email("alice@example.com"));
        assert_eq!(email("Alice@Example.COM").to_string(), "alice@example.com");
        // U+212A KELVIN SIGN lowercases to "k" under Unicode rules: it must
        // not be folded into the ASCII letter.
        assert_ne!(email("\u{212a}ate@example.com"), email("kate@example.com"));
        assert_ne!(
            email("JOS\u{c9}@example.com"),
            email("jos\u{e9}@example.com")
        );
    }

    #[test]
    fn subjects_are_exact() {
        assert_ne!(subject("Abc"), subject("abc"));
        assert_eq!(subject("abc").to_string(), "sub:abc");
        assert_eq!(subject("abc").as_subject(), Some("abc"));
        assert_eq!(subject("abc").as_email(), None);
        assert_eq!(email("a@b.c").as_email(), Some("a@b.c"));
        assert_eq!(email("a@b.c").as_subject(), None);
    }

    #[test]
    fn allow_list_matching_table() {
        let list =
            AllowList::parse(" Alice@Example.com , sub:auth0|42,sub:bob@example.com,").unwrap();
        assert_eq!(list.identities().len(), 3);
        let cases = [
            (email("alice@example.com"), true),
            (email("ALICE@EXAMPLE.COM"), true),
            (email("alice@example.org"), false),
            (email("alice2@example.com"), false),
            (email("alice@example.com.evil.example"), false),
            (subject("auth0|42"), true),
            (subject("AUTH0|42"), false),
            (subject("auth0|4"), false),
            (subject("auth0|421"), false),
            // A subject never matches an email entry, and the reverse.
            (subject("alice@example.com"), false),
            (subject("bob@example.com"), true),
            (email("bob@example.com"), false),
        ];
        for (identity, expected) in cases {
            assert_eq!(list.allows(&identity), expected, "{identity}");
        }
    }

    #[test]
    fn allow_list_ignores_duplicates_and_blank_entries() {
        let list = AllowList::parse("a@b.c,,A@B.C, ,sub:x,sub:x").unwrap();
        assert_eq!(list.identities(), &[email("a@b.c"), subject("x")]);
    }

    #[test]
    fn empty_allow_lists_are_refused() {
        for text in ["", " ", ",", " , ,"] {
            assert_eq!(
                AllowList::parse(text),
                Err(vec![AllowListProblem::Empty]),
                "{text:?}"
            );
        }
    }

    #[test]
    fn every_bad_entry_is_reported() {
        let problems =
            AllowList::parse("alice@example.com, bob, sub:, sub: x, *@example.com.").unwrap_err();
        assert_eq!(problems.len(), 4);
        assert_eq!(
            problems[0].to_string(),
            "allow-list entry \"bob\" is not a plausible email address"
        );
        assert_eq!(
            problems[1].to_string(),
            "allow-list entry \"sub:\" is not a usable subject"
        );
        assert_eq!(
            problems[2].to_string(),
            "allow-list entry \"sub: x\" is not a usable subject"
        );
    }

    #[test]
    fn sub_prefix_is_case_sensitive() {
        // "SUB:x" is not the subject form, and is not an address either.
        assert!(AllowList::parse("SUB:x").is_err());
    }

    #[test]
    fn oversized_allow_list_is_refused() {
        let text: Vec<String> = (0..=MAX_ALLOW_LIST_ENTRIES)
            .map(|i| format!("u{i}@example.com"))
            .collect();
        assert_eq!(
            AllowList::parse(&text.join(",")),
            Err(vec![AllowListProblem::TooLong])
        );
    }

    #[test]
    fn identities_round_trip_through_json_with_their_kind() {
        for identity in [email("Alice@Example.com"), subject("alice@example.com")] {
            let json = serde_json::to_string(&identity).unwrap();
            let back: Identity = serde_json::from_str(&json).unwrap();
            assert_eq!(back, identity);
        }
        assert_eq!(
            serde_json::to_string(&subject("x")).unwrap(),
            r#"{"kind":"sub","value":"x"}"#
        );
        // What is stored is validated again when read.
        assert!(serde_json::from_str::<Identity>(r#"{"kind":"email","value":"nope"}"#).is_err());
        let stored: Identity = serde_json::from_str(r#"{"kind":"email","value":"A@B.C"}"#).unwrap();
        assert_eq!(stored, email("a@b.c"));
    }

    #[test]
    fn debug_cannot_break_a_log_line() {
        // Control characters cannot be in an identity, and Debug is one line.
        let text = format!("{:?}", subject("a\u{e9}b"));
        assert_eq!(text, "Identity(sub:a?b)");
    }
}
