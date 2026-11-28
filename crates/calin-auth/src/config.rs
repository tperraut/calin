//! The sign-in configuration of spec section 2, parsed and validated once.
//!
//! The outcome is local mode, a complete public-mode configuration, or an
//! error that lists everything wrong: a server never starts half configured.

use std::fmt;

use url::Url;

use crate::identity::{AllowList, AllowListProblem};
use crate::secret::redacted_debug;

/// The public origin. Setting it selects public mode.
pub const ENV_PUBLIC_URL: &str = "CALIN_PUBLIC_URL";
/// The provider's issuer URL.
pub const ENV_OIDC_ISSUER: &str = "CALIN_OIDC_ISSUER";
/// The client id registered with the provider.
pub const ENV_OIDC_CLIENT_ID: &str = "CALIN_OIDC_CLIENT_ID";
/// The client secret. Optional: without it Calin is a public client.
pub const ENV_OIDC_CLIENT_SECRET: &str = "CALIN_OIDC_CLIENT_SECRET";
/// Comma-separated identities that may sign in.
pub const ENV_ALLOWED_USERS: &str = "CALIN_ALLOWED_USERS";

/// Path of the redirect URI under the public origin.
pub const CALLBACK_PATH: &str = "/auth/callback";

const MAX_CLIENT_ID_BYTES: usize = 512;
const MAX_URL_BYTES: usize = 2048;

/// Whether plain `http` to `localhost` or `127.0.0.1` is acceptable.
///
/// Production is always [`LoopbackHttp::Refuse`]. The other value exists for
/// development and for tests, which run a provider on a loopback port; it
/// covers the public URL, the issuer and the provider's endpoints, and never
/// any other host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackHttp {
    /// Only `https` anywhere.
    Refuse,
    /// Also `http://localhost:<port>` and `http://127.0.0.1:<port>`.
    AllowForDevelopment,
}

/// The mode the server runs in (spec section 1).
#[derive(Debug, Clone)]
pub enum AuthMode {
    /// No public URL: loopback only, no sign-in.
    Local,
    /// A public URL and everything sign-in needs.
    Public(PublicConfig),
}

