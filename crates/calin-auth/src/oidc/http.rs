//! The only code that talks to the provider: `https` only, bounded in time
//! and size, and never following a redirect.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::header::{self, HeaderValue};
use hyper::{Method, Request};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use url::Url;

use crate::config::{LoopbackHttp, check_scheme};
use crate::secret::log_safe;

/// Time allowed to connect to the provider.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Time allowed for one whole exchange, body included.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest response read. Discovery documents and key sets are a few
/// kilobytes; a megabyte is far beyond any real one.
pub(crate) const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

const USER_AGENT: &str = concat!("calin-auth/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HttpError {
    #[error("the URL {0}")]
    UrlRefused(&'static str),
    #[error("no answer within {0:?}")]
    Timeout(Duration),
    #[error("{0}")]
    Transport(String),
    #[error("the response is larger than {0} bytes")]
    TooLarge(usize),
}

pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
    /// Never followed; kept so a redirect can be reported, and for tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) location: Option<String>,
}

pub(crate) struct ProviderHttp {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    loopback_http: LoopbackHttp,
    timeout: Duration,
    max_response_bytes: usize,
}

impl ProviderHttp {
    /// TLS is rustls with the `ring` provider and the Mozilla roots that
    /// `webpki-roots` embeds: no system library is involved.
    pub(crate) fn new(loopback_http: LoopbackHttp, timeout: Duration) -> Result<Self, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // The provider is named here rather than taken from the process
        // default, which another crate could install differently.
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();

        let mut tcp = HttpConnector::new();
        tcp.enforce_http(false);
        tcp.set_connect_timeout(Some(CONNECT_TIMEOUT.min(timeout)));
        let builder = hyper_rustls::HttpsConnectorBuilder::new().with_tls_config(tls);
        let connector = match loopback_http {
            LoopbackHttp::Refuse => builder.https_only(),
            LoopbackHttp::AllowForDevelopment => builder.https_or_http(),
        }
        .enable_http1()
        .wrap_connector(tcp);

        // No idle connections are kept: sign-ins are rare, and a pooled
        // connection the provider has closed would fail the next one.
        let client = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(0)
            .build(connector);
        Ok(Self {
            client,
            loopback_http,
            timeout,
            max_response_bytes: MAX_RESPONSE_BYTES,
        })
    }

    pub(crate) async fn get(&self, url: &Url) -> Result<Response, HttpError> {
        let request = self.request(Method::GET, url)?.body(Full::default());
        self.send(request.map_err(transport)?).await
    }

    /// Posts a form. `basic` is the client id and secret, sent as HTTP Basic
    /// credentials encoded the way RFC 6749 section 2.3.1 says.
    pub(crate) async fn post_form(
        &self,
        url: &Url,
        form: &[(&str, &str)],
        basic: Option<(&str, &str)>,
    ) -> Result<Response, HttpError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let mut request = self
            .request(Method::POST, url)?
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some((id, secret)) = basic {
            let encode = |text: &str| -> String {
                url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
            };
            let credentials = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                encode(id),
                encode(secret)
            ));
            let mut value =
                HeaderValue::try_from(format!("Basic {credentials}")).map_err(transport)?;
            // Keeps the secret out of any Debug output of the request.
            value.set_sensitive(true);
            request = request.header(header::AUTHORIZATION, value);
        }
        let request = request.body(Full::from(body)).map_err(transport)?;
        self.send(request).await
    }

    fn request(
        &self,
        method: Method,
        url: &Url,
    ) -> Result<hyper::http::request::Builder, HttpError> {
        check_scheme(url, self.loopback_http).map_err(HttpError::UrlRefused)?;
        if url.fragment().is_some() {
            return Err(HttpError::UrlRefused("must not have a fragment"));
        }
        Ok(Request::builder()
            .method(method)
            .uri(url.as_str())
            .header(header::ACCEPT, "application/json")
            .header(header::USER_AGENT, USER_AGENT))
    }

    async fn send(&self, request: Request<Full<Bytes>>) -> Result<Response, HttpError> {
        let exchange = async {
            let response = self.client.request(request).await.map_err(transport)?;
            let status = response.status().as_u16();
            let header_text = |name| {
                response
                    .headers()
                    .get(name)
                    .and_then(|value: &HeaderValue| value.to_str().ok())
            };
            let location = header_text(header::LOCATION).map(str::to_owned);
            let declared = header_text(header::CONTENT_LENGTH).and_then(|v| v.parse::<u64>().ok());
            if declared.is_some_and(|length| length > self.max_response_bytes as u64) {
                return Err(HttpError::TooLarge(self.max_response_bytes));
            }
            // The declared length is only a claim: count what arrives.
            let mut body = response.into_body();
            let mut collected = Vec::new();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(transport)?;
                if let Some(data) = frame.data_ref() {
                    if collected.len() + data.len() > self.max_response_bytes {
                        return Err(HttpError::TooLarge(self.max_response_bytes));
                    }
                    collected.extend_from_slice(data);
                }
            }
            Ok(Response {
                status,
                body: collected,
                location,
            })
        };
        match tokio::time::timeout(self.timeout, exchange).await {
            Ok(result) => result,
            Err(_) => Err(HttpError::Timeout(self.timeout)),
        }
    }
}

