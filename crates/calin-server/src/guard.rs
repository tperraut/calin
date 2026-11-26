//! The request guard for `/api`. Until sign-in exists (design spec, section
//! 8) the server is only reachable on loopback, and these two checks are what
//! keeps a web page the user happens to visit from driving it.

use std::net::{Ipv4Addr, Ipv6Addr};

use axum::extract::Request;
use axum::http::header::{HOST, ORIGIN, UPGRADE};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::ApiFailure;

pub(crate) fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

pub(crate) async fn guard(request: Request, next: Next) -> Response {
    if is_api_path(request.uri().path())
        && let Err(reason) = check(request.method(), request.headers())
    {
        return ApiFailure::new(StatusCode::FORBIDDEN, reason).into_response();
    }
    next.run(request).await
}

fn check(method: &Method, headers: &HeaderMap) -> Result<(), &'static str> {
    // DNS rebinding: a hostile page can point its own name at 127.0.0.1, but
    // the browser still sends that name as Host.
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|host| is_loopback_authority(host))
        .ok_or("this server only answers on a loopback address")?;

    // Cross-site requests: browsers attach Origin to anything that can change
    // state and to WebSocket handshakes. Clients that are not browsers send none.
    let reads_only = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if (!reads_only || is_websocket_upgrade(headers))
        && let Some(origin) = headers.get(ORIGIN)
    {
        let allowed = origin
            .to_str()
            .is_ok_and(|origin| origin_allowed(origin, host));
        if !allowed {
            return Err("requests from this origin are not allowed");
        }
    }
    Ok(())
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("websocket"))
}

/// Same origin as the request's Host, or any loopback origin: the Vite dev
/// server runs on another loopback port.
fn origin_allowed(origin: &str, host: &str) -> bool {
    let Some((scheme, authority)) = origin.split_once("://") else {
        // Includes `null`, which sandboxed frames and local files send.
        return false;
    };
    matches!(scheme, "http" | "https")
        && (authority.eq_ignore_ascii_case(host) || is_loopback_authority(authority))
}

/// True for `localhost`, `127.0.0.0/8` and `[::1]`, with or without a port.
fn is_loopback_authority(authority: &str) -> bool {
    let Some(host) = host_of(authority) else {
        return false;
    };
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return v6.parse::<Ipv6Addr>().is_ok_and(|ip| ip.is_loopback());
    }
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
}

/// The host of `host[:port]`, brackets of an IPv6 literal included.
/// `None` when the port is not a port.
fn host_of(authority: &str) -> Option<&str> {
    let (host, port) = if authority.starts_with('[') {
        authority.split_at(authority.find(']')? + 1)
    } else {
        authority.split_at(authority.rfind(':').unwrap_or(authority.len()))
    };
    let port_ok = match port.strip_prefix(':') {
        Some(digits) => {
            !digits.is_empty() && digits.len() <= 5 && digits.bytes().all(|b| b.is_ascii_digit())
        }
        None => port.is_empty(),
    };
    (port_ok && !host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_are_recognized() {
        for host in [
            "localhost",
            "LOCALHOST:4680",
            "127.0.0.1",
            "127.0.0.1:4680",
            "127.8.9.10:1",
            "[::1]",
            "[::1]:4680",
        ] {
            assert!(is_loopback_authority(host), "{host}");
        }
        for host in [
            "",
            "example.com",
            "example.com:4680",
            "localhost.example.com",
            "127.0.0.1.example.com",
            "evil@127.0.0.1",
            "192.168.1.5:4680",
            "0.0.0.0",
            "[::]",
            "[2001:db8::1]:4680",
            "::1",
            "localhost:",
            "localhost:port",
            "localhost:123456",
            "[::1]4680",
            "127.1",
        ] {
            assert!(!is_loopback_authority(host), "{host}");
        }
    }

    #[test]
    fn origins_must_match_the_host_or_be_loopback() {
        let host = "127.0.0.1:4680";
        for origin in [
            "http://127.0.0.1:4680",
            "http://localhost:5173",
            "https://localhost",
            "http://[::1]:5173",
        ] {
            assert!(origin_allowed(origin, host), "{origin}");
        }
        for origin in [
            "null",
            "",
            "https://example.com",
            "http://localhost.example.com",
            "http://127.0.0.1:4680.example.com",
            "file://localhost",
            "http://localhost:5173/path",
        ] {
            assert!(!origin_allowed(origin, host), "{origin}");
        }
        // Same origin as a host that is not loopback, for when sign-in lifts that limit.
        assert!(origin_allowed("https://calin.example", "calin.example"));
        assert!(!origin_allowed("https://other.example", "calin.example"));
    }
}
