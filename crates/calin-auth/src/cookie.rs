//! The secrets a client presents, and the only code that reads them from
//! request headers or writes them into response headers (spec sections 2 to 4).
//!
//! The HTTP layer passes header values through as text and gets back a typed
//! token or nothing; it never assembles a cookie by hand.

use crate::secret::{
    RandomError, TOKEN_CHARS, has_token_shape, random_token, redacted_debug, sha256,
};

/// Name of the session cookie (spec section 3). The `__Host-` prefix makes
/// browsers refuse it unless it is `Secure`, has `Path=/` and no `Domain`,
/// so a neighbouring subdomain cannot plant one.
pub const SESSION_COOKIE: &str = "__Host-calin_session";
/// Name of the cookie that references a pending login (spec section 2).
/// `__Secure-` and not `__Host-`, because it is limited to `Path=/auth`.
pub const PENDING_LOGIN_COOKIE: &str = "__Secure-calin_login";
/// Prefix of every device token (spec section 4).
pub const DEVICE_TOKEN_PREFIX: &str = "calin_";

/// Lifetime of a session without use, and of its cookie: 30 days.
pub(crate) const SESSION_LIFETIME_SECS: u64 = 30 * 24 * 60 * 60;
/// Lifetime of a pending login and of its cookie: 10 minutes.
pub(crate) const PENDING_LOGIN_LIFETIME_SECS: u64 = 10 * 60;

/// Session cookies one sign-out request may end. A browser sends one; two
/// or three mean someone planted the others.
const MAX_SIGN_OUT_TOKENS: usize = 8;
/// An `Authorization` header longer than this is not looked at.
const MAX_AUTHORIZATION_BYTES: usize = 256;

const CLEAR_SESSION_COOKIE: &str =
    "__Host-calin_session=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax";
const CLEAR_PENDING_LOGIN_COOKIE: &str =
    "__Secure-calin_login=; Path=/auth; Max-Age=0; HttpOnly; Secure; SameSite=Lax";

/// A session token as presented in the `__Host-calin_session` cookie.
///
/// Holding one proves nothing: it is a candidate until the store finds its
/// hash. It has no `Display` and its `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken(String);
redacted_debug!(SessionToken);

impl SessionToken {
    pub(crate) fn generate() -> Result<Self, RandomError> {
        random_token().map(Self)
    }

    /// Reads the session token from the request's `Cookie` header values
    /// (HTTP/2 may split the header into several fields: pass them all).
    ///
    /// Returns nothing when the cookie is absent, is not shaped like a token,
    /// or appears with two different values. The `__Host-` prefix keeps a
    /// conforming browser from holding two; if two arrive anyway, one was
    /// planted to swap the user into another session, and there is no
    /// telling which one is genuine. Only the prefixed name is read.
    pub fn from_cookie_headers<'a>(headers: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        single_cookie(headers, SESSION_COOKIE)
            .filter(|value| has_token_shape(value))
            .map(|value| Self(value.to_owned()))
    }

    /// Every distinct, well-shaped session token among the request's
    /// cookies, for sign-out: when a second session cookie was planted,
    /// [`SessionToken::from_cookie_headers`] rightly authenticates nobody,
    /// but signing out must still end whichever of them is a session.
    pub(crate) fn all_from_cookie_headers<'a>(
        headers: impl IntoIterator<Item = &'a str>,
    ) -> Vec<Self> {
        let mut tokens: Vec<Self> = Vec::new();
        for value in cookie_values(headers, SESSION_COOKIE).filter_map(unquoted) {
            if has_token_shape(value) && !tokens.iter().any(|token| token.0 == value) {
                tokens.push(Self(value.to_owned()));
            }
            if tokens.len() == MAX_SIGN_OUT_TOKENS {
                break;
            }
        }
        tokens
    }

    /// The `Set-Cookie` value that stores this token in the browser, with
    /// the attributes of spec section 3 and a 30-day `Max-Age`.
    pub fn set_cookie(&self) -> String {
        format!(
            "{SESSION_COOKIE}={}; Path=/; Max-Age={SESSION_LIFETIME_SECS}; HttpOnly; Secure; SameSite=Lax",
            self.0
        )
    }

    /// The `Set-Cookie` value that removes the session cookie.
    pub fn clear_cookie() -> &'static str {
        CLEAR_SESSION_COOKIE
    }

    pub(crate) fn hash(&self) -> [u8; 32] {
        sha256(self.0.as_bytes())
    }
}

