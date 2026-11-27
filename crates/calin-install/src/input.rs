//! What an installation needs, validated once so that rendering cannot fail.

use std::fmt;
use std::path::{Path, PathBuf};

/// Defaults from `docs/specs/2026-11-27-remote-access.md` section 5 and the
/// README (`calin serve --bind 127.0.0.1:4680`).
pub const DEFAULT_USER: &str = "calin";
pub const DEFAULT_DATA_DIR: &str = "/var/lib/calin";
pub const DEFAULT_BINARY: &str = "/usr/local/bin/calin";
pub const DEFAULT_PORT: u16 = 4680;
pub const DEFAULT_EXTRA_DIR: &str = "/srv/calin";

/// A field of the input, named the way error messages and the CLI name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Domain,
    Issuer,
    ClientId,
    ClientSecret,
    AllowedUsers,
    User,
    HomeDir,
    DataDir,
    Binary,
    Port,
    ExtraDirs,
}

impl Field {
    pub fn as_str(self) -> &'static str {
        match self {
            Field::Domain => "domain",
            Field::Issuer => "issuer",
            Field::ClientId => "client id",
            Field::ClientSecret => "client secret",
            Field::AllowedUsers => "allowed users",
            Field::User => "service user",
            Field::HomeDir => "home directory",
            Field::DataDir => "data directory",
            Field::Binary => "binary",
            Field::Port => "port",
            Field::ExtraDirs => "extra directories",
        }
    }
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One problem with one field. The message never contains the client secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: Field,
    pub message: String,
}

impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// Every problem found in an input, in field order.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct InputErrors(pub Vec<FieldError>);

impl InputErrors {
    pub fn errors(&self) -> &[FieldError] {
        &self.0
    }

    /// Whether any problem concerns `field`.
    pub fn has(&self, field: Field) -> bool {
        self.0.iter().any(|error| error.field == field)
    }
}

impl fmt::Display for InputErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, error) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{error}")?;
        }
        Ok(())
    }
}

/// The OpenID Connect client secret. Its `Debug` never shows the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// Where and as whom the service runs: everything the unit file depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceLayout {
    pub(crate) user: String,
    pub(crate) home_dir: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) binary: PathBuf,
    pub(crate) port: u16,
    pub(crate) extra_dirs: Vec<PathBuf>,
}

impl ServiceLayout {
    pub fn user(&self) -> &str {
        &self.user
    }
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
    pub fn binary(&self) -> &Path {
        &self.binary
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn extra_dirs(&self) -> &[PathBuf] {
        &self.extra_dirs
    }
    /// The address `calin serve --bind` takes.
    pub fn bind(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

/// Everything `install` needs. Built only through [`InstallInputBuilder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallInput {
    pub(crate) domain: String,
    pub(crate) issuer: String,
    pub(crate) client_id: String,
    pub(crate) client_secret: Option<Secret>,
    pub(crate) allowed_users: Vec<String>,
    pub(crate) layout: ServiceLayout,
}

impl InstallInput {
    pub fn builder() -> InstallInputBuilder {
        InstallInputBuilder::default()
    }
    pub fn domain(&self) -> &str {
        &self.domain
    }
    /// `https://<domain>`: the value of `CALIN_PUBLIC_URL`.
    pub fn public_url(&self) -> String {
        public_url(&self.domain)
    }
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
    pub fn client_secret(&self) -> Option<&Secret> {
        self.client_secret.as_ref()
    }
    pub fn allowed_users(&self) -> &[String] {
        &self.allowed_users
    }
    pub fn layout(&self) -> &ServiceLayout {
        &self.layout
    }
    /// What `check` needs, without the credentials.
    pub fn check_target(&self) -> CheckTarget {
        CheckTarget {
            domain: self.domain.clone(),
            issuer: self.issuer.clone(),
            layout: self.layout.clone(),
        }
    }
}

/// What `check` needs: no client id, secret or allow-list, so the owner can
/// run it with only the domain and the issuer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckTarget {
    pub(crate) domain: String,
    pub(crate) issuer: String,
    pub(crate) layout: ServiceLayout,
}

impl CheckTarget {
    pub fn domain(&self) -> &str {
        &self.domain
    }
    pub fn public_url(&self) -> String {
        public_url(&self.domain)
    }
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn layout(&self) -> &ServiceLayout {
        &self.layout
    }
}

fn public_url(domain: &str) -> String {
    format!("https://{domain}")
}

/// Collects the input; `build` validates all of it at once.
#[derive(Clone, Default)]
pub struct InstallInputBuilder {
    domain: Option<String>,
    issuer: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    allowed_users: Vec<String>,
    user: Option<String>,
    home_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    binary: Option<PathBuf>,
    port: Option<u16>,
    extra_dirs: Option<Vec<PathBuf>>,
}

impl fmt::Debug for InstallInputBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstallInputBuilder")
            .field("domain", &self.domain)
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[redacted]"),
            )
            .field("allowed_users", &self.allowed_users)
            .field("user", &self.user)
            .field("home_dir", &self.home_dir)
            .field("data_dir", &self.data_dir)
            .field("binary", &self.binary)
            .field("port", &self.port)
            .field("extra_dirs", &self.extra_dirs)
            .finish()
    }
}

