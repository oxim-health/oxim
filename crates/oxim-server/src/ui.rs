//! The web UI: files embedded at build time (the `embedded-ui` feature), a
//! directory on disk (`ui_dir`, which takes precedence), or a page that
//! explains how to build the UI when neither is available.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::error::ApiError;
use crate::routes::API_PREFIX;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));
}

/// Cache policy for content-hashed build output (`assets/` from Vite).
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Cache policy for everything else, notably `index.html`, so a new
/// version is picked up at once.
const REVALIDATE: &str = "no-cache";

/// Whether this build carries the web UI.
pub(crate) fn embedded() -> bool {
    !embedded::ASSETS.is_empty()
}

fn asset(path: &str) -> Option<&'static [u8]> {
    embedded::ASSETS
        .binary_search_by(|(name, _)| (*name).cmp(path))
        .ok()
        .map(|index| embedded::ASSETS[index].1)
}

/// The media type of a file name.
pub(crate) fn content_type(path: &str) -> &'static str {
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

fn file_response(path: &str, bytes: &'static [u8]) -> Response {
    let cache = if path.starts_with("assets/") {
        IMMUTABLE
    } else {
        REVALIDATE
    };
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(path)),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
        ],
        bytes,
    )
        .into_response()
}

/// Whether the last path segment looks like a file name (has an
/// extension), as opposed to a client-side route such as `/channels/lab`.
fn looks_like_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'))
}

/// Serves the embedded UI: a file when it exists, `index.html` for
/// client-side routes, and `404` for missing files.
pub(crate) async fn serve_embedded(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if uri.path().starts_with(API_PREFIX) {
        return ApiError::not_found("no such API endpoint").into_response();
    }
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(bytes) = asset(path) {
        return file_response(path, bytes);
    }
    if looks_like_file(path) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match asset("index.html") {
        Some(bytes) => file_response("index.html", bytes),
        None => ApiError::not_found("not found").into_response(),
    }
}

fn wants_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
}

/// A page without scripts or styles (the content security policy allows
/// neither inline), shown by browsers when the build has no UI.
const MISSING_PAGE: &str = "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>OXIM</title>
</head>
<body>
<main>
<h1>OXIM is running</h1>
<p>This build does not include the web UI. The REST API is available at
<a href=\"/api/v1/openapi.json\">/api/v1/openapi.json</a>.</p>
<h2>Adding the web UI</h2>
<ol>
<li>Build it: <code>cd ui</code>, <code>npm ci</code>, <code>npm run build</code>.</li>
<li>Either rebuild OXIM (the <code>embedded-ui</code> feature includes <code>ui/dist</code>
in the binary), or set <code>server.ui_dir</code> in <code>oxim.yaml</code> to the
<code>ui/dist</code> directory and restart.</li>
</ol>
</main>
</body>
</html>
";

/// `/` and client-side routes when the build has no UI and no `ui_dir` is
/// set: an explanatory page for browsers, a JSON note for other clients.
pub(crate) async fn missing(request: Request) -> Response {
    let path = request.uri().path();
    if wants_html(request.headers()) && !path.starts_with(API_PREFIX) {
        return (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static(REVALIDATE)),
            ],
            MISSING_PAGE,
        )
            .into_response();
    }
    if path == "/" {
        return axum::Json(json!({
            "name": "oxim",
            "version": env!("CARGO_PKG_VERSION"),
            "api": API_PREFIX,
            "openapi": format!("{API_PREFIX}/openapi.json"),
            "note": "this build has no web UI; build ui/ and rebuild, or set server.ui_dir",
        }))
        .into_response();
    }
    ApiError::not_found("no such API endpoint").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_content_types_and_routes() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("assets/index-3f2a.JS"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type("favicon.svg"), "image/svg+xml");
        assert_eq!(content_type("LICENSE"), "application/octet-stream");
        assert!(looks_like_file("assets/app.js"));
        assert!(!looks_like_file("channels/lab"));
        assert!(!looks_like_file("messages/01HZX"));
    }

    #[test]
    fn embedded_files_are_sorted() {
        assert!(
            embedded::ASSETS
                .windows(2)
                .all(|pair| pair[0].0 < pair[1].0)
        );
    }
}
