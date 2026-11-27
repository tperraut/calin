//! Starts the real `calin` binary in each mode, on loopback, in dry run:
//! what it refuses to start with, and what it answers once it serves.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use tempfile::TempDir;

type Vars = Vec<(&'static str, &'static str)>;

const TIMEOUT: Duration = Duration::from_secs(30);
const SWITCH: &str = "CALIN_UNSAFE_DEV_ALLOW_HTTP_LOOPBACK";

/// `calin serve` on a free loopback port with `home` as its data directory
/// and nothing of this machine's environment but `PATH`.
fn calin(home: &Path, vars: &[(&str, &str)]) -> Command {
    calin_on("127.0.0.1:0", home, vars)
}

fn calin_on(bind: &str, home: &Path, vars: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_calin"));
    command
        .args(["serve", "--bind", bind, "--home"])
        .arg(home)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("CALIN_DRY_RUN", "1")
        .envs(vars.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Runs to the end; for starts that must fail.
fn run(home: &Path, vars: &[(&str, &str)]) -> Output {
    calin(home, vars).output().unwrap()
}

struct Serving {
    child: Child,
    /// `127.0.0.1:<port>`
    address: String,
}

impl Serving {
    /// Starts the server and waits for the line that says where it listens.
    fn start(home: &Path, vars: &[(&str, &str)]) -> Self {
        let mut child = calin(home, vars).spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (line_out, line_in) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = line_out.send(line);
        });
        let line = line_in.recv_timeout(TIMEOUT).unwrap_or_default();
        let Some(address) = line.trim().strip_prefix("Calin is serving on http://") else {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "the server did not start: {line:?}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        Self {
            child,
            address: address.to_owned(),
        }
    }

    /// One request; the status and the body.
    fn get(&self, path: &str, headers: &[(&str, &str)]) -> (u16, String) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let mut request = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.address
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response.split(' ').nth(1).unwrap().parse().unwrap();
        let body = response.split_once("\r\n\r\n").map(|(_, body)| body);
        (status, body.unwrap_or_default().to_owned())
    }

    /// SIGTERM, as systemd stops a service; the exit must be a clean one.
    fn stop(mut self) -> String {
        let pid = self.child.id().to_string();
        assert!(
            Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .unwrap()
                .success()
        );
        let mut log = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            stderr.read_to_string(&mut log).unwrap();
        }
        let status = self.child.wait().unwrap();
        assert!(status.success(), "{status:?}\n{log}");
        log
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// A complete public-mode configuration whose provider is a loopback port
/// nothing listens on: the server must start all the same.
const DEV: [(&str, &str); 5] = [
    ("CALIN_PUBLIC_URL", "http://localhost:4680"),
    ("CALIN_OIDC_ISSUER", "http://127.0.0.1:9"),
    ("CALIN_OIDC_CLIENT_ID", "calin-dev"),
    ("CALIN_ALLOWED_USERS", "alice@example.com"),
    (SWITCH, "1"),
];

#[test]
fn local_mode_serves_without_sign_in_and_keeps_no_auth_file() {
    let home = TempDir::new().unwrap();
    let serving = Serving::start(home.path(), &[]);
    let (status, body) = serving.get("/api/v1/me", &[]);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""mode":"local""#), "{body}");
    assert!(body.contains(r#""identity":null"#), "{body}");
    assert_eq!(serving.get("/api/v1/sessions", &[]).0, 200);
    assert_eq!(serving.get("/auth/login", &[]).0, 404);
    assert_eq!(serving.get("/api/v1/tokens", &[]).0, 409);
    serving.stop();
    assert!(!home.path().join("auth.json").exists());
    assert!(!home.path().join("auth.json.tmp").exists());
}

#[test]
fn public_mode_starts_with_the_provider_down_and_asks_for_sign_in() {
    let home = TempDir::new().unwrap();
    let serving = Serving::start(home.path(), &DEV);
    let (status, body) = serving.get("/api/v1/health", &[]);
    assert_eq!(status, 200);
    assert_eq!(body, r#"{"name":"calin","auth":"required"}"#);
    for path in [
        "/api/v1/me",
        "/api/v1/sessions",
        "/api/v1/settings",
        "/api/v1/ws",
    ] {
        assert_eq!(serving.get(path, &[]).0, 401, "{path}");
    }
    let stranger = [(
        "Authorization",
        "Bearer calin_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )];
    assert_eq!(serving.get("/api/v1/me", &stranger).0, 401);
    // The web app itself needs no session; sign-in cannot start without a provider.
    assert_eq!(serving.get("/", &[]).0, 200);
    let (status, body) = serving.get("/auth/login", &[]);
    assert_eq!(status, 503, "{body}");
    assert!(body.contains("temporarily unavailable"), "{body}");
    let log = serving.stop();
    assert!(log.contains("public mode: sign-in is required"), "{log}");
    assert!(log.contains(SWITCH), "the switch is not announced:\n{log}");
}

#[test]
fn a_configuration_error_prints_every_problem_and_nothing_listens() {
    let home = TempDir::new().unwrap();
    // (variables, lines the error must hold)
    let cases: Vec<(Vars, Vec<&str>)> = vec![
        (
            vec![("CALIN_PUBLIC_URL", "https://calin.example.com")],
            vec![
                "CALIN_OIDC_ISSUER is not set",
                "CALIN_OIDC_CLIENT_ID is not set",
                "CALIN_ALLOWED_USERS",
            ],
        ),
        // The development configuration without its switch.
        (
            DEV[..4].to_vec(),
            vec![
                "CALIN_PUBLIC_URL must be an https URL",
                "CALIN_OIDC_ISSUER must be an https URL",
            ],
        ),
        (
            DEV[..4].iter().copied().chain([(SWITCH, "true")]).collect(),
            vec!["CALIN_UNSAFE_DEV_ALLOW_HTTP_LOOPBACK must be 1 or not set"],
        ),
        // Sign-in configured, but not the URL that turns it on.
        (
            vec![("CALIN_ALLOWED_USERS", "alice@example.com")],
            vec!["CALIN_PUBLIC_URL is not set although CALIN_ALLOWED_USERS is set"],
        ),
        (
            vec![("CALIN_PUBLIC_URL", "")],
            vec!["CALIN_PUBLIC_URL is empty"],
        ),
    ];
    for (vars, expected) in cases {
        let output = run(home.path(), &vars);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{vars:?} started");
        // The serving line is the only thing ever printed to stdout.
        assert!(output.stdout.is_empty(), "{vars:?} listened");
        for line in expected {
            assert!(stderr.contains(line), "{vars:?}: {line:?} not in\n{stderr}");
        }
    }
    // Refused before the data directory was even opened.
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn a_bind_address_that_is_not_loopback_is_refused_in_both_modes() {
    let home = TempDir::new().unwrap();
    for vars in [&[][..], &DEV[..]] {
        let output = calin_on("0.0.0.0:0", home.path(), vars).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("only binds loopback addresses"), "{stderr}");
    }
}
