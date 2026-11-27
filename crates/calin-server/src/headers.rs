//! The headers every response carries, in both modes: what a browser may
//! load and where from, and what may be cached.

use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, HOST, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;
use crate::guard::{PublicSite, is_api_path, is_auth_path, is_loopback_authority};

/// What the web app really loads, and nothing else. Checked against the
/// built app (`apps/web/dist`): it references no other origin, has no
/// inline script, and uses neither `eval` nor workers from `blob:`.
///
/// - `script-src 'self'`: the bundles under `/assets`.
/// - `style-src 'self' 'unsafe-inline'`: the stylesheets, plus the `<style>`
///   elements and `style` attributes the terminal (xterm) and the editor
///   (CodeMirror) create at run time.
/// - `img-src` and `font-src` with `data:`: the bundler inlines small files
///   as `data:` URLs, and the editor's own CSS holds one.
/// - `connect-src 'self'` and the page's own WebSocket origin (added per
///   request): the REST API and the two sockets. Not every browser reads
///   `'self'` as covering `ws:`.
/// - `manifest-src` and `worker-src 'self'`: the installable app of the
///   remote-access spec, section 6.
/// - `form-action 'self'`: no form posts to the provider.
const CSP_BEFORE_SOCKET: &str = "default-src 'none'; script-src 'self'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; \
    connect-src 'self'";
const CSP_AFTER_SOCKET: &str = "; manifest-src 'self'; worker-src 'self'; \
    frame-ancestors 'none'; base-uri 'self'; form-action 'self'";
/// The policy when the request's Host is not one the server knows.
const CSP_WITHOUT_SOCKET: &str = "default-src 'none'; script-src 'self'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; \
    connect-src 'self'; manifest-src 'self'; worker-src 'self'; \
    frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

/// One year, as the remote-access spec asks. Only the host itself: its
/// sibling subdomains are not Calin's to decide for.
const HSTS: &str = "max-age=31536000";

/// The WebSocket origin of the page a request for `host` serves, or nothing
/// when `host` is not a name this server answers to. Only such a name is
/// ever written into the policy: the Host header is the client's to choose.
fn socket_source(host: Option<&str>, public: Option<PublicSite<'_>>) -> Option<String> {
    let host = host?;
    // The policy syntax has no IPv6 literal; `'self'` has to do for `[::1]`.
    if host.starts_with('[') {
        return None;
    }
    if let Some(public) = public
        && host.eq_ignore_ascii_case(public.authority)
    {
        let scheme = if public.origin.starts_with("https://") {
            "wss"
        } else {
            "ws"
        };
        return Some(format!("{scheme}://{}", public.authority));
    }
    is_loopback_authority(host).then(|| format!("ws://{}", host.to_ascii_lowercase()))
}

fn content_security_policy(host: Option<&str>, public: Option<PublicSite<'_>>) -> HeaderValue {
    socket_source(host, public)
        .and_then(|source| {
            HeaderValue::from_str(&format!("{CSP_BEFORE_SOCKET} {source}{CSP_AFTER_SOCKET}")).ok()
        })
        .unwrap_or(HeaderValue::from_static(CSP_WITHOUT_SOCKET))
}

pub(crate) async fn security_headers(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let public = state.public_site();
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok());
    let policy = content_security_policy(host, public);
    let path = request.uri().path();
    // Answers of the API and of sign-in are about one caller at one moment.
    let private = is_api_path(path) || is_auth_path(path);

    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(CONTENT_SECURITY_POLICY, policy);
    if public.is_some() {
        headers.insert(STRICT_TRANSPORT_SECURITY, HeaderValue::from_static(HSTS));
    }
    if private {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE: PublicSite<'static> = PublicSite {
        origin: "https://calin.example.com",
        authority: "calin.example.com",
    };

    #[test]
    fn the_two_spellings_of_the_policy_differ_only_by_the_socket_source() {
        assert_eq!(
            format!("{CSP_BEFORE_SOCKET}{CSP_AFTER_SOCKET}"),
            CSP_WITHOUT_SOCKET
        );
    }

    #[test]
    fn the_socket_source_is_the_pages_own_origin_and_never_what_a_client_made_up() {
        // (host, public mode, expected)
        for (host, public, expected) in [
            ("127.0.0.1:4680", None, Some("ws://127.0.0.1:4680")),
            ("LOCALHOST:4680", None, Some("ws://localhost:4680")),
            ("localhost", None, Some("ws://localhost")),
            ("[::1]:4680", None, None),
            ("calin.example.com", None, None),
            ("evil.example", None, None),
            ("x; script-src *", None, None),
            ("localhost:1 https://evil.example", None, None),
            (
                "calin.example.com",
                Some(SITE),
                Some("wss://calin.example.com"),
            ),
            (
                "Calin.Example.com",
                Some(SITE),
                Some("wss://calin.example.com"),
            ),
            ("127.0.0.1:4680", Some(SITE), Some("ws://127.0.0.1:4680")),
            ("evil.example", Some(SITE), None),
            ("calin.example.com; script-src *", Some(SITE), None),
        ] {
            assert_eq!(
                socket_source(Some(host), public).as_deref(),
                expected,
                "{host}"
            );
        }
        assert_eq!(socket_source(None, Some(SITE)), None);
        // The development switch: a public origin over plain http.
        let dev = PublicSite {
            origin: "http://localhost:4680",
            authority: "localhost:4680",
        };
        assert_eq!(
            socket_source(Some("localhost:4680"), Some(dev)).as_deref(),
            Some("ws://localhost:4680")
        );
    }

    #[test]
    fn the_policy_names_every_directive_once() {
        let policy = content_security_policy(Some("calin.example.com"), Some(SITE));
        assert_eq!(
            policy.to_str().unwrap(),
            "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; font-src 'self' data:; \
             connect-src 'self' wss://calin.example.com; manifest-src 'self'; \
             worker-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'"
        );
        let unknown = content_security_policy(Some("evil.example"), Some(SITE));
        assert_eq!(unknown.to_str().unwrap(), CSP_WITHOUT_SOCKET);
    }
}
