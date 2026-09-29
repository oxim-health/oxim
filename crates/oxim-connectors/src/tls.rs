//! TLS and mutual TLS for the stream connectors (`mllp`, `tcp`, the HTTP
//! listener), with rustls and the ring provider.
//!
//! A listener enables TLS with a `tls` block:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `cert_file` | required | PEM certificate chain, server certificate first |
//! | `key_file` | required | PEM private key (PKCS #8, PKCS #1 or SEC1) |
//! | `client_ca_file` | none | PEM certificate authorities for client certificates; enables mutual TLS |
//! | `require_client_cert` | `true` | With `client_ca_file`: reject clients without a certificate |
//! | `handshake_timeout` | `10s` | Limit for the TLS handshake |
//!
//! A sender enables TLS with a `tls` block (`tls: {}` for the defaults):
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `ca_file` | none | PEM certificate authorities to trust in addition to the system's |
//! | `system_roots` | `true` | Whether to trust the operating system's certificate authorities |
//! | `cert_file`, `key_file` | none | Client certificate and key for mutual TLS |
//! | `server_name` | host of the target | Name to verify the server certificate against |
//!
//! Paths are used as written; relative paths depend on the working
//! directory of the process, so services should use absolute paths.
//! TLS 1.2 and 1.3 are accepted; certificates are always verified.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use oxim_core::EngineError;
use oxim_core::config::DurationText;
use rustls::RootCertStore;
use rustls::server::WebPkiClientVerifier;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

fn default_true() -> bool {
    true
}

fn default_handshake_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

/// TLS settings of a listener.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerTlsSettings {
    /// PEM certificate chain.
    pub cert_file: PathBuf,
    /// PEM private key.
    pub key_file: PathBuf,
    /// PEM certificate authorities for client certificates.
    #[serde(default)]
    pub client_ca_file: Option<PathBuf>,
    /// Whether clients must present a certificate when `client_ca_file`
    /// is set.
    #[serde(default = "default_true")]
    pub require_client_cert: bool,
    /// Limit for the TLS handshake.
    #[serde(default = "default_handshake_timeout")]
    pub handshake_timeout: DurationText,
}

/// TLS settings of a sender.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientTlsSettings {
    /// Extra PEM certificate authorities to trust.
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
    /// Whether to trust the operating system's certificate authorities.
    #[serde(default = "default_true")]
    pub system_roots: bool,
    /// PEM client certificate chain for mutual TLS.
    #[serde(default)]
    pub cert_file: Option<PathBuf>,
    /// PEM private key of the client certificate.
    #[serde(default)]
    pub key_file: Option<PathBuf>,
    /// Name to verify the server certificate against.
    #[serde(default)]
    pub server_name: Option<String>,
}

fn config_error(message: String) -> EngineError {
    EngineError::Config(format!("TLS: {message}"))
}

fn certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, EngineError> {
    let certificates = CertificateDer::pem_file_iter(path)
        .and_then(|items| items.collect::<Result<Vec<_>, _>>())
        .map_err(|e| {
            config_error(format!(
                "cannot read certificates from {}: {e}",
                path.display()
            ))
        })?;
    if certificates.is_empty() {
        return Err(config_error(format!(
            "{} contains no PEM certificate",
            path.display()
        )));
    }
    Ok(certificates)
}

fn private_key(path: &Path) -> Result<PrivateKeyDer<'static>, EngineError> {
    PrivateKeyDer::from_pem_file(path).map_err(|e| {
        config_error(format!(
            "cannot read a private key from {}: {e}",
            path.display()
        ))
    })
}

fn roots(path: &Path) -> Result<RootCertStore, EngineError> {
    let mut store = RootCertStore::empty();
    for certificate in certificates(path)? {
        store.add(certificate).map_err(|e| {
            config_error(format!("invalid CA certificate in {}: {e}", path.display()))
        })?;
    }
    Ok(store)
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Builds the acceptor of a listener.
pub(crate) fn acceptor(settings: &ServerTlsSettings) -> Result<TlsAcceptor, EngineError> {
    let builder = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| config_error(e.to_string()))?;
    let builder = match &settings.client_ca_file {
        Some(path) => {
            let verifier =
                WebPkiClientVerifier::builder_with_provider(Arc::new(roots(path)?), provider());
            let verifier = if settings.require_client_cert {
                verifier
            } else {
                verifier.allow_unauthenticated()
            };
            builder.with_client_cert_verifier(
                verifier
                    .build()
                    .map_err(|e| config_error(format!("client certificate verifier: {e}")))?,
            )
        }
        None => builder.with_no_client_auth(),
    };
    let config = builder
        .with_single_cert(
            certificates(&settings.cert_file)?,
            private_key(&settings.key_file)?,
        )
        .map_err(|e| config_error(format!("certificate and key: {e}")))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// A connector for a sender, with the name to verify.
#[derive(Clone)]
pub(crate) struct Client {
    connector: TlsConnector,
    server_name: ServerName<'static>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("server_name", &self.server_name)
            .finish_non_exhaustive()
    }
}

/// The host part of `host:port`, `[v6]:port` or a bare host.
fn host_of(target: &str) -> &str {
    if let Some(rest) = target.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match target.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => {
            host
        }
        _ => target,
    }
}

