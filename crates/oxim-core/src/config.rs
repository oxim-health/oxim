//! Channel configuration files.
//!
//! Every channel is one YAML file (ADR 0008). The engine validates the file
//! when it is deployed; component-specific settings are passed to the
//! registered connector and step factories.
//!
//! ```yaml
//! id: chemistry
//! name: Chemistry analyzer to LIS
//! source:
//!   type: astm-tcp
//!   data_type: astm
//!   normalize: true
//!   settings:
//!     listen: 0.0.0.0:5100
//! destinations:
//!   - id: lis
//!     type: mllp
//!     encoder:
//!       type: hl7v2-oru-r01
//!     queue:
//!       ordering: strict
//!       retry: { initial_delay: 5s, max_delay: 5m, multiplier: 2, max_attempts: 100 }
//!     settings:
//!       target: 10.0.0.20:2575
//! ```

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use oxim_model::{ChannelId, ConnectorId, DataType};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::EngineError;

/// Free-form settings of a connector or step, validated by its factory.
pub type Settings = serde_json::Map<String, serde_json::Value>;

/// One channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    /// Unique channel identifier.
    pub id: ChannelId,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Longer description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the channel is deployed when the engine starts.
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    /// Where messages come from.
    pub source: SourceConfig,
    /// Filters every message must pass; a rejected message is stored as
    /// filtered and not sent anywhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<StepConfig>,
    /// Transformers applied to every message before the destinations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transformers: Vec<StepConfig>,
    /// Where messages go.
    #[serde(default)]
    pub destinations: Vec<DestinationConfig>,
}

fn enabled_default() -> bool {
    true
}

fn source_id_default() -> ConnectorId {
    match ConnectorId::new("source") {
        Ok(id) => id,
        Err(_) => unreachable!("\"source\" is a valid connector identifier"),
    }
}

/// The source connector of a channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    /// Connector identifier, `source` by default.
    #[serde(default = "source_id_default")]
    pub id: ConnectorId,
    /// Connector type, for example `mllp` or `astm-tcp`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Data type of the received messages.
    pub data_type: DataType,
    /// Options for generic data types (JSON, XML, delimited, fixed width).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Settings>,
    /// Whether to map messages to the normalized clinical model.
    #[serde(default)]
    pub normalize: bool,
    /// Connector-specific settings.
    #[serde(default, skip_serializing_if = "Settings::is_empty")]
    pub settings: Settings,
}

/// One destination of a channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationConfig {
    /// Destination identifier, unique within the channel.
    pub id: ConnectorId,
    /// Connector type, for example `mllp` or `file`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Filters for this destination only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<StepConfig>,
    /// Transformers for this destination only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transformers: Vec<StepConfig>,
    /// How to produce the bytes to send. Without an encoder the (possibly
    /// transformed) source document is sent as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder: Option<StepConfig>,
    /// Queue behavior.
    #[serde(default)]
    pub queue: QueueConfig,
    /// Connector-specific settings.
    #[serde(default, skip_serializing_if = "Settings::is_empty")]
    pub settings: Settings,
}

/// A filter, transformer or encoder: a registered step type plus its
/// settings, written inline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepConfig {
    /// Step type, for example `path-equals`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Step settings.
    #[serde(flatten)]
    pub settings: Settings,
}

impl StepConfig {
    /// A step without settings.
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            settings: Settings::new(),
        }
    }

    /// Adds a text setting.
    pub fn with(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.settings.insert(key.to_owned(), value.into());
        self
    }

    /// A required text setting.
    pub fn text(&self, key: &str) -> Result<&str, EngineError> {
        self.settings
            .get(key)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                EngineError::Config(format!("step {:?} needs a text setting {key:?}", self.kind))
            })
    }
}

/// Queue behavior of a destination.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueConfig {
    /// Delivery order.
    #[serde(default)]
    pub ordering: Ordering,
    /// When to retry failed deliveries.
    #[serde(default)]
    pub retry: RetryPolicy,
}