impl AuthMode {
    /// Reads the configuration from the process environment. The only place
    /// in this crate that does.
    pub fn from_process_environment(loopback_http: LoopbackHttp) -> Result<Self, ConfigError> {
        Self::from_lookup(
            // A value that is not Unicode is kept as a set value so it is
            // reported, not mistaken for an unset variable.
            |name| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned()),
            loopback_http,
        )
    }

    /// Reads the configuration through `lookup`, which answers for each
    /// `CALIN_*` variable name (`|name| map.get(name).cloned()` for a map).
    ///
    /// - `CALIN_PUBLIC_URL` unset and no other sign-in variable set: local.
    /// - `CALIN_PUBLIC_URL` unset but another sign-in variable set: an error,
    ///   because an operator who configured sign-in must not get a server
    ///   without it.
    /// - `CALIN_PUBLIC_URL` set: public, or an error naming every variable
    ///   that is missing or malformed.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
        loopback_http: LoopbackHttp,
    ) -> Result<Self, ConfigError> {
        // A variable set to nothing but blanks counts as unset, except the
        // public URL: an empty one must not silently select local mode.
        let set = |name: &str| lookup(name).filter(|value| !value.trim().is_empty());
        let issuer = set(ENV_OIDC_ISSUER);
        let client_id = set(ENV_OIDC_CLIENT_ID);
        let client_secret = set(ENV_OIDC_CLIENT_SECRET);
        let allowed_users = set(ENV_ALLOWED_USERS);

        let Some(public_url) = lookup(ENV_PUBLIC_URL) else {
            return Self::without_public_url([
                (ENV_OIDC_ISSUER, issuer.is_some()),
                (ENV_OIDC_CLIENT_ID, client_id.is_some()),
                (ENV_OIDC_CLIENT_SECRET, client_secret.is_some()),
                (ENV_ALLOWED_USERS, allowed_users.is_some()),
            ]);
        };

        // Every variable is checked, in this order, so the error lists every
        // problem at once.
        let mut problems = Vec::new();
        let origin = parse_public_url(public_url.trim(), loopback_http)
            .map_err(|reason| invalid(ENV_PUBLIC_URL, reason));
        let origin = keep(origin, &mut problems);
        let issuer = keep(read_issuer(issuer.as_deref(), loopback_http), &mut problems);
        let client_id = keep(read_client_id(client_id.as_deref()), &mut problems);
        let client_secret = keep(read_client_secret(client_secret), &mut problems).flatten();
        let allow_list = AllowList::parse(allowed_users.as_deref().unwrap_or(""))
            .map_err(|list| problems.extend(list.into_iter().map(ConfigProblem::AllowList)))
            .ok();

        match (origin, issuer, client_id, allow_list) {
            (Some(origin), Some(issuer), Some(client_id), Some(allow_list))
                if problems.is_empty() =>
            {
                Ok(Self::Public(PublicConfig {
                    authority: authority_of(&origin),
                    origin,
                    issuer,
                    client_id,
                    client_secret,
                    allow_list,
                    loopback_http,
                }))
            }
            _ => Err(ConfigError { problems }),
        }
    }

    /// Local mode, unless a sign-in variable is set (`present` pairs each
    /// name with whether it is).
    fn without_public_url(present: [(&'static str, bool); 4]) -> Result<Self, ConfigError> {
        let present: Vec<&'static str> = present
            .into_iter()
            .filter_map(|(name, is_set)| is_set.then_some(name))
            .collect();
        if present.is_empty() {
            return Ok(Self::Local);
        }
        Err(ConfigError {
            problems: vec![ConfigProblem::PublicUrlNotSet { present }],
        })
    }
}

/// The value, or `None` with the problem recorded.
fn keep<T>(result: Result<T, ConfigProblem>, problems: &mut Vec<ConfigProblem>) -> Option<T> {
    result.map_err(|problem| problems.push(problem)).ok()
}

fn invalid(variable: &'static str, reason: &'static str) -> ConfigProblem {
    ConfigProblem::Invalid { variable, reason }
}

fn read_issuer(issuer: Option<&str>, loopback_http: LoopbackHttp) -> Result<String, ConfigProblem> {
    let issuer = issuer
        .map(str::trim)
        .ok_or(ConfigProblem::Missing(ENV_OIDC_ISSUER))?;
    check_issuer(issuer, loopback_http).map_err(|reason| invalid(ENV_OIDC_ISSUER, reason))?;
    Ok(issuer.to_owned())
}

fn read_client_id(client_id: Option<&str>) -> Result<String, ConfigProblem> {
    let id = client_id
        .map(str::trim)
        .ok_or(ConfigProblem::Missing(ENV_OIDC_CLIENT_ID))?;
    if id.len() > MAX_CLIENT_ID_BYTES {
        return Err(invalid(ENV_OIDC_CLIENT_ID, "is too long"));
    }
    if id.chars().any(char::is_control) {
        return Err(invalid(ENV_OIDC_CLIENT_ID, "contains a control character"));
    }
    Ok(id.to_owned())
}

/// Not trimmed: a secret is used byte for byte, so stray blanks (a CRLF env
/// file) are an error here, not a refusal at the provider.
fn read_client_secret(secret: Option<String>) -> Result<Option<ClientSecret>, ConfigProblem> {
    match secret {
        Some(secret) if secret.trim() != secret || secret.chars().any(char::is_control) => {
            Err(invalid(
                ENV_OIDC_CLIENT_SECRET,
                "has surrounding whitespace or a control character",
            ))
        }
        secret => Ok(secret.map(ClientSecret)),
    }
}

/// `calin.example.com[:port]` from a normalized origin.
fn authority_of(origin: &str) -> String {
    origin
        .split_once("://")
        .map(|(_, authority)| authority.to_owned())
        .unwrap_or_default()
}

/// A complete public-mode configuration. Exists only if every part is valid.
#[derive(Debug, Clone)]
pub struct PublicConfig {
    origin: String,
    authority: String,
    issuer: String,
    client_id: String,
    client_secret: Option<ClientSecret>,
    allow_list: AllowList,
    loopback_http: LoopbackHttp,
}

impl PublicConfig {
    /// The public origin, normalized: `https://calin.example.com`, no
    /// trailing slash. What the Origin guard compares against.
    pub fn public_origin(&self) -> &str {
        &self.origin
    }

    /// Host and, when not the default, port of the public origin:
    /// `calin.example.com` or `calin.example.com:8443`. What the Host guard
    /// compares against.
    pub fn public_authority(&self) -> &str {
        &self.authority
    }

    /// The redirect URI to register with the provider.
    pub fn redirect_uri(&self) -> String {
        format!("{}{CALLBACK_PATH}", self.origin)
    }

    /// The issuer exactly as configured.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The client id registered with the provider.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Whether a client secret is configured (a confidential client).
    pub fn has_client_secret(&self) -> bool {
        self.client_secret.is_some()
    }

    pub(crate) fn client_secret(&self) -> Option<&str> {
        self.client_secret.as_ref().map(|secret| secret.0.as_str())
    }

    /// Who may sign in.
    pub fn allow_list(&self) -> &AllowList {
        &self.allow_list
    }

    /// The loopback exception this configuration was parsed with.
    pub fn loopback_http(&self) -> LoopbackHttp {
        self.loopback_http
    }
}

#[derive(Clone)]
struct ClientSecret(String);
redacted_debug!(ClientSecret);

/// Everything wrong with the configuration, all at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    problems: Vec<ConfigProblem>,
}