impl InstallInputBuilder {
    pub fn domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }
    pub fn issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }
    pub fn client_id(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = Some(client_id.into());
        self
    }
    pub fn client_secret(mut self, secret: impl Into<String>) -> Self {
        self.client_secret = Some(secret.into());
        self
    }
    /// Adds one allow-list entry: an email address or `sub:<subject>`.
    pub fn allowed_user(mut self, entry: impl Into<String>) -> Self {
        self.allowed_users.push(entry.into());
        self
    }
    pub fn allowed_users<I, S>(mut self, entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_users
            .extend(entries.into_iter().map(Into::into));
        self
    }
    /// Default `calin`.
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }
    /// Default `/home/<user>`. Agent CLIs keep their logins there.
    pub fn home_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.home_dir = Some(path.into());
        self
    }
    /// Default `/var/lib/calin`.
    pub fn data_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(path.into());
        self
    }
    /// Default `/usr/local/bin/calin`.
    pub fn binary(mut self, path: impl Into<PathBuf>) -> Self {
        self.binary = Some(path.into());
        self
    }
    /// Default 4680.
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }
    /// Replaces the default `/srv/calin`. An empty list is allowed.
    pub fn extra_dirs<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.extra_dirs = Some(paths.into_iter().map(Into::into).collect());
        self
    }

    /// Validates every field and reports every problem together.
    pub fn build(self) -> Result<InstallInput, InputErrors> {
        let mut errors = Vec::new();
        let domain = self.check_domain(&mut errors);
        let issuer = self.check_issuer(&mut errors);
        let client_id = self.check_client_id(&mut errors);
        let client_secret = self.check_client_secret(&mut errors);
        let allowed_users = self.check_allowed_users(&mut errors);
        let layout = self.check_layout(&mut errors);
        finish(errors)?;
        Ok(InstallInput {
            domain,
            issuer,
            client_id,
            client_secret,
            allowed_users,
            layout,
        })
    }

    /// Validates only what `check` needs: the domain, the issuer and the
    /// layout. Credentials given anyway are ignored.
    pub fn build_check_target(self) -> Result<CheckTarget, InputErrors> {
        let mut errors = Vec::new();
        let domain = self.check_domain(&mut errors);
        let issuer = self.check_issuer(&mut errors);
        let layout = self.check_layout(&mut errors);
        finish(errors)?;
        Ok(CheckTarget {
            domain,
            issuer,
            layout,
        })
    }

    fn check_domain(&self, errors: &mut Vec<FieldError>) -> String {
        let domain = self.domain.clone().unwrap_or_default();
        if let Err(message) = validate_domain(&domain) {
            push(errors, Field::Domain, message);
        }
        domain
    }

    fn check_issuer(&self, errors: &mut Vec<FieldError>) -> String {
        let issuer = self.issuer.clone().unwrap_or_default();
        if let Err(message) = validate_issuer(&issuer) {
            push(errors, Field::Issuer, message);
        }
        issuer
    }

    fn check_client_id(&self, errors: &mut Vec<FieldError>) -> String {
        let id = self.client_id.clone().unwrap_or_default();
        if id.is_empty() {
            push(errors, Field::ClientId, "is required".into());
        } else if id.chars().any(char::is_control) {
            push(errors, Field::ClientId, NO_CONTROL.into());
        } else if id.trim() != id {
            push(
                errors,
                Field::ClientId,
                "must not start or end with spaces".into(),
            );
        }
        id
    }

    fn check_client_secret(&self, errors: &mut Vec<FieldError>) -> Option<Secret> {
        let secret = self.client_secret.clone()?;
        // The messages describe the problem without quoting the value.
        if secret.is_empty() {
            push(
                errors,
                Field::ClientSecret,
                "is empty; leave it out for a public client".into(),
            );
        } else if secret.chars().any(char::is_control) {
            push(errors, Field::ClientSecret, NO_CONTROL.into());
        }
        Some(Secret(secret))
    }

    fn check_allowed_users(&self, errors: &mut Vec<FieldError>) -> Vec<String> {
        if self.allowed_users.is_empty() {
            push(
                errors,
                Field::AllowedUsers,
                "needs at least one entry (an email address or sub:<subject>)".into(),
            );
        }
        let mut seen: Vec<String> = Vec::new();
        for entry in &self.allowed_users {
            match validate_allowed_user(entry) {
                Err(message) => push(errors, Field::AllowedUsers, message),
                Ok(()) => {
                    // Emails compare case-insensitively at sign-in (spec section 2).
                    let key = if entry.starts_with("sub:") {
                        entry.clone()
                    } else {
                        entry.to_lowercase()
                    };
                    if seen.contains(&key) {
                        push(
                            errors,
                            Field::AllowedUsers,
                            format!("`{entry}` is listed twice"),
                        );
                    } else {
                        seen.push(key);
                    }
                }
            }
        }
        self.allowed_users.clone()
    }

    fn check_layout(&self, errors: &mut Vec<FieldError>) -> ServiceLayout {
        let user = self.user.clone().unwrap_or_else(|| DEFAULT_USER.into());
        if let Err(message) = validate_user(&user) {
            push(errors, Field::User, message);
        }
        let home_dir = self
            .home_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("/home/{user}")));
        if let Err(message) = validate_state_dir(&home_dir) {
            push(errors, Field::HomeDir, message);
        }
        let data_dir = self
            .data_dir
            .clone()
            .unwrap_or_else(|| DEFAULT_DATA_DIR.into());
        if let Err(message) = validate_state_dir(&data_dir) {
            push(errors, Field::DataDir, message);
        }
        let binary = self.binary.clone().unwrap_or_else(|| DEFAULT_BINARY.into());
        if let Err(message) = validate_path(&binary) {
            push(errors, Field::Binary, message);
        }
        let port = self.port.unwrap_or(DEFAULT_PORT);
        if port < 1024 {
            push(
                errors,
                Field::Port,
                format!(
                    "{port} is not allowed: use 1024 to 65535, which an unprivileged service may bind"
                ),
            );
        }
        let extra_dirs = self
            .extra_dirs
            .clone()
            .unwrap_or_else(|| vec![DEFAULT_EXTRA_DIR.into()]);
        let mut seen: Vec<&Path> = Vec::new();
        for dir in &extra_dirs {
            if let Err(message) = validate_state_dir(dir) {
                push(errors, Field::ExtraDirs, message);
            } else if dir == &data_dir || dir == &home_dir {
                push(
                    errors,
                    Field::ExtraDirs,
                    format!("{} is already the data or home directory", dir.display()),
                );
            } else if seen.contains(&dir.as_path()) {
                push(
                    errors,
                    Field::ExtraDirs,
                    format!("{} is listed twice", dir.display()),
                );
            } else {
                seen.push(dir);
            }
        }
        ServiceLayout {
            user,
            home_dir,
            data_dir,
            binary,
            port,
            extra_dirs,
        }
    }
}

