//! The engine configuration file, `oxim.yaml`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oxim_core::config::DurationText;
use serde::{Deserialize, Serialize};

use crate::CliResult;

/// Everything in `oxim.yaml`. Relative paths are resolved against the
/// directory that contains the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    /// Where the message database lives.
    #[serde(default = "data_dir_default")]
    pub(crate) data_dir: PathBuf,
    /// Where channel files (`*.yaml`) live.
    #[serde(default = "channels_dir_default")]
    pub(crate) channels_dir: PathBuf,
    /// Where code tables live; channel steps resolve table paths here.
    #[serde(default = "tables_dir_default")]
    pub(crate) tables_dir: PathBuf,
    /// Where script files live; `script` steps resolve `file` here.
    #[serde(default = "scripts_dir_default")]
    pub(crate) scripts_dir: PathBuf,
    /// Logging.
    #[serde(default)]
    pub(crate) log: LogSettings,
    /// Engine tuning.
    #[serde(default)]
    pub(crate) engine: EngineSettings,
    /// How long messages are kept.
    #[serde(default)]
    pub(crate) retention: RetentionSettings,
    /// Automatic redeployment of changed channel files.
    #[serde(default)]
    pub(crate) reload: ReloadSettings,
    /// The web server: REST API, metrics and web UI.
    #[serde(default)]
    pub(crate) server: ServerSettings,
    /// Alert rules and notification targets.
    #[serde(default)]
    pub(crate) alerts: oxim_alert::AlertSettings,
    /// Backups.
    #[serde(default)]
    pub(crate) backups: BackupSettings,
    /// The file these settings were read from.
    #[serde(skip)]
    pub(crate) config_file: Option<PathBuf>,
}

/// Backups (`oxim backup`, the web UI and the daily schedule).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackupSettings {
    /// Where backups go; `backups` in the data directory by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dir: Option<PathBuf>,
    /// How many backups in that directory are kept.
    #[serde(default = "backups_keep_default")]
    pub(crate) keep: usize,
    /// Take a backup every day at this time (`HH:MM`) while OXIM runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) schedule: Option<String>,
    /// UTC offset of `schedule` in minutes (`180` for UTC+3).
    #[serde(default)]
    pub(crate) utc_offset: i16,
}

fn backups_keep_default() -> usize {
    7
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self {
            dir: None,
            keep: backups_keep_default(),
            schedule: None,
            utc_offset: 0,
        }
    }
}

fn data_dir_default() -> PathBuf {
    PathBuf::from("data")
}

fn channels_dir_default() -> PathBuf {
    PathBuf::from("channels")
}

fn tables_dir_default() -> PathBuf {
    PathBuf::from("tables")
}

fn scripts_dir_default() -> PathBuf {
    PathBuf::from("scripts")
}

/// Log output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LogSettings {
    /// Filter such as `info` or `info,oxim_connectors=debug`. The
    /// `OXIM_LOG` environment variable overrides it.
    #[serde(default = "log_level_default")]
    pub(crate) level: String,
    /// `text` or `json`.
    #[serde(default)]
    pub(crate) format: LogFormat,
    /// Write daily log files to this directory instead of standard output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) directory: Option<PathBuf>,
}

fn log_level_default() -> String {
    "info".into()
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            level: log_level_default(),
            format: LogFormat::default(),
            directory: None,
        }
    }
}

/// Log line format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LogFormat {
    /// Human-readable lines.
    #[default]
    Text,
    /// One JSON object per line, for log collectors.
    Json,
}

/// Engine tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineSettings {
    /// Messages waiting for processing per channel before sources slow
    /// down.
    #[serde(default = "processing_queue_default")]
    pub(crate) processing_queue: usize,
    /// How long stopping a channel waits for in-flight work.
    #[serde(default = "shutdown_grace_default")]
    pub(crate) shutdown_grace: DurationText,
    /// How often idle destination workers check their queues.
    #[serde(default = "idle_poll_default")]
    pub(crate) idle_poll: DurationText,
}

fn processing_queue_default() -> usize {
    1024
}

fn shutdown_grace_default() -> DurationText {
    DurationText(Duration::from_secs(30))
}

fn idle_poll_default() -> DurationText {
    DurationText(Duration::from_secs(30))
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            processing_queue: processing_queue_default(),
            shutdown_grace: shutdown_grace_default(),
            idle_poll: idle_poll_default(),
        }
    }
}

/// Retention of completed messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetentionSettings {
    /// Delete message contents this long after receipt, keeping the record.
    #[serde(default = "contents_after_default")]
    pub(crate) contents_after: Option<DurationText>,
    /// Delete messages entirely this long after receipt.
    #[serde(default = "messages_after_default")]
    pub(crate) messages_after: Option<DurationText>,
    /// Delete cached lab orders (see `oxim-lab`) not changed for this long.
    #[serde(default = "orders_after_default")]
    pub(crate) orders_after: Option<DurationText>,
    /// How often retention runs.
    #[serde(default = "retention_interval_default")]
    pub(crate) interval: DurationText,
}

