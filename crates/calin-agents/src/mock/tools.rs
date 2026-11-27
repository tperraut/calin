//! The mock's client for Calin's tool endpoint: one JSON-RPC `tools/call`
//! per HTTP POST, plain HTTP, loopback only. The endpoint's URL holds the
//! agent's token, so neither it nor its path ever goes into a message.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{ACCEPT, CONTENT_TYPE, HOST};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};

/// Long enough for a dispatch that prepares worktrees; a call that takes
/// longer fails instead of holding the turn forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the tool endpoint is: always this machine.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Endpoint {
    /// As the URL wrote it, for the Host header the server checks.
    authority: String,
    port: u16,
    path: String,
}

/// The path holds the token: it never shows.
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("authority", &self.authority)
            .field("path", &"<redacted>")
            .finish()
    }
}

impl Endpoint {
    /// Accepts `http://127.0.0.1:<port>/...` and `http://localhost:<port>/...`
    /// only. The mock talks to nothing but the orchestrator that started it.
    pub(super) fn parse(url: &str) -> Result<Self, String> {
        let refused = || {
            "the tool endpoint is not a plain http:// URL on 127.0.0.1 or localhost with a \
             port, and the mock calls nothing else"
                .to_owned()
        };
        let rest = url.strip_prefix("http://").ok_or_else(refused)?;
        let slash = rest.find('/').ok_or_else(refused)?;
        let (authority, path) = rest.split_at(slash);
        let (host, port) = authority.rsplit_once(':').ok_or_else(refused)?;
        let loopback = host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost");
        // Digits only: `u16::from_str` would also take a leading `+`.
        let port = Some(port)
            .filter(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|port| port.parse::<u16>().ok())
            .filter(|port| *port != 0);
        match (loopback, port) {
            (true, Some(port)) => Ok(Self {
                authority: authority.to_owned(),
                port,
                path: path.to_owned(),
            }),
            _ => Err(refused()),
        }
    }
}

/// Calls one tool. `Ok` with the tool's text, `Err` with what went wrong:
/// the tool's own error text when it answered `isError`, else why the call
/// did not go through.
pub(super) async fn call_tool(
    endpoint: Endpoint,
    rpc_id: u64,
    name: &str,
    arguments: Value,
) -> Result<String, String> {
    let message = json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    });
    let posted = tokio::time::timeout(CALL_TIMEOUT, post(&endpoint, message.to_string()));
    let (status, body) = posted
        .await
        .map_err(|_| "the tool endpoint did not answer in time".to_owned())??;
    let answer: Option<Value> = serde_json::from_slice(&body).ok();
    if status != StatusCode::OK {
        let said = answer
            .as_ref()
            .and_then(|answer| answer.get("error"))
            .and_then(|error| error.get("message").or(Some(error)))
            .and_then(Value::as_str)
            .map(|said| format!(": {said}"))
            .unwrap_or_default();
        return Err(format!("the tool endpoint answered HTTP {status}{said}"));
    }
    let answer = answer.ok_or_else(|| "the tool endpoint's answer is not JSON".to_owned())?;
    if let Some(error) = answer.get("error") {
        let message = error.get("message").and_then(Value::as_str);
        return Err(message.unwrap_or("the tool call failed").to_owned());
    }
    let result = answer.get("result").unwrap_or(&Value::Null);
    let text: Vec<&str> = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    let text = text.join("\n");
    match result.get("isError").and_then(Value::as_bool) {
        Some(true) => Err(text),
        _ => Ok(text),
    }
}

async fn post(endpoint: &Endpoint, body: String) -> Result<(StatusCode, Bytes), String> {
    // `localhost` is reached on IPv4 loopback too: no name lookup, ever.
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let unreachable =
        |err: &dyn std::fmt::Display| format!("the tool endpoint is unreachable: {err}");
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|err| unreachable(&err))?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|err| unreachable(&err))?;
    // Drives the connection; it ends when `sender` is dropped.
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = Request::post(endpoint.path.as_str())
        .header(HOST, endpoint.authority.as_str())
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream")
        .body(Full::new(Bytes::from(body)))
        .map_err(|err| format!("the tool call could not be written: {err}"))?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|err| format!("the tool call failed in transit: {err}"))?;
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|err| format!("the tool endpoint's answer was cut short: {err}"))?
        .to_bytes();
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_loopback_http_url_with_a_port_is_accepted() {
        for url in [
            "http://127.0.0.1:4680/api/v1/mcp/token",
            "http://localhost:1/x",
            "http://LOCALHOST:65535/",
        ] {
            assert!(Endpoint::parse(url).is_ok(), "{url}");
        }
        for url in [
            "https://127.0.0.1:4680/api/v1/mcp/token",
            "http://127.0.0.1/api",
            "http://127.0.0.1:4680",
            "http://127.0.0.1:0/x",
            "http://127.0.0.1:+80/x",
            "http://127.0.0.1:99999/x",
            "http://127.0.0.2:80/x",
            "http://10.0.0.1:80/x",
            "http://[::1]:80/x",
            "http://example.com:80/x",
            "http://localhost.example.com:80/x",
            "http://user@127.0.0.1:80/x",
            "http://127.0.0.1:80@example.com:80/x",
            "ftp://127.0.0.1:21/x",
            "127.0.0.1:80/x",
            "",
        ] {
            let refused = Endpoint::parse(url).unwrap_err();
            assert!(
                refused.contains("127.0.0.1 or localhost"),
                "{url}: {refused}"
            );
        }
        let endpoint = Endpoint::parse("http://127.0.0.1:4680/api/v1/mcp/secret").unwrap();
        assert!(!format!("{endpoint:?}").contains("secret"));
    }
}
