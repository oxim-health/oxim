//! SFTP (SSH File Transfer Protocol): a poller for a remote directory and
//! an atomic file writer, with russh and the ring crypto backend.
//!
//! Connection settings of type `sftp` (source and destination), in addition
//! to the shared polling and writing settings (see the crate README):
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `host` | required | Server name or address |
//! | `port` | `22` | Server port |
//! | `username` | required | User name |
//! | `password_env` | none | Environment variable holding the password |
//! | `password` | none | The password inline (discouraged) |
//! | `private_key_file` | none | OpenSSH or PEM private key (Ed25519 or ECDSA) |
//! | `private_key_passphrase_env` | none | Environment variable holding the key's passphrase |
//! | `known_hosts_file` | none | OpenSSH `known_hosts` file the server key must be listed in |
//! | `host_key_fingerprint` | none | The server key's SHA-256 fingerprint, as `ssh-keygen -lf` prints it (`SHA256:...`) |
//! | `connect_timeout` | `10s` | Limit for connecting, the key exchange and authentication |
//! | `timeout` | `60s` | Limit for each file operation |
//!
//! The server's host key is always verified: exactly one of
//! `known_hosts_file` and `host_key_fingerprint` is required, and a key
//! that is unknown or changed refuses the connection. Authentication uses
//! the private key when one is set, otherwise the password.
//!
//! Host and client keys must be Ed25519 or ECDSA (P-256, P-384, P-521).
//! RSA keys are not supported: the Rust RSA implementation that SSH would
//! need carries an unresolved timing side-channel advisory
//! (RUSTSEC-2023-0071), which OXIM's dependency policy does not accept.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::config::DurationText;
use oxim_core::{
    DestinationConfig, DestinationConnector, EngineError, Registry, SourceConfig, SourceConnector,
    async_trait,
};
use russh::client::{self, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, load_secret_key};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::remote::{
    Connect, PollingSource, RemoteDestination, RemoteEntry, Session, poll_settings, write_settings,
};
use crate::util::{check_secret, parse, secret};

fn default_port() -> u16 {
    22
}

fn default_connect_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(60))
}

/// Connection settings of the `sftp` connectors.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SftpSettings {
    /// Server name or address.
    pub host: String,
    /// Server port.
    #[serde(default = "default_port")]
    pub port: u16,
    /// User name.
    pub username: String,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// The password inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Private key file.
    #[serde(default)]
    pub private_key_file: Option<PathBuf>,
    /// Environment variable holding the private key's passphrase.
    #[serde(default)]
    pub private_key_passphrase_env: Option<String>,
    /// `known_hosts` file listing the server key.
    #[serde(default)]
    pub known_hosts_file: Option<PathBuf>,
    /// SHA-256 fingerprint of the server key.
    #[serde(default)]
    pub host_key_fingerprint: Option<String>,
    /// Limit for connecting and authenticating.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: DurationText,
    /// Limit for each file operation.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
}

/// How the server's key is trusted.
#[derive(Debug, Clone)]
enum HostKey {
    KnownHosts(PathBuf),
    Fingerprint(String),
}

/// A fingerprint as compared: without the `SHA256:` prefix and padding.
fn normalize_fingerprint(text: &str) -> String {
    let text = text.trim();
    let text = text
        .strip_prefix("SHA256:")
        .or_else(|| text.strip_prefix("sha256:"))
        .unwrap_or(text);
    text.trim_end_matches('=').to_owned()
}

/// The SFTP location and credentials, validated at deploy time.
#[derive(Debug, Clone)]
pub(crate) struct SftpConnect {
    settings: Arc<SftpSettings>,
    host_key: HostKey,
}

impl SftpConnect {
    fn new(settings: SftpSettings) -> Result<Self, EngineError> {
        let error = |message: &str| EngineError::Config(format!("sftp: {message}"));
        if settings.host.trim().is_empty() {
            return Err(error("host must not be empty"));
        }
        let host_key = match (&settings.known_hosts_file, &settings.host_key_fingerprint) {
            (Some(path), None) => HostKey::KnownHosts(path.clone()),
            (None, Some(fingerprint)) => {
                let normalized = normalize_fingerprint(fingerprint);
                if normalized.is_empty() {
                    return Err(error("host_key_fingerprint is empty"));
                }
                HostKey::Fingerprint(normalized)
            }
            _ => {
                return Err(error(
                    "set exactly one of known_hosts_file and host_key_fingerprint: \
                     the server's host key must always be verified",
                ));
            }
        };
        check_secret(
            settings.password.as_ref(),
            settings.password_env.as_ref(),
            "password",
            "sftp",
        )?;
        if settings.private_key_file.is_none()
            && settings.password.is_none()
            && settings.password_env.is_none()
        {
            return Err(error(
                "set private_key_file, password_env or password for authentication",
            ));
        }
        Ok(Self {
            settings: Arc::new(settings),
            host_key,
        })
    }
}