/// An error and its causes on one line. These come from the HTTP and TLS
/// stack and describe the connection, never the request's content.
fn transport(error: impl std::error::Error) -> HttpError {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    HttpError::Transport(log_safe(&text))
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use super::*;

    fn client(loopback_http: LoopbackHttp) -> ProviderHttp {
        ProviderHttp::new(loopback_http, Duration::from_secs(5)).unwrap()
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    /// Accepts one request, answers `200 {}`, and returns what was received.
    async fn capture_one_request(listener: TcpListener) -> String {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        loop {
            let mut chunk = [0u8; 4096];
            let read = stream.read(&mut chunk).await.unwrap();
            received.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&received);
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })
                .unwrap_or(0);
            if body.len() >= length {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .await
            .unwrap();
        String::from_utf8(received).unwrap()
    }

    #[tokio::test]
    async fn plain_http_is_refused_before_anything_is_sent() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let target = url(&format!("http://{}/token", listener.local_addr().unwrap()));
        let refused = Err(HttpError::UrlRefused("must be an https URL"));

        let strict = client(LoopbackHttp::Refuse);
        assert_eq!(strict.get(&target).await.map(|r| r.status), refused);
        assert_eq!(
            strict
                .post_form(&target, &[("code", "c")], Some(("id", "secret")))
                .await
                .map(|r| r.status),
            refused
        );
        // Nobody connected.
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test]
    async fn the_development_flag_opens_loopback_http_only() {
        let lenient = client(LoopbackHttp::AllowForDevelopment);
        // Refused from the URL alone: none of these is ever contacted.
        for target in [
            "http://example.com/token",
            "http://example.com:8080/token",
            "http://192.0.2.1:8080/token",
            "http://localhost/token",
            "http://127.0.0.1/token",
            "http://[::1]:8080/token",
            "ftp://127.0.0.1:8080/token",
        ] {
            assert_eq!(
                lenient.get(&url(target)).await.map(|r| r.status),
                Err(HttpError::UrlRefused("must be an https URL")),
                "{target}"
            );
        }
        assert_eq!(
            lenient
                .get(&url("https://example.com/keys#fragment"))
                .await
                .map(|r| r.status),
            Err(HttpError::UrlRefused("must not have a fragment"))
        );
    }

    #[tokio::test]
    async fn a_form_post_carries_basic_credentials_encoded_per_rfc_6749() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = url(&format!(
            "http://{}/token?tenant=a",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(capture_one_request(listener));

        let lenient = client(LoopbackHttp::AllowForDevelopment);
        let form = [
            ("grant_type", "authorization_code"),
            ("code", "a/b+c d&e=f"),
        ];
        let response = lenient
            .post_form(&target, &form, Some(("my id:1", "p@ss/w~rd+")))
            .await
            .unwrap();
        assert_eq!(
            (response.status, response.body.as_slice()),
            (200, b"{}".as_slice())
        );

        let request = server.await.unwrap();
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(
            head.starts_with("POST /token?tenant=a HTTP/1.1\r\n"),
            "{head}"
        );
        let head = head.to_ascii_lowercase();
        assert!(
            head.contains("\r\ncontent-type: application/x-www-form-urlencoded\r\n"),
            "{head}"
        );
        assert!(head.contains("\r\naccept: application/json"), "{head}");
        assert!(head.contains("\r\nuser-agent: calin-auth/"), "{head}");
        assert_eq!(
            body,
            "grant_type=authorization_code&code=a%2Fb%2Bc+d%26e%3Df"
        );
        // base64("my+id%3A1:p%40ss%2Fw%7Erd%2B"): id and secret are
        // form-encoded first, so a colon in the id cannot shift the split.
        let expected =
            base64::engine::general_purpose::STANDARD.encode("my+id%3A1:p%40ss%2Fw%7Erd%2B");
        assert!(
            request.contains(&format!("uthorization: Basic {expected}\r\n")),
            "{request}"
        );
    }

    #[tokio::test]
    async fn a_get_sends_no_credentials_and_no_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = url(&format!("http://{}/keys", listener.local_addr().unwrap()));
        let server = tokio::spawn(capture_one_request(listener));
        let response = client(LoopbackHttp::AllowForDevelopment)
            .get(&target)
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.location, None);

        let request = server.await.unwrap();
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("GET /keys HTTP/1.1\r\n"), "{head}");
        assert!(
            !head.to_ascii_lowercase().contains("authorization"),
            "{head}"
        );
        assert!(!head.to_ascii_lowercase().contains("cookie"), "{head}");
        assert_eq!(body, "");
    }
}
