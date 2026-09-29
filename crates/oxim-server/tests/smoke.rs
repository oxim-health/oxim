//! The server on a real socket.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use oxim_auth::{AuthStore, NewUser, Role};
use oxim_core::{Engine, EngineOptions, Registry, SystemClock};
use oxim_server::{AppState, ServerConfig, TlsFiles};
use oxim_store::SqliteStore;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

async fn http(address: std::net::SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

async fn engine() -> Engine {
    Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        Registry::new(),
        Arc::new(SystemClock),
        EngineOptions::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn serves_https() {
    let dir = tempfile::tempdir().unwrap();
    let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let engine = engine().await;
    let auth = Arc::new(AuthStore::open_in_memory().unwrap());
    let mut config = ServerConfig::new(
        dir.path().join("channels"),
        dir.path().join("tables"),
        dir.path().join("data"),
    );
    config.listen = "127.0.0.1:0".parse().unwrap();
    config.tls = Some(TlsFiles {
        cert: data.join("localhost.crt"),
        key: data.join("localhost.key"),
    });
    let listener = oxim_server::bind(&config).await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = AppState::new(engine.clone(), auth, config);
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(oxim_server::serve(listener, state, shutdown.clone()));

    // A client that stalls during the handshake does not block others.
    let _stalled = TcpStream::connect(address).await.unwrap();

    let mut roots = rustls::RootCertStore::empty();
    for certificate in CertificateDer::pem_file_iter(data.join("localhost.crt")).unwrap() {
        roots.add(certificate.unwrap()).unwrap();
    }
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let tcp = TcpStream::connect(address).await.unwrap();
    let mut tls = connector
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    tls.write_all(b"GET /api/v1/auth/me HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    let _ = tls.read_to_end(&mut response).await;
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("strict-transport-security: max-age=")
    );

    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    engine.shutdown().await;
}

#[tokio::test]
async fn serves_on_a_socket_and_shuts_down() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine().await;
    let auth = Arc::new(AuthStore::open_in_memory().unwrap());
    auth.create_user(
        &NewUser {
            username: "admin",
            display_name: "Administrator",
            password: "correct horse battery",
            role: Role::Admin,
        },
        engine.clock().now(),
    )
    .unwrap();
    let mut config = ServerConfig::new(
        dir.path().join("channels"),
        dir.path().join("tables"),
        dir.path().join("data"),
    );
    config.listen = "127.0.0.1:0".parse().unwrap();
    let listener = oxim_server::bind(&config).await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = AppState::new(engine.clone(), auth, config);
    let shutdown = CancellationToken::new();
    let server = tokio::spawn(oxim_server::serve(listener, state, shutdown.clone()));

    let response = http(
        address,
        "GET /api/v1/openapi.json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("\"openapi\":\"3.1.0\""));

    let body = r#"{"username":"admin","password":"correct horse battery"}"#;
    let response = http(
        address,
        &format!(
            "POST /api/v1/auth/login HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");

    // The client address reaches the audit trail.
    let events = engine
        .store()
        .run(|store| store.audit_trail(None, 10))
        .await
        .unwrap();
    assert!(
        events.iter().any(|event| event.action == "auth.login"
            && event.detail.as_deref() == Some("client 127.0.0.1"))
    );

    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    engine.shutdown().await;
}