/// The reference to a pending login, as carried by the `__Secure-calin_login` cookie.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingLoginRef(String);
redacted_debug!(PendingLoginRef);

impl PendingLoginRef {
    pub(crate) fn generate() -> Result<Self, RandomError> {
        random_token().map(Self)
    }

    /// Reads the reference from the request's `Cookie` header values, with
    /// the same rules as [`SessionToken::from_cookie_headers`].
    pub fn from_cookie_headers<'a>(headers: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        single_cookie(headers, PENDING_LOGIN_COOKIE)
            .filter(|value| has_token_shape(value))
            .map(|value| Self(value.to_owned()))
    }

    /// The `Set-Cookie` value: sent only to `/auth`, gone after 10 minutes.
    pub(crate) fn set_cookie(&self) -> String {
        format!(
            "{PENDING_LOGIN_COOKIE}={}; Path=/auth; Max-Age={PENDING_LOGIN_LIFETIME_SECS}; HttpOnly; Secure; SameSite=Lax",
            self.0
        )
    }

    /// The `Set-Cookie` value that removes the pending-login cookie. Send it
    /// with every answer to `/auth/callback`, success or not.
    pub fn clear_cookie() -> &'static str {
        CLEAR_PENDING_LOGIN_COOKIE
    }

    pub(crate) fn hash(&self) -> [u8; 32] {
        sha256(self.0.as_bytes())
    }
}

/// A device token: `calin_` followed by 256 random bits (spec section 4).
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceToken(String);
redacted_debug!(DeviceToken);

impl DeviceToken {
    pub(crate) fn generate() -> Result<Self, RandomError> {
        Ok(Self(format!("{DEVICE_TOKEN_PREFIX}{}", random_token()?)))
    }

    /// Reads a device token from an `Authorization` header value.
    ///
    /// Returns nothing unless the header is `Bearer` (any case) followed by
    /// something shaped exactly like a device token.
    pub fn from_authorization_header(header: &str) -> Option<Self> {
        Self::parse(bearer_credentials(header)?)
    }

    /// Reads a device token from its bare text, as a user pastes it.
    pub fn parse(text: &str) -> Option<Self> {
        let random = text.strip_prefix(DEVICE_TOKEN_PREFIX)?;
        has_token_shape(random).then(|| Self(text.to_owned()))
    }

    /// The token itself. For showing it once at creation, and for a client
    /// that sends it; never for logs.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub(crate) fn hash(&self) -> [u8; 32] {
        sha256(self.0.as_bytes())
    }
}

/// The credentials of an `Authorization: Bearer` header, or nothing when the
/// header uses another scheme or is malformed.
pub(crate) fn bearer_credentials(header: &str) -> Option<&str> {
    if header.len() > MAX_AUTHORIZATION_BYTES {
        return None;
    }
    let (scheme, rest) = header.trim_matches(' ').split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let credentials = rest.trim_start_matches(' ');
    (!credentials.is_empty() && !credentials.contains(' ')).then_some(credentials)
}

/// Whether an `Authorization` header value uses the `Bearer` scheme at all,
/// whatever follows it.
pub(crate) fn is_bearer(header: &str) -> bool {
    let scheme = header
        .trim_start_matches(' ')
        .split(' ')
        .next()
        .unwrap_or("");
    scheme.eq_ignore_ascii_case("Bearer")
}

/// Every value of the cookie `name` across `headers`, trimmed, in order.
fn cookie_values<'a>(
    headers: impl IntoIterator<Item = &'a str>,
    name: &'a str,
) -> impl Iterator<Item = &'a str> {
    headers
        .into_iter()
        .flat_map(|header| header.split(';'))
        .filter_map(|pair| pair.split_once('='))
        .filter(move |(pair_name, _)| pair_name.trim_matches([' ', '\t']) == name)
        .map(|(_, value)| value.trim_matches([' ', '\t']))
}

