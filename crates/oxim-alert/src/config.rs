//! Alert rules and notification targets, as written in `oxim.yaml`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use oxim_core::config::DurationText;
use oxim_core::{EngineError, Settings};
use serde::{Deserialize, Serialize};

fn default_interval() -> DurationText {
    DurationText(Duration::from_secs(60))
}

fn default_repeat() -> DurationText {
    DurationText(Duration::from_secs(3600))
}

/// The `alerts` section of `oxim.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertSettings {
    /// How often rules are evaluated.
    #[serde(default = "default_interval")]
    pub interval: DurationText,
    /// How often a firing alert is notified again; `0s` notifies once.
    #[serde(default = "default_repeat")]
    pub repeat: DurationText,
    /// Where notifications go.
    #[serde(default)]
    pub targets: Vec<TargetConfig>,
    /// What is watched.
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
}

impl Default for AlertSettings {
    fn default() -> Self {
        Self {
            interval: default_interval(),
            repeat: default_repeat(),
            targets: Vec::new(),
            rules: Vec::new(),
        }
    }
}

/// How serious an alert is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth a look.
    Info,
    /// Needs attention soon.
    #[default]
    Warning,
    /// Needs attention now.
    Critical,
}

impl Severity {
    /// The configuration name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// One rule. Unknown settings are rejected by the rule kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleConfig {
    /// Unique rule identifier.
    pub id: String,
    /// What the rule watches.
    #[serde(flatten)]
    pub condition: Condition,
    /// How long the condition must hold before the alert fires.
    #[serde(default, rename = "for")]
    pub hold: Option<DurationText>,
    /// Severity of the alert.
    #[serde(default)]
    pub severity: Severity,
    /// Target identifiers; all targets when empty.
    #[serde(default)]
    pub targets: Vec<String>,
}

/// A size limit: bytes (`5GiB`, `500MB`, `1024`) or a percentage (`10%`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "serde_json::Value", into = "String")]
pub enum Space {
    /// Free bytes.
    Bytes(u64),
    /// Free share of the volume, in percent.
    Percent(f64),
}

impl TryFrom<serde_json::Value> for Space {
    type Error = String;

    fn try_from(value: serde_json::Value) -> Result<Self, String> {
        match value {
            serde_json::Value::Number(n) => n
                .as_u64()
                .map(Self::Bytes)
                .ok_or_else(|| format!("invalid size {n}")),
            serde_json::Value::String(text) => Self::parse(&text),
            other => Err(format!("invalid size {other}")),
        }
    }
}

impl From<Space> for String {
    fn from(space: Space) -> Self {
        match space {
            Space::Bytes(bytes) => bytes.to_string(),
            Space::Percent(percent) => format!("{percent}%"),
        }
    }
}

impl Space {
    fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if let Some(percent) = text.strip_suffix('%') {
            let value: f64 = percent
                .trim()
                .parse()
                .map_err(|_| format!("invalid percentage {text:?}"))?;
            if !(0.0..=100.0).contains(&value) {
                return Err(format!("percentage out of range: {text:?}"));
            }
            return Ok(Self::Percent(value));
        }
        let split = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        let (digits, unit) = text.split_at(split);
        let number: u64 = digits
            .parse()
            .map_err(|_| format!("invalid size {text:?}"))?;
        let factor: u64 = match unit.trim() {
            "" | "B" => 1,
            "KB" => 1000,
            "MB" => 1000 * 1000,
            "GB" => 1000 * 1000 * 1000,
            "TB" => 1000 * 1000 * 1000 * 1000,
            "KiB" => 1 << 10,
            "MiB" => 1 << 20,
            "GiB" => 1 << 30,
            "TiB" => 1 << 40,
            other => return Err(format!("unknown size unit {other:?}")),
        };
        number
            .checked_mul(factor)
            .map(Self::Bytes)
            .ok_or_else(|| format!("size too large: {text:?}"))
    }
}