/// Builds the rustls client configuration of a `tls` settings block: the
/// trusted authorities and the optional client certificate, with the ring
/// provider. Other connector crates use it for protocols whose client
/// libraries accept a rustls configuration.
pub fn client_config(settings: &ClientTlsSettings) -> Result<rustls::ClientConfig, EngineError> {
    let mut store = RootCertStore::empty();
    if settings.system_roots {
        let native = rustls_native_certs::load_native_certs();
        for certificate in native.certs {
            // Unusable system certificates are skipped, as browsers do.
            let _ = store.add(certificate);
        }
    }
    if let Some(path) = &settings.ca_file {
        for certificate in certificates(path)? {
            store.add(certificate).map_err(|e| {
                config_error(format!("invalid CA certificate in {}: {e}", path.display()))
            })?;
        }
    }
    if store.is_empty() {
        return Err(config_error(
            "no trusted certificate authorities: set ca_file or enable system_roots".into(),
        ));
    }
    let builder = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| config_error(e.to_string()))?
        .with_root_certificates(store);
    match (&settings.cert_file, &settings.key_file) {
        (Some(cert), Some(key)) => builder
            .with_client_auth_cert(certificates(cert)?, private_key(key)?)
            .map_err(|e| config_error(format!("client certificate and key: {e}"))),
        (None, None) => Ok(builder.with_no_client_auth()),
        _ => Err(config_error(
            "cert_file and key_file must be set together".into(),
        )),
    }
}

/// The name a server certificate must match: `server_name` if set,
/// otherwise the host part of `target` (`host:port`, `[v6]:port` or a bare
/// host).
pub fn server_name(
    settings: &ClientTlsSettings,
    target: &str,
) -> Result<ServerName<'static>, EngineError> {
    let name = settings
        .server_name
        .clone()
        .unwrap_or_else(|| host_of(target).to_owned());
    ServerName::try_from(name.clone())
        .map_err(|e| config_error(format!("invalid server name {name:?}: {e}")))
}

/// Builds the TLS client of a sender connecting to `target`.
pub(crate) fn client(settings: &ClientTlsSettings, target: &str) -> Result<Client, EngineError> {
    Ok(Client {
        connector: TlsConnector::from(Arc::new(client_config(settings)?)),
        server_name: server_name(settings, target)?,
    })
}

impl Client {
    /// Performs the client handshake on a connected socket.
    pub(crate) async fn connect(&self, tcp: TcpStream) -> io::Result<Stream> {
        let stream = self
            .connector
            .connect(self.server_name.clone(), tcp)
            .await?;
        Ok(Stream::Client(Box::new(stream)))
    }
}

/// A connection, plain or with TLS.
#[derive(Debug)]
pub enum Stream {
    /// Plain TCP.
    Plain(TcpStream),
    /// TLS accepted by a listener.
    Server(Box<tokio_rustls::server::TlsStream<TcpStream>>),
    /// TLS opened by a sender.
    Client(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl Stream {
    /// The underlying socket.
    pub fn tcp(&self) -> &TcpStream {
        match self {
            Self::Plain(stream) => stream,
            Self::Server(stream) => stream.get_ref().0,
            Self::Client(stream) => stream.get_ref().0,
        }
    }

    /// Whether the connection is encrypted.
    pub fn is_tls(&self) -> bool {
        !matches!(self, Self::Plain(_))
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Server(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
            Self::Client(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Server(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
            Self::Client(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Server(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
            Self::Client(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Server(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
            Self::Client(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }
}

/// How a sender opens connections: plain or with TLS.
#[derive(Debug, Clone)]
pub struct Dialer {
    target: String,
    connect_timeout: Duration,
    tls: Option<Client>,
}

impl Dialer {
    /// A dialer for `target`, with TLS when `tls` is set. Certificate files
    /// are read here, so bad settings fail at deploy time.
    pub fn new(
        target: &str,
        connect_timeout: Duration,
        tls: Option<&ClientTlsSettings>,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            target: target.to_owned(),
            connect_timeout,
            tls: tls.map(|settings| client(settings, target)).transpose()?,
        })
    }

    /// Connects, including the TLS handshake, within the connect timeout.
    pub async fn connect(&self) -> Result<Stream, String> {
        let target = &self.target;
        let work = async {
            let tcp = TcpStream::connect(target)
                .await
                .map_err(|e| format!("cannot connect to {target}: {e}"))?;
            let _ = tcp.set_nodelay(true);
            match &self.tls {
                Some(client) => client
                    .connect(tcp)
                    .await
                    .map_err(|e| format!("TLS handshake with {target} failed: {e}")),
                None => Ok(Stream::Plain(tcp)),
            }
        };
        tokio::time::timeout(self.connect_timeout, work)
            .await
            .map_err(|_| format!("connecting to {target} timed out"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_host_of_a_target() {
        assert_eq!(host_of("lis.example.org:2575"), "lis.example.org");
        assert_eq!(host_of("10.0.0.5:2575"), "10.0.0.5");
        assert_eq!(host_of("[::1]:2575"), "::1");
        assert_eq!(host_of("lis.example.org"), "lis.example.org");
    }

    #[test]
    fn reports_missing_files() {
        let settings = ServerTlsSettings {
            cert_file: "missing-cert.pem".into(),
            key_file: "missing-key.pem".into(),
            client_ca_file: None,
            require_client_cert: true,
            handshake_timeout: default_handshake_timeout(),
        };
        let Err(error) = acceptor(&settings) else {
            panic!("missing files must fail");
        };
        let error = error.to_string();
        assert!(error.contains("missing-cert.pem"), "{error}");
        let client_settings = ClientTlsSettings {
            ca_file: None,
            system_roots: false,
            cert_file: None,
            key_file: None,
            server_name: None,
        };
        let error = client(&client_settings, "x:1").unwrap_err().to_string();
        assert!(
            error.contains("no trusted certificate authorities"),
            "{error}"
        );
    }
}
