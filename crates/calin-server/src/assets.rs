//! The web app, embedded at build time, served with a single-page-app fallback.

use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::api::ApiFailure;

/// `apps/web/dist` as it was when this crate was compiled, sorted by path.
static EMBEDDED: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

const INDEX: &str = "index.html";

const NOT_BUILT: &str = "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>Calin</title>
</head>
<body>
<h1>Calin is running</h1>
<p>The web app has not been built, so there is nothing to show here yet.
Run <code>make build</code> and start the server again.</p>
<p>The API is available under <code>/api/v1</code>.</p>
</body>
</html>
";

/// The files of the web app, by path relative to its root.
#[derive(Clone, Copy, Debug)]
pub struct Assets {
    /// Sorted by path.
    files: &'static [(&'static str, &'static [u8])],
}

impl Assets {
    /// The web app built into this binary. Empty when `apps/web/dist` did
    /// not exist at compile time.
    pub const fn embedded() -> Self {
        Self { files: EMBEDDED }
    }

    /// A given set of files, sorted by path. For tests.
    pub const fn new(files: &'static [(&'static str, &'static [u8])]) -> Self {
        Self { files }
    }

    pub const fn empty() -> Self {
        Self { files: &[] }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    fn get(&self, path: &str) -> Option<&'static [u8]> {
        self.files
            .binary_search_by(|(name, _)| (*name).cmp(path))
            .ok()
            .map(|index| self.files[index].1)
    }
}

/// The router's fallback: everything that is not an API route.
pub(crate) async fn serve(
    State(state): State<AppState>,
    method: Method,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let path = uri.path();
    if crate::guard::is_api_path(path) {
        // An API client must never get the web app's HTML for a mistyped route.
        return ApiFailure::new(StatusCode::NOT_FOUND, format!("no such route: {path}"))
            .into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return (StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, "GET, HEAD")]).into_response();
    }

    let assets = state.assets;
    let name = path.trim_start_matches('/');
    let (name, bytes) = match assets.get(name) {
        Some(bytes) => (name, bytes),
        // Not a file: a route of the single-page app, which the client resolves.
        None => match assets.get(INDEX) {
            Some(bytes) => (INDEX, bytes),
            None => {
                return (
                    [
                        (CONTENT_TYPE, "text/html; charset=utf-8"),
                        (CACHE_CONTROL, "no-cache"),
                    ],
                    NOT_BUILT,
                )
                    .into_response();
            }
        },
    };
    // Vite puts a content hash in the names under `assets/`; everything else
    // keeps its name across builds and must be revalidated.
    let cache = if name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [(CONTENT_TYPE, content_type(name)), (CACHE_CONTROL, cache)],
        Body::from(bytes),
    )
        .into_response()
}

fn content_type(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_table_is_sorted_so_lookups_work() {
        assert!(EMBEDDED.windows(2).all(|pair| pair[0].0 < pair[1].0));
        for (name, bytes) in EMBEDDED {
            assert_eq!(Assets::embedded().get(name), Some(*bytes));
        }
    }

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("assets/index-B90H.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type("favicon.svg"), "image/svg+xml");
        assert_eq!(content_type("LICENSE"), "application/octet-stream");
    }
}