/// Verifies the server key and remembers why a key was refused.
struct Verifier {
    host: String,
    port: u16,
    host_key: HostKey,
    refusal: Arc<Mutex<Option<String>>>,
}

impl Verifier {
    fn refuse(&self, reason: String) -> bool {
        if let Ok(mut slot) = self.refusal.lock() {
            *slot = Some(reason);
        }
        false
    }
}

impl client::Handler for Verifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_public_key else {
            return Ok(self.refuse(
                "the server presented a host certificate; configure its plain host key".into(),
            ));
        };
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        Ok(match &self.host_key {
            HostKey::Fingerprint(expected) => {
                if normalize_fingerprint(&fingerprint) == *expected {
                    true
                } else {
                    self.refuse(format!(
                        "host key {fingerprint} does not match host_key_fingerprint"
                    ))
                }
            }
            HostKey::KnownHosts(path) => {
                match russh::keys::check_known_hosts_path(&self.host, self.port, key, path) {
                    Ok(true) => true,
                    Ok(false) => self.refuse(format!(
                        "host key {fingerprint} is not listed in {}",
                        path.display()
                    )),
                    Err(russh::keys::Error::KeyChanged { line }) => self.refuse(format!(
                        "HOST KEY CHANGED: {fingerprint} differs from line {line} of {}",
                        path.display()
                    )),
                    Err(e) => self.refuse(format!("cannot read {}: {e}", path.display())),
                }
            }
        })
    }
}

/// An authenticated SFTP session.
struct SftpSessionHandle {
    ssh: Handle<Verifier>,
    sftp: SftpSession,
    timeout: Duration,
}

fn sftp_error(e: russh_sftp::client::error::Error) -> String {
    e.to_string()
}

impl SftpSessionHandle {
    async fn within<T>(
        &self,
        what: &str,
        work: impl Future<Output = Result<T, russh_sftp::client::error::Error>>,
    ) -> Result<T, String> {
        tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| format!("{what} timed out"))?
            .map_err(|e| format!("{what}: {}", sftp_error(e)))
    }
}

fn path_or_dot(path: &str) -> String {
    if path.is_empty() {
        ".".to_owned()
    } else {
        path.to_owned()
    }
}

