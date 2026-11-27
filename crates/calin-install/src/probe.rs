//! The real probes: processes, HTTP(S) and the file system of this machine.
//! All read-only.

use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper::Request;
use hyper::header;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

use crate::check::{
    CommandError, CommandOutput, CommandRunner, FileInfo, FileProbe, HttpError, HttpGet,
    HttpResponse, Probes,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Health answers and discovery documents are small; nothing larger is read.
const MAX_BODY: usize = 256 * 1024;
const USER_AGENT: &str = concat!("calin-install/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, thiserror::Error)]
#[error("cannot set up TLS: {0}")]
pub struct ProbeSetupError(String);

/// Probes of this machine. HTTPS is rustls on the `ring` provider with the
/// Mozilla roots compiled in, like `calin-auth`: no system TLS library.
pub struct SystemProbes {
    client: Client<HttpsConnector<HttpConnector>, Empty<Bytes>>,
}

impl SystemProbes {
    pub fn new() -> Result<Self, ProbeSetupError> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| ProbeSetupError(error.to_string()))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let mut tcp = HttpConnector::new();
        tcp.enforce_http(false);
        tcp.set_connect_timeout(Some(CONNECT_TIMEOUT));
        // Plain HTTP is needed for the loopback health check.
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .wrap_connector(tcp);
        let client = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(0)
            .build(connector);
        Ok(Self { client })
    }

    pub fn probes(&self) -> Probes<'_> {
        Probes {
            commands: self,
            http: self,
            files: self,
        }
    }

    async fn fetch(&self, url: &str) -> Result<HttpResponse, HttpError> {
        let request = Request::get(url)
            .header(header::USER_AGENT, USER_AGENT)
            .header(header::ACCEPT, "application/json")
            .body(Empty::new())
            .map_err(|error| HttpError::Other(error.to_string()))?;
        let response = self.client.request(request).await.map_err(classify)?;
        let status = response.status().as_u16();
        let mut body = response.into_body();
        let mut collected = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|error| HttpError::Other(chain(&error)))?;
            if let Some(data) = frame.data_ref() {
                if collected.len() + data.len() > MAX_BODY {
                    return Err(HttpError::Other(format!(
                        "the answer is larger than {MAX_BODY} bytes"
                    )));
                }
                collected.extend_from_slice(data);
            }
        }
        Ok(HttpResponse {
            status,
            body: collected,
        })
    }
}

/// An error and its causes on one line.
fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn classify(error: hyper_util::client::legacy::Error) -> HttpError {
    let text = chain(&error);
    if is_tls(&error) {
        HttpError::Tls(text)
    } else if error.is_connect() {
        HttpError::Connect(text)
    } else {
        HttpError::Other(text)
    }
}

/// Whether a rustls error is anywhere in the chain. hyper-rustls wraps the
/// handshake's `io::Error` in another one, and `io::Error::source` skips the
/// error it wraps, so each `io::Error` is opened by hand.
fn is_tls(error: &(dyn std::error::Error + 'static)) -> bool {
    if error.downcast_ref::<rustls::Error>().is_some() {
        return true;
    }
    if let Some(inner) = error
        .downcast_ref::<io::Error>()
        .and_then(|io| io.get_ref())
        && is_tls(inner)
    {
        return true;
    }
    error.source().is_some_and(is_tls)
}

impl HttpGet for SystemProbes {
    /// Runs on a thread of its own with its own runtime, so it can be called
    /// from anywhere, inside an async runtime included.
    fn get(&self, url: &str) -> Result<HttpResponse, HttpError> {
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| HttpError::Other(error.to_string()))?;
                runtime.block_on(async {
                    match tokio::time::timeout(REQUEST_TIMEOUT, self.fetch(url)).await {
                        Ok(result) => result,
                        Err(_) => Err(HttpError::TimedOut),
                    }
                })
            });
            worker
                .join()
                .unwrap_or_else(|_| Err(HttpError::Other("the request thread panicked".into())))
        })
    }
}