fn orders_after_default() -> Option<DurationText> {
    Some(DurationText(Duration::from_secs(90 * 86_400)))
}

fn contents_after_default() -> Option<DurationText> {
    Some(DurationText(Duration::from_secs(90 * 86_400)))
}

fn messages_after_default() -> Option<DurationText> {
    Some(DurationText(Duration::from_secs(365 * 86_400)))
}

fn retention_interval_default() -> DurationText {
    DurationText(Duration::from_secs(3600))
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            contents_after: contents_after_default(),
            messages_after: messages_after_default(),
            orders_after: orders_after_default(),
            interval: retention_interval_default(),
        }
    }
}

/// Watching the channel directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReloadSettings {
    /// Redeploy channels whose files change, deploy new files and undeploy
    /// removed ones while running.
    #[serde(default = "reload_enabled_default")]
    pub(crate) enabled: bool,
    /// How often to check the directory.
    #[serde(default = "reload_interval_default")]
    pub(crate) interval: DurationText,
}

fn reload_enabled_default() -> bool {
    true
}

fn reload_interval_default() -> DurationText {
    DurationText(Duration::from_secs(5))
}

impl Default for ReloadSettings {
    fn default() -> Self {
        Self {
            enabled: reload_enabled_default(),
            interval: reload_interval_default(),
        }
    }
}

/// The web server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerSettings {
    /// Whether `oxim run` starts the web server. It only starts when at
    /// least one user exists (`oxim users create-admin`).
    #[serde(default = "server_enabled_default")]
    pub(crate) enabled: bool,
    /// Address and port to listen on. Only the local machine can connect
    /// with the default; use `0.0.0.0:8443` with TLS for remote access.
    #[serde(default = "listen_default")]
    pub(crate) listen: SocketAddr,
    /// Serve HTTPS with this certificate and key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tls: Option<TlsSettings>,
    /// Directory with the web UI's files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ui_dir: Option<PathBuf>,
    /// A session ends after this long without requests.
    #[serde(default = "session_idle_default")]
    pub(crate) session_idle: DurationText,
    /// A session ends this long after login regardless of activity.
    #[serde(default = "session_max_default")]
    pub(crate) session_max: DurationText,
}

/// TLS certificate and key files (PEM).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TlsSettings {
    /// Certificate chain, leaf first.
    pub(crate) cert: PathBuf,
    /// Private key.
    pub(crate) key: PathBuf,
}

fn server_enabled_default() -> bool {
    true
}

fn listen_default() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8080))
}

fn session_idle_default() -> DurationText {
    DurationText(Duration::from_secs(30 * 60))
}

fn session_max_default() -> DurationText {
    DurationText(Duration::from_secs(12 * 3600))
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            enabled: server_enabled_default(),
            listen: listen_default(),
            tls: None,
            ui_dir: None,
            session_idle: session_idle_default(),
            session_max: session_max_default(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            data_dir: data_dir_default(),
            channels_dir: channels_dir_default(),
            tables_dir: tables_dir_default(),
            scripts_dir: scripts_dir_default(),
            log: LogSettings::default(),
            engine: EngineSettings::default(),
            retention: RetentionSettings::default(),
            reload: ReloadSettings::default(),
            server: ServerSettings::default(),
            alerts: oxim_alert::AlertSettings::default(),
            backups: BackupSettings::default(),
            config_file: None,
        }
    }
}

impl Settings {
    /// Reads `path` and resolves relative paths against its directory.
    pub(crate) fn load(path: &Path) -> CliResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "cannot read {}: {e}\nhint: run `oxim init` to create a starter configuration",
                path.display()
            )
        })?;
        let mut settings: Self = serde_saphyr::from_str(&text)
            .map_err(|e| format!("invalid {}: {e}", path.display()))?;
        if let Some(schedule) = &settings.backups.schedule {
            crate::backups::parse_schedule(schedule)
                .map_err(|e| format!("invalid {}: {e}", path.display()))?;
        }
        let canonical = path.canonicalize().ok().map(simplified);
        let base = canonical
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        settings.config_file = canonical;
        Ok(settings.resolved(&base))
    }

    fn resolved(mut self, base: &Path) -> Self {
        let resolve = |path: &mut PathBuf| {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        };
        resolve(&mut self.data_dir);
        resolve(&mut self.channels_dir);
        resolve(&mut self.tables_dir);
        resolve(&mut self.scripts_dir);
        if let Some(directory) = &mut self.log.directory {
            resolve(directory);
        }
        if let Some(tls) = &mut self.server.tls {
            resolve(&mut tls.cert);
            resolve(&mut tls.key);
        }
        if let Some(directory) = &mut self.server.ui_dir {
            resolve(directory);
        }
        if let Some(directory) = &mut self.backups.dir {
            resolve(directory);
        }
        for rule in &mut self.alerts.rules {
            match &mut rule.condition {
                oxim_alert::Condition::DiskSpace {
                    path: Some(path), ..
                } => resolve(path),
                oxim_alert::Condition::CertificateExpiry { files, .. } => {
                    files.iter_mut().for_each(resolve);
                }
                _ => {}
            }
        }
        for target in &mut self.alerts.targets {
            if let oxim_alert::TargetKind::Webhook {
                ca_file: Some(path),
                ..
            } = &mut target.kind
            {
                resolve(path);
            }
        }
        self
    }

    /// The message database file.
    pub(crate) fn database_path(&self) -> PathBuf {
        self.data_dir.join("oxim.db")
    }

    /// Where backups go.
    pub(crate) fn backups_dir(&self) -> PathBuf {
        self.backups
            .dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("backups"))
    }

    /// The channel version history.
    pub(crate) fn history_path(&self) -> PathBuf {
        self.data_dir.join("history.db")
    }

    /// The lab order cache file.
    pub(crate) fn orders_path(&self) -> PathBuf {
        self.data_dir.join("orders.db")
    }

    /// The database of users, sessions and API tokens.
    pub(crate) fn auth_database_path(&self) -> PathBuf {
        self.data_dir.join("auth.db")
    }
}

