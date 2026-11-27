//! Where to send the browser after sign-in (spec section 2, step 1).

use std::fmt;

use url::Url;

const MAX_NEXT_BYTES: usize = 2048;

/// A path on this origin to redirect to after sign-in. Can only be built by
/// [`NextPath::sanitize`], so holding one means the open-redirect checks ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextPath(String);

impl NextPath {
    /// Keeps `raw` if it is a path on this origin and nothing else,
    /// otherwise answers `/`.
    ///
    /// `raw` is the decoded value of the `next` query parameter. It is kept
    /// only if it starts with exactly one `/`, is printable ASCII without
    /// spaces or backslashes (browsers read `\` as `/` and drop tabs and
    /// newlines, which turns `/\evil.example` into another site), and still
    /// resolves to this origin when a URL parser reads it.
    pub fn sanitize(raw: Option<&str>) -> Self {
        match raw {
            Some(raw) if is_local_path(raw) => Self(raw.to_owned()),
            _ => Self::root(),
        }
    }

    /// The default destination, `/`.
    pub fn root() -> Self {
        Self("/".to_owned())
    }

    /// The path, ready for a `Location` header.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NextPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_local_path(raw: &str) -> bool {
    if raw.len() > MAX_NEXT_BYTES || !raw.starts_with('/') || raw.starts_with("//") {
        return false;
    }
    if !raw.bytes().all(|b| b.is_ascii_graphic() && b != b'\\') {
        return false;
    }
    // Second opinion from a real parser: resolved against a placeholder
    // origin, the result must still be on that origin.
    const BASE: &str = "https://calin.invalid";
    let Ok(base) = Url::parse(BASE) else {
        return false;
    };
    match base.join(raw) {
        Ok(joined) => joined.origin() == base.origin() && joined.as_str().starts_with(BASE),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_sanitization_table() {
        let long_ok = format!("/{}", "a".repeat(MAX_NEXT_BYTES - 1));
        let too_long = format!("/{}", "a".repeat(MAX_NEXT_BYTES));
        let cases: &[(Option<&str>, &str)] = &[
            // Kept as they are.
            (Some("/"), "/"),
            (Some("/sessions"), "/sessions"),
            (
                Some("/sessions/01KB2M5S7Q9ZC4X8HVTPA3DWEN"),
                "/sessions/01KB2M5S7Q9ZC4X8HVTPA3DWEN",
            ),
            (Some("/ok?x=1#y"), "/ok?x=1#y"),
            (
                Some("/a/b/?q=%2F%2Fevil.example"),
                "/a/b/?q=%2F%2Fevil.example",
            ),
            (
                Some("/a?next=https://evil.example"),
                "/a?next=https://evil.example",
            ),
            (Some("/@evil.example"), "/@evil.example"),
            (Some("/a//b"), "/a//b"),
            (Some("/%5Cevil.example"), "/%5Cevil.example"),
            (Some("/caf%C3%A9"), "/caf%C3%A9"),
            (Some("/a:b"), "/a:b"),
            (Some(long_ok.as_str()), long_ok.as_str()),
            // Anything else becomes "/".
            (None, "/"),
            (Some(""), "/"),
            (Some("//evil.example"), "/"),
            (Some("///evil.example"), "/"),
            (Some("/\\evil"), "/"),
            (Some("/\\/evil.example"), "/"),
            (Some("\\\\evil.example"), "/"),
            (Some("/a\\b"), "/"),
            (Some("https://evil.example"), "/"),
            (Some("https://evil.example/"), "/"),
            (Some("http://localhost/"), "/"),
            (Some("//"), "/"),
            (Some("evil.example"), "/"),
            (Some("sessions"), "/"),
            (Some("./sessions"), "/"),
            (Some("../sessions"), "/"),
            (Some("?x=1"), "/"),
            (Some("#frag"), "/"),
            (Some("javascript:alert(1)"), "/"),
            (Some("data:text/html,x"), "/"),
            (Some(" /sessions"), "/"),
            (Some("/sessions "), "/"),
            (Some("/a b"), "/"),
            (Some("/\t/evil.example"), "/"),
            (Some("/\n/evil.example"), "/"),
            (Some("/\r\nLocation: https://evil.example"), "/"),
            (Some("/a\u{0}b"), "/"),
            (Some("/a\u{7f}b"), "/"),
            (Some("/caf\u{e9}"), "/"),
            (Some("/\u{202e}evil"), "/"),
            (Some("\u{ff0f}\u{ff0f}evil.example"), "/"),
            (Some(too_long.as_str()), "/"),
        ];
        for (raw, expected) in cases {
            let shown = raw.map(|r| r.chars().take(60).collect::<String>());
            assert_eq!(NextPath::sanitize(*raw).as_str(), *expected, "{shown:?}");
        }
    }

    #[test]
    fn root_is_slash_and_display_is_the_path() {
        assert_eq!(NextPath::root().as_str(), "/");
        assert_eq!(NextPath::sanitize(Some("/x?y")).to_string(), "/x?y");
    }
}