impl ConfigError {
    /// Each problem found. Never empty.
    pub fn problems(&self) -> &[ConfigProblem] {
        &self.problems
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sign-in is not configured correctly: ")?;
        for (index, problem) in self.problems.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

/// One thing wrong with the configuration. Never quotes a URL or the secret:
/// either could carry credentials into a log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigProblem {
    /// A required variable is unset or blank.
    #[error("{0} is not set")]
    Missing(&'static str),
    /// A variable is set to something unusable.
    #[error("{variable} {reason}")]
    Invalid {
        /// The variable's name.
        variable: &'static str,
        /// What is wrong with its value.
        reason: &'static str,
    },
    /// A problem with `CALIN_ALLOWED_USERS`.
    #[error("CALIN_ALLOWED_USERS: {0}")]
    AllowList(AllowListProblem),
    /// Sign-in variables are set without a public URL.
    #[error(
        "CALIN_PUBLIC_URL is not set although {} set; refusing to run without the sign-in that was configured",
        list_with_verb(.present)
    )]
    PublicUrlNotSet {
        /// The sign-in variables that are set.
        present: Vec<&'static str>,
    },
}

fn list_with_verb(names: &[&'static str]) -> String {
    let verb = if names.len() == 1 { "is" } else { "are" };
    format!("{} {verb}", names.join(", "))
}

/// Whether `url` may be contacted or redirected to: `https`, or `http` to a
/// loopback name with an explicit port under the development flag.
pub(crate) fn check_scheme(url: &Url, loopback_http: LoopbackHttp) -> Result<(), &'static str> {
    if url.host_str().is_none_or(str::is_empty) {
        return Err("has no host");
    }
    match url.scheme() {
        "https" => Ok(()),
        "http"
            if loopback_http == LoopbackHttp::AllowForDevelopment
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
                && url.port().is_some() =>
        {
            Ok(())
        }
        _ => Err("must be an https URL"),
    }
}

/// Parses text into a URL only when the parser has nothing to repair: the
/// URL standard silently drops tabs and newlines and reads `\` as `/`, which
/// is how a value that looks harmless ends up pointing elsewhere.
pub(crate) fn parse_strict_url(text: &str) -> Result<Url, &'static str> {
    if text.len() > MAX_URL_BYTES {
        return Err("is too long");
    }
    if text.chars().any(the_parser_repairs) {
        return Err("contains whitespace, a control character or a backslash");
    }
    if !(text.starts_with("https://") || text.starts_with("http://")) {
        return Err("must be an https URL");
    }
    let url = Url::parse(text).map_err(|_| "is not a valid URL")?;
    if has_credentials(&url) {
        return Err("must not contain credentials");
    }
    Ok(url)
}

/// A character the URL parser drops or rewrites instead of refusing.
fn the_parser_repairs(c: char) -> bool {
    c.is_whitespace() || c.is_control() || c == '\\'
}