/// Delivery order of a destination queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ordering {
    /// Strict first-in, first-out.
    #[default]
    Strict,
    /// Messages waiting for a retry do not block newer ones.
    BestEffort,
}

impl From<Ordering> for oxim_store::QueueOrdering {
    fn from(ordering: Ordering) -> Self {
        match ordering {
            Ordering::Strict => Self::Strict,
            Ordering::BestEffort => Self::BestEffort,
        }
    }
}

/// Exponential backoff between delivery attempts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    /// Delay after the first failure.
    #[serde(default = "initial_delay_default")]
    pub initial_delay: DurationText,
    /// Upper bound of the delay.
    #[serde(default = "max_delay_default")]
    pub max_delay: DurationText,
    /// Factor applied to the delay after each failure.
    #[serde(default = "multiplier_default")]
    pub multiplier: f64,
    /// Give up after this many attempts; retry forever when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
}

fn initial_delay_default() -> DurationText {
    DurationText(Duration::from_secs(5))
}

fn max_delay_default() -> DurationText {
    DurationText(Duration::from_secs(300))
}

fn multiplier_default() -> f64 {
    2.0
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            initial_delay: initial_delay_default(),
            max_delay: max_delay_default(),
            multiplier: multiplier_default(),
            max_attempts: None,
        }
    }
}

impl RetryPolicy {
    /// The delay before the next attempt after `attempts` failed attempts,
    /// or `None` when the policy gives up.
    pub fn delay_after(&self, attempts: u32) -> Option<Duration> {
        if self.max_attempts.is_some_and(|max| attempts >= max) {
            return None;
        }
        let exponent = i32::try_from(attempts.saturating_sub(1)).unwrap_or(i32::MAX);
        let factor = self.multiplier.max(1.0).powi(exponent);
        let delay = self.initial_delay.0.as_secs_f64() * factor;
        let capped = delay.min(self.max_delay.0.as_secs_f64()).max(0.0);
        Some(Duration::from_secs_f64(capped))
    }

    fn validate(&self) -> Result<(), String> {
        if !self.multiplier.is_finite() || self.multiplier < 1.0 {
            return Err("retry multiplier must be at least 1".into());
        }
        if self.initial_delay.0 > self.max_delay.0 {
            return Err("retry initial_delay must not exceed max_delay".into());
        }
        if self.max_attempts == Some(0) {
            return Err("retry max_attempts must be at least 1".into());
        }
        Ok(())
    }
}

/// A duration written as text: `250ms`, `5s`, `2m`, `1h`, `90d` or a
/// combination such as `1m30s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DurationText(pub Duration);

impl FromStr for DurationText {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let invalid =
            || format!("invalid duration {s:?}; use values like 250ms, 5s, 2m, 1h or 90d");
        let mut total = Duration::ZERO;
        let mut rest = s.trim();
        if rest.is_empty() {
            return Err(invalid());
        }
        while !rest.is_empty() {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return Err(invalid());
            }
            let value: u64 = rest[..digits].parse().map_err(|_| invalid())?;
            rest = &rest[digits..];
            let (unit, len) = if rest.starts_with("ms") {
                (Duration::from_millis(1), 2)
            } else if rest.starts_with('s') {
                (Duration::from_secs(1), 1)
            } else if rest.starts_with('m') {
                (Duration::from_secs(60), 1)
            } else if rest.starts_with('h') {
                (Duration::from_secs(3600), 1)
            } else if rest.starts_with('d') {
                (Duration::from_secs(86_400), 1)
            } else {
                return Err(invalid());
            };
            rest = &rest[len..];
            let part = unit
                .checked_mul(u32::try_from(value).map_err(|_| invalid())?)
                .ok_or_else(invalid)?;
            total = total.checked_add(part).ok_or_else(invalid)?;
        }
        Ok(Self(total))
    }
}