/// What a rule watches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    /// Messages waiting in destination queues (queued, sending and
    /// retrying).
    QueueDepth {
        /// Only this channel.
        #[serde(default)]
        channel: Option<String>,
        /// Only this destination.
        #[serde(default)]
        destination: Option<String>,
        /// Fires when the depth exceeds this.
        above: u64,
    },
    /// The oldest message still waiting in a destination queue.
    QueueAge {
        /// Only this channel.
        #[serde(default)]
        channel: Option<String>,
        /// Only this destination.
        #[serde(default)]
        destination: Option<String>,
        /// Fires when a message waits longer than this.
        older_than: DurationText,
    },
    /// Deliveries that were given up and wait for an operator.
    FailedDeliveries {
        /// Only this channel.
        #[serde(default)]
        channel: Option<String>,
        /// Only this destination.
        #[serde(default)]
        destination: Option<String>,
        /// Fires when there are more than this many.
        #[serde(default)]
        above: u64,
    },
    /// Messages that ended in error.
    ErrorRate {
        /// Only this channel.
        #[serde(default)]
        channel: Option<String>,
        /// Fires when more messages than this ended in error...
        above: u64,
        /// ...among those received within this window.
        within: DurationText,
    },
    /// Devices that sent nothing for longer than their silence window
    /// (`track-device` with `silence_after`).
    DeviceSilence {
        /// Only this device.
        #[serde(default)]
        device: Option<String>,
    },
    /// Free space on the volume of the data directory (or `path`).
    DiskSpace {
        /// A directory on the watched volume.
        #[serde(default)]
        path: Option<PathBuf>,
        /// Fires when less than this is free.
        below: Space,
    },
    /// Certificates that expire soon.
    CertificateExpiry {
        /// PEM certificate files; the server certificate is always checked.
        #[serde(default)]
        files: Vec<PathBuf>,
        /// Fires this long before a certificate expires.
        within: DurationText,
    },
}

impl Condition {
    /// The configuration name of the rule kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::QueueDepth { .. } => "queue_depth",
            Self::QueueAge { .. } => "queue_age",
            Self::FailedDeliveries { .. } => "failed_deliveries",
            Self::ErrorRate { .. } => "error_rate",
            Self::DeviceSilence { .. } => "device_silence",
            Self::DiskSpace { .. } => "disk_space",
            Self::CertificateExpiry { .. } => "certificate_expiry",
        }
    }
}

/// A notification target. Unknown settings are rejected by the target type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetConfig {
    /// Unique target identifier.
    pub id: String,
    /// How notifications are sent.
    #[serde(flatten)]
    pub kind: TargetKind,
    /// Only alerts at least this severe.
    #[serde(default)]
    pub min_severity: Option<Severity>,
}

/// How a target sends notifications.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TargetKind {
    /// Writes to the OXIM log only.
    Log,
    /// A JSON POST to a URL, through the `http` destination connector.
    Webhook {
        /// The URL.
        url: String,
        /// Extra headers.
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
        /// Extra trusted certificate authorities.
        #[serde(default)]
        ca_file: Option<PathBuf>,
    },
    /// A Microsoft Teams incoming webhook or workflow URL.
    Teams {
        /// The webhook URL.
        url: String,
    },
    /// A Slack incoming webhook URL.
    Slack {
        /// The webhook URL.
        url: String,
    },
    /// Email through the `smtp` destination connector, with its settings.
    Email {
        /// Settings of the `smtp` destination (server, sender, recipients).
        settings: Settings,
    },
    /// Any registered destination connector type, receiving the
    /// notification as text or JSON.
    Destination {
        /// The destination connector type, for example `mllp` or `file`.
        connector: String,
        /// Its settings.
        #[serde(default)]
        settings: Settings,
        /// `text` (default) or `json`.
        #[serde(default)]
        format: Format,
    },
    /// Syslog (RFC 5424) over UDP or TCP.
    Syslog {
        /// Collector address, for example `10.0.0.5:514`.
        address: String,
        /// `udp` (default) or `tcp`.
        #[serde(default)]
        protocol: Protocol,
        /// Facility name, `local0` by default.
        #[serde(default = "default_facility")]
        facility: String,
        /// The APP-NAME field.
        #[serde(default = "default_app_name")]
        app_name: String,
    },
    /// SNMPv2c traps.
    Snmp {
        /// Manager address, for example `10.0.0.6:162`.
        address: String,
        /// Environment variable holding the community (default `public`
        /// when unset).
        #[serde(default)]
        community_env: Option<String>,
        /// The notification OID; the alert fields are sent below it.
        #[serde(default = "default_trap_oid")]
        trap_oid: String,
    },
}