fn has_credentials(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

/// Validates the public URL and returns its normalized origin.
fn parse_public_url(text: &str, loopback_http: LoopbackHttp) -> Result<String, &'static str> {
    if text.is_empty() {
        return Err("is empty");
    }
    let url = parse_strict_url(text)?;
    check_scheme(&url, loopback_http)?;
    // Checked on the text, not on the parsed path: the parser normalizes
    // "/." and "/a/.." to "/", which would pass for no path.
    let after_scheme = text.split_once("://").map(|(_, rest)| rest).unwrap_or("");
    let rest = after_scheme
        .find(['/', '?', '#'])
        .map(|index| &after_scheme[index..])
        .unwrap_or("");
    if !(rest.is_empty() || rest == "/") {
        return Err("must be an origin only, with no path, query or fragment");
    }
    Ok(url.origin().ascii_serialization())
}

fn check_issuer(text: &str, loopback_http: LoopbackHttp) -> Result<(), &'static str> {
    let url = parse_strict_url(text)?;
    check_scheme(&url, loopback_http)?;
    if text.contains(['?', '#']) {
        return Err("must not have a query or a fragment");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;

    use super::*;

    /// A public configuration for tests, built through the real parser.
    pub(crate) fn public_config(
        issuer: &str,
        client_secret: Option<&str>,
        allowed_users: &str,
    ) -> PublicConfig {
        let mut vars = HashMap::from([
            (ENV_PUBLIC_URL, "http://localhost:4680"),
            (ENV_OIDC_ISSUER, issuer),
            (ENV_OIDC_CLIENT_ID, "calin-test-client"),
            (ENV_ALLOWED_USERS, allowed_users),
        ]);
        if let Some(secret) = client_secret {
            vars.insert(ENV_OIDC_CLIENT_SECRET, secret);
        }
        let lookup = |name: &str| vars.get(name).map(|value| (*value).to_owned());
        match AuthMode::from_lookup(lookup, LoopbackHttp::AllowForDevelopment) {
            Ok(AuthMode::Public(config)) => config,
            other => panic!("test configuration is not public mode: {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::identity::{Identity, InvalidIdentity};

    const SECRET: &str = "s3cr3t-Value~42";

    fn parse(vars: &[(&str, &str)], loopback_http: LoopbackHttp) -> Result<AuthMode, ConfigError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        AuthMode::from_lookup(|name| map.get(name).cloned(), loopback_http)
    }

    fn full() -> Vec<(&'static str, &'static str)> {
        vec![
            (ENV_PUBLIC_URL, "https://calin.example.com"),
            (ENV_OIDC_ISSUER, "https://tenant.eu.auth0.com/"),
            (ENV_OIDC_CLIENT_ID, "client-123"),
            (ENV_OIDC_CLIENT_SECRET, SECRET),
            (ENV_ALLOWED_USERS, "alice@example.com, sub:auth0|42"),
        ]
    }

    fn with(name: &'static str, value: &'static str) -> Vec<(&'static str, &'static str)> {
        let mut vars = without(name);
        vars.push((name, value));
        vars
    }

    fn without(name: &str) -> Vec<(&'static str, &'static str)> {
        full().into_iter().filter(|(n, _)| *n != name).collect()
    }

    fn public(vars: &[(&str, &str)], loopback_http: LoopbackHttp) -> PublicConfig {
        match parse(vars, loopback_http) {
            Ok(AuthMode::Public(config)) => config,
            other => panic!("expected public mode, got {other:?}"),
        }
    }

    fn problems(vars: &[(&str, &str)], loopback_http: LoopbackHttp) -> Vec<ConfigProblem> {
        match parse(vars, loopback_http) {
            Err(error) => error.problems().to_vec(),
            Ok(mode) => panic!("expected an error, got {mode:?}"),
        }
    }

    #[test]
    fn nothing_set_is_local_mode() {
        assert!(matches!(
            parse(&[], LoopbackHttp::Refuse),
            Ok(AuthMode::Local)
        ));
        // Blank sign-in variables (an env file template) are still nothing.
        let blanks = [(ENV_OIDC_ISSUER, ""), (ENV_ALLOWED_USERS, "  ")];
        assert!(matches!(
            parse(&blanks, LoopbackHttp::Refuse),
            Ok(AuthMode::Local)
        ));
    }

    #[test]
    fn full_configuration_is_public_mode() {
        let config = public(&full(), LoopbackHttp::Refuse);
        assert_eq!(config.public_origin(), "https://calin.example.com");
        assert_eq!(config.public_authority(), "calin.example.com");
        assert_eq!(
            config.redirect_uri(),
            "https://calin.example.com/auth/callback"
        );
        assert_eq!(config.issuer(), "https://tenant.eu.auth0.com/");
        assert_eq!(config.client_id(), "client-123");
        assert!(config.has_client_secret());
        assert_eq!(config.client_secret(), Some(SECRET));
        assert_eq!(config.loopback_http(), LoopbackHttp::Refuse);
        assert!(
            config
                .allow_list()
                .allows(&Identity::email("ALICE@example.com").unwrap())
        );
        assert!(
            config
                .allow_list()
                .allows(&Identity::subject("auth0|42").unwrap())
        );
    }

    #[test]
    fn the_secret_is_optional_and_blank_means_none() {
        let config = public(&without(ENV_OIDC_CLIENT_SECRET), LoopbackHttp::Refuse);
        assert!(!config.has_client_secret());
        let config = public(&with(ENV_OIDC_CLIENT_SECRET, ""), LoopbackHttp::Refuse);
        assert!(!config.has_client_secret());
    }

    #[test]
    fn values_are_trimmed_except_the_secret() {
        let mut vars = with(ENV_PUBLIC_URL, "  https://calin.example.com  ");
        vars.retain(|(name, _)| *name != ENV_OIDC_ISSUER && *name != ENV_OIDC_CLIENT_ID);
        vars.push((ENV_OIDC_ISSUER, " https://idp.example.com/realms/a "));
        vars.push((ENV_OIDC_CLIENT_ID, " client-123 "));
        let config = public(&vars, LoopbackHttp::Refuse);
        assert_eq!(config.issuer(), "https://idp.example.com/realms/a");
        assert_eq!(config.client_id(), "client-123");

        for secret in [" padded", "padded ", "line\r", "a\tb\u{0}"] {
            let mut vars = without(ENV_OIDC_CLIENT_SECRET);
            let secret: &'static str = Box::leak(secret.to_owned().into_boxed_str());
            vars.push((ENV_OIDC_CLIENT_SECRET, secret));
            assert_eq!(
                problems(&vars, LoopbackHttp::Refuse),
                vec![invalid(
                    ENV_OIDC_CLIENT_SECRET,
                    "has surrounding whitespace or a control character"
                )],
                "{secret:?}"
            );
        }
    }

    #[test]
    fn public_url_forms_accepted() {
        let cases = [
            (
                "https://calin.example.com",
                "https://calin.example.com",
                "calin.example.com",
            ),
            (
                "https://calin.example.com/",
                "https://calin.example.com",
                "calin.example.com",
            ),
            (
                "https://Calin.Example.COM",
                "https://calin.example.com",
                "calin.example.com",
            ),
            (
                "https://calin.example.com:8443",
                "https://calin.example.com:8443",
                "calin.example.com:8443",
            ),
            (
                "https://calin.example.com:443",
                "https://calin.example.com",
                "calin.example.com",
            ),
            ("https://192.0.2.7", "https://192.0.2.7", "192.0.2.7"),
            ("https://localhost", "https://localhost", "localhost"),
        ];
        for (input, origin, authority) in cases {
            let config = public(&with(ENV_PUBLIC_URL, input), LoopbackHttp::Refuse);
            assert_eq!(config.public_origin(), origin, "{input}");
            assert_eq!(config.public_authority(), authority, "{input}");
        }
    }

    #[test]
    fn public_url_forms_refused() {
        let origin_only = "must be an origin only, with no path, query or fragment";
        let https = "must be an https URL";
        let repaired = "contains whitespace, a control character or a backslash";
        let cases = [
            ("", "is empty"),
            ("   ", "is empty"),
            ("calin.example.com", https),
            ("http://calin.example.com", https),
            ("http://localhost:4680", https),
            ("http://127.0.0.1:4680", https),
            ("ftp://calin.example.com", https),
            ("HTTPS://calin.example.com", https),
            ("https:calin.example.com", https),
            ("https:///", "is not a valid URL"),
            ("https://", "is not a valid URL"),
            ("https://calin.example.com/app", origin_only),
            ("https://calin.example.com//", origin_only),
            ("https://calin.example.com/.", origin_only),
            ("https://calin.example.com/a/..", origin_only),
            ("https://calin.example.com?x=1", origin_only),
            ("https://calin.example.com/?", origin_only),
            ("https://calin.example.com#top", origin_only),
            ("https://calin.example.com/#", origin_only),
            (
                "https://user@calin.example.com",
                "must not contain credentials",
            ),
            (
                "https://user:pw@calin.example.com",
                "must not contain credentials",
            ),
            ("https://calin.example.com\\@evil.example", repaired),
            ("https://calin.exa\tmple.com", repaired),
            ("https://calin.example.com evil", repaired),
            ("https://calin.example.com:99999", "is not a valid URL"),
        ];
        for (input, reason) in cases {
            let input: &'static str = Box::leak(input.to_owned().into_boxed_str());
            assert_eq!(
                problems(&with(ENV_PUBLIC_URL, input), LoopbackHttp::Refuse),
                vec![invalid(ENV_PUBLIC_URL, reason)],
                "{input:?}"
            );
        }
        let long: &'static str =
            Box::leak(format!("https://{}.example.com", "a".repeat(2100)).into_boxed_str());
        assert_eq!(
            problems(&with(ENV_PUBLIC_URL, long), LoopbackHttp::Refuse),
            vec![invalid(ENV_PUBLIC_URL, "is too long")]
        );
    }

    #[test]
    fn strict_url_checks_run_in_order() {
        let long_and_spaced = format!("ftp://a b.example.com/{}", "a".repeat(2100));
        let cases = [
            (long_and_spaced.as_str(), "is too long"),
            (
                "ftp://a b.example.com",
                "contains whitespace, a control character or a backslash",
            ),
            ("ftp://user:pw@idp.example.com", "must be an https URL"),
            ("https://user@", "is not a valid URL"),
            (
                "https://user:pw@idp.example.com",
                "must not contain credentials",
            ),
            (
                "https://:pw@idp.example.com",
                "must not contain credentials",
            ),
        ];
        for (text, reason) in cases {
            assert_eq!(parse_strict_url(text).map(|_| ()), Err(reason), "{text}");
        }
        assert!(parse_strict_url("https://idp.example.com/a?b#c").is_ok());
    }

    #[test]
    fn a_client_id_too_long_is_refused_before_its_characters_are_read() {
        let long: &'static str = Box::leak(format!("{}\u{7}", "c".repeat(513)).into_boxed_str());
        assert_eq!(
            problems(&with(ENV_OIDC_CLIENT_ID, long), LoopbackHttp::Refuse),
            vec![invalid(ENV_OIDC_CLIENT_ID, "is too long")]
        );
    }

    #[test]
    fn loopback_http_needs_the_flag_a_loopback_name_and_a_port() {
        let allow = LoopbackHttp::AllowForDevelopment;
        for input in [
            "http://localhost:4680",
            "http://127.0.0.1:4680",
            "http://localhost:4680/",
        ] {
            let config = public(&with(ENV_PUBLIC_URL, input), allow);
            assert!(config.public_origin().starts_with("http://"), "{input}");
            assert_eq!(config.loopback_http(), allow);
        }
        for input in [
            "http://localhost",
            "http://127.0.0.1",
            "http://localhost:80",
            "http://calin.example.com:4680",
            "http://127.0.0.2:4680",
            "http://[::1]:4680",
            "http://localhost.evil.example:4680",
            "http://0.0.0.0:4680",
        ] {
            assert_eq!(
                problems(&with(ENV_PUBLIC_URL, input), allow),
                vec![invalid(ENV_PUBLIC_URL, "must be an https URL")],
                "{input}"
            );
        }
        // The flag also covers the issuer, and only with it.
        let vars = with(ENV_OIDC_ISSUER, "http://127.0.0.1:9000");
        assert_eq!(public(&vars, allow).issuer(), "http://127.0.0.1:9000");
        assert_eq!(
            problems(&vars, LoopbackHttp::Refuse),
            vec![invalid(ENV_OIDC_ISSUER, "must be an https URL")]
        );
    }

    #[test]
    fn issuer_forms() {
        for good in [
            "https://accounts.google.com",
            "https://tenant.eu.auth0.com/",
            "https://login.microsoftonline.com/tenant-id/v2.0",
            "https://idp.example.com:8443/realms/calin",
        ] {
            assert_eq!(
                public(&with(ENV_OIDC_ISSUER, good), LoopbackHttp::Refuse).issuer(),
                good
            );
        }
        let cases = [
            ("idp.example.com", "must be an https URL"),
            ("http://idp.example.com", "must be an https URL"),
            (
                "https://idp.example.com?tenant=a",
                "must not have a query or a fragment",
            ),
            (
                "https://idp.example.com/#x",
                "must not have a query or a fragment",
            ),
            (
                "https://user:pw@idp.example.com",
                "must not contain credentials",
            ),
            (
                "https://idp.example.com/a b",
                "contains whitespace, a control character or a backslash",
            ),
            ("https://", "is not a valid URL"),
        ];
        for (input, reason) in cases {
            assert_eq!(
                problems(&with(ENV_OIDC_ISSUER, input), LoopbackHttp::Refuse),
                vec![invalid(ENV_OIDC_ISSUER, reason)],
                "{input}"
            );
        }
    }

    #[test]
    fn client_id_forms() {
        assert_eq!(
            problems(&with(ENV_OIDC_CLIENT_ID, "a\u{7}b"), LoopbackHttp::Refuse),
            vec![invalid(ENV_OIDC_CLIENT_ID, "contains a control character")]
        );
        let longest: &'static str = Box::leak("c".repeat(512).into_boxed_str());
        assert_eq!(
            public(&with(ENV_OIDC_CLIENT_ID, longest), LoopbackHttp::Refuse).client_id(),
            longest
        );
        let long: &'static str = Box::leak("c".repeat(513).into_boxed_str());
        assert_eq!(
            problems(&with(ENV_OIDC_CLIENT_ID, long), LoopbackHttp::Refuse),
            vec![invalid(ENV_OIDC_CLIENT_ID, "is too long")]
        );
    }

    #[test]
    fn a_url_of_up_to_2048_bytes_is_read() {
        let issuer_of = |total: usize| -> &'static str {
            let head = "https://idp.example.com/";
            Box::leak(format!("{head}{}", "a".repeat(total - head.len())).into_boxed_str())
        };
        let longest = issuer_of(MAX_URL_BYTES);
        assert_eq!(
            public(&with(ENV_OIDC_ISSUER, longest), LoopbackHttp::Refuse).issuer(),
            longest
        );
        assert_eq!(
            problems(
                &with(ENV_OIDC_ISSUER, issuer_of(MAX_URL_BYTES + 1)),
                LoopbackHttp::Refuse
            ),
            vec![invalid(ENV_OIDC_ISSUER, "is too long")]
        );
    }

    #[test]
    fn each_missing_variable_is_named() {
        assert_eq!(
            problems(&without(ENV_OIDC_ISSUER), LoopbackHttp::Refuse),
            vec![ConfigProblem::Missing(ENV_OIDC_ISSUER)]
        );
        assert_eq!(
            problems(&without(ENV_OIDC_CLIENT_ID), LoopbackHttp::Refuse),
            vec![ConfigProblem::Missing(ENV_OIDC_CLIENT_ID)]
        );
        assert_eq!(
            problems(&with(ENV_OIDC_CLIENT_ID, "  "), LoopbackHttp::Refuse),
            vec![ConfigProblem::Missing(ENV_OIDC_CLIENT_ID)]
        );
        assert_eq!(
            problems(&without(ENV_ALLOWED_USERS), LoopbackHttp::Refuse),
            vec![ConfigProblem::AllowList(AllowListProblem::Empty)]
        );
        assert_eq!(
            problems(&with(ENV_ALLOWED_USERS, " , "), LoopbackHttp::Refuse),
            vec![ConfigProblem::AllowList(AllowListProblem::Empty)]
        );
    }

    #[test]
    fn all_problems_are_listed_together() {
        let vars = [
            (ENV_PUBLIC_URL, "http://calin.example.com/app"),
            (ENV_OIDC_ISSUER, "idp.example.com"),
            (ENV_OIDC_CLIENT_SECRET, " spaced "),
            (ENV_ALLOWED_USERS, "bob, alice@example.com, sub:"),
        ];
        let error = parse(&vars, LoopbackHttp::Refuse).unwrap_err();
        assert_eq!(
            error.problems(),
            &[
                invalid(ENV_PUBLIC_URL, "must be an https URL"),
                invalid(ENV_OIDC_ISSUER, "must be an https URL"),
                ConfigProblem::Missing(ENV_OIDC_CLIENT_ID),
                invalid(
                    ENV_OIDC_CLIENT_SECRET,
                    "has surrounding whitespace or a control character"
                ),
                ConfigProblem::AllowList(AllowListProblem::InvalidEntry {
                    entry: "bob".to_owned(),
                    reason: InvalidIdentity::Email,
                }),
                ConfigProblem::AllowList(AllowListProblem::InvalidEntry {
                    entry: "sub:".to_owned(),
                    reason: InvalidIdentity::Subject,
                }),
            ]
        );
        assert_eq!(
            error.to_string(),
            "sign-in is not configured correctly: CALIN_PUBLIC_URL must be an https URL; \
             CALIN_OIDC_ISSUER must be an https URL; CALIN_OIDC_CLIENT_ID is not set; \
             CALIN_OIDC_CLIENT_SECRET has surrounding whitespace or a control character; \
             CALIN_ALLOWED_USERS: allow-list entry \"bob\" is not a plausible email address; \
             CALIN_ALLOWED_USERS: allow-list entry \"sub:\" is not a usable subject"
        );
    }

    #[test]
    fn only_the_public_url_set_lists_everything_missing() {
        assert_eq!(
            problems(
                &[(ENV_PUBLIC_URL, "https://calin.example.com")],
                LoopbackHttp::Refuse
            ),
            vec![
                ConfigProblem::Missing(ENV_OIDC_ISSUER),
                ConfigProblem::Missing(ENV_OIDC_CLIENT_ID),
                ConfigProblem::AllowList(AllowListProblem::Empty),
            ]
        );
    }

    #[test]
    fn sign_in_variables_without_a_public_url_are_an_error_not_local_mode() {
        let error = parse(&without(ENV_PUBLIC_URL), LoopbackHttp::Refuse).unwrap_err();
        assert_eq!(
            error.problems(),
            &[ConfigProblem::PublicUrlNotSet {
                present: vec![
                    ENV_OIDC_ISSUER,
                    ENV_OIDC_CLIENT_ID,
                    ENV_OIDC_CLIENT_SECRET,
                    ENV_ALLOWED_USERS
                ],
            }]
        );
        let error = parse(&[(ENV_ALLOWED_USERS, "a@b.c")], LoopbackHttp::Refuse).unwrap_err();
        assert_eq!(
            error.to_string(),
            "sign-in is not configured correctly: CALIN_PUBLIC_URL is not set although \
             CALIN_ALLOWED_USERS is set; refusing to run without the sign-in that was configured"
        );
    }

    #[test]
    fn errors_and_debug_never_show_the_secret_or_a_url_with_credentials() {
        let config = public(&full(), LoopbackHttp::Refuse);
        assert!(!format!("{config:?}").contains(SECRET));
        assert!(!format!("{:?}", AuthMode::Public(config)).contains(SECRET));

        let mut vars = with(ENV_PUBLIC_URL, "https://admin:hunter2@calin.example.com");
        vars.retain(|(name, _)| *name != ENV_OIDC_ISSUER);
        vars.push((ENV_OIDC_ISSUER, "https://admin:hunter2@idp.example.com"));
        let error = parse(&vars, LoopbackHttp::Refuse).unwrap_err();
        for text in [error.to_string(), format!("{error:?}")] {
            assert!(!text.contains("hunter2"), "{text}");
            assert!(!text.contains(SECRET), "{text}");
        }
    }
}
