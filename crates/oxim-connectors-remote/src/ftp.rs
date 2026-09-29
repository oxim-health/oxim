//! FTP and FTPS: a poller for a remote directory and an atomic file writer,
//! with suppaftp over rustls.
//!
//! Connection settings of type `ftp` (source and destination), in addition
//! to the shared polling and writing settings (see the crate README):
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `host` | required | Server name or address |
//! | `port` | `21` (`990` with `security: implicit`) | Server port |
//! | `security` | `explicit` | `explicit` (FTPS with `AUTH TLS`), `implicit` (FTPS on a TLS port) or `none` (plain FTP) |
//! | `tls` | system roots | TLS settings; see [`oxim_connectors::tls`] |
//! | `username` | `anonymous` | User name |
//! | `password_env` | none | Environment variable holding the password |
//! | `password` | none | The password inline (discouraged) |
//! | `passive` | `pasv` | Passive mode: `pasv` or `epsv` (extended passive, for IPv6 and some firewalls) |
//! | `connect_timeout` | `10s` | Limit for connecting, TLS and login |
//! | `timeout` | `60s` | Limit for each file operation |
//!
//! Plain FTP sends credentials and data unencrypted and must be chosen
//! explicitly with `security: none`. Data connections use passive mode; the
//! data address the server announces is replaced by the control
//! connection's address, which is what NAT gateways need. Directory
//! listings use `MLSD` and fall back to `LIST` for servers without it.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use oxim_connectors::tls::{ClientTlsSettings, client_config};
use oxim_core::config::DurationText;
use oxim_core::{
    DestinationConfig, DestinationConnector, EngineError, Registry, SourceConfig, SourceConnector,
    async_trait,
};
use serde::Deserialize;
use suppaftp::list::{File as Listed, ListParser};
use suppaftp::tokio::{AsyncRustlsConnector, AsyncRustlsFtpStream};
use suppaftp::types::FileType;
use suppaftp::{FtpError, Mode, Status};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::remote::{
    Connect, PollingSource, RemoteDestination, RemoteEntry, Session, poll_settings, write_settings,
};
use crate::util::{check_secret, parse, secret};

/// How the FTP connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FtpSecurity {
    /// FTPS: TLS negotiated with `AUTH TLS` on the normal port.
    #[default]
    Explicit,
    /// FTPS: TLS from the first byte, on a dedicated port.
    Implicit,
    /// Plain FTP without encryption.
    None,
}

/// The passive data connection command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Passive {
    /// `PASV`.
    #[default]
    Pasv,
    /// `EPSV`.
    Epsv,
}

fn default_username() -> String {
    "anonymous".to_owned()
}

fn default_connect_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(60))
}

/// Connection settings of the `ftp` connectors.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FtpSettings {
    /// Server name or address.
    pub host: String,
    /// Server port.
    #[serde(default)]
    pub port: Option<u16>,
    /// Connection protection.
    #[serde(default)]
    pub security: FtpSecurity,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// User name.
    #[serde(default = "default_username")]
    pub username: String,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// The password inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Passive mode command.
    #[serde(default)]
    pub passive: Passive,
    /// Limit for connecting, TLS and login.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: DurationText,
    /// Limit for each file operation.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
}

/// The FTP location and credentials, validated at deploy time.
#[derive(Clone)]
pub(crate) struct FtpConnect {
    settings: Arc<FtpSettings>,
    port: u16,
    tls: Option<(Arc<rustls::ClientConfig>, String)>,
}

impl std::fmt::Debug for FtpConnect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FtpConnect")
            .field("host", &self.settings.host)
            .field("port", &self.port)
            .field("security", &self.settings.security)
            .finish_non_exhaustive()
    }
}

impl FtpConnect {
    fn new(settings: FtpSettings) -> Result<Self, EngineError> {
        if settings.host.trim().is_empty() {
            return Err(EngineError::Config("ftp: host must not be empty".into()));
        }
        check_secret(
            settings.password.as_ref(),
            settings.password_env.as_ref(),
            "password",
            "ftp",
        )?;
        let port = settings.port.unwrap_or(match settings.security {
            FtpSecurity::Implicit => 990,
            _ => 21,
        });
        let tls = match settings.security {
            FtpSecurity::None => {
                if settings.tls.is_some() {
                    return Err(EngineError::Config(
                        "ftp: tls settings need security explicit or implicit".into(),
                    ));
                }
                None
            }
            FtpSecurity::Explicit | FtpSecurity::Implicit => {
                let tls = settings.tls.clone().unwrap_or_default();
                let target = format!("{}:{port}", settings.host);
                let name = oxim_connectors::tls::server_name(&tls, &target)?;
                Some((Arc::new(client_config(&tls)?), name.to_str().into_owned()))
            }
        };
        Ok(Self {
            settings: Arc::new(settings),
            port,
            tls,
        })
    }

