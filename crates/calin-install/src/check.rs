//! `check`: read-only verification of an installation. Every probe of the
//! machine goes through [`Probes`], so each check is tested with fakes.

use std::fmt;
use std::io;
use std::path::Path;

use crate::envfile::parse_env_file;
use crate::input::CheckTarget;
use crate::render::{
    ENV_PATH, REQUIRED_VARIABLES, UNIT_NAME, UNIT_PATH, normalize_unit, render_unit, service_path,
};

/// What a command printed and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandOutput {
    /// The exit code; `None` when a signal ended the process.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("not found")]
    NotFound,
    #[error("did not finish in time")]
    TimedOut,
    #[error("{0}")]
    Failed(String),
}

/// Runs a program with arguments, no shell involved, and waits for it.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    /// Nothing accepted the connection, or the name did not resolve.
    #[error("could not connect: {0}")]
    Connect(String),
    /// The TLS handshake failed, a certificate problem most often.
    #[error("TLS failed: {0}")]
    Tls(String),
    #[error("no answer in time")]
    TimedOut,
    #[error("{0}")]
    Other(String),
}

/// One GET, no redirects followed, no credentials sent.
pub trait HttpGet: Send + Sync {
    fn get(&self, url: &str) -> Result<HttpResponse, HttpError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileInfo {
    /// Permission bits, `0o600` for example.
    pub mode: u32,
    pub uid: u32,
}

/// Reads files and their metadata.
pub trait FileProbe: Send + Sync {
    fn metadata(&self, path: &Path) -> io::Result<FileInfo>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
}

/// The probes one run of the checks uses.
#[derive(Clone, Copy)]
pub struct Probes<'a> {
    pub commands: &'a dyn CommandRunner,
    pub http: &'a dyn HttpGet,
    pub files: &'a dyn FileProbe,
}

/// The checks, in the order they run and print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Check {
    UnitFile,
    ServiceActive,
    ServiceEnabled,
    EnvFile,
    LoopbackListening,
    LoopbackHealth,
    PublicUrl,
    Provider,
    Claude,
    Gh,
}

impl Check {
    pub fn label(self) -> &'static str {
        match self {
            Check::UnitFile => "unit file",
            Check::ServiceActive => "service running",
            Check::ServiceEnabled => "starts at boot",
            Check::EnvFile => "environment file",
            Check::LoopbackListening => "loopback port",
            Check::LoopbackHealth => "public mode",
            Check::PublicUrl => "public URL",
            Check::Provider => "identity provider",
            Check::Claude => "claude CLI",
            Check::Gh => "gh signed in",
        }
    }

    /// Whether Calin cannot be used from a browser while this check fails.
    pub fn essential(self) -> bool {
        matches!(
            self,
            Check::ServiceActive
                | Check::EnvFile
                | Check::LoopbackListening
                | Check::LoopbackHealth
                | Check::PublicUrl
                | Check::Provider
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    CouldNotCheck,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "FAIL",
            Status::CouldNotCheck => "could not check",
        }
    }
}

/// One check's outcome. `reason` and `remedy` never contain a value from
/// the environment file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub check: Check,
    pub status: Status,
    pub reason: String,
    /// What to do, on failure (and for some checks that could not run).
    pub remedy: Option<String>,
}

impl CheckResult {
    fn new(check: Check, status: Status, reason: impl Into<String>) -> Self {
        Self {
            check,
            status,
            reason: reason.into(),
            remedy: None,
        }
    }
    fn pass(check: Check, reason: impl Into<String>) -> Self {
        Self::new(check, Status::Pass, reason)
    }
    fn fail(check: Check, reason: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self::new(check, Status::Fail, reason).with_remedy(remedy)
    }
    fn unknown(check: Check, reason: impl Into<String>) -> Self {
        Self::new(check, Status::CouldNotCheck, reason)
    }
    fn with_remedy(mut self, remedy: impl Into<String>) -> Self {
        self.remedy = Some(remedy.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usability {
    /// Every essential check passed.
    Usable,
    /// An essential check failed.
    NotUsable,
    /// No essential check failed, but some could not run.
    Unknown,
}

/// Every result, in order. Its `Display` is the table the CLI prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub results: Vec<CheckResult>,
}

impl CheckReport {
    pub fn usability(&self) -> Usability {
        let essential = self.results.iter().filter(|r| r.check.essential());
        let mut unknown = false;
        for result in essential {
            match result.status {
                Status::Fail => return Usability::NotUsable,
                Status::CouldNotCheck => unknown = true,
                Status::Pass => {}
            }
        }
        if unknown {
            Usability::Unknown
        } else {
            Usability::Usable
        }
    }

    pub fn is_usable(&self) -> bool {
        self.usability() == Usability::Usable
    }

    pub fn get(&self, check: Check) -> Option<&CheckResult> {
        self.results.iter().find(|r| r.check == check)
    }
}

impl fmt::Display for CheckReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label_width = self
            .results
            .iter()
            .map(|r| r.check.label().len())
            .max()
            .unwrap_or(0);
        let status_width = Status::CouldNotCheck.label().len();
        for result in &self.results {
            writeln!(
                f,
                "{:<label_width$}  {:<status_width$}  {}",
                result.check.label(),
                result.status.label(),
                result.reason
            )?;
            if let Some(remedy) = &result.remedy {
                writeln!(
                    f,
                    "{:<label_width$}  {:<status_width$}  -> {remedy}",
                    "", ""
                )?;
            }
        }
        let verdict = match self.usability() {
            Usability::Usable => "yes",
            Usability::NotUsable => "no",
            Usability::Unknown => "unknown: some essential checks could not run (try as root)",
        };
        writeln!(f, "\nUsable: {verdict}")
    }
}