const NO_CONTROL: &str = "must not contain line breaks or other control characters (the environment file cannot carry them)";

fn push(errors: &mut Vec<FieldError>, field: Field, message: String) {
    errors.push(FieldError { field, message });
}

fn finish(mut errors: Vec<FieldError>) -> Result<(), InputErrors> {
    if errors.is_empty() {
        return Ok(());
    }
    // Stable: problems of one field keep the order they were found in.
    errors.sort_by_key(|error| error.field);
    Err(InputErrors(errors))
}

pub(crate) fn validate_domain(domain: &str) -> Result<(), String> {
    if domain.is_empty() {
        return Err("is required, for example calin.example.com".into());
    }
    if domain.contains("://") {
        return Err(format!(
            "`{domain}` has a scheme; give the host name alone, for example calin.example.com"
        ));
    }
    if domain.contains('/') {
        return Err(format!("`{domain}` has a path; give the host name alone"));
    }
    if domain.contains('[') || domain.contains(']') {
        return Err(format!(
            "`{domain}` is an IP address; a domain name is needed for a certificate"
        ));
    }
    if domain.contains(':') {
        return Err(format!(
            "`{domain}` has a port; give the host name alone (HTTPS uses 443)"
        ));
    }
    if domain.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(format!("`{domain}` must be lowercase"));
    }
    if !domain
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
    {
        return Err(format!(
            "`{}` may contain only lowercase letters, digits, hyphens and dots",
            domain.escape_debug()
        ));
    }
    if !domain.contains('.') {
        return Err(format!(
            "`{domain}` needs at least one dot, for example calin.example.com"
        ));
    }
    if domain.len() > 253 {
        return Err("is longer than 253 characters".into());
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.iter().any(|label| label.is_empty()) {
        return Err(format!(
            "`{domain}` has an empty label (two dots in a row, or a dot at an end)"
        ));
    }
    if labels.iter().any(|label| label.len() > 63) {
        return Err(format!("`{domain}` has a label longer than 63 characters"));
    }
    if labels
        .iter()
        .any(|label| label.starts_with('-') || label.ends_with('-'))
    {
        return Err(format!(
            "`{domain}` has a label that starts or ends with a hyphen"
        ));
    }
    // No top-level domain is numeric, so this catches every IPv4 form.
    if labels
        .last()
        .is_some_and(|label| label.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(format!(
            "`{domain}` is an IP address; a domain name is needed for a certificate"
        ));
    }
    Ok(())
}

