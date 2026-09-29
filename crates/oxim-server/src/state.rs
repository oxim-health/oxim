//! Shared server state and configuration.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oxim_auth::{AuthStore, LoginThrottle, SessionPolicy};
use oxim_core::Engine;
use oxim_model::Timestamp;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// TLS certificate and key files in PEM form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    /// Certificate chain, leaf first.
    pub cert: PathBuf,
    /// Private key (PKCS#8, PKCS#1 or SEC1).
    pub key: PathBuf,
}

/// Server settings.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ServerConfig {
    /// Address to listen on.
    pub listen: SocketAddr,
    /// Serve HTTPS with these files instead of plain HTTP.
    pub tls: Option<TlsFiles>,
    /// Directory with the web UI's static files, if installed.
    pub ui_dir: Option<PathBuf>,
    /// Session timeouts.
    pub sessions: SessionPolicy,
    /// Where channel files live.
    pub channels_dir: PathBuf,
    /// Where code tables live.
    pub tables_dir: PathBuf,
    /// Where the databases live.
    pub data_dir: PathBuf,
    /// Whether session cookies carry the `Secure` attribute. Browsers accept
    /// secure cookies from `http://localhost`, so this stays on unless a
    /// plain-HTTP remote deployment requires otherwise (not recommended).
    pub secure_cookies: bool,
    /// Largest accepted request body.
    pub max_body_bytes: usize,
    /// Time limit for API requests (the event stream is exempt).
    pub request_timeout: Duration,
    /// Most concurrent event-stream clients.
    pub max_event_clients: usize,
}

impl ServerConfig {
    /// Settings with the defaults: `127.0.0.1:8080`, no TLS, no UI.
    pub fn new(channels_dir: PathBuf, tables_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            tls: None,
            ui_dir: None,
            sessions: SessionPolicy::default(),
            channels_dir,
            tables_dir,
            data_dir,
            secure_cookies: true,
            max_body_bytes: 4 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            max_event_clients: 32,
        }
    }
}

pub(crate) struct Inner {
    pub(crate) engine: Engine,
    pub(crate) auth: Arc<AuthStore>,
    pub(crate) config: ServerConfig,
    pub(crate) throttle: LoginThrottle,
    pub(crate) started: Instant,
    pub(crate) started_at: Timestamp,
    pub(crate) event_clients: Arc<Semaphore>,
    pub(crate) shutdown: CancellationToken,
    pub(crate) counters: crate::routes::metrics::Counters,
}

/// State shared by every request handler.
#[derive(Clone)]
pub struct AppState {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("listen", &self.inner.config.listen)
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// Creates the state for `engine`, authenticating against `auth`.
    pub fn new(engine: Engine, auth: Arc<AuthStore>, config: ServerConfig) -> Self {
        let now = engine.clock().now();
        let clients = config.max_event_clients.max(1);
        Self {
            inner: Arc::new(Inner {
                engine,
                auth,
                config,
                throttle: LoginThrottle::default(),
                started: Instant::now(),
                started_at: now,
                event_clients: Arc::new(Semaphore::new(clients)),
                shutdown: CancellationToken::new(),
                counters: Default::default(),
            }),
        }
    }

    /// The engine.
    pub fn engine(&self) -> &Engine {
        &self.inner.engine
    }

    /// Ends open event streams so a graceful shutdown can finish.
    pub fn close_streams(&self) {
        self.inner.shutdown.cancel();
    }
}