#[async_trait]
impl Session for SftpSessionHandle {
    async fn list(&mut self, directory: &str) -> Result<Vec<RemoteEntry>, String> {
        let entries = self
            .within(
                &format!("listing {}", path_or_dot(directory)),
                self.sftp.read_dir(path_or_dot(directory)),
            )
            .await?;
        Ok(entries
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| {
                let metadata = entry.metadata();
                RemoteEntry {
                    name: entry.file_name(),
                    size: metadata.size.unwrap_or(0),
                    modified: metadata.mtime.map(i64::from),
                }
            })
            .collect())
    }

    async fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
        self.within(&format!("reading {path}"), self.sftp.read(path))
            .await
    }

    async fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        let work = async {
            let mut file = self
                .sftp
                .open_with_flags(
                    path,
                    OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
                )
                .await?;
            file.write_all(data).await?;
            file.flush().await?;
            file.shutdown().await?;
            Ok(())
        };
        self.within(&format!("writing {path}"), work).await
    }

    async fn remove(&mut self, path: &str) -> Result<(), String> {
        self.within(&format!("removing {path}"), self.sftp.remove_file(path))
            .await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        self.within(
            &format!("renaming {from} to {to}"),
            self.sftp.rename(from, to),
        )
        .await
    }

    async fn exists(&mut self, path: &str) -> Result<bool, String> {
        self.within(&format!("checking {path}"), self.sftp.try_exists(path))
            .await
    }

    async fn create_dir_all(&mut self, directory: &str) -> Result<(), String> {
        let mut current = String::new();
        if directory.starts_with('/') {
            current.push('/');
        }
        for part in directory.split('/').filter(|part| !part.is_empty()) {
            if !current.is_empty() && !current.ends_with('/') {
                current.push('/');
            }
            current.push_str(part);
            if part == "." || part == ".." {
                continue;
            }
            if !self.exists(&current).await? {
                self.within(
                    &format!("creating {current}"),
                    self.sftp.create_dir(current.clone()),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn close(&mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(2), self.sftp.close()).await;
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.ssh
                .disconnect(russh::Disconnect::ByApplication, "", "en"),
        )
        .await;
    }
}

impl SftpConnect {
    async fn open(&self) -> Result<SftpSessionHandle, String> {
        let settings = &self.settings;
        let refusal = Arc::new(Mutex::new(None));
        let verifier = Verifier {
            host: settings.host.clone(),
            port: settings.port,
            host_key: self.host_key.clone(),
            refusal: refusal.clone(),
        };
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(Duration::from_secs(600)),
            keepalive_interval: Some(Duration::from_secs(30)),
            ..client::Config::default()
        });
        let mut ssh = client::connect(config, (settings.host.as_str(), settings.port), verifier)
            .await
            .map_err(|e| {
                let refused = refusal.lock().ok().and_then(|slot| slot.clone());
                match refused {
                    Some(reason) => format!("refusing the server: {reason}"),
                    None => format!("cannot connect: {e}"),
                }
            })?;
        let authenticated = match &settings.private_key_file {
            Some(path) => {
                let passphrase = secret(
                    None,
                    settings.private_key_passphrase_env.as_ref(),
                    "private key passphrase",
                )?;
                let key = load_secret_key(path, passphrase.as_deref())
                    .map_err(|e| format!("cannot load the private key {}: {e}", path.display()))?;
                ssh.authenticate_publickey(
                    settings.username.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), None),
                )
                .await
                .map_err(|e| format!("public key authentication failed: {e}"))?
                .success()
            }
            None => {
                let password = secret(
                    settings.password.as_ref(),
                    settings.password_env.as_ref(),
                    "password",
                )?
                .unwrap_or_default();
                ssh.authenticate_password(settings.username.clone(), password)
                    .await
                    .map_err(|e| format!("password authentication failed: {e}"))?
                    .success()
            }
        };
        if !authenticated {
            return Err(format!(
                "the server rejected the credentials of {}",
                settings.username
            ));
        }
        let channel = ssh
            .channel_open_session()
            .await
            .map_err(|e| format!("cannot open a session channel: {e}"))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| format!("the server refused the sftp subsystem: {e}"))?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| format!("cannot start SFTP: {e}"))?;
        sftp.set_timeout(settings.timeout.0.as_secs().max(1));
        Ok(SftpSessionHandle {
            ssh,
            sftp,
            timeout: settings.timeout.0,
        })
    }
}

#[async_trait]
impl Connect for SftpConnect {
    async fn connect(&self) -> Result<Box<dyn Session>, String> {
        let session = tokio::time::timeout(self.settings.connect_timeout.0, self.open())
            .await
            .map_err(|_| "connecting timed out".to_owned())??;
        Ok(Box::new(session))
    }

    fn location(&self) -> String {
        format!(
            "sftp://{}@{}:{}",
            self.settings.username, self.settings.host, self.settings.port
        )
    }
}

/// Registers the `sftp` source and destination.
pub fn register(registry: &mut Registry) {
    registry
        .add_source("sftp", |config: &SourceConfig| {
            let (poll, rest) = poll_settings(&config.settings, "sftp source")?;
            let connect = SftpConnect::new(parse(&rest, "sftp source")?)?;
            Ok(Arc::new(PollingSource::new(connect, poll, "sftp")) as Arc<dyn SourceConnector>)
        })
        .add_destination("sftp", |config: &DestinationConfig| {
            let (write, rest) = write_settings(&config.settings, "sftp destination")?;
            let connect = SftpConnect::new(parse(&rest, "sftp destination")?)?;
            Ok(Arc::new(RemoteDestination::new(connect, write)) as Arc<dyn DestinationConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_fingerprints() {
        assert_eq!(
            normalize_fingerprint(" SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s= "),
            "uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s"
        );
    }

    fn settings(extra: &str) -> Result<SftpConnect, EngineError> {
        let settings: SftpSettings = serde_json::from_str(&format!(
            r#"{{"host":"files.lab.test","username":"lab"{extra}}}"#
        ))
        .unwrap();
        SftpConnect::new(settings)
    }

    #[test]
    fn requires_host_key_verification_and_credentials() {
        assert!(settings(r#","password":"x""#).is_err());
        assert!(settings(r#","host_key_fingerprint":"SHA256:abc""#).is_err());
        assert!(
            settings(
                r#","password":"x","known_hosts_file":"k","host_key_fingerprint":"SHA256:abc""#
            )
            .is_err()
        );
        assert!(settings(r#","password":"x","password_env":"Y","known_hosts_file":"k""#).is_err());
        assert!(settings(r#","password_env":"Y","known_hosts_file":"k""#).is_ok());
        assert!(
            settings(r#","private_key_file":"id","host_key_fingerprint":"SHA256:abc""#).is_ok()
        );
    }
}