pub(crate) fn validate_issuer(issuer: &str) -> Result<(), String> {
    if issuer.is_empty() {
        return Err("is required, for example https://your-tenant.eu.auth0.com/".into());
    }
    if issuer.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("must not contain spaces, line breaks or control characters".into());
    }
    let Some(rest) = issuer.strip_prefix("https://") else {
        return Err(format!("`{issuer}` must start with https://"));
    };
    if issuer.contains('?') || issuer.contains('#') {
        return Err(format!("`{issuer}` must not have a query or a fragment"));
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        return Err(format!("`{issuer}` has no host"));
    }
    if authority.contains('@') {
        return Err(format!("`{issuer}` must not contain credentials"));
    }
    if !authority
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
    {
        return Err(format!("`{issuer}` has an invalid host"));
    }
    Ok(())
}

pub(crate) fn validate_allowed_user(entry: &str) -> Result<(), String> {
    if entry.chars().any(char::is_control) {
        return Err(format!("`{}`: {NO_CONTROL}", entry.escape_debug()));
    }
    if entry.contains(',') {
        return Err(format!(
            "`{entry}` contains a comma, which separates entries; give each entry separately"
        ));
    }
    if entry.chars().any(char::is_whitespace) {
        return Err(format!("`{entry}` contains a space"));
    }
    if let Some(subject) = entry.strip_prefix("sub:") {
        if subject.is_empty() {
            return Err("`sub:` needs a subject after the colon".into());
        }
        return Ok(());
    }
    let Some((local, domain)) = entry.split_once('@') else {
        return Err(format!(
            "`{entry}` is neither an email address nor sub:<subject>"
        ));
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') || !domain.contains('.') {
        return Err(format!("`{entry}` is not a valid email address"));
    }
    if domain.starts_with('.') || domain.ends_with('.') {
        return Err(format!("`{entry}` is not a valid email address"));
    }
    Ok(())
}