impl CommandRunner for SystemProbes {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => CommandError::NotFound,
                _ => CommandError::Failed(error.to_string()),
            })?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        std::thread::scope(|scope| {
            // Drained while waiting, so a chatty program cannot fill a pipe and stall.
            let out = scope.spawn(|| read_all(stdout));
            let err = scope.spawn(|| read_all(stderr));
            let started = Instant::now();
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Ok(status),
                    Ok(None) if started.elapsed() > COMMAND_TIMEOUT => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(CommandError::TimedOut);
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                    Err(error) => break Err(CommandError::Failed(error.to_string())),
                }
            };
            let stdout = out.join().unwrap_or_default();
            let stderr = err.join().unwrap_or_default();
            status.map(|status| CommandOutput {
                code: status.code(),
                stdout,
                stderr,
            })
        })
    }
}

fn read_all(stream: Option<impl io::Read>) -> String {
    let mut bytes = Vec::new();
    if let Some(mut stream) = stream {
        let _ = stream.read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

impl FileProbe for SystemProbes {
    fn metadata(&self, path: &Path) -> io::Result<FileInfo> {
        let metadata = std::fs::metadata(path)?;
        Ok(FileInfo {
            mode: metadata.permissions().mode() & 0o7777,
            uid: metadata.uid(),
        })
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    use super::*;

    fn serve_once(answer: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut chunk = [0u8; 1024];
            while !String::from_utf8_lossy(&received).contains("\r\n\r\n") {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                received.extend_from_slice(&chunk[..read]);
            }
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8(received).unwrap()
        });
        (format!("http://{address}/api/v1/health"), handle)
    }

    #[test]
    fn a_loopback_get_reads_status_and_body() {
        let body = r#"{"name":"calin","auth":"required"}"#;
        let answer: &'static str = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        );
        let (url, server) = serve_once(answer);
        let probes = SystemProbes::new().unwrap();
        let response = probes.get(&url).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, body.as_bytes());
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /api/v1/health HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(!request.to_ascii_lowercase().contains("authorization"));
    }

    #[test]
    fn redirects_are_not_followed() {
        let (url, server) = serve_once(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let response = SystemProbes::new().unwrap().get(&url).unwrap();
        assert_eq!(response.status, 302);
        server.join().unwrap();
    }

    #[test]
    fn a_closed_port_is_a_connect_error() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let error = SystemProbes::new()
            .unwrap()
            .get(&format!("http://127.0.0.1:{port}/api/v1/health"))
            .unwrap_err();
        assert!(matches!(error, HttpError::Connect(_)), "{error:?}");
    }

    #[test]
    fn tls_to_a_plain_http_server_is_a_tls_error() {
        // Answers whatever arrives first, a TLS ClientHello here, in plain HTTP.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut chunk = [0u8; 1024];
            let _ = stream.read(&mut chunk);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        });
        let error = SystemProbes::new().unwrap().get(&url).unwrap_err();
        assert!(matches!(error, HttpError::Tls(_)), "{error:?}");
        server.join().unwrap();
    }

    #[test]
    fn commands_report_exit_codes_output_and_absence() {
        let probes = SystemProbes::new().unwrap();
        let output = probes
            .run("sh", &["-c", "echo out; echo err >&2; exit 3"])
            .unwrap();
        assert_eq!(output.code, Some(3));
        assert_eq!(output.stdout, "out\n");
        assert_eq!(output.stderr, "err\n");
        assert_eq!(
            probes.run("calin-install-no-such-program", &[]),
            Err(CommandError::NotFound)
        );
    }

    #[test]
    fn files_report_mode_and_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("calin.env");
        std::fs::write(&path, "A=1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let probes = SystemProbes::new().unwrap();
        let info = probes.metadata(&path).unwrap();
        assert_eq!(info.mode, 0o600);
        assert_eq!(info.uid, std::fs::metadata(dir.path()).unwrap().uid());
        assert_eq!(probes.read(&path).unwrap(), b"A=1\n");
        let missing = probes.metadata(&dir.path().join("absent")).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }
}