fn default_facility() -> String {
    "local0".to_owned()
}

fn default_app_name() -> String {
    "oxim".to_owned()
}

/// The default notification OID, in the IANA experimental arc; sites
/// should set an OID from their own enterprise arc.
pub const DEFAULT_TRAP_OID: &str = "1.3.6.1.3.8469.1";

fn default_trap_oid() -> String {
    DEFAULT_TRAP_OID.to_owned()
}

/// Transport for syslog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// One datagram per message.
    #[default]
    Udp,
    /// Octet-counted frames (RFC 6587).
    Tcp,
}

/// Payload format for generic destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// One line of text.
    #[default]
    Text,
    /// A JSON object.
    Json,
}

impl AlertSettings {
    /// Checks identifiers and references.
    pub fn validate(&self) -> Result<(), EngineError> {
        let error = |message: String| EngineError::Config(format!("alerts: {message}"));
        let mut targets = BTreeSet::new();
        for target in &self.targets {
            if target.id.trim().is_empty() || !targets.insert(target.id.as_str()) {
                return Err(error(format!(
                    "target identifiers must be unique and not empty ({:?})",
                    target.id
                )));
            }
        }
        let mut rules = BTreeSet::new();
        for rule in &self.rules {
            if rule.id.trim().is_empty() || !rules.insert(rule.id.as_str()) {
                return Err(error(format!(
                    "rule identifiers must be unique and not empty ({:?})",
                    rule.id
                )));
            }
            for target in &rule.targets {
                if !targets.contains(target.as_str()) {
                    return Err(error(format!(
                        "rule {:?} names the unknown target {target:?}",
                        rule.id
                    )));
                }
            }
        }
        if self.interval.0 < Duration::from_secs(1) {
            return Err(error("interval must be at least 1s".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rules_and_targets() {
        let settings: AlertSettings = serde_json::from_value(serde_json::json!({
            "interval": "30s",
            "targets": [
                {"id": "ops", "type": "webhook", "url": "https://ops.example.org/hook"},
                {"id": "noc", "type": "syslog", "address": "10.0.0.5:514", "protocol": "tcp"},
                {"id": "log", "type": "log", "min_severity": "critical"}
            ],
            "rules": [
                {"id": "lis", "kind": "queue_depth", "destination": "lis", "above": 100, "for": "5m", "targets": ["ops"]},
                {"id": "disk", "kind": "disk_space", "below": "10%", "severity": "critical"},
                {"id": "space", "kind": "disk_space", "below": "5GiB"},
                {"id": "errors", "kind": "error_rate", "above": 5, "within": "15m"}
            ]
        }))
        .unwrap();
        settings.validate().unwrap();
        assert_eq!(settings.rules[0].hold.unwrap().0, Duration::from_secs(300));
        assert_eq!(
            settings.rules[1].condition,
            Condition::DiskSpace {
                path: None,
                below: Space::Percent(10.0)
            }
        );
        assert_eq!(
            settings.rules[2].condition,
            Condition::DiskSpace {
                path: None,
                below: Space::Bytes(5 << 30)
            }
        );
        assert_eq!(settings.targets[2].min_severity, Some(Severity::Critical));
    }

    #[test]
    fn rejects_bad_references_and_values() {
        let unknown = serde_json::json!({
            "rules": [{"id": "a", "kind": "device_silence", "targets": ["nowhere"]}]
        });
        let settings: AlertSettings = serde_json::from_value(unknown).unwrap();
        assert!(settings.validate().is_err());
        for bad in [
            serde_json::json!({"rules": [{"id": "a", "kind": "disk_space", "below": "110%"}]}),
            serde_json::json!({"rules": [{"id": "a", "kind": "disk_space", "below": "5 parsecs"}]}),
            serde_json::json!({"rules": [{"id": "a", "kind": "sometimes"}]}),
            serde_json::json!({"targets": [{"id": "a", "type": "pager"}]}),
        ] {
            assert!(
                serde_json::from_value::<AlertSettings>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }
}
