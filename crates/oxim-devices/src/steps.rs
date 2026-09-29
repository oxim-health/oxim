//! The `track-device` step and the environment it records into.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::config::DurationText;
use oxim_core::{EngineError, MessageContext, StepConfig, StepError, Transformer};
use oxim_model::{ClinicalContent, DeviceId, Timestamp};
use tracing::warn;

use crate::registry::{
    Declaration, DeviceIdentity, DeviceRegistry, DeviceState, RegistryError, RegistryResult,
    Sighting,
};

/// The device registry shared by the `track-device` steps of every
/// channel.
///
/// The database is opened when a step first records a message, so
/// validating channel files does not create it. Devices named by
/// `track-device` steps are declared when their channel is compiled, so a
/// snapshot lists configured devices that never sent anything.
#[derive(Debug, Clone)]
pub struct DeviceEnvironment {
    path: Option<PathBuf>,
    registry: Arc<Mutex<Option<Arc<DeviceRegistry>>>>,
    declared: Arc<Mutex<BTreeMap<DeviceId, Option<Duration>>>>,
}

impl DeviceEnvironment {
    /// Devices are recorded in the SQLite database at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            registry: Arc::new(Mutex::new(None)),
            declared: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Uses an open registry.
    pub fn with_registry(registry: Arc<DeviceRegistry>) -> Self {
        Self {
            path: None,
            registry: Arc::new(Mutex::new(Some(registry))),
            declared: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// The registry, opened on first use. A failed open is retried on the
    /// next use.
    pub fn registry(&self) -> RegistryResult<Arc<DeviceRegistry>> {
        let mut slot = self
            .registry
            .lock()
            .map_err(|_| RegistryError::Unavailable)?;
        if let Some(registry) = slot.as_ref() {
            return Ok(registry.clone());
        }
        let path = self.path.as_ref().ok_or(RegistryError::Unavailable)?;
        let registry = Arc::new(DeviceRegistry::open(path)?);
        *slot = Some(registry.clone());
        Ok(registry)
    }

    /// Declares a device that a channel expects.
    pub fn declare(&self, device: DeviceId, silence_after: Option<Duration>) {
        if let Ok(mut declared) = self.declared.lock() {
            declared.insert(device, silence_after);
        }
    }

    /// The declared devices.
    pub fn declarations(&self) -> Vec<Declaration> {
        self.declared
            .lock()
            .map(|declared| {
                declared
                    .iter()
                    .map(|(device, silence_after)| Declaration {
                        device: device.clone(),
                        silence_after: *silence_after,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every known device with its status at `now`.
    pub fn snapshot(&self, now: Timestamp) -> RegistryResult<Vec<DeviceState>> {
        self.registry()?.snapshot(&self.declarations(), now)
    }

    /// The devices past their silence window at `now`.
    pub fn silent(&self, now: Timestamp) -> RegistryResult<Vec<DeviceState>> {
        self.registry()?.silent(&self.declarations(), now)
    }
}

/// Records every message in the device registry.
///
/// ```yaml
/// transformers:
///   - type: track-device
///     device: chem-1        # the device's identifier
///     silence_after: 30m    # optional: the device counts as silent after this
/// ```
///
/// The device's own identification is kept when a message carries it: the
/// normalized `Device` of results, quality control, queries and device
/// events (ASTM H-5 name, version and serial number; HL7 MSH-3), and the
/// POCT1-A `HEL` device data the `poct1a` source attaches as metadata.
///
/// The step never changes or rejects a message. If the registry cannot be
/// written, it logs a warning and the message continues: device tracking
/// must not hold up results.
#[derive(Debug, Clone)]
pub struct TrackDevice {
    environment: DeviceEnvironment,
    device: DeviceId,
    silence_after: Option<Duration>,
}

impl TrackDevice {
    /// Builds the step from its configuration and declares the device.
    pub fn from_step(
        step: &StepConfig,
        environment: &DeviceEnvironment,
    ) -> Result<Self, EngineError> {
        let config_error =
            |message: String| EngineError::Config(format!("step \"track-device\": {message}"));
        let device =
            DeviceId::new(step.text("device")?).map_err(|e| config_error(e.to_string()))?;
        let silence_after = match step.settings.get("silence_after") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(text)) => Some(
                text.parse::<DurationText>()
                    .map_err(|e| config_error(format!("silence_after: {e}")))?
                    .0,
            ),
            Some(_) => {
                return Err(config_error(
                    "silence_after must be a duration such as 30m".into(),
                ));
            }
        };
        if silence_after.is_some_and(|d| d.is_zero()) {
            return Err(config_error("silence_after must be positive".into()));
        }
        environment.declare(device.clone(), silence_after);
        Ok(Self {
            environment: environment.clone(),
            device,
            silence_after,
        })
    }
}

/// What a message says about the device that sent it.
pub fn identity(context: &MessageContext) -> DeviceIdentity {
    let mut identity = DeviceIdentity::default();
    let device = match &context.clinical {
        Some(
            ClinicalContent::Results { device, .. }
            | ClinicalContent::Query { device, .. }
            | ClinicalContent::QualityControl { device, .. }
            | ClinicalContent::DeviceEvent { device, .. },
        ) => device.as_ref(),
        Some(ClinicalContent::Orders { .. }) | None => None,
    };
    if let Some(device) = device {
        identity.name.clone_from(&device.name);
        identity.manufacturer.clone_from(&device.manufacturer);
        identity.model.clone_from(&device.model);
        identity.serial_number.clone_from(&device.serial_number);
        identity
            .software_version
            .clone_from(&device.software_version);
        identity.identifiers = device.identifiers.iter().map(|i| i.value.clone()).collect();
    }
    let metadata = &context.envelope.metadata;
    let get = |key: &str| metadata.get(key).filter(|v| !v.is_empty()).cloned();
    if let Some(id) = get("poct1a.device_id")
        && !identity.identifiers.contains(&id)
    {
        identity.identifiers.push(id);
    }
    identity.manufacturer = identity.manufacturer.or_else(|| get("poct1a.vendor_id"));
    identity.model = identity.model.or_else(|| get("poct1a.model_id"));
    identity.serial_number = identity.serial_number.or_else(|| get("poct1a.serial_id"));
    identity
}

impl Transformer for TrackDevice {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let identity = identity(context);
        let envelope = &context.envelope;
        let recorded = self.environment.registry().and_then(|registry| {
            registry.record(&Sighting {
                device: &self.device,
                channel: &envelope.channel,
                message: envelope.id,
                at: envelope.received_at,
                silence_after: self.silence_after,
                identity: &identity,
            })
        });
        if let Err(error) = recorded {
            warn!(device = %self.device, message = %envelope.id, %error, "cannot record the device");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use oxim_core::Document;
    use oxim_model::{ChannelId, ConnectorId, DataType, Device, Envelope, MessageId};

    use super::*;
    use crate::registry::DeviceStatus;

    fn context(
        seconds: i64,
        clinical: Option<ClinicalContent>,
        metadata: &[(&str, &str)],
    ) -> MessageContext {
        let mut envelope = Envelope::new(
            MessageId::from_parts(1_790_000_000_000, seconds as u128),
            ChannelId::new("lab").unwrap(),
            ConnectorId::new("source").unwrap(),
            Timestamp::from_unix_nanos(seconds * 1_000_000_000),
            DataType::Raw,
            Vec::new(),
        );
        envelope.metadata = metadata
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        MessageContext {
            envelope,
            document: Document::Raw(Vec::new()),
            clinical,
            variables: BTreeMap::new(),
            response: None,
        }
    }

    #[test]
    fn records_devices_and_their_identity() {
        let registry = Arc::new(DeviceRegistry::open_in_memory().unwrap());
        let environment = DeviceEnvironment::with_registry(registry);
        let step = |settings: serde_json::Value| {
            let mut config = StepConfig::new("track-device");
            config.settings = settings.as_object().unwrap().clone();
            TrackDevice::from_step(&config, &environment)
        };
        let chem = step(serde_json::json!({"device": "chem-1", "silence_after": "10m"})).unwrap();
        let poct = step(serde_json::json!({"device": "poct-1"})).unwrap();
        step(serde_json::json!({"device": "spare-1"})).unwrap();
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"device": "Chem 1"}),
            serde_json::json!({"device": "x", "silence_after": "soon"}),
            serde_json::json!({"device": "x", "silence_after": "0s"}),
            serde_json::json!({"device": "x", "silence_after": 5}),
        ] {
            assert!(step(bad.clone()).is_err(), "{bad}");
        }

        let results = ClinicalContent::Results {
            device: Some(Device {
                name: Some("ChemAnalyzer".into()),
                software_version: Some("2.1".into()),
                serial_number: Some("SN1234".into()),
                ..Device::default()
            }),
            groups: Vec::new(),
        };
        chem.apply(&mut context(10, Some(results), &[])).unwrap();
        chem.apply(&mut context(20, None, &[])).unwrap();
        poct.apply(&mut context(
            30,
            None,
            &[
                ("poct1a.device_id", "POCT-7"),
                ("poct1a.vendor_id", "Example"),
                ("poct1a.serial_id", "0001"),
            ],
        ))
        .unwrap();

        let states = environment
            .snapshot(Timestamp::from_unix_nanos(60 * 1_000_000_000))
            .unwrap();
        assert_eq!(states.len(), 3);
        assert_eq!(states[0].device.as_str(), "chem-1");
        assert_eq!(states[0].messages, 2);
        assert_eq!(states[0].identity.serial_number.as_deref(), Some("SN1234"));
        assert_eq!(states[0].status, DeviceStatus::Online);
        assert_eq!(states[1].identity.manufacturer.as_deref(), Some("Example"));
        assert_eq!(states[1].identity.identifiers, ["POCT-7"]);
        assert_eq!(states[2].device.as_str(), "spare-1");
        assert_eq!(states[2].status, DeviceStatus::NeverSeen);
        let silent = environment
            .silent(Timestamp::from_unix_nanos(3600 * 1_000_000_000))
            .unwrap();
        assert_eq!(silent.len(), 1);
        assert_eq!(silent[0].device.as_str(), "chem-1");
    }
}