    fn connector(&self) -> Option<(AsyncRustlsConnector, String)> {
        self.tls.as_ref().map(|(config, name)| {
            (
                AsyncRustlsConnector::from(suppaftp::tokio_rustls::TlsConnector::from(
                    config.clone(),
                )),
                name.clone(),
            )
        })
    }

    async fn open(&self) -> Result<FtpSession, String> {
        let settings = &self.settings;
        let address = (settings.host.as_str(), self.port);
        let mut ftp = match (settings.security, self.connector()) {
            (FtpSecurity::Implicit, Some((connector, name))) => {
                AsyncRustlsFtpStream::connect_secure_implicit(address, connector, &name)
                    .await
                    .map_err(|e| format!("cannot connect with implicit TLS: {e}"))?
            }
            (FtpSecurity::Explicit, Some((connector, name))) => {
                AsyncRustlsFtpStream::connect(address)
                    .await
                    .map_err(|e| format!("cannot connect: {e}"))?
                    .into_secure(connector, &name)
                    .await
                    .map_err(|e| format!("cannot start TLS (AUTH TLS): {e}"))?
            }
            _ => AsyncRustlsFtpStream::connect(address)
                .await
                .map_err(|e| format!("cannot connect: {e}"))?,
        };
        ftp.set_mode(match settings.passive {
            Passive::Pasv => Mode::Passive,
            Passive::Epsv => Mode::ExtendedPassive,
        });
        ftp.set_passive_nat_workaround(true);
        let password = secret(
            settings.password.as_ref(),
            settings.password_env.as_ref(),
            "password",
        )?
        .unwrap_or_default();
        ftp.login(settings.username.as_str(), password.as_str())
            .await
            .map_err(|e| format!("login as {} failed: {e}", settings.username))?;
        ftp.transfer_type(FileType::Binary)
            .await
            .map_err(|e| format!("cannot switch to binary mode: {e}"))?;
        Ok(FtpSession {
            ftp,
            mlsd: None,
            timeout: settings.timeout.0,
        })
    }
}

/// A logged-in FTP session.
struct FtpSession {
    ftp: AsyncRustlsFtpStream,
    /// Whether the server supports `MLSD`, once known.
    mlsd: Option<bool>,
    timeout: Duration,
}

fn not_found(error: &FtpError) -> bool {
    matches!(error, FtpError::UnexpectedResponse(response) if response.status == Status::FileUnavailable)
}

fn unsupported(error: &FtpError) -> bool {
    matches!(
        error,
        FtpError::UnexpectedResponse(response)
            if matches!(
                response.status,
                Status::CommandNotImplemented | Status::BadCommand | Status::BadArguments
            )
    )
}

fn seconds(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
}

fn entries(listed: Vec<Listed>) -> Vec<RemoteEntry> {
    listed
        .into_iter()
        .filter(Listed::is_file)
        .map(|file| RemoteEntry {
            name: file
                .name()
                .rsplit('/')
                .next()
                .unwrap_or(file.name())
                .to_owned(),
            size: file.size() as u64,
            modified: seconds(file.modified()),
        })
        .collect()
}

fn directory_argument(directory: &str) -> Option<&str> {
    (!directory.is_empty()).then_some(directory)
}

#[async_trait]
impl Session for FtpSession {
    async fn list(&mut self, directory: &str) -> Result<Vec<RemoteEntry>, String> {
        let timeout = self.timeout;
        let what = format!("listing {directory:?}");
        if self.mlsd != Some(false) {
            match tokio::time::timeout(timeout, self.ftp.mlsd(directory_argument(directory))).await
            {
                Err(_) => return Err(format!("{what} timed out")),
                Ok(Ok(lines)) => {
                    self.mlsd = Some(true);
                    return Ok(entries(
                        lines
                            .iter()
                            .filter_map(|line| ListParser::parse_mlsd(line).ok())
                            .collect(),
                    ));
                }
                Ok(Err(e)) if unsupported(&e) => self.mlsd = Some(false),
                Ok(Err(e)) => return Err(format!("{what}: {e}")),
            }
        }
        let lines = tokio::time::timeout(timeout, self.ftp.list(directory_argument(directory)))
            .await
            .map_err(|_| format!("{what} timed out"))?
            .map_err(|e| format!("{what}: {e}"))?;
        Ok(entries(
            lines
                .iter()
                .filter_map(|line| line.parse::<Listed>().ok())
                .collect(),
        ))
    }