impl fmt::Display for DurationText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let millis = self.0.as_millis();
        if millis.is_multiple_of(86_400_000) && millis > 0 {
            write!(f, "{}d", millis / 86_400_000)
        } else if millis.is_multiple_of(3_600_000) && millis > 0 {
            write!(f, "{}h", millis / 3_600_000)
        } else if millis.is_multiple_of(60_000) && millis > 0 {
            write!(f, "{}m", millis / 60_000)
        } else if millis.is_multiple_of(1000) {
            write!(f, "{}s", millis / 1000)
        } else {
            write!(f, "{millis}ms")
        }
    }
}

impl Serialize for DurationText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DurationText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl ChannelConfig {
    /// Parses a channel from YAML and validates it.
    pub fn from_yaml(text: &str) -> Result<Self, EngineError> {
        let config: Self = serde_saphyr::from_str(text)
            .map_err(|e| EngineError::Config(format!("invalid channel YAML: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    /// Writes the channel as YAML.
    pub fn to_yaml(&self) -> Result<String, EngineError> {
        serde_saphyr::to_string(self)
            .map_err(|e| EngineError::Config(format!("cannot write channel YAML: {e}")))
    }

    /// Reads every `*.yaml` and `*.yml` file in `dir`, sorted by file name.
    pub fn load_dir(dir: &Path) -> Result<Vec<Self>, EngineError> {
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| EngineError::Config(format!("cannot read {}: {e}", dir.display())))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|ext| ext == "yaml" || ext == "yml")
            })
            .collect();
        paths.sort();
        let mut channels = Vec::with_capacity(paths.len());
        let mut ids = BTreeSet::new();
        for path in paths {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| EngineError::Config(format!("cannot read {}: {e}", path.display())))?;
            let channel = Self::from_yaml(&text)
                .map_err(|e| EngineError::Config(format!("{}: {e}", path.display())))?;
            if !ids.insert(channel.id.clone()) {
                return Err(EngineError::Config(format!(
                    "{}: channel {} is defined twice",
                    path.display(),
                    channel.id
                )));
            }
            channels.push(channel);
        }
        Ok(channels)
    }

    /// Checks rules that the YAML structure cannot express.
    pub fn validate(&self) -> Result<(), EngineError> {
        let mut ids = BTreeSet::from([self.source.id.clone()]);
        for destination in &self.destinations {
            if !ids.insert(destination.id.clone()) {
                return Err(EngineError::Config(format!(
                    "channel {}: connector identifier {} is used twice",
                    self.id, destination.id
                )));
            }
            destination.queue.retry.validate().map_err(|e| {
                EngineError::Config(format!(
                    "channel {}, destination {}: {e}",
                    self.id, destination.id
                ))
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
id: chemistry
name: Chemistry analyzer to LIS
source:
  type: astm-tcp
  data_type: astm
  normalize: true
  settings:
    listen: 0.0.0.0:5100
filters:
  - type: path-equals
    path: H-5.1
    value: ANALYZER
destinations:
  - id: lis
    type: mllp
    encoder:
      type: hl7v2-oru-r01
      sending_application: OXIM
    queue:
      ordering: strict
      retry: { initial_delay: 5s, max_delay: 5m, multiplier: 2, max_attempts: 100 }
    settings:
      target: 10.0.0.20:2575
  - id: archive
    type: file
    queue:
      ordering: best_effort
    settings:
      directory: /var/lib/oxim/archive
"#;

    #[test]
    fn parses_channel_yaml() {
        let channel = ChannelConfig::from_yaml(EXAMPLE).unwrap();
        assert_eq!(channel.id.as_str(), "chemistry");
        assert!(channel.enabled);
        assert_eq!(channel.source.id.as_str(), "source");
        assert_eq!(channel.source.data_type, DataType::Astm);
        assert!(channel.source.normalize);
        assert_eq!(channel.source.settings["listen"], "0.0.0.0:5100");
        assert_eq!(channel.filters[0].kind, "path-equals");
        assert_eq!(channel.filters[0].text("path").unwrap(), "H-5.1");
        let lis = &channel.destinations[0];
        assert_eq!(lis.encoder.as_ref().unwrap().kind, "hl7v2-oru-r01");
        assert_eq!(lis.queue.retry.max_attempts, Some(100));
        assert_eq!(
            lis.queue.retry.max_delay,
            DurationText(Duration::from_secs(300))
        );
        assert_eq!(channel.destinations[1].queue.ordering, Ordering::BestEffort);

        let written = channel.to_yaml().unwrap();
        assert_eq!(ChannelConfig::from_yaml(&written).unwrap(), channel);
    }

    #[test]
    fn rejects_invalid_channels() {
        for (yaml, reason) in [
            (
                "id: Bad Id\nsource: {type: mllp, data_type: hl7v2}\n",
                "invalid id",
            ),
            (
                "id: a\nsource: {type: mllp, data_type: hl7}\n",
                "unknown data type",
            ),
            (
                "id: a\nsource: {type: mllp, data_type: hl7v2}\nunknown: 1\n",
                "unknown field",
            ),
            (
                "id: a\nsource: {type: mllp, data_type: hl7v2}\ndestinations:\n  - {id: x, type: file}\n  - {id: x, type: file}\n",
                "duplicate destination",
            ),
            (
                "id: a\nsource: {type: mllp, data_type: hl7v2}\ndestinations:\n  - {id: source, type: file}\n",
                "destination named like the source",
            ),
            (
                "id: a\nsource: {type: mllp, data_type: hl7v2}\ndestinations:\n  - id: x\n    type: file\n    queue: {retry: {initial_delay: 10m, max_delay: 1m}}\n",
                "inverted retry delays",
            ),
            (
                "id: a\nsource: {type: mllp, data_type: hl7v2}\ndestinations:\n  - id: x\n    type: file\n    queue: {retry: {initial_delay: soon}}\n",
                "bad duration",
            ),
        ] {
            assert!(ChannelConfig::from_yaml(yaml).is_err(), "{reason}");
        }
    }

    #[test]
    fn parses_and_formats_durations() {
        for (text, duration) in [
            ("250ms", Duration::from_millis(250)),
            ("5s", Duration::from_secs(5)),
            ("2m", Duration::from_secs(120)),
            ("1h", Duration::from_secs(3600)),
            ("1m30s", Duration::from_secs(90)),
            ("90d", Duration::from_secs(90 * 86_400)),
        ] {
            let parsed: DurationText = text.parse().unwrap();
            assert_eq!(parsed.0, duration);
        }
        assert_eq!(DurationText(Duration::from_secs(90)).to_string(), "90s");
        assert_eq!(DurationText(Duration::from_secs(120)).to_string(), "2m");
        assert_eq!(
            DurationText(Duration::from_secs(2 * 86_400)).to_string(),
            "2d"
        );
        for bad in ["", "5", "s", "5 s", "5w", "-5s"] {
            assert!(bad.parse::<DurationText>().is_err(), "{bad}");
        }
    }

    #[test]
    fn computes_backoff() {
        let policy = RetryPolicy {
            initial_delay: DurationText(Duration::from_secs(5)),
            max_delay: DurationText(Duration::from_secs(60)),
            multiplier: 2.0,
            max_attempts: Some(6),
        };
        let delays: Vec<_> = (1..=6).map(|n| policy.delay_after(n)).collect();
        assert_eq!(
            delays,
            [
                Some(Duration::from_secs(5)),
                Some(Duration::from_secs(10)),
                Some(Duration::from_secs(20)),
                Some(Duration::from_secs(40)),
                Some(Duration::from_secs(60)),
                None
            ]
        );
        assert_eq!(
            RetryPolicy::default().delay_after(1000),
            Some(Duration::from_secs(300))
        );
    }
}
