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

    /// The file `name` and its bytes; for what is not a file, a route of
    /// the single-page app that the client resolves, the index. `None`
    /// when the web app was not built.
    fn page<'a>(&self, name: &'a str) -> Option<(&'a str, &'static [u8])> {
        match self.get(name) {
            Some(bytes) => Some((name, bytes)),
            None => self.get(INDEX).map(|bytes| (INDEX, bytes)),
        }
    }
}

/// The router's fallback: everything that is not an API route.
pub(crate) async fn serve(
    State(state): State<AppState>,
    method: Method,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let path = uri.path();
    if let Some(refusal) = refusal(path, &method) {
        return refusal;
    }
    let Some((name, bytes)) = state.assets.page(path.trim_start_matches('/')) else {
        return (
            [
                (CONTENT_TYPE, "text/html; charset=utf-8"),
                (CACHE_CONTROL, "no-cache"),
            ],
            NOT_BUILT,
        )
            .into_response();
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

/// The answer to a request the web app does not serve: an API or sign-in
/// path, or a method other than GET and HEAD.
fn refusal(path: &str, method: &Method) -> Option<Response> {
    if crate::guard::is_api_path(path) {
        // An API client must never get the web app's HTML for a mistyped route.
        let failure = ApiFailure::new(StatusCode::NOT_FOUND, format!("no such route: {path}"));
        return Some(failure.into_response());
    }
    // Sign-in's own prefix: what is not one of its routes is nothing, not
    // the web app (which never has a page there).
    if crate::guard::is_auth_path(path) {
        return Some(crate::auth::not_found());
    }
    if method != Method::GET && method != Method::HEAD {
        return Some((StatusCode::METHOD_NOT_ALLOWED, [(ALLOW, "GET, HEAD")]).into_response());
    }
    None
}

/// The type of each extension the web app ships; anything else is bytes.
const CONTENT_TYPES: [(&str, &str); 17] = [
    ("html", "text/html; charset=utf-8"),
    ("js", "text/javascript; charset=utf-8"),
    ("mjs", "text/javascript; charset=utf-8"),
    ("css", "text/css; charset=utf-8"),
    ("json", "application/json"),
    ("map", "application/json"),
    ("webmanifest", "application/manifest+json"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("ico", "image/x-icon"),
    ("woff2", "font/woff2"),
    ("woff", "font/woff"),
    ("txt", "text/plain; charset=utf-8"),
    ("wasm", "application/wasm"),
];

fn content_type(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    CONTENT_TYPES
        .iter()
        .find(|(known, _)| *known == extension)
        .map_or("application/octet-stream", |(_, content_type)| content_type)
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

    #[test]
    fn every_known_extension_has_its_type_and_others_are_bytes() {
        for (name, expected) in [
            ("a.html", "text/html; charset=utf-8"),
            ("a.js", "text/javascript; charset=utf-8"),
            ("a.mjs", "text/javascript; charset=utf-8"),
            ("a.css", "text/css; charset=utf-8"),
            ("a.json", "application/json"),
            ("a.js.map", "application/json"),
            ("manifest.webmanifest", "application/manifest+json"),
            ("a.svg", "image/svg+xml"),
            ("a.png", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.webp", "image/webp"),
            ("favicon.ico", "image/x-icon"),
            ("a.woff2", "font/woff2"),
            ("a.woff", "font/woff"),
            ("robots.txt", "text/plain; charset=utf-8"),
            ("a.wasm", "application/wasm"),
            // The extension is the text after the last dot, compared exactly.
            ("a.HTML", "application/octet-stream"),
            ("html", "application/octet-stream"),
            ("a.html.gz", "application/octet-stream"),
            ("a.", "application/octet-stream"),
        ] {
            assert_eq!(content_type(name), expected, "{name}");
        }
    }
}