/// Runs every check. Nothing here changes the machine.
pub fn run_checks(target: &CheckTarget, probes: Probes<'_>) -> CheckReport {
    let mut results = vec![
        unit_file(target, probes),
        systemctl(probes, "is-active", Check::ServiceActive),
        systemctl(probes, "is-enabled", Check::ServiceEnabled),
        env_file(target, probes),
    ];
    results.extend(loopback(target, probes));
    results.push(public_url(target, probes));
    results.push(provider(target, probes));
    results.extend(agent_clis(target, probes));
    CheckReport { results }
}

fn unit_file(target: &CheckTarget, probes: Probes<'_>) -> CheckResult {
    let check = Check::UnitFile;
    let installed = match probes.files.read(Path::new(UNIT_PATH)) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return CheckResult::fail(
                check,
                format!("{UNIT_PATH} does not exist"),
                "run the steps that `calin server install` prints",
            );
        }
        Err(error) => {
            return CheckResult::unknown(check, format!("cannot read {UNIT_PATH}: {error}"));
        }
    };
    let expected = normalize_unit(&render_unit(target.layout()));
    let found = normalize_unit(&installed);
    if expected == found {
        return CheckResult::pass(check, format!("{UNIT_PATH} matches what install writes"));
    }
    // Only the expected line is quoted: the installed file may hold anything.
    let detail = match expected.iter().zip(&found).position(|(a, b)| a != b) {
        Some(index) => format!("differs at `{}`", expected[index]),
        None if found.len() > expected.len() => "has lines install does not write".to_owned(),
        None => format!("lacks `{}`", expected[found.len()]),
    };
    CheckResult::fail(
        check,
        format!("{UNIT_PATH} drifted from what install writes: {detail}"),
        "install the unit `calin server install` renders, then `systemctl daemon-reload && systemctl restart calin`",
    )
}

fn systemctl(probes: Probes<'_>, verb: &str, check: Check) -> CheckResult {
    let wanted = if verb == "is-active" {
        "active"
    } else {
        "enabled"
    };
    match probes.commands.run("systemctl", &[verb, UNIT_NAME]) {
        Ok(output) => {
            let state = output.stdout.trim();
            if state == wanted {
                CheckResult::pass(check, format!("{UNIT_NAME} is {state}"))
            } else {
                let state = if state.is_empty() { "unknown" } else { state };
                let remedy = if check == Check::ServiceActive {
                    "`journalctl -u calin -n 50` shows why; start it with `systemctl start calin`"
                } else {
                    "`systemctl enable calin` (as root)"
                };
                CheckResult::fail(check, format!("{UNIT_NAME} is {state}"), remedy)
            }
        }
        Err(CommandError::NotFound) => CheckResult::unknown(
            check,
            "systemctl not found: this machine does not run systemd",
        ),
        Err(error) => CheckResult::unknown(check, format!("systemctl {verb} failed: {error}")),
    }
}

const REINSTALL_ENV: &str = "install the calin.env that `calin server install` renders";

/// The environment file in three steps; the first that fails gives the result.
fn env_file(target: &CheckTarget, probes: Probes<'_>) -> CheckResult {
    let checked = env_file_guarded(probes)
        .and_then(|()| env_file_pairs(probes))
        .and_then(|pairs| env_file_values(target, &pairs));
    match checked {
        Ok(()) => CheckResult::pass(
            Check::EnvFile,
            format!("{ENV_PATH} has mode 0600, owner root, and every required variable"),
        ),
        Err(result) => result,
    }
}

/// The file exists with mode 0600 and owner root: it holds a secret.
fn env_file_guarded(probes: Probes<'_>) -> Result<(), CheckResult> {
    let check = Check::EnvFile;
    let info = match probes.files.metadata(Path::new(ENV_PATH)) {
        Ok(info) => info,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(CheckResult::fail(
                check,
                format!("{ENV_PATH} does not exist"),
                "install it from the staged calin.env as `calin server install` prints",
            ));
        }
        Err(error) => {
            return Err(
                CheckResult::unknown(check, format!("cannot inspect {ENV_PATH}: {error}"))
                    .with_remedy("run the check as root"),
            );
        }
    };
    let mode = info.mode & 0o7777;
    if mode != 0o600 {
        return Err(CheckResult::fail(
            check,
            format!("{ENV_PATH} has mode {mode:04o}; it holds a secret and must be 0600"),
            format!("chmod 600 {ENV_PATH}"),
        ));
    }
    if info.uid != 0 {
        return Err(CheckResult::fail(
            check,
            format!("{ENV_PATH} is owned by uid {}, not root", info.uid),
            format!("chown root:root {ENV_PATH}"),
        ));
    }
    Ok(())
}

/// The file's variables, read as systemd reads them.
fn env_file_pairs(probes: Probes<'_>) -> Result<Vec<(String, String)>, CheckResult> {
    let check = Check::EnvFile;
    let bytes = match probes.files.read(Path::new(ENV_PATH)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(CheckResult::unknown(
                check,
                format!(
                    "{ENV_PATH} has mode 0600 and owner root, but only root can read its variables"
                ),
            )
            .with_remedy("run the check as root to verify the variables"));
        }
        Err(error) => {
            return Err(CheckResult::unknown(
                check,
                format!("cannot read {ENV_PATH}: {error}"),
            ));
        }
    };
    let text = String::from_utf8(bytes).map_err(|_| {
        CheckResult::fail(
            check,
            format!("{ENV_PATH} is not valid UTF-8"),
            REINSTALL_ENV,
        )
    })?;
    parse_env_file(&text).map_err(|error| {
        CheckResult::fail(
            check,
            format!("{ENV_PATH} cannot be read as systemd reads it ({error})"),
            REINSTALL_ENV,
        )
    })
}