    async fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let ftp = &mut self.ftp;
        let work = async {
            let mut stream = ftp.retr_as_stream(path).await?;
            let mut data = Vec::new();
            stream
                .read_to_end(&mut data)
                .await
                .map_err(FtpError::ConnectionError)?;
            stream.finish().await?;
            Ok(data)
        };
        tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| format!("reading {path} timed out"))?
            .map_err(|e: FtpError| format!("reading {path}: {e}"))
    }

    async fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        let ftp = &mut self.ftp;
        let work = async {
            let mut stream = ftp.put_with_stream(path).await?;
            stream
                .write_all(data)
                .await
                .map_err(FtpError::ConnectionError)?;
            stream.finish().await?;
            Ok(())
        };
        tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| format!("writing {path} timed out"))?
            .map_err(|e: FtpError| format!("writing {path}: {e}"))
    }

    async fn remove(&mut self, path: &str) -> Result<(), String> {
        let timeout = self.timeout;
        tokio::time::timeout(timeout, self.ftp.rm(path))
            .await
            .map_err(|_| format!("removing {path} timed out"))?
            .map_err(|e| format!("removing {path}: {e}"))
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let timeout = self.timeout;
        tokio::time::timeout(timeout, self.ftp.rename(from, to))
            .await
            .map_err(|_| format!("renaming {from} timed out"))?
            .map_err(|e| format!("renaming {from} to {to}: {e}"))
    }

    async fn exists(&mut self, path: &str) -> Result<bool, String> {
        let timeout = self.timeout;
        match tokio::time::timeout(timeout, self.ftp.size(path)).await {
            Err(_) => Err(format!("checking {path} timed out")),
            Ok(Ok(_)) => Ok(true),
            Ok(Err(e)) if not_found(&e) => Ok(false),
            Ok(Err(e)) => Err(format!("checking {path}: {e}")),
        }
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
            // An existing directory answers 550; a real failure shows when
            // the file is written.
            let timeout = self.timeout;
            let _ = tokio::time::timeout(timeout, self.ftp.mkdir(&current)).await;
        }
        Ok(())
    }

    async fn close(&mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(2), self.ftp.quit()).await;
    }
}

#[async_trait]
impl Connect for FtpConnect {
    async fn connect(&self) -> Result<Box<dyn Session>, String> {
        let session = tokio::time::timeout(self.settings.connect_timeout.0, self.open())
            .await
            .map_err(|_| "connecting timed out".to_owned())??;
        Ok(Box::new(session))
    }

    fn location(&self) -> String {
        let scheme = match self.settings.security {
            FtpSecurity::None => "ftp",
            _ => "ftps",
        };
        format!(
            "{scheme}://{}@{}:{}",
            self.settings.username, self.settings.host, self.port
        )
    }
}

/// Registers the `ftp` source and destination.
pub fn register(registry: &mut Registry) {
    registry
        .add_source("ftp", |config: &SourceConfig| {
            let (poll, rest) = poll_settings(&config.settings, "ftp source")?;
            let connect = FtpConnect::new(parse(&rest, "ftp source")?)?;
            Ok(Arc::new(PollingSource::new(connect, poll, "ftp")) as Arc<dyn SourceConnector>)
        })
        .add_destination("ftp", |config: &DestinationConfig| {
            let (write, rest) = write_settings(&config.settings, "ftp destination")?;
            let connect = FtpConnect::new(parse(&rest, "ftp destination")?)?;
            Ok(Arc::new(RemoteDestination::new(connect, write)) as Arc<dyn DestinationConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect(extra: &str) -> Result<FtpConnect, EngineError> {
        FtpConnect::new(
            serde_json::from_str(&format!(r#"{{"host":"files.lab.test"{extra}}}"#)).unwrap(),
        )
    }

    #[test]
    fn chooses_ports_and_checks_settings() {
        assert_eq!(connect("").unwrap().port, 21);
        assert_eq!(connect(r#","security":"implicit""#).unwrap().port, 990);
        assert_eq!(
            connect(r#","security":"none","port":2121"#).unwrap().port,
            2121
        );
        assert!(connect(r#","security":"none","tls":{}"#).is_err());
        assert!(connect(r#","password":"a","password_env":"B""#).is_err());
        assert_eq!(
            connect(r#","security":"none""#).unwrap().location(),
            "ftp://anonymous@files.lab.test:21"
        );
    }

    #[test]
    fn parses_listings() {
        let posix = "-rw-r--r--    1 1000     1000          123 Sep 29 12:00 result.hl7"
            .parse::<Listed>()
            .unwrap();
        let directory = "drwxr-xr-x    2 1000     1000         4096 Sep 29 12:00 done"
            .parse::<Listed>()
            .unwrap();
        let mlsd =
            ListParser::parse_mlsd("type=file;size=42;modify=20260929120000; order-7.hl7").unwrap();
        let listed = entries(vec![posix, directory, mlsd]);
        assert_eq!(listed.len(), 2);
        assert_eq!(
            (listed[0].name.as_str(), listed[0].size),
            ("result.hl7", 123)
        );
        assert_eq!(
            (listed[1].name.as_str(), listed[1].size),
            ("order-7.hl7", 42)
        );
    }
}
