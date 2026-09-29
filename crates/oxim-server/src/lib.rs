//! The OXIM REST API: authentication, channels, messages with PHI masking,
//! code tables, users, API tokens, the audit trail, Prometheus metrics and
//! server-sent events for the dashboard.
//!
//! Build an [`AppState`] from a running [`oxim_core::Engine`] and an
//! [`oxim_auth::AuthStore`], then either mount [`router`] yourself (for
//! example in tests, with `tower::ServiceExt::oneshot`) or call [`serve`].
//!
//! Every endpoint except login and the OpenAPI document requires a session
//! (cookie or bearer) or an API token. Callers without the
//! `view_unmasked` permission see patient-identifying values replaced by
//! `***`; every content view is audited.

mod audit;
mod caller;
mod error;
mod extract;
mod files;
mod mask;
mod routes;
mod state;
mod tls;
mod ui;

use std::net::SocketAddr;
use std::path::Path;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::services::{ServeDir, ServeFile};

pub use caller::{CSRF_COOKIE, CSRF_HEADER, Caller, SESSION_COOKIE};
pub use error::{ApiError, ApiResult};
pub use mask::{MASK, Masked, mask};
pub use routes::API_PREFIX;
pub use state::{AppState, ServerConfig, TlsFiles};

/// The OpenAPI 3.1 description of the API.
pub fn openapi() -> serde_json::Value {
    routes::openapi_document()
}

/// Whether this build carries the web UI (the `embedded-ui` feature with a
/// built `ui/dist`). A configured `ui_dir` takes precedence over it.
pub fn ui_embedded() -> bool {
    ui::embedded()
}

const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-resource-policy", "same-origin"),
    (
        "content-security-policy",
        "default-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; \
         frame-ancestors 'none'; form-action 'self'",
    ),
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=()",
    ),
];

/// Applies the request timeout (except to the event stream), security
/// headers and response counting.
async fn harden(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let streaming = path == format!("{API_PREFIX}/events");
    let mut response = if streaming {
        next.run(request).await
    } else {
        match tokio::time::timeout(state.inner.config.request_timeout, next.run(request)).await {
            Ok(response) => response,
            Err(_) => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "timeout",
                "the request took too long",
            )
            .into_response(),
        }
    };
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        headers
            .entry(HeaderName::from_static(name))
            .or_insert(HeaderValue::from_static(value));
    }
    if state.inner.config.tls.is_some() {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    if path.starts_with(API_PREFIX) || path == "/metrics" {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    state
        .inner
        .counters
        .record_status(response.status().as_u16());
    response
}

fn ui_dir(dir: &Path) -> ServeDir<ServeFile> {
    ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html")))
}

/// The complete application: the API under [`API_PREFIX`], `/metrics`, and
/// the web UI: from `ui_dir` when set, else the files embedded with the
/// `embedded-ui` feature, else a page (or, for non-browser clients, a JSON
/// note at `/`) explaining how to add it.
pub fn router(state: AppState) -> Router {
    let config = &state.inner.config;
    let (timed, streaming) = routes::api();
    let api = timed
        .merge(streaming)
        .fallback(routes::not_found)
        .method_not_allowed_fallback(routes::method_not_allowed);
    let mut app = Router::new()
        .nest(API_PREFIX, api)
        .route("/metrics", axum::routing::get(routes::metrics::metrics));
    app = match config.ui_dir.as_deref().filter(|dir| dir.is_dir()) {
        Some(dir) => app.fallback_service(ui_dir(dir)),
        None if ui::embedded() => app.fallback(ui::serve_embedded),
        None => app.fallback(ui::missing),
    };
    app.layer(DefaultBodyLimit::max(config.max_body_bytes))
        .layer(middleware::from_fn_with_state(state.clone(), harden))
        .with_state(state)
}

/// Errors starting the server.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServeError {
    /// The listen address cannot be bound.
    #[error("cannot listen on {address}: {source}")]
    Bind {
        /// The address.
        address: SocketAddr,
        /// The cause.
        source: std::io::Error,
    },
    /// The TLS certificate or key cannot be used.
    #[error("TLS: {0}")]
    Tls(String),
    /// The server failed while running.
    #[error("server: {0}")]
    Io(#[from] std::io::Error),
}

/// A bound socket with the TLS configuration loaded, ready to [`serve`].
pub struct Listener {
    tcp: TcpListener,
    tls: Option<tokio_rustls::TlsAcceptor>,
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listener")
            .field("address", &self.tcp.local_addr().ok())
            .field("tls", &self.tls.is_some())
            .finish()
    }
}

impl Listener {
    /// The bound address (useful after binding port 0).
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.tcp.local_addr()
    }
}

/// Binds the configured address and loads the TLS certificate and key, so
/// configuration errors surface before the server runs.
pub async fn bind(config: &ServerConfig) -> Result<Listener, ServeError> {
    let tls = config
        .tls
        .as_ref()
        .map(tls::acceptor)
        .transpose()
        .map_err(ServeError::Tls)?;
    let tcp = TcpListener::bind(config.listen)
        .await
        .map_err(|source| ServeError::Bind {
            address: config.listen,
            source,
        })?;
    Ok(Listener { tcp, tls })
}

/// How often expired sessions are deleted.
const SESSION_PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Serves the application on `listener` (HTTP, or HTTPS when the
/// configuration has TLS files) until `shutdown` is cancelled, then closes
/// the event streams and waits for requests in progress.
pub async fn serve(
    listener: Listener,
    state: AppState,
    shutdown: CancellationToken,
) -> Result<(), ServeError> {
    let address = listener.local_addr()?;
    let app = router(state.clone()).into_make_service_with_connect_info::<caller::ClientAddr>();
    let stopping = {
        let shutdown = shutdown.clone();
        let state = state.clone();
        async move {
            shutdown.cancelled().await;
            state.close_streams();
        }
    };
    let pruning = {
        let state = state.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                let now = state.inner.engine.clock().now();
                if let Err(error) = state.inner.auth.prune_sessions(now) {
                    tracing::warn!(%error, "cannot delete expired sessions");
                }
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(SESSION_PRUNE_INTERVAL) => {}
                }
            }
        })
    };
    let Listener { tcp: listener, tls } = listener;
    let result = match tls {
        Some(acceptor) => {
            tracing::info!(%address, "web server listening (HTTPS)");
            let listener = tls::TlsListener::new(listener, acceptor, shutdown.clone());
            axum::serve(listener, app)
                .with_graceful_shutdown(stopping)
                .await
        }
        None => {
            tracing::info!(%address, "web server listening (HTTP)");
            axum::serve(listener, app)
                .with_graceful_shutdown(stopping)
                .await
        }
    };
    pruning.abort();
    result?;
    tracing::info!("web server stopped");
    Ok(())
}