/// Every required variable is set, and the domain and issuer match the target.
fn env_file_values(target: &CheckTarget, pairs: &[(String, String)]) -> Result<(), CheckResult> {
    let check = Check::EnvFile;
    let value = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    };
    let missing: Vec<&str> = REQUIRED_VARIABLES
        .iter()
        .copied()
        .filter(|key| value(key).is_none())
        .collect();
    if !missing.is_empty() {
        return Err(CheckResult::fail(
            check,
            format!("{ENV_PATH} lacks {}", missing.join(", ")),
            REINSTALL_ENV,
        ));
    }
    // Compared, never printed.
    let mut mismatched = Vec::new();
    if value("CALIN_PUBLIC_URL") != Some(target.public_url().as_str()) {
        mismatched.push("CALIN_PUBLIC_URL does not match the domain");
    }
    if !value("CALIN_OIDC_ISSUER").is_some_and(|issuer| same_issuer(target.issuer(), issuer)) {
        mismatched.push("CALIN_OIDC_ISSUER does not match the issuer");
    }
    if !mismatched.is_empty() {
        return Err(CheckResult::fail(
            check,
            format!("{ENV_PATH}: {}", mismatched.join("; ")),
            "check the domain and issuer given to check, or re-install calin.env",
        ));
    }
    Ok(())
}

fn is_public_health(body: &[u8]) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object()?;
    if object.get("name")?.as_str()? != "calin" {
        return None;
    }
    Some(object.len() == 2 && object.get("auth").and_then(|a| a.as_str()) == Some("required"))
}

fn loopback(target: &CheckTarget, probes: Probes<'_>) -> [CheckResult; 2] {
    let bind = target.layout().bind();
    let url = format!("http://{bind}/api/v1/health");
    let response = match probes.http.get(&url) {
        Ok(response) => response,
        Err(HttpError::Connect(_)) => {
            return [
                CheckResult::fail(
                    Check::LoopbackListening,
                    format!("nothing listens on {bind}"),
                    "`journalctl -u calin -n 50` shows why the service is not serving",
                ),
                CheckResult::unknown(Check::LoopbackHealth, "nothing to ask: the port is closed"),
            ];
        }
        Err(error) => {
            return [
                CheckResult::fail(
                    Check::LoopbackListening,
                    format!("{bind} does not answer HTTP: {error}"),
                    format!(
                        "make sure no other program uses port {}",
                        target.layout().port()
                    ),
                ),
                CheckResult::unknown(Check::LoopbackHealth, "no answer to ask"),
            ];
        }
    };
    let listening = CheckResult::pass(Check::LoopbackListening, format!("{bind} answers HTTP"));
    let health = match (response.status, is_public_health(&response.body)) {
        (200, Some(true)) => CheckResult::pass(
            Check::LoopbackHealth,
            "/api/v1/health answers {\"name\":\"calin\",\"auth\":\"required\"}: sign-in is required",
        ),
        (200, Some(false)) => CheckResult::fail(
            Check::LoopbackHealth,
            "Calin answers without requiring sign-in: public mode is off",
            format!(
                "check that the unit has EnvironmentFile={ENV_PATH} and that it sets CALIN_PUBLIC_URL"
            ),
        ),
        (status, _) => CheckResult::fail(
            Check::LoopbackHealth,
            format!("{url} answered HTTP {status} without Calin's health body"),
            format!(
                "another program may hold port {}; `ss -ltnp` shows which",
                target.layout().port()
            ),
        ),
    };
    [listening, health]
}

fn public_url(target: &CheckTarget, probes: Probes<'_>) -> CheckResult {
    let check = Check::PublicUrl;
    let origin = target.public_url();
    let url = format!("{origin}/api/v1/health");
    let domain = target.domain();
    match probes.http.get(&url) {
        Ok(response)
            if response.status == 200 && is_public_health(&response.body) == Some(true) =>
        {
            CheckResult::pass(
                check,
                format!("{origin} answers through the proxy with a valid certificate"),
            )
        }
        Ok(response) if matches!(response.status, 502..=504) => CheckResult::fail(
            check,
            format!(
                "the proxy answers HTTP {} at {origin}: it cannot reach Calin",
                response.status
            ),
            format!(
                "check that calin runs and that the Caddy site proxies to {}",
                target.layout().bind()
            ),
        ),
        Ok(response) => CheckResult::fail(
            check,
            format!(
                "{url} answered HTTP {} without Calin's health body",
                response.status
            ),
            "check that the main Caddyfile imports the Calin site, then `systemctl reload caddy`",
        ),
        Err(HttpError::Tls(detail)) => CheckResult::fail(
            check,
            format!("TLS to {domain} failed: {detail}"),
            "Caddy gets a certificate once the domain's DNS points here and ports 80 and 443 are open; `journalctl -u caddy` says why it has not",
        ),
        Err(HttpError::Connect(detail)) => CheckResult::fail(
            check,
            format!("cannot connect to {domain}: {detail}"),
            "check the DNS record for the domain, the firewall (port 443) and that Caddy runs",
        ),
        Err(error) => CheckResult::fail(
            check,
            format!("{url}: {error}"),
            "check that Caddy runs: `systemctl status caddy`",
        ),
    }
}

/// The same comparison Calin makes: equal, or differing by one final `/`.
fn same_issuer(configured: &str, published: &str) -> bool {
    configured == published
        || configured.strip_suffix('/') == Some(published)
        || published.strip_suffix('/') == Some(configured)
}