/// Removes the Windows verbatim prefix (`\\?\`) that `canonicalize` adds to
/// local paths, so paths print the way users type them.
fn simplified(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_and_paths_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oxim.yaml");
        std::fs::write(
            &path,
            "log:\n  level: debug\nretention:\n  contents_after: 30d\n",
        )
        .unwrap();
        let settings = Settings::load(&path).unwrap();
        assert_eq!(settings.log.level, "debug");
        assert_eq!(
            settings.retention.contents_after,
            Some(DurationText(Duration::from_secs(30 * 86_400)))
        );
        assert!(settings.data_dir.is_absolute());
        assert!(
            settings.database_path().ends_with("data/oxim.db")
                || settings.database_path().ends_with("data\\oxim.db")
        );
        assert!(settings.reload.enabled);
        assert!(settings.server.enabled);
        assert_eq!(settings.server.listen.to_string(), "127.0.0.1:8080");
        assert_eq!(settings.server.session_idle.0, Duration::from_secs(1800));
    }

    #[test]
    fn reads_server_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oxim.yaml");
        std::fs::write(
            &path,
            "server:\n  listen: 0.0.0.0:8443\n  tls: {cert: tls/cert.pem, key: tls/key.pem}\n  session_idle: 15m\n",
        )
        .unwrap();
        let settings = Settings::load(&path).unwrap();
        assert_eq!(settings.server.listen.port(), 8443);
        let tls = settings.server.tls.unwrap();
        assert!(tls.cert.is_absolute());
        assert!(tls.key.ends_with("tls/key.pem") || tls.key.ends_with("tls\\key.pem"));
        assert_eq!(settings.server.session_idle.0, Duration::from_secs(900));
    }

    #[test]
    fn reads_alert_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oxim.yaml");
        std::fs::write(
            &path,
            "alerts:
  interval: 30s
  targets:
    - {id: noc, type: syslog, address: '10.0.0.5:514', protocol: tcp, min_severity: critical}
    - {id: ops, type: teams, url: 'https://example.org/hook'}
  rules:
    - {id: lis, kind: queue_depth, destination: lis, above: 100, for: 5m, targets: [ops]}
    - {id: disk, kind: disk_space, below: 10%}
    - {id: space, kind: disk_space, path: spool, below: 5GiB}
    - {id: certs, kind: certificate_expiry, files: [tls/lab.pem], within: 30d}
",
        )
        .unwrap();
        let settings = Settings::load(&path).unwrap();
        let alerts = &settings.alerts;
        alerts.validate().unwrap();
        assert_eq!(alerts.interval.0, Duration::from_secs(30));
        assert_eq!(alerts.rules.len(), 4);
        assert_eq!(alerts.rules[0].hold.unwrap().0, Duration::from_secs(300));
        assert_eq!(
            alerts.targets[0].min_severity,
            Some(oxim_alert::Severity::Critical)
        );
        match &alerts.rules[2].condition {
            oxim_alert::Condition::DiskSpace {
                path: Some(path),
                below,
            } => {
                assert!(path.is_absolute());
                assert_eq!(*below, oxim_alert::Space::Bytes(5 << 30));
            }
            other => panic!("unexpected {other:?}"),
        }
        std::fs::write(
            &path,
            "alerts:\n  rules:\n    - {id: x, kind: queue_depth, above: 1, typo: 2}\n",
        )
        .unwrap();
        assert!(Settings::load(&path).is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oxim.yaml");
        std::fs::write(&path, "data_directory: x\n").unwrap();
        assert!(Settings::load(&path).is_err());
        assert!(Settings::load(&dir.path().join("missing.yaml")).is_err());
    }
}
