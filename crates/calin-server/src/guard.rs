//! The request guard for `/api` and `/auth`: the Host and Origin checks of
//! the remote-access spec, section 1. In local mode they are what keeps a
//! web page the user happens to visit from driving the server; in public
//! mode sign-in comes on top (`auth.rs`) and the Origin check is what stops
//! a cross-site request that carries the session cookie.

use std::net::{Ipv4Addr, Ipv6Addr};

use axum::extract::{Request, State};
use axum::http::header::{HOST, ORIGIN, UPGRADE};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::api::ApiFailure;

pub(crate) fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// The sign-in routes a browser navigates to, and everything else under
/// their prefix: never the web app's.
pub(crate) fn is_auth_path(path: &str) -> bool {
    path == "/auth" || path.starts_with("/auth/")
}

/// The origin users type in public mode, as the guard compares it.
#[derive(Clone, Copy)]
pub(crate) struct PublicSite<'a> {
    /// `https://calin.example.com`
    pub(crate) origin: &'a str,
    /// `calin.example.com`
    pub(crate) authority: &'a str,
}

pub(crate) async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if (is_api_path(path) || is_auth_path(path))
        && let Err(reason) = check(request.method(), request.headers(), state.public_site())
    {
        return ApiFailure::new(StatusCode::FORBIDDEN, reason).into_response();
    }
    next.run(request).await
}

