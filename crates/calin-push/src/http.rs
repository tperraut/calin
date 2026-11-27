//! The only code that talks to push services: bounded in time and size,
//! never following a redirect, and refusing a host name that resolves to a
//! private address.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::header::{self, HeaderName, HeaderValue};
use hyper::{Method, Request};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::connect::dns::{GaiResolver, Name};
use hyper_util::rt::TokioExecutor;
use tokio::time::Instant;
use url::Url;

use crate::endpoint::{EndpointPolicy, is_public_address};
use crate::secret::log_safe;

/// Time allowed to connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Most of a response body read. Push services answer with a few bytes, and
/// the body is only ever logged.
pub(crate) const MAX_RESPONSE_BYTES: usize = 4096;
/// Characters of a response body kept for a log line.
const EXCERPT_CHARS: usize = 200;

const USER_AGENT: &str = concat!("calin-push/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HttpError {
    Timeout(Duration),
    /// The cause, on one line, from the HTTP and TLS stack: it describes the
    /// connection, never the request.
    Transport(String),
}

pub(crate) struct Response {
    pub(crate) status: u16,
    /// The start of the body, safe to log.
    pub(crate) excerpt: String,
}

pub(crate) struct PushHttp {
    client: Client<HttpsConnector<HttpConnector<PublicResolver>>, Full<Bytes>>,
    timeout: Duration,
}

impl PushHttp {
    /// TLS is rustls with the `ring` provider and the Mozilla roots that
    /// `webpki-roots` embeds: no system library is involved.
    pub(crate) fn new(policy: EndpointPolicy, timeout: Duration) -> Result<Self, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // Named here rather than taken from the process default, which
        // another crate could install differently.
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();

        let mut tcp = HttpConnector::new_with_resolver(PublicResolver(GaiResolver::new()));
        tcp.enforce_http(false);
        tcp.set_connect_timeout(Some(CONNECT_TIMEOUT.min(timeout)));
        let builder = hyper_rustls::HttpsConnectorBuilder::new().with_tls_config(tls);
        let connector = match policy {
            EndpointPolicy::PublicHttpsOnly => builder.https_only(),
            EndpointPolicy::AllowLoopbackHttpForDevelopment => builder.https_or_http(),
        }
        .enable_http1()
        .wrap_connector(tcp);

        // A few idle connections per push service: a burst of notifications
        // goes to the same two or three hosts.
        let client = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(4)
            .pool_idle_timeout(Duration::from_secs(60))
            .build(connector);
        Ok(Self { client, timeout })
    }

    /// Posts `body` to `url`. A redirect is returned as its status, never
    /// followed: the client has no redirect support at all.
    pub(crate) async fn post(
        &self,
        url: &Url,
        headers: Vec<(HeaderName, HeaderValue)>,
        body: Vec<u8>,
    ) -> Result<Response, HttpError> {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(url.as_str())
            .header(header::USER_AGENT, USER_AGENT)
            .header(header::CONTENT_TYPE, "application/octet-stream");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let request = request.body(Full::from(body)).map_err(transport)?;

        let deadline = Instant::now() + self.timeout;
        let response = tokio::time::timeout_at(deadline, self.client.request(request))
            .await
            .map_err(|_| HttpError::Timeout(self.timeout))?
            .map_err(transport)?;
        let status = response.status().as_u16();

        // The status is the answer. The body is read only for a log line,
        // within what is left of the deadline and up to a few kilobytes; an
        // error or a stall there does not change the outcome.
        let mut body = response.into_body();
        let mut collected = Vec::new();
        let read = async {
            while let Some(Ok(frame)) = body.frame().await {
                if let Some(data) = frame.data_ref() {
                    let room = MAX_RESPONSE_BYTES - collected.len();
                    collected.extend_from_slice(&data[..data.len().min(room)]);
                    if collected.len() >= MAX_RESPONSE_BYTES {
                        break;
                    }
                }
            }
        };
        let _ = tokio::time::timeout_at(deadline, read).await;
        let excerpt = log_safe(String::from_utf8_lossy(&collected).trim(), EXCERPT_CHARS);
        Ok(Response { status, excerpt })
    }
}

/// An error and its causes on one line.
fn transport(error: impl std::error::Error) -> HttpError {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    HttpError::Transport(log_safe(&text, 200))
}

/// The system resolver, keeping only public addresses. A push endpoint's
/// host passed the name rules already; this catches a public-looking name
/// that points inside the network. An IP literal never reaches a resolver,
/// and only the development policy lets one through.
#[derive(Clone)]
pub(crate) struct PublicResolver(GaiResolver);

type ResolveFuture =
    Pin<Box<dyn Future<Output = Result<std::vec::IntoIter<SocketAddr>, io::Error>> + Send>>;

impl tower_service::Service<Name> for PublicResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = io::Error;
    type Future = ResolveFuture;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.0.poll_ready(context)
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let resolving = self.0.call(name);
        Box::pin(async move {
            let addresses: Vec<SocketAddr> = resolving
                .await?
                .filter(|address| is_public_address(address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(io::Error::other("the host resolves to no public address"));
            }
            Ok(addresses.into_iter())
        })
    }
}

#[cfg(test)]
mod tests {
    use tower_service::Service as _;

    use super::*;

    #[tokio::test]
    async fn names_that_resolve_to_loopback_are_refused() {
        // `localhost` resolves from the hosts file: no network involved.
        let mut resolver = PublicResolver(GaiResolver::new());
        std::future::poll_fn(|cx| resolver.poll_ready(cx))
            .await
            .unwrap();
        let error = resolver
            .call("localhost".parse().unwrap())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no public address"), "{error}");
    }

    #[tokio::test]
    async fn the_client_builds_with_both_policies() {
        PushHttp::new(EndpointPolicy::PublicHttpsOnly, Duration::from_secs(1)).unwrap();
        PushHttp::new(
            EndpointPolicy::AllowLoopbackHttpForDevelopment,
            Duration::from_secs(1),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn the_strict_client_never_speaks_plain_http() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/p", listener.local_addr().unwrap())).unwrap();
        let client =
            PushHttp::new(EndpointPolicy::PublicHttpsOnly, Duration::from_secs(2)).unwrap();
        let result = client.post(&url, Vec::new(), b"x".to_vec()).await;
        assert!(matches!(result, Err(HttpError::Transport(_))));
        let accepted = tokio::time::timeout(Duration::from_millis(100), listener.accept()).await;
        assert!(accepted.is_err(), "nobody connected");
    }
}