fn provider(target: &CheckTarget, probes: Probes<'_>) -> CheckResult {
    let check = Check::Provider;
    let issuer = target.issuer();
    let base = issuer.strip_suffix('/').unwrap_or(issuer);
    let url = format!("{base}/.well-known/openid-configuration");
    let remedy =
        "check the issuer URL with your identity provider, and that this server can reach it";
    let response = match probes.http.get(&url) {
        Ok(response) => response,
        Err(error) => return CheckResult::fail(check, format!("{url}: {error}"), remedy),
    };
    if response.status != 200 {
        return CheckResult::fail(
            check,
            format!("{url} answered HTTP {}", response.status),
            remedy,
        );
    }
    let published = serde_json::from_slice::<serde_json::Value>(&response.body)
        .ok()
        .and_then(|v| v.get("issuer")?.as_str().map(str::to_owned));
    match published {
        None => CheckResult::fail(
            check,
            format!("{url} is not an OpenID Connect discovery document"),
            remedy,
        ),
        Some(published) if same_issuer(issuer, &published) => {
            CheckResult::pass(check, format!("{url} is reachable and names this issuer"))
        }
        Some(published) => CheckResult::fail(
            check,
            format!("the provider calls itself `{published}`, not `{issuer}`"),
            format!("set CALIN_OIDC_ISSUER to `{published}` (re-run install with it)"),
        ),
    }
}

/// Runs a program the way the service would: as its user, with its HOME and
/// PATH. Errs with a reason when this process cannot become that user.
fn as_service_user(
    target: &CheckTarget,
    probes: Probes<'_>,
    program: &str,
    args: &[&str],
) -> Result<Result<CommandOutput, CommandError>, String> {
    let layout = target.layout();
    let user = layout.user();
    let me = match probes.commands.run("id", &["-un"]) {
        Ok(output) if output.code == Some(0) => output.stdout.trim().to_owned(),
        _ => return Err("cannot tell which user runs this check".to_owned()),
    };
    let home = format!("HOME={}", layout.home_dir().display());
    let path = format!("PATH={}", service_path(layout));
    let mut env_args: Vec<&str> = vec![&home, &path, program];
    env_args.extend_from_slice(args);
    if me == user {
        Ok(probes.commands.run("env", &env_args))
    } else if me == "root" {
        let mut runuser: Vec<&str> = vec!["-u", user, "--", "env"];
        runuser.extend_from_slice(&env_args);
        match probes.commands.run("runuser", &runuser) {
            Err(CommandError::NotFound) => Err("runuser not found".to_owned()),
            other => Ok(other),
        }
    } else {
        Err(format!(
            "run the check as root or as `{user}` to check this"
        ))
    }
}

fn agent_clis(target: &CheckTarget, probes: Probes<'_>) -> [CheckResult; 2] {
    [claude_cli(target, probes), gh_cli(target, probes)]
}

fn claude_cli(target: &CheckTarget, probes: Probes<'_>) -> CheckResult {
    let user = target.layout().user();
    match as_service_user(target, probes, "claude", &["--version"]) {
        Err(reason) => CheckResult::unknown(Check::Claude, reason),
        Ok(Ok(output)) if output.code == Some(0) => {
            let version = output.stdout.lines().next().unwrap_or_default().trim();
            CheckResult::pass(
                Check::Claude,
                format!("`claude --version` runs as {user}: {version}"),
            )
        }
        // `env` exits 127 when it cannot find the program.
        Ok(Ok(output)) if output.code == Some(127) => CheckResult::fail(
            Check::Claude,
            format!("`claude` is not on {user}'s PATH"),
            format!(
                "install Claude Code for {user} (in {}/.local/bin or /usr/local/bin)",
                target.layout().home_dir().display()
            ),
        ),
        Ok(Ok(output)) => CheckResult::fail(
            Check::Claude,
            format!("`claude --version` exited with {}", exit_text(&output)),
            format!("run `sudo -u {user} -H claude --version` to see why"),
        ),
        Ok(Err(error)) => {
            CheckResult::unknown(Check::Claude, format!("could not run claude: {error}"))
        }
    }
}

/// The output of `gh auth status` is not shown: it describes the token.
fn gh_cli(target: &CheckTarget, probes: Probes<'_>) -> CheckResult {
    let user = target.layout().user();
    match as_service_user(target, probes, "gh", &["auth", "status"]) {
        Err(reason) => CheckResult::unknown(Check::Gh, reason),
        Ok(Ok(output)) if output.code == Some(0) => {
            CheckResult::pass(Check::Gh, format!("`gh auth status` succeeds as {user}"))
        }
        Ok(Ok(output)) if output.code == Some(127) => CheckResult::fail(
            Check::Gh,
            format!("`gh` is not on {user}'s PATH"),
            "install the GitHub CLI (gh)",
        ),
        Ok(Ok(output)) => CheckResult::fail(
            Check::Gh,
            format!(
                "`gh auth status` exited with {}: not signed in",
                exit_text(&output)
            ),
            format!("sign in as {user}: in Calin's terminal, or `sudo -u {user} -H gh auth login`"),
        ),
        Ok(Err(error)) => CheckResult::unknown(Check::Gh, format!("could not run gh: {error}")),
    }
}

