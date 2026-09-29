//! HTTPS: a listener that completes TLS handshakes in background tasks, so
//! a slow or stalled client cannot block other connections.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use tokio_util::sync::CancellationToken;

use crate::state::TlsFiles;

/// Longest time a client may take to complete the handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Most handshakes in progress at once.
const MAX_HANDSHAKES: usize = 128;

/// Builds the TLS configuration from PEM files. TLS 1.2 and 1.3 with the
/// `ring` provider's default cipher suites; HTTP/1.1 only.
pub(crate) fn acceptor(files: &TlsFiles) -> Result<TlsAcceptor, String> {
    let certificates = CertificateDer::pem_file_iter(&files.cert)
        .map_err(|e| format!("{}: {e}", files.cert.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{}: {e}", files.cert.display()))?;
    if certificates.is_empty() {
        return Err(format!("{}: no certificate found", files.cert.display()));
    }
    let key = PrivateKeyDer::from_pem_file(&files.key)
        .map_err(|e| format!("{}: {e}", files.key.display()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|e| e.to_string())?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Accepts TCP connections and hands out those that completed a TLS
/// handshake.
pub(crate) struct TlsListener {
    connections: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
}

impl TlsListener {
    pub(crate) fn new(
        listener: TcpListener,
        acceptor: TlsAcceptor,
        shutdown: CancellationToken,
    ) -> Self {
        let local = listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(accept_loop(listener, acceptor, tx, shutdown));
        Self {
            connections: rx,
            local,
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    tx: mpsc::Sender<(TlsStream<TcpStream>, SocketAddr)>,
    shutdown: CancellationToken,
) {
    let handshakes = Arc::new(Semaphore::new(MAX_HANDSHAKES));
    loop {
        let accepted = tokio::select! {
            () = shutdown.cancelled() => return,
            () = tx.closed() => return,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(connection) => connection,
            Err(error) => {
                tracing::debug!(%error, "accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(permit) = handshakes.clone().try_acquire_owned() else {
            tracing::warn!(%peer, "too many TLS handshakes in progress; connection dropped");
            continue;
        };
        let acceptor = acceptor.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                Ok(Ok(tls)) => {
                    let _ = tx.send((tls, peer)).await;
                }
                Ok(Err(error)) => tracing::debug!(%peer, %error, "TLS handshake failed"),
                Err(_) => tracing::debug!(%peer, "TLS handshake timed out"),
            }
        });
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.connections.recv().await {
            Some(connection) => connection,
            // The accept loop ended (shutdown); wait for the graceful
            // shutdown to stop the server.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}