pub(crate) fn validate_user(user: &str) -> Result<(), String> {
    if user.is_empty() {
        return Err("is required".into());
    }
    if user.len() > 32 {
        return Err(format!("`{user}` is longer than 32 characters"));
    }
    let mut chars = user.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_');
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !first_ok || !rest_ok {
        return Err(format!(
            "`{}` must start with a lowercase letter or _ and contain only lowercase letters, digits, _ and -",
            user.escape_debug()
        ));
    }
    if user == "root" {
        return Err("must not be root: the service runs unprivileged".into());
    }
    Ok(())
}

/// The shape every path must have. The character set keeps paths usable
/// unquoted in the unit file and in shell commands.
pub(crate) fn validate_path(path: &Path) -> Result<(), String> {
    let Some(text) = path.to_str() else {
        return Err("is not valid UTF-8".into());
    };
    if text.is_empty() {
        return Err("is required".into());
    }
    if !text.starts_with('/') {
        return Err(format!("`{}` must be absolute", text.escape_debug()));
    }
    if !text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '@'))
    {
        return Err(format!(
            "`{}` may contain only letters, digits, and / . _ - + @",
            text.escape_debug()
        ));
    }
    if text == "/" {
        return Err("must not be the root directory".into());
    }
    if text.ends_with('/') || text.contains("//") {
        return Err(format!("`{text}` must not end with / or contain //"));
    }
    if text.split('/').any(|part| part == "." || part == "..") {
        return Err(format!("`{text}` must not contain . or .. components"));
    }
    Ok(())
}