/// A cookie value without the double quotes it may be wrapped in, or
/// nothing when it is longer than any token: bounded before anything else
/// looks at it.
fn unquoted(value: &str) -> Option<&str> {
    if value.len() > TOKEN_CHARS + 2 {
        return None;
    }
    Some(
        value
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(value),
    )
}

/// The value of the cookie `name`, when every occurrence across `headers`
/// carries the same value.
fn single_cookie<'a>(headers: impl IntoIterator<Item = &'a str>, name: &'a str) -> Option<&'a str> {
    let mut found: Option<&'a str> = None;
    for value in cookie_values(headers, name) {
        let value = unquoted(value)?;
        match found {
            Some(previous) if previous != value => return None,
            _ => found = Some(value),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA-";
    const OTHER: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB_";

    fn session(headers: &[&str]) -> Option<String> {
        SessionToken::from_cookie_headers(headers.iter().copied()).map(|token| token.0)
    }

    #[test]
    fn session_cookie_has_exactly_the_specified_attributes() {
        let token = SessionToken(TOKEN.to_owned());
        assert_eq!(
            token.set_cookie(),
            format!(
                "__Host-calin_session={TOKEN}; Path=/; Max-Age=2592000; HttpOnly; Secure; SameSite=Lax"
            )
        );
        assert_eq!(
            SessionToken::clear_cookie(),
            "__Host-calin_session=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax"
        );
    }

    #[test]
    fn pending_login_cookie_has_exactly_the_specified_attributes() {
        let pending = PendingLoginRef(TOKEN.to_owned());
        assert_eq!(
            pending.set_cookie(),
            format!(
                "__Secure-calin_login={TOKEN}; Path=/auth; Max-Age=600; HttpOnly; Secure; SameSite=Lax"
            )
        );
        assert_eq!(
            PendingLoginRef::clear_cookie(),
            "__Secure-calin_login=; Path=/auth; Max-Age=0; HttpOnly; Secure; SameSite=Lax"
        );
    }

    #[test]
    fn what_is_set_is_what_is_read_back() {
        let token = SessionToken::generate().unwrap();
        let set = token.set_cookie();
        let pair = set.split(';').next().unwrap();
        assert_eq!(SessionToken::from_cookie_headers([pair]), Some(token));

        let pending = PendingLoginRef::generate().unwrap();
        let set = pending.set_cookie();
        let pair = set.split(';').next().unwrap();
        assert_eq!(PendingLoginRef::from_cookie_headers([pair]), Some(pending));
    }

    #[test]
    fn session_cookie_parsing_table() {
        let ok = Some(TOKEN.to_owned());
        let cases: Vec<(Vec<String>, Option<String>)> = vec![
            (vec![format!("__Host-calin_session={TOKEN}")], ok.clone()),
            (
                vec![format!("theme=dark; __Host-calin_session={TOKEN}; lang=fr")],
                ok.clone(),
            ),
            (
                vec![format!("theme=dark;__Host-calin_session={TOKEN};lang=fr")],
                ok.clone(),
            ),
            (
                vec![format!("  __Host-calin_session = {TOKEN} ")],
                ok.clone(),
            ),
            (
                vec![format!("__Host-calin_session=\"{TOKEN}\"")],
                ok.clone(),
            ),
            // Split across several header fields, as HTTP/2 allows.
            (
                vec![
                    "theme=dark".to_owned(),
                    format!("__Host-calin_session={TOKEN}"),
                ],
                ok.clone(),
            ),
            // The same value twice is not a conflict.
            (
                vec![format!(
                    "__Host-calin_session={TOKEN}; __Host-calin_session={TOKEN}"
                )],
                ok.clone(),
            ),
            // Other cookies with lookalike names are ignored.
            (
                vec![format!(
                    "x__Host-calin_session={OTHER}; __Host-calin_session={TOKEN}; __Host-calin_session2={OTHER}"
                )],
                ok.clone(),
            ),
            (
                vec![format!(
                    "__Secure-calin_login={OTHER}; __Host-calin_session={TOKEN}"
                )],
                ok.clone(),
            ),
            (
                vec![format!(
                    "a=b=c; =x; ; novalue; __Host-calin_session={TOKEN}"
                )],
                ok.clone(),
            ),
            // Two different values: refuse both.
            (
                vec![format!(
                    "__Host-calin_session={OTHER}; __Host-calin_session={TOKEN}"
                )],
                None,
            ),
            (
                vec![
                    format!("__Host-calin_session={TOKEN}"),
                    format!("__Host-calin_session={OTHER}"),
                ],
                None,
            ),
            (
                vec![format!(
                    "__Host-calin_session=garbage; __Host-calin_session={TOKEN}"
                )],
                None,
            ),
            (
                vec![format!(
                    "__Host-calin_session=; __Host-calin_session={TOKEN}"
                )],
                None,
            ),
            // Absent, empty, misspelled, wrong shape.
            (vec![], None),
            (vec![String::new()], None),
            (vec!["theme=dark".to_owned()], None),
            (vec!["__Host-calin_session".to_owned()], None),
            (vec!["__Host-calin_session=".to_owned()], None),
            (vec![format!("__host-calin_session={TOKEN}")], None),
            (vec![format!("__HOST-CALIN_SESSION={TOKEN}")], None),
            // The names without their prefix, or with the other one, are
            // not the session cookie.
            (vec![format!("calin_session={TOKEN}")], None),
            (vec![format!("Host-calin_session={TOKEN}")], None),
            (vec![format!("__Secure-calin_session={TOKEN}")], None),
            (vec![format!("__Host-calin_login={TOKEN}")], None),
            (vec![format!("__Host-calin_session={}", &TOKEN[1..])], None),
            (vec![format!("__Host-calin_session={TOKEN}A")], None),
            (vec![format!("__Host-calin_session={}+", &TOKEN[1..])], None),
            (
                vec![format!("__Host-calin_session={}%41", &TOKEN[3..])],
                None,
            ),
            (vec![format!("__Host-calin_session=\"{TOKEN}")], None),
            (
                vec![format!("__Host-calin_session={}", "A".repeat(100_000))],
                None,
            ),
            (
                vec![format!(
                    "__Host-calin_session={TOKEN}, __Host-calin_session={TOKEN}"
                )],
                None,
            ),
            (vec![";;;===;;;".to_owned()], None),
        ];
        for (headers, expected) in cases {
            let refs: Vec<&str> = headers.iter().map(String::as_str).collect();
            let shown: Vec<String> = headers
                .iter()
                .map(|h| h.chars().take(80).collect())
                .collect();
            assert_eq!(session(&refs), expected, "{shown:?}");
        }
    }

    #[test]
    fn sign_out_sees_every_session_cookie() {
        let all = |headers: &[&str]| -> Vec<String> {
            SessionToken::all_from_cookie_headers(headers.iter().copied())
                .into_iter()
                .map(|token| token.0)
                .collect()
        };
        let both =
            format!("__Host-calin_session={OTHER}; theme=dark; __Host-calin_session=\"{TOKEN}\"");
        assert_eq!(all(&[&both]), [OTHER, TOKEN]);
        let repeated = format!("__Host-calin_session={TOKEN}; __Host-calin_session={TOKEN}");
        assert_eq!(all(&[&repeated, &repeated]), [TOKEN]);
        let with_junk = format!(
            "__Host-calin_session=garbage; __Host-calin_session={}; __Host-calin_session={TOKEN}; __Secure-calin_login={OTHER}",
            "A".repeat(5000)
        );
        assert_eq!(all(&[&with_junk]), [TOKEN]);
        assert!(all(&[]).is_empty());
        assert!(all(&["theme=dark", "__Host-calin_session="]).is_empty());

        // Bounded, however many are sent.
        let many: Vec<String> = (0..50)
            .map(|n| format!("__Host-calin_session={n:0>43}"))
            .collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(all(&many).len(), MAX_SIGN_OUT_TOKENS);
    }

    #[test]
    fn an_oversized_value_ends_the_search() {
        // A huge value is not kept around to be compared with later ones.
        let header = format!(
            "__Host-calin_session={}; __Host-calin_session={TOKEN}",
            "A".repeat(4096)
        );
        assert_eq!(session(&[&header]), None);
    }

    #[test]
    fn pending_login_cookie_is_read_from_its_own_name_only() {
        let header = format!("__Host-calin_session={OTHER}; __Secure-calin_login={TOKEN}");
        assert_eq!(
            PendingLoginRef::from_cookie_headers([header.as_str()]).map(|p| p.0),
            Some(TOKEN.to_owned())
        );
        let header = format!("__Host-calin_session={TOKEN}");
        assert_eq!(
            PendingLoginRef::from_cookie_headers([header.as_str()]),
            None
        );
        for name in ["calin_login", "__Host-calin_login", "__secure-calin_login"] {
            let header = format!("{name}={TOKEN}");
            assert_eq!(
                PendingLoginRef::from_cookie_headers([header.as_str()]),
                None
            );
        }
        let header = format!("__Secure-calin_login={TOKEN}; __Secure-calin_login={OTHER}");
        assert_eq!(
            PendingLoginRef::from_cookie_headers([header.as_str()]),
            None
        );
    }

    #[test]
    fn bearer_parsing_table() {
        let token = format!("calin_{TOKEN}");
        let ok = Some(token.clone());
        let cases: Vec<(String, Option<String>)> = vec![
            (format!("Bearer {token}"), ok.clone()),
            (format!("bearer {token}"), ok.clone()),
            (format!("BEARER {token}"), ok.clone()),
            (format!("Bearer    {token}"), ok.clone()),
            (format!(" Bearer {token} "), ok.clone()),
            (String::new(), None),
            ("Bearer".to_owned(), None),
            ("Bearer ".to_owned(), None),
            (token.clone(), None),
            (format!("Bearer{token}"), None),
            (format!("Basic {token}"), None),
            (format!("Token {token}"), None),
            (format!("Bearer {token} extra"), None),
            (format!("Bearer {token},Bearer {token}"), None),
            (format!("Bearer\t{token}"), None),
            (format!("Bearer {TOKEN}"), None),
            (format!("Bearer Calin_{TOKEN}"), None),
            (format!("Bearer calin_{}", &TOKEN[1..]), None),
            (format!("Bearer calin_{TOKEN}A"), None),
            (format!("Bearer calin_{}=", &TOKEN[1..]), None),
            (format!("Bearer {}", "A".repeat(100_000)), None),
            (format!("Bearer {token}{}", " ".repeat(300)), None),
        ];
        for (header, expected) in cases {
            let shown: String = header.chars().take(80).collect();
            assert_eq!(
                DeviceToken::from_authorization_header(&header).map(|t| t.0),
                expected,
                "{shown:?}"
            );
        }
    }

    #[test]
    fn device_tokens_carry_the_prefix_and_parse_back() {
        let token = DeviceToken::generate().unwrap();
        assert!(token.expose().starts_with("calin_"));
        assert_eq!(token.expose().len(), 6 + 43);
        assert_eq!(DeviceToken::parse(token.expose()), Some(token.clone()));
        let header = format!("Bearer {}", token.expose());
        assert_eq!(DeviceToken::from_authorization_header(&header), Some(token));
        assert_eq!(DeviceToken::parse(TOKEN), None);
    }

    #[test]
    fn bearer_scheme_detection() {
        for header in ["Bearer x", "bearer", " BEARER  y z", "Bearer "] {
            assert!(is_bearer(header), "{header:?}");
        }
        for header in ["", "Basic abc", "Bearerx y", "Digest Bearer"] {
            assert!(!is_bearer(header), "{header:?}");
        }
    }

    #[test]
    fn debug_never_shows_a_token() {
        let session = SessionToken(TOKEN.to_owned());
        let pending = PendingLoginRef(TOKEN.to_owned());
        let device = DeviceToken(format!("calin_{TOKEN}"));
        for text in [
            format!("{session:?}"),
            format!("{pending:?}"),
            format!("{device:?}"),
        ] {
            assert!(!text.contains(TOKEN), "{text}");
            assert!(text.ends_with("(REDACTED)"), "{text}");
        }
    }

    #[test]
    fn hashes_are_of_the_whole_presented_text() {
        let device = DeviceToken(format!("calin_{TOKEN}"));
        assert_eq!(device.hash(), sha256(format!("calin_{TOKEN}").as_bytes()));
        assert_ne!(device.hash(), SessionToken(TOKEN.to_owned()).hash());
    }
}