fn exit_text(output: &CommandOutput) -> String {
    match output.code {
        Some(code) => format!("code {code}"),
        None => "a signal".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::input::InstallInput;
    use crate::plan::install_plan;
    use crate::render::tests::{AWKWARD_SECRET, awkward, typical};

    const HEALTH: &str = r#"{"name":"calin","auth":"required"}"#;
    const ISSUER: &str = "https://example.eu.auth0.com/";

    /// A machine where everything is installed and working, which each test
    /// then breaks in one place.
    struct Fake {
        commands: HashMap<String, Result<CommandOutput, CommandError>>,
        http: HashMap<String, Result<HttpResponse, HttpError>>,
        files: HashMap<String, (io::Result<FileInfo>, io::Result<Vec<u8>>)>,
        ran: Mutex<Vec<String>>,
    }

    fn ok(code: i32, stdout: &str) -> Result<CommandOutput, CommandError> {
        Ok(CommandOutput {
            code: Some(code),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }

    fn page(status: u16, body: &str) -> Result<HttpResponse, HttpError> {
        Ok(HttpResponse {
            status,
            body: body.as_bytes().to_vec(),
        })
    }

    fn io_error(kind: io::ErrorKind) -> io::Error {
        io::Error::from(kind)
    }

    const CLAUDE: &str = "runuser -u calin -- env HOME=/home/calin PATH=/home/calin/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin claude --version";
    const GH: &str = "runuser -u calin -- env HOME=/home/calin PATH=/home/calin/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin gh auth status";

    impl Fake {
        fn healthy(input: &InstallInput) -> Self {
            let plan = install_plan(input, Path::new("/stage")).unwrap();
            let mut files = HashMap::new();
            for file in plan.files {
                files.insert(
                    file.target.display().to_string(),
                    (
                        Ok(FileInfo {
                            mode: file.mode,
                            uid: 0,
                        }),
                        Ok(file.content.into_bytes()),
                    ),
                );
            }
            let mut commands = HashMap::new();
            commands.insert(
                "systemctl is-active calin.service".to_owned(),
                ok(0, "active\n"),
            );
            commands.insert(
                "systemctl is-enabled calin.service".to_owned(),
                ok(0, "enabled\n"),
            );
            commands.insert("id -un".to_owned(), ok(0, "root\n"));
            commands.insert(CLAUDE.to_owned(), ok(0, "2.1.0 (Claude Code)\n"));
            commands.insert(GH.to_owned(), ok(0, "Logged in to github.com account x\n"));
            let mut http = HashMap::new();
            http.insert(
                "http://127.0.0.1:4680/api/v1/health".to_owned(),
                page(200, HEALTH),
            );
            http.insert(
                "https://calin.example.com/api/v1/health".to_owned(),
                page(200, HEALTH),
            );
            http.insert(
                "https://example.eu.auth0.com/.well-known/openid-configuration".to_owned(),
                page(200, &format!(r#"{{"issuer":"{ISSUER}","jwks_uri":"x"}}"#)),
            );
            Self {
                commands,
                http,
                files,
                ran: Mutex::new(Vec::new()),
            }
        }

        fn command(mut self, line: &str, result: Result<CommandOutput, CommandError>) -> Self {
            self.commands.insert(line.to_owned(), result);
            self
        }
        fn http(mut self, url: &str, result: Result<HttpResponse, HttpError>) -> Self {
            self.http.insert(url.to_owned(), result);
            self
        }
        fn file(
            mut self,
            path: &str,
            info: io::Result<FileInfo>,
            content: io::Result<Vec<u8>>,
        ) -> Self {
            self.files.insert(path.to_owned(), (info, content));
            self
        }
        fn probes(&self) -> Probes<'_> {
            Probes {
                commands: self,
                http: self,
                files: self,
            }
        }
    }

    fn clone_io<T: Clone>(result: &io::Result<T>) -> io::Result<T> {
        match result {
            Ok(value) => Ok(value.clone()),
            Err(error) => Err(io::Error::from(error.kind())),
        }
    }

    impl CommandRunner for Fake {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
            let line = std::iter::once(program)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            self.ran.lock().unwrap().push(line.clone());
            self.commands
                .get(&line)
                .cloned()
                .unwrap_or(Err(CommandError::NotFound))
        }
    }

    impl HttpGet for Fake {
        fn get(&self, url: &str) -> Result<HttpResponse, HttpError> {
            self.http
                .get(url)
                .cloned()
                .unwrap_or_else(|| Err(HttpError::Connect("connection refused".into())))
        }
    }

    impl FileProbe for Fake {
        fn metadata(&self, path: &Path) -> io::Result<FileInfo> {
            match self.files.get(&path.display().to_string()) {
                Some((info, _)) => clone_io(info),
                None => Err(io_error(io::ErrorKind::NotFound)),
            }
        }
        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            match self.files.get(&path.display().to_string()) {
                Some((_, content)) => clone_io(content),
                None => Err(io_error(io::ErrorKind::NotFound)),
            }
        }
    }

    fn input() -> InstallInput {
        typical().client_secret(AWKWARD_SECRET).build().unwrap()
    }

    fn run(fake: &Fake) -> CheckReport {
        run_checks(&input().check_target(), fake.probes())
    }

    fn status(report: &CheckReport, check: Check) -> Status {
        report.get(check).unwrap().status
    }

    /// Every output a report can produce, for leak checks.
    fn all_text(report: &CheckReport) -> String {
        format!("{report}\n{report:?}")
    }

    #[test]
    fn a_healthy_installation_passes_everything() {
        let fake = Fake::healthy(&input());
        let report = run(&fake);
        for result in &report.results {
            assert_eq!(result.status, Status::Pass, "{result:?}");
            assert!(result.remedy.is_none());
        }
        assert_eq!(report.results.len(), 10);
        assert!(report.is_usable());
        let text = report.to_string();
        assert!(text.ends_with("Usable: yes\n"), "{text}");
        assert!(!all_text(&report).contains("s3cr"));
        // Nothing but read-only commands ran.
        for line in fake.ran.lock().unwrap().iter() {
            assert!(
                line.starts_with("systemctl is-")
                    || line == "id -un"
                    || line == CLAUDE
                    || line == GH,
                "{line}"
            );
        }
    }

    #[test]
    fn the_unit_file_missing_drifted_or_unreadable() {
        let input = input();
        let rendered = render_unit(input.layout());
        let info = || {
            Ok(FileInfo {
                mode: 0o644,
                uid: 0,
            })
        };

        let fake = Fake::healthy(&input).file(
            UNIT_PATH,
            Err(io_error(io::ErrorKind::NotFound)),
            Err(io_error(io::ErrorKind::NotFound)),
        );
        let report = run(&fake);
        assert_eq!(status(&report, Check::UnitFile), Status::Fail);
        // A drifted or missing unit alone does not make Calin unusable.
        assert!(report.is_usable());

        let restyled: String = rendered
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| format!("{}  \n\n# a note\n", l.replace(' ', "   ")))
            .collect();
        let fake = Fake::healthy(&input).file(UNIT_PATH, info(), Ok(restyled.into_bytes()));
        assert_eq!(status(&run(&fake), Check::UnitFile), Status::Pass);

        let drifted = rendered.replace(
            "ProtectSystem=strict",
            "ProtectSystem=full\nEnvironment=TOKEN=hidden-value",
        );
        let fake = Fake::healthy(&input).file(
            UNIT_PATH,
            Ok(FileInfo {
                mode: 0o644,
                uid: 0,
            }),
            Ok(drifted.into_bytes()),
        );
        let report = run(&fake);
        let result = report.get(Check::UnitFile).unwrap();
        assert_eq!(result.status, Status::Fail);
        assert!(result.reason.contains("ProtectSystem=strict"), "{result:?}");
        assert!(!all_text(&report).contains("hidden-value"));
        assert!(result.remedy.is_some());

        let extra = format!("{rendered}\n[Service]\nNice=5\n");
        let fake = Fake::healthy(&input).file(
            UNIT_PATH,
            Ok(FileInfo {
                mode: 0o644,
                uid: 0,
            }),
            Ok(extra.into_bytes()),
        );
        assert_eq!(status(&run(&fake), Check::UnitFile), Status::Fail);

        let fake = Fake::healthy(&input).file(
            UNIT_PATH,
            Ok(FileInfo {
                mode: 0o644,
                uid: 0,
            }),
            Err(io_error(io::ErrorKind::PermissionDenied)),
        );
        assert_eq!(status(&run(&fake), Check::UnitFile), Status::CouldNotCheck);
    }

    #[test]
    fn the_service_states() {
        let base = || Fake::healthy(&input());
        let report = run(&base().command("systemctl is-active calin.service", ok(3, "inactive\n")));
        let result = report.get(Check::ServiceActive).unwrap();
        assert_eq!(result.status, Status::Fail);
        assert!(result.reason.contains("inactive"));
        assert!(result.remedy.as_deref().unwrap().contains("journalctl"));
        assert_eq!(report.usability(), Usability::NotUsable);
        assert!(report.to_string().ends_with("Usable: no\n"));

        let report =
            run(&base().command("systemctl is-enabled calin.service", ok(1, "disabled\n")));
        assert_eq!(status(&report, Check::ServiceEnabled), Status::Fail);
        assert!(report.is_usable());

        let report = run(&base()
            .command(
                "systemctl is-active calin.service",
                Err(CommandError::NotFound),
            )
            .command(
                "systemctl is-enabled calin.service",
                Err(CommandError::NotFound),
            ));
        assert_eq!(status(&report, Check::ServiceActive), Status::CouldNotCheck);
        assert_eq!(
            status(&report, Check::ServiceEnabled),
            Status::CouldNotCheck
        );
        assert_eq!(report.usability(), Usability::Unknown);
    }

    #[test]
    fn the_environment_file() {
        let input = input();
        let env = crate::render::render_env(&input);
        let root600 = || {
            Ok(FileInfo {
                mode: 0o600,
                uid: 0,
            })
        };
        let case = |info: io::Result<FileInfo>, content: io::Result<Vec<u8>>| {
            let fake = Fake::healthy(&input).file(ENV_PATH, info, content);
            let report = run(&fake);
            assert!(!all_text(&report).contains("s3cr"), "{report}");
            assert!(!all_text(&report).contains("Xq3BfZ9kLmN0pRsT"), "{report}");
            assert!(!all_text(&report).contains("owner@example.com"), "{report}");
            report.get(Check::EnvFile).unwrap().clone()
        };

        let missing = case(
            Err(io_error(io::ErrorKind::NotFound)),
            Err(io_error(io::ErrorKind::NotFound)),
        );
        assert_eq!(missing.status, Status::Fail);

        let loose = case(
            Ok(FileInfo {
                mode: 0o644,
                uid: 0,
            }),
            Ok(env.clone().into_bytes()),
        );
        assert_eq!(loose.status, Status::Fail);
        assert!(loose.reason.contains("0644"));
        assert_eq!(
            loose.remedy.as_deref(),
            Some("chmod 600 /etc/calin/calin.env")
        );

        let not_root = case(
            Ok(FileInfo {
                mode: 0o600,
                uid: 1000,
            }),
            Ok(env.clone().into_bytes()),
        );
        assert_eq!(not_root.status, Status::Fail);
        assert!(not_root.remedy.unwrap().contains("chown"));

        let unreadable = case(root600(), Err(io_error(io::ErrorKind::PermissionDenied)));
        assert_eq!(unreadable.status, Status::CouldNotCheck);
        assert!(unreadable.remedy.unwrap().contains("root"));

        let no_stat = case(
            Err(io_error(io::ErrorKind::PermissionDenied)),
            Err(io_error(io::ErrorKind::PermissionDenied)),
        );
        assert_eq!(no_stat.status, Status::CouldNotCheck);

        let without_allow_list: String = env
            .lines()
            .filter(|l| !l.starts_with("CALIN_ALLOWED_USERS"))
            .map(|l| format!("{l}\n"))
            .collect();
        let lacking = case(root600(), Ok(without_allow_list.into_bytes()));
        assert_eq!(lacking.status, Status::Fail);
        assert!(lacking.reason.contains("CALIN_ALLOWED_USERS"));

        let broken = format!("{env}CALIN_X=\"s3cr unterminated\n");
        let broken = case(root600(), Ok(broken.into_bytes()));
        assert_eq!(broken.status, Status::Fail);
        assert!(broken.reason.contains("line"));

        let other_domain = env.replace("https://calin.example.com", "https://other.example.com");
        let other = case(root600(), Ok(other_domain.into_bytes()));
        assert_eq!(other.status, Status::Fail);
        assert!(other.reason.contains("CALIN_PUBLIC_URL does not match"));
        assert!(!other.reason.contains("CALIN_OIDC_ISSUER"), "{other:?}");

        let other_issuer = env.replace(ISSUER, "https://other.example.com/");
        let other = case(root600(), Ok(other_issuer.into_bytes()));
        assert_eq!(other.status, Status::Fail);
        assert_eq!(
            other.reason,
            "/etc/calin/calin.env: CALIN_OIDC_ISSUER does not match the issuer"
        );

        let both = env
            .replace("https://calin.example.com", "https://other.example.com")
            .replace(ISSUER, "https://other.example.com/");
        let both = case(root600(), Ok(both.into_bytes()));
        assert_eq!(
            both.reason,
            "/etc/calin/calin.env: CALIN_PUBLIC_URL does not match the domain; CALIN_OIDC_ISSUER does not match the issuer"
        );

        let not_utf8 = case(root600(), Ok(b"CALIN_X=\xff\n".to_vec()));
        assert_eq!(not_utf8.status, Status::Fail);
        assert!(not_utf8.reason.contains("not valid UTF-8"), "{not_utf8:?}");

        let read_error = case(root600(), Err(io_error(io::ErrorKind::InvalidData)));
        assert_eq!(read_error.status, Status::CouldNotCheck);
        assert!(
            read_error.reason.starts_with("cannot read"),
            "{read_error:?}"
        );
        assert!(read_error.remedy.is_none());

        let ok = case(root600(), Ok(env.into_bytes()));
        assert_eq!(ok.status, Status::Pass);
    }

    #[test]
    fn the_loopback_port_and_public_mode() {
        const LOOPBACK: &str = "http://127.0.0.1:4680/api/v1/health";
        let base = || Fake::healthy(&input());

        let report = run(&base().http(LOOPBACK, Err(HttpError::Connect("refused".into()))));
        assert_eq!(status(&report, Check::LoopbackListening), Status::Fail);
        assert_eq!(
            status(&report, Check::LoopbackHealth),
            Status::CouldNotCheck
        );
        assert!(!report.is_usable());

        let report = run(&base().http(LOOPBACK, Err(HttpError::TimedOut)));
        assert_eq!(status(&report, Check::LoopbackListening), Status::Fail);

        let local = r#"{"name":"calin","version":"0.0.1","dry_run":false,"runners":[]}"#;
        let report = run(&base().http(LOOPBACK, page(200, local)));
        assert_eq!(status(&report, Check::LoopbackListening), Status::Pass);
        let result = report.get(Check::LoopbackHealth).unwrap();
        assert_eq!(result.status, Status::Fail);
        assert!(result.reason.contains("public mode is off"));

        let report = run(&base().http(LOOPBACK, page(404, "<html>nginx</html>")));
        assert_eq!(status(&report, Check::LoopbackHealth), Status::Fail);
        let report = run(&base().http(LOOPBACK, page(200, "not json")));
        assert_eq!(status(&report, Check::LoopbackHealth), Status::Fail);
        let other = r#"{"name":"other","auth":"required"}"#;
        let report = run(&base().http(LOOPBACK, page(200, other)));
        let result = report.get(Check::LoopbackHealth).unwrap();
        assert_eq!(result.status, Status::Fail);
        assert!(result.reason.contains("without Calin's health body"));
    }

    #[test]
    fn the_public_url() {
        const PUBLIC: &str = "https://calin.example.com/api/v1/health";
        let base = || Fake::healthy(&input());
        let cases = [
            (
                Err(HttpError::Tls(
                    "invalid peer certificate: UnknownIssuer".into(),
                )),
                "TLS",
                "DNS",
            ),
            (
                Err(HttpError::Connect("dns error".into())),
                "cannot connect",
                "DNS",
            ),
            (Err(HttpError::TimedOut), "no answer", "Caddy"),
            (
                page(502, ""),
                "cannot reach Calin",
                "proxies to 127.0.0.1:4680",
            ),
            (
                page(200, "<html>Caddy works!</html>"),
                "without Calin's health",
                "import",
            ),
        ];
        for (answer, reason, remedy) in cases {
            let report = run(&base().http(PUBLIC, answer));
            let result = report.get(Check::PublicUrl).unwrap();
            assert_eq!(result.status, Status::Fail, "{result:?}");
            assert!(result.reason.contains(reason), "{result:?}");
            assert!(
                result.remedy.as_deref().unwrap().contains(remedy),
                "{result:?}"
            );
            assert_eq!(report.usability(), Usability::NotUsable);
        }
    }

    #[test]
    fn the_provider() {
        const DISCOVERY: &str = "https://example.eu.auth0.com/.well-known/openid-configuration";
        let base = || Fake::healthy(&input());
        // Auth0 publishes its issuer with a final slash; configured without is fine.
        let report = run(&base().http(
            DISCOVERY,
            page(200, r#"{"issuer":"https://example.eu.auth0.com"}"#),
        ));
        assert_eq!(status(&report, Check::Provider), Status::Pass);

        let report = run(&base().http(
            DISCOVERY,
            page(200, r#"{"issuer":"https://evil.example.com/"}"#),
        ));
        let result = report.get(Check::Provider).unwrap();
        assert_eq!(result.status, Status::Fail);
        assert!(
            result
                .remedy
                .as_deref()
                .unwrap()
                .contains("https://evil.example.com/")
        );

        for answer in [
            page(404, "not found"),
            page(200, "[]"),
            page(200, r#"{"issuer":5}"#),
            Err(HttpError::Connect("dns error".into())),
        ] {
            let report = run(&base().http(DISCOVERY, answer));
            assert_eq!(status(&report, Check::Provider), Status::Fail);
        }
    }

    #[test]
    fn the_agent_clis() {
        let base = || Fake::healthy(&input());

        let report = run(&base().command(CLAUDE, ok(127, "")));
        let result = report.get(Check::Claude).unwrap();
        assert_eq!(result.status, Status::Fail);
        assert!(result.reason.contains("not on calin's PATH"));
        assert!(report.is_usable());

        let report = run(&base().command(CLAUDE, ok(1, "")));
        assert_eq!(status(&report, Check::Claude), Status::Fail);

        let report = run(&base().command(GH, ok(1, "token gho_SECRETTOKEN invalid")));
        let result = report.get(Check::Gh).unwrap();
        assert_eq!(result.status, Status::Fail);
        assert!(result.remedy.as_deref().unwrap().contains("gh auth login"));
        assert!(!all_text(&report).contains("gho_SECRETTOKEN"));

        let report = run(&base().command(GH, ok(127, "")));
        assert_eq!(status(&report, Check::Gh), Status::Fail);

        // Neither root nor the service user: both cannot be checked.
        let report = run(&base().command("id -un", ok(0, "alice\n")));
        for check in [Check::Claude, Check::Gh] {
            let result = report.get(check).unwrap();
            assert_eq!(result.status, Status::CouldNotCheck);
            assert!(result.reason.contains("as root or as `calin`"));
        }

        // As the service user itself, no runuser is needed.
        let fake = base()
            .command("id -un", ok(0, "calin\n"))
            .command(
                "env HOME=/home/calin PATH=/home/calin/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin claude --version",
                ok(0, "2.1.0\n"),
            )
            .command(
                "env HOME=/home/calin PATH=/home/calin/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin gh auth status",
                ok(0, ""),
            );
        let report = run(&fake);
        assert_eq!(status(&report, Check::Claude), Status::Pass);
        assert_eq!(status(&report, Check::Gh), Status::Pass);

        // Root without runuser.
        let report = run(&base().command(CLAUDE, Err(CommandError::NotFound)));
        assert_eq!(status(&report, Check::Claude), Status::CouldNotCheck);

        let report = run(&base().command("id -un", Err(CommandError::NotFound)));
        assert_eq!(status(&report, Check::Gh), Status::CouldNotCheck);

        let report = run(&base().command(CLAUDE, Err(CommandError::TimedOut)));
        assert_eq!(status(&report, Check::Claude), Status::CouldNotCheck);

        let report = run(&base().command(GH, Err(CommandError::TimedOut)));
        let result = report.get(Check::Gh).unwrap();
        assert_eq!(result.status, Status::CouldNotCheck);
        assert!(result.reason.starts_with("could not run gh"), "{result:?}");

        let killed = Ok(CommandOutput {
            code: None,
            stdout: String::new(),
            stderr: String::new(),
        });
        let report = run(&base().command(CLAUDE, killed));
        let result = report.get(Check::Claude).unwrap();
        assert_eq!(result.status, Status::Fail);
        assert_eq!(result.reason, "`claude --version` exited with a signal");
    }

    #[test]
    fn the_check_uses_the_layout() {
        let input = awkward().build().unwrap();
        let fake = Fake::healthy(&input);
        let report = run_checks(&input.check_target(), fake.probes());
        // The fake only knows the typical URLs: the awkward ones are probed.
        assert_eq!(status(&report, Check::UnitFile), Status::Pass);
        assert_eq!(status(&report, Check::EnvFile), Status::Pass);
        assert_eq!(status(&report, Check::LoopbackListening), Status::Fail);
        assert!(
            report
                .get(Check::LoopbackListening)
                .unwrap()
                .reason
                .contains("127.0.0.1:18080")
        );
        assert!(!all_text(&report).contains("s3cr"));
        let ran = fake.ran.lock().unwrap().join("\n");
        assert!(
            ran.contains("runuser -u agents -- env HOME=/var/lib/agents-home"),
            "{ran}"
        );
    }

    #[test]
    fn the_table_lists_every_check_with_its_fix() {
        let fake =
            Fake::healthy(&input()).command("systemctl is-active calin.service", ok(3, "failed\n"));
        let text = run(&fake).to_string();
        for check in [
            Check::UnitFile,
            Check::ServiceActive,
            Check::ServiceEnabled,
            Check::EnvFile,
            Check::LoopbackListening,
            Check::LoopbackHealth,
            Check::PublicUrl,
            Check::Provider,
            Check::Claude,
            Check::Gh,
        ] {
            assert!(text.contains(check.label()), "{}", check.label());
        }
        let line = text
            .lines()
            .find(|l| l.starts_with("service running"))
            .unwrap();
        assert_eq!(line.split_whitespace().nth(2), Some("FAIL"), "{text}");
        assert!(text.contains("-> `journalctl"), "{text}");
    }
}