fn check(
    method: &Method,
    headers: &HeaderMap,
    public: Option<PublicSite<'_>>,
) -> Result<(), &'static str> {
    // DNS rebinding: a hostile page can point its own name at 127.0.0.1, but
    // the browser still sends that name as Host. Two Host headers are two
    // answers to "which site is this for": refused.
    let mut hosts = headers.get_all(HOST).iter();
    let host = match (hosts.next(), hosts.next()) {
        (Some(host), None) => host.to_str().ok(),
        _ => None,
    };
    let to_loopback = host.is_some_and(is_loopback_authority);
    let to_public = match (host, public) {
        (Some(host), Some(public)) => host.eq_ignore_ascii_case(public.authority),
        _ => false,
    };
    let Some(host) = host.filter(|_| to_loopback || to_public) else {
        return Err(match public {
            Some(_) => "this server does not answer for that host",
            None => "this server only answers on a loopback address",
        });
    };

    // Cross-site requests: browsers attach Origin to anything that can change
    // state and to WebSocket handshakes. Clients that are not browsers send none.
    let reads_only = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if !reads_only || is_websocket_upgrade(headers) {
        for origin in headers.get_all(ORIGIN) {
            let allowed = origin.to_str().is_ok_and(|origin| match public {
                None => origin_allowed(origin, host),
                // A loopback origin only for a request to loopback itself: a
                // page on http://localhost:1234 must not drive the public site.
                Some(public) => {
                    origin == public.origin || (to_loopback && is_loopback_origin(origin))
                }
            });
            if !allowed {
                return Err("requests from this origin are not allowed");
            }
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
    web_origin_authority(origin).is_some_and(|authority| {
        authority.eq_ignore_ascii_case(host) || is_loopback_authority(authority)
    })
}

fn is_loopback_origin(origin: &str) -> bool {
    web_origin_authority(origin).is_some_and(is_loopback_authority)
}

/// The authority of an `http` or `https` origin.
fn web_origin_authority(origin: &str) -> Option<&str> {
    // No scheme includes `null`, which sandboxed frames and local files send.
    let (scheme, authority) = origin.split_once("://")?;
    matches!(scheme, "http" | "https").then_some(authority)
}

/// True for `localhost`, `127.0.0.0/8` and `[::1]`, with or without a port.
pub(crate) fn is_loopback_authority(authority: &str) -> bool {
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

    const SITE: PublicSite<'static> = PublicSite {
        origin: "https://calin.example.com",
        authority: "calin.example.com",
    };

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn in_public_mode_the_host_is_the_public_one_or_loopback() {
        for host in [
            "calin.example.com",
            "CALIN.example.COM",
            "localhost:4680",
            "127.0.0.1:4680",
            "[::1]:4680",
        ] {
            let headers = headers(&[("host", host)]);
            assert_eq!(check(&Method::GET, &headers, Some(SITE)), Ok(()), "{host}");
        }
        for host in [
            "evil.example",
            "calin.example.com.evil.example",
            "sub.calin.example.com",
            "calin.example.com:443",
            "calin.example.com:8443",
            "calin.example.com.",
            "192.168.1.10:4680",
            "",
        ] {
            let headers = headers(&[("host", host)]);
            assert!(check(&Method::GET, &headers, Some(SITE)).is_err(), "{host}");
        }
        assert!(check(&Method::GET, &HeaderMap::new(), Some(SITE)).is_err());
    }

    #[test]
    fn two_host_or_origin_headers_are_refused_unless_all_would_pass() {
        for public in [None, Some(SITE)] {
            let two_hosts = headers(&[("host", "localhost"), ("host", "evil.example")]);
            assert!(check(&Method::GET, &two_hosts, public).is_err());
            let same_twice = headers(&[("host", "localhost"), ("host", "localhost")]);
            assert!(check(&Method::GET, &same_twice, public).is_err());
            let mixed = headers(&[
                ("host", "localhost"),
                ("origin", "http://localhost"),
                ("origin", "https://evil.example"),
            ]);
            assert!(check(&Method::POST, &mixed, public).is_err());
        }
    }

    #[test]
    fn in_public_mode_only_the_public_origin_drives_the_public_host() {
        let post = |host: &'static str, origin: &'static str| {
            check(
                &Method::POST,
                &headers(&[("host", host), ("origin", origin)]),
                Some(SITE),
            )
        };
        // (host, origin, allowed)
        for (host, origin, allowed) in [
            ("calin.example.com", "https://calin.example.com", true),
            ("localhost:4680", "https://calin.example.com", true),
            ("localhost:4680", "http://localhost:5173", true),
            ("127.0.0.1:4680", "http://127.0.0.1:4680", true),
            // A page on the user's own machine cannot drive the public site.
            ("calin.example.com", "http://localhost:5173", false),
            ("calin.example.com", "http://127.0.0.1:4680", false),
            ("calin.example.com", "http://[::1]:4680", false),
            // Same host, other scheme or port: another origin.
            ("calin.example.com", "http://calin.example.com", false),
            ("calin.example.com", "https://calin.example.com:8443", false),
            ("calin.example.com", "https://calin.example.com/", false),
            ("calin.example.com", "https://CALIN.example.com", false),
            ("calin.example.com", "https://sub.calin.example.com", false),
            ("calin.example.com", "https://evil.example", false),
            ("calin.example.com", "null", false),
            ("calin.example.com", "", false),
            ("localhost:4680", "https://evil.example", false),
            ("localhost:4680", "null", false),
        ] {
            assert_eq!(post(host, origin).is_ok(), allowed, "{host} from {origin}");
        }
        // Reads pass whatever the origin; a WebSocket handshake does not.
        let read = headers(&[
            ("host", "calin.example.com"),
            ("origin", "https://evil.example"),
        ]);
        assert_eq!(check(&Method::GET, &read, Some(SITE)), Ok(()));
        for (origin, allowed) in [
            ("https://calin.example.com", true),
            ("https://evil.example", false),
            ("http://localhost:5173", false),
        ] {
            let handshake = headers(&[
                ("host", "calin.example.com"),
                ("origin", origin),
                ("upgrade", "websocket"),
            ]);
            assert_eq!(
                check(&Method::GET, &handshake, Some(SITE)).is_ok(),
                allowed,
                "{origin}"
            );
        }
        // No Origin at all: a client that is not a browser.
        let bare = headers(&[("host", "calin.example.com")]);
        assert_eq!(check(&Method::POST, &bare, Some(SITE)), Ok(()));
    }
}
