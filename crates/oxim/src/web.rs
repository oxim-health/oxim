//! Starting and stopping the web server with the engine.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use oxim_auth::{AuthStore, SessionPolicy};
use oxim_core::Engine;
use oxim_server::{AppState, BackupSettings, ServerConfig, Services, TlsFiles};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::CliResult;
use crate::settings::Settings;

/// A running web server.
pub(crate) struct WebServer {
    address: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

/// The server configuration from `oxim.yaml`.
pub(crate) fn server_config(settings: &Settings) -> ServerConfig {
    let server = &settings.server;
    let mut config = ServerConfig::new(
        settings.channels_dir.clone(),
        settings.tables_dir.clone(),
        settings.data_dir.clone(),
    );
    config.listen = server.listen;
    config.tls = server.tls.as_ref().map(|tls| TlsFiles {
        cert: tls.cert.clone(),
        key: tls.key.clone(),
    });
    config.ui_dir = server.ui_dir.clone();
    config.scripts_dir = Some(settings.scripts_dir.clone());
    config.config_file = settings.config_file.clone();
    config.backups = BackupSettings {
        dir: settings.backups_dir(),
        keep: settings.backups.keep.max(1),
    };
    config.sessions = SessionPolicy {
        idle: server.session_idle.0,
        max: server.session_max.0,
    };
    config
}

/// Starts the web server unless it is disabled. Without any user the
/// server does not start (there would be no way to log in); the engine
/// runs regardless. A port that cannot be bound or unusable TLS files stop
/// `oxim run` with an error.
pub(crate) async fn start(
    settings: &Settings,
    engine: &Engine,
    services: Services,
) -> CliResult<Option<WebServer>> {
    if !settings.server.enabled {
        info!("web server disabled in the configuration");
        return Ok(None);
    }
    let path = settings.auth_database_path();
    let auth =
        AuthStore::open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    if auth.user_count()? == 0 {
        warn!(
            "web server not started: no users exist; create an administrator with \
             `oxim users create-admin` and restart"
        );
        return Ok(None);
    }
    let config = server_config(settings);
    let listener = oxim_server::bind(&config).await?;
    let address = listener.local_addr()?;
    if config.tls.is_none() && !config.listen.ip().is_loopback() {
        warn!(
            listen = %config.listen,
            "the web server accepts remote connections without TLS; configure server.tls"
        );
    }
    let state = AppState::with_services(engine.clone(), Arc::new(auth), config, services);
    let shutdown = CancellationToken::new();
    let token = shutdown.clone();
    let task = tokio::spawn(async move {
        if let Err(e) = oxim_server::serve(listener, state, token).await {
            error!(error = %e, "web server failed");
        }
    });
    Ok(Some(WebServer {
        address,
        shutdown,
        task,
    }))
}

/// Stops the web server, waiting briefly for requests in progress.
pub(crate) async fn stop(server: Option<WebServer>) {
    let Some(server) = server else { return };
    info!(address = %server.address, "stopping the web server");
    server.shutdown.cancel();
    if tokio::time::timeout(Duration::from_secs(10), server.task)
        .await
        .is_err()
    {
        warn!("web server did not stop in time");
    }
}

#[cfg(test)]
mod tests {
    use oxim_auth::{NewUser, Role};
    use oxim_core::{EngineOptions, Registry, SystemClock};
    use oxim_store::SqliteStore;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn starts_only_with_users_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings {
            data_dir: dir.path().join("data"),
            channels_dir: dir.path().join("channels"),
            tables_dir: dir.path().join("tables"),
            ..Settings::default()
        };
        settings.server.listen = "127.0.0.1:0".parse().unwrap();
        std::fs::create_dir_all(&settings.data_dir).unwrap();
        let engine = Engine::start(
            Box::new(SqliteStore::open_in_memory().unwrap()),
            Registry::new(),
            Arc::new(SystemClock),
            EngineOptions::default(),
        )
        .await
        .unwrap();

        assert!(
            start(&settings, &engine, Services::new())
                .await
                .unwrap()
                .is_none()
        );

        AuthStore::open(settings.auth_database_path())
            .unwrap()
            .create_user(
                &NewUser {
                    username: "admin",
                    display_name: "Administrator",
                    password: "correct horse battery",
                    role: Role::Admin,
                },
                engine.clock().now(),
            )
            .unwrap();
        let server = start(&settings, &engine, Services::new())
            .await
            .unwrap()
            .unwrap();
        let mut stream = tokio::net::TcpStream::connect(server.address)
            .await
            .unwrap();
        stream
            .write_all(b"GET /api/v1/auth/me HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 401"));
        stop(Some(server)).await;

        settings.server.enabled = false;
        assert!(
            start(&settings, &engine, Services::new())
                .await
                .unwrap()
                .is_none()
        );
        engine.shutdown().await;
    }
}
