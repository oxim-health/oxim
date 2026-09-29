//! Shared server state and configuration.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oxim_alert::AlertEngine;
use oxim_auth::{AuthStore, LoginThrottle, SessionPolicy};
use oxim_core::Engine;
use oxim_devices::DeviceEnvironment;
use oxim_model::Timestamp;

use crate::history::ChannelHistory;
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

/// Where backups go and how many are kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSettings {
    /// The backup directory.
    pub dir: PathBuf,
    /// How many backups are kept; older ones are deleted after a new one.
    pub keep: usize,
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
    /// Where script files live, if configured (included in backups).
    pub scripts_dir: Option<PathBuf>,
    /// The configuration file, if known (included in backups).
    pub config_file: Option<PathBuf>,
    /// Backups made through the API.
    pub backups: BackupSettings,
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
            backups: BackupSettings {
                dir: data_dir.join("backups"),
                keep: 7,
            },
            channels_dir,
            tables_dir,
            data_dir,
            scripts_dir: None,
            config_file: None,
            secure_cookies: true,
            max_body_bytes: 4 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            max_event_clients: 32,
        }
    }
}

/// Parts of a running installation the operations endpoints report on.
/// Each is optional: without alerts or a device registry the endpoints
/// report nothing; without history the history endpoints answer `404`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Services {
    /// The alert engine, for `GET /alerts`.
    pub alerts: Option<Arc<AlertEngine>>,
    /// The device registry, for `/devices`.
    pub devices: Option<DeviceEnvironment>,
    /// Channel version history, for `/channels/{id}/history`.
    pub history: Option<Arc<ChannelHistory>>,
}

impl Services {
    /// No services.
    pub fn new() -> Self {
        Self::default()
    }

    /// With the alert engine.
    pub fn with_alerts(mut self, alerts: Arc<AlertEngine>) -> Self {
        self.alerts = Some(alerts);
        self
    }

    /// With the device registry.
    pub fn with_devices(mut self, devices: DeviceEnvironment) -> Self {
        self.devices = Some(devices);
        self
    }

    /// With channel version history.
    pub fn with_history(mut self, history: Arc<ChannelHistory>) -> Self {
        self.history = Some(history);
        self
    }
}

/// Maintenance mode as switched through the API.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Maintenance {
    pub(crate) reason: String,
    pub(crate) by: String,
    pub(crate) since: Timestamp,
}

pub(crate) struct Inner {
    pub(crate) engine: Engine,
    pub(crate) services: Services,
    pub(crate) maintenance: std::sync::Mutex<Option<Maintenance>>,
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
        Self::with_services(engine, auth, config, Services::default())
    }

    /// Creates the state with the operations services of the installation.
    pub fn with_services(
        engine: Engine,
        auth: Arc<AuthStore>,
        config: ServerConfig,
        services: Services,
    ) -> Self {
        let now = engine.clock().now();
        let clients = config.max_event_clients.max(1);
        Self {
            inner: Arc::new(Inner {
                engine,
                services,
                maintenance: std::sync::Mutex::new(None),
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