/// Directories the service writes to: they become `ReadWritePaths=` and are
/// handed to the service user, so system locations are refused.
pub(crate) fn validate_state_dir(path: &Path) -> Result<(), String> {
    validate_path(path)?;
    let text = path.to_str().unwrap_or_default();
    const SYSTEM: [&str; 13] = [
        "/etc", "/usr", "/boot", "/proc", "/sys", "/dev", "/bin", "/sbin", "/lib", "/lib64",
        "/run", "/root", "/tmp",
    ];
    if SYSTEM
        .iter()
        .any(|system| text == *system || text.starts_with(&format!("{system}/")))
        || text == "/var/tmp"
        || text.starts_with("/var/tmp/")
    {
        return Err(format!(
            "`{text}` is a system location; the service must not own it"
        ));
    }
    if text.matches('/').count() < 2 {
        return Err(format!(
            "`{text}` is a top-level directory; use a directory inside it, such as {text}/calin"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn typical() -> InstallInputBuilder {
        InstallInput::builder()
            .domain("calin.example.com")
            .issuer("https://example.eu.auth0.com/")
            .client_id("abc123")
            .allowed_user("owner@example.com")
    }

    #[test]
    fn defaults_fill_the_layout() {
        let input = typical().build().unwrap();
        let layout = input.layout();
        assert_eq!(layout.user(), "calin");
        assert_eq!(layout.home_dir(), Path::new("/home/calin"));
        assert_eq!(layout.data_dir(), Path::new("/var/lib/calin"));
        assert_eq!(layout.binary(), Path::new("/usr/local/bin/calin"));
        assert_eq!(layout.port(), 4680);
        assert_eq!(layout.extra_dirs(), [PathBuf::from("/srv/calin")]);
        assert_eq!(input.public_url(), "https://calin.example.com");
        assert!(input.client_secret().is_none());
    }

    #[test]
    fn domain_table() {
        let good = [
            "calin.example.com",
            "a.b",
            "x-1.example.co.uk",
            "123.example.com",
            "xn--bcher-kva.example",
        ];
        for domain in good {
            assert_eq!(validate_domain(domain), Ok(()), "{domain}");
        }
        let bad = [
            ("", "required"),
            ("https://calin.example.com", "scheme"),
            ("calin.example.com/app", "path"),
            ("calin.example.com:8443", "port"),
            ("Calin.Example.com", "lowercase"),
            ("calin_example.com", "only lowercase"),
            ("calin example.com", "only lowercase"),
            ("calin.example.com\n", "only lowercase"),
            ("localhost", "dot"),
            ("calin..example.com", "empty label"),
            (".example.com", "empty label"),
            ("example.com.", "empty label"),
            ("-calin.example.com", "hyphen"),
            ("calin-.example.com", "hyphen"),
            ("192.0.2.1", "IP address"),
            ("[::1]", "IP address"),
            ("::1", "port"),
        ];
        for (domain, expected) in bad {
            let message = validate_domain(domain).unwrap_err();
            assert!(message.contains(expected), "{domain:?}: {message}");
        }
        let long_label = format!("{}.com", "a".repeat(64));
        assert!(validate_domain(&long_label).unwrap_err().contains("63"));
    }

    #[test]
    fn issuer_table() {
        for issuer in [
            "https://example.eu.auth0.com/",
            "https://accounts.google.com",
            "https://login.example.com:8443/realms/main",
        ] {
            assert_eq!(validate_issuer(issuer), Ok(()), "{issuer}");
        }
        for (issuer, expected) in [
            ("", "required"),
            ("http://example.com", "https://"),
            ("example.com", "https://"),
            ("https://", "no host"),
            ("https:///path", "no host"),
            ("https://example.com/?x=1", "query"),
            ("https://example.com/#a", "fragment"),
            ("https://user:pw@example.com", "credentials"),
            ("https://exa mple.com", "spaces"),
            ("https://example.com\n", "line breaks"),
            ("https://ex_ample.com", "invalid host"),
        ] {
            let message = validate_issuer(issuer).unwrap_err();
            assert!(message.contains(expected), "{issuer:?}: {message}");
        }
    }

    #[test]
    fn allowed_user_table() {
        for entry in [
            "owner@example.com",
            "First.Last+tag@sub.example.org",
            "sub:auth0|123456",
            "sub:google-oauth2|1",
        ] {
            assert_eq!(validate_allowed_user(entry), Ok(()), "{entry}");
        }
        for (entry, expected) in [
            ("", "neither"),
            ("owner", "neither"),
            ("owner@localhost", "valid email"),
            ("@example.com", "valid email"),
            ("owner@", "valid email"),
            ("a@b@example.com", "valid email"),
            ("owner@example.com.", "valid email"),
            ("sub:", "subject"),
            ("a@example.com,b@example.com", "comma"),
            ("owner @example.com", "space"),
            ("owner@example.com\n", "line breaks"),
        ] {
            let message = validate_allowed_user(entry).unwrap_err();
            assert!(message.contains(expected), "{entry:?}: {message}");
        }
    }

    #[test]
    fn user_table() {
        for user in ["calin", "_calin", "calin-svc", "c4lin_2"] {
            assert_eq!(validate_user(user), Ok(()), "{user}");
        }
        for user in [
            "",
            "root",
            "Calin",
            "1calin",
            "cal in",
            "cal.in",
            &"a".repeat(33),
        ] {
            assert!(validate_user(user).is_err(), "{user:?}");
        }
    }

    #[test]
    fn path_table() {
        for path in [
            "/var/lib/calin",
            "/srv/calin",
            "/home/calin",
            "/opt/repos/a-b_c.d",
        ] {
            assert_eq!(validate_state_dir(Path::new(path)), Ok(()), "{path}");
        }
        for (path, expected) in [
            ("", "required"),
            ("var/lib/calin", "absolute"),
            ("/", "root directory"),
            ("/srv/calin/", "end with"),
            ("/srv//calin", "//"),
            ("/srv/../etc", ".."),
            ("/srv/./calin", ".."),
            ("/srv/my repos", "only letters"),
            ("/srv/calin\n", "only letters"),
            ("/srv/$HOME", "only letters"),
            ("/etc/calin", "system"),
            ("/usr/local/calin", "system"),
            ("/tmp/calin", "system"),
            ("/var/tmp/calin", "system"),
            ("/srv", "top-level"),
            ("/home", "top-level"),
        ] {
            let message = validate_state_dir(Path::new(path)).unwrap_err();
            assert!(message.contains(expected), "{path:?}: {message}");
        }
        // The binary may live anywhere well-formed, system locations included.
        assert_eq!(validate_path(Path::new("/usr/local/bin/calin")), Ok(()));
    }

    #[test]
    fn port_table() {
        for port in [1024, 4680, 65535] {
            assert!(typical().port(port).build().is_ok(), "{port}");
        }
        for port in [0, 80, 443, 1023] {
            let errors = typical().port(port).build().unwrap_err();
            assert!(errors.has(Field::Port), "{port}");
        }
    }

    #[test]
    fn every_problem_is_reported_together_and_names_its_field() {
        let errors = InstallInput::builder()
            .domain("https://Calin.example.com")
            .issuer("http://idp.example.com")
            .client_secret("")
            .allowed_users(["not-an-email", "sub:"])
            .user("root")
            .home_dir("relative")
            .data_dir("/etc/calin")
            .binary("calin")
            .port(80)
            .extra_dirs(["/srv/calin", "/srv/calin"])
            .build()
            .unwrap_err();
        let fields: Vec<Field> = errors.errors().iter().map(|e| e.field).collect();
        assert_eq!(
            fields,
            [
                Field::Domain,
                Field::Issuer,
                Field::ClientId,
                Field::ClientSecret,
                Field::AllowedUsers,
                Field::AllowedUsers,
                Field::User,
                Field::HomeDir,
                Field::DataDir,
                Field::Binary,
                Field::Port,
                Field::ExtraDirs,
            ]
        );
        let text = errors.to_string();
        for line in text.lines() {
            let field = line.split(':').next().unwrap();
            assert!(
                fields.iter().any(|f| f.as_str() == field),
                "line does not start with a field name: {line}"
            );
        }
        assert!(text.contains("domain: "), "{text}");
        assert!(text.contains("client id: is required"), "{text}");
    }

    #[test]
    fn missing_allow_list_is_an_error() {
        let errors = InstallInput::builder()
            .domain("calin.example.com")
            .issuer("https://idp.example.com/")
            .client_id("id")
            .build()
            .unwrap_err();
        assert_eq!(errors.errors().len(), 1);
        assert!(errors.has(Field::AllowedUsers));
    }

    #[test]
    fn duplicates_are_reported() {
        let errors = typical()
            .allowed_user("OWNER@example.com")
            .extra_dirs(["/srv/repos", "/var/lib/calin"])
            .build()
            .unwrap_err();
        assert!(errors.to_string().contains("listed twice"), "{errors}");
        assert!(errors.to_string().contains("already the data"), "{errors}");
    }

    #[test]
    fn a_newline_in_any_environment_value_is_rejected() {
        let cases: [(Field, InstallInputBuilder); 5] = [
            (Field::Domain, typical().domain("calin.example.com\nX=1")),
            (
                Field::Issuer,
                typical().issuer("https://idp.example.com/\nX=1"),
            ),
            (Field::ClientId, typical().client_id("id\nX=1")),
            (Field::ClientSecret, typical().client_secret("s3cret\nX=1")),
            (
                Field::AllowedUsers,
                typical().allowed_user("a@example.com\nX=1"),
            ),
        ];
        for (field, builder) in cases {
            let errors = builder.build().unwrap_err();
            assert!(errors.has(field), "{field}: {errors}");
        }
        let errors = typical().client_secret("a\rb").build().unwrap_err();
        assert!(errors.has(Field::ClientSecret));
    }

    #[test]
    fn the_secret_never_appears_in_errors_or_debug() {
        let secret = "hunter2-very-secret";
        let errors = typical()
            .client_secret(format!("{secret}\n"))
            .build()
            .unwrap_err();
        assert!(!errors.to_string().contains(secret));
        assert!(!format!("{errors:?}").contains(secret));

        let builder = typical().client_secret(secret);
        assert!(!format!("{builder:?}").contains(secret));
        let input = builder.build().unwrap();
        assert!(!format!("{input:?}").contains(secret));
        assert_eq!(input.client_secret().unwrap().expose(), secret);
    }

    #[test]
    fn a_check_target_needs_no_credentials() {
        let target = InstallInput::builder()
            .domain("calin.example.com")
            .issuer("https://idp.example.com/")
            .build_check_target()
            .unwrap();
        assert_eq!(target.public_url(), "https://calin.example.com");
        assert_eq!(target.layout().user(), "calin");
        let errors = InstallInput::builder()
            .domain("bad")
            .port(1)
            .build_check_target()
            .unwrap_err();
        let fields: Vec<Field> = errors.errors().iter().map(|e| e.field).collect();
        assert_eq!(fields, [Field::Domain, Field::Issuer, Field::Port]);
    }
}
