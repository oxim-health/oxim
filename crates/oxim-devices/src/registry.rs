//! The device registry: which devices sent messages, when, and how they
//! identified themselves.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use oxim_model::{ChannelId, DeviceId, MessageId, Timestamp};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors of the device registry.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// The database reported an error.
    #[error("device registry database: {0}")]
    Database(#[from] rusqlite::Error),
    /// A stored value could not be decoded.
    #[error("corrupt device registry entry: {0}")]
    Corrupt(String),
    /// The lock was poisoned by a panic in another thread.
    #[error("the device registry is unavailable")]
    Unavailable,
}

/// Result alias for registry operations.
pub type RegistryResult<T> = Result<T, RegistryError>;

/// What a device reported about itself (ASTM H-5, POCT1-A `HEL`/`DST`
/// device data, HL7 sending application).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DeviceIdentity {
    /// Name given by the device or the site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Manufacturer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Serial number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    /// Software or firmware version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub software_version: Option<String>,
    /// Other identifiers the device reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<String>,
}

impl DeviceIdentity {
    /// Whether nothing was reported.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Fills fields that `self` lacks from `newer`, and replaces those
    /// `newer` reports.
    fn merge(&mut self, newer: &Self) {
        for (target, value) in [
            (&mut self.name, &newer.name),
            (&mut self.manufacturer, &newer.manufacturer),
            (&mut self.model, &newer.model),
            (&mut self.serial_number, &newer.serial_number),
            (&mut self.software_version, &newer.software_version),
        ] {
            if value.is_some() {
                target.clone_from(value);
            }
        }
        for identifier in &newer.identifiers {
            if !self.identifiers.contains(identifier) {
                self.identifiers.push(identifier.clone());
            }
        }
    }
}

/// Whether a device is sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatus {
    /// Sent a message within its silence window (or has no window).
    Online,
    /// Sent nothing for longer than its silence window.
    Silent,
    /// Configured, but no message seen yet.
    NeverSeen,
}

/// A device OXIM expects, declared by a channel's `track-device` step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// The device.
    pub device: DeviceId,
    /// How long it may stay silent.
    pub silence_after: Option<Duration>,
}

/// One device as seen on one channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceState {
    /// The device.
    pub device: DeviceId,
    /// The channel it was seen on; `None` when never seen.
    pub channel: Option<ChannelId>,
    /// Status at the time of the snapshot.
    pub status: DeviceStatus,
    /// First message.
    pub first_seen: Option<Timestamp>,
    /// Latest message.
    pub last_seen: Option<Timestamp>,
    /// The latest message's identifier.
    pub last_message: Option<MessageId>,
    /// Messages seen.
    pub messages: u64,
    /// The silence window.
    #[serde(with = "optional_duration")]
    pub silence_after: Option<Duration>,
    /// What the device reported about itself.
    pub identity: DeviceIdentity,
}

mod optional_duration {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(duration) => serializer.serialize_some(&duration.as_secs_f64()),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        Ok(Option::<f64>::deserialize(deserializer)?
            .filter(|secs| secs.is_finite() && *secs >= 0.0)
            .map(Duration::from_secs_f64))
    }
}

/// A sighting of a device, recorded by [`DeviceRegistry::record`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sighting<'a> {
    /// The device.
    pub device: &'a DeviceId,
    /// The channel.
    pub channel: &'a ChannelId,
    /// The message.
    pub message: MessageId,
    /// When the message arrived.
    pub at: Timestamp,
    /// The silence window configured for the device.
    pub silence_after: Option<Duration>,
    /// What the message said about the device.
    pub identity: &'a DeviceIdentity,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS devices (
    channel TEXT NOT NULL,
    device TEXT NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    last_message TEXT NOT NULL,
    messages INTEGER NOT NULL,
    silence_after_ms INTEGER,
    identity TEXT NOT NULL,
    PRIMARY KEY (channel, device)
);
";

fn status(
    last_seen: Option<Timestamp>,
    silence_after: Option<Duration>,
    now: Timestamp,
) -> DeviceStatus {
    match (last_seen, silence_after) {
        (None, _) => DeviceStatus::NeverSeen,
        (Some(_), None) => DeviceStatus::Online,
        (Some(last), Some(window)) => {
            let window = i64::try_from(window.as_nanos()).unwrap_or(i64::MAX);
            if now.unix_nanos().saturating_sub(last.unix_nanos()) > window {
                DeviceStatus::Silent
            } else {
                DeviceStatus::Online
            }
        }
    }
}

/// Durable device registry in SQLite (`devices.db`).
///
/// The registry is operational data: losing it loses history, not
/// messages, so it favours speed (`synchronous=NORMAL`).
#[derive(Debug)]
pub struct DeviceRegistry {
    conn: Mutex<Connection>,
}

impl DeviceRegistry {
    /// Opens or creates the registry at `path`.
    pub fn open(path: impl AsRef<Path>) -> RegistryResult<Self> {
        Self::prepare(Connection::open(path)?)
    }

    /// A private in-memory registry, for tests.
    pub fn open_in_memory() -> RegistryResult<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> RegistryResult<Self> {
        let _mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(
        &self,
        work: impl FnOnce(&mut Connection) -> RegistryResult<T>,
    ) -> RegistryResult<T> {
        let mut conn = self.conn.lock().map_err(|_| RegistryError::Unavailable)?;
        work(&mut conn)
    }

    /// Records a message from a device.
    pub fn record(&self, sighting: &Sighting<'_>) -> RegistryResult<()> {
        let at = sighting.at.unix_nanos();
        let window = sighting
            .silence_after
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        self.with(|conn| {
            let tx = conn.transaction()?;
            let stored: Option<String> = tx
                .query_row(
                    "SELECT identity FROM devices WHERE channel = ?1 AND device = ?2",
                    params![sighting.channel.as_str(), sighting.device.as_str()],
                    |row| row.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            let mut identity: DeviceIdentity = match stored {
                Some(text) => serde_json::from_str(&text).map_err(|e| RegistryError::Corrupt(e.to_string()))?,
                None => DeviceIdentity::default(),
            };
            identity.merge(sighting.identity);
            let identity = serde_json::to_string(&identity).map_err(|e| RegistryError::Corrupt(e.to_string()))?;
            tx.execute(
                "INSERT INTO devices (channel, device, first_seen, last_seen, last_message, messages, silence_after_ms, identity)
                 VALUES (?1, ?2, ?3, ?3, ?4, 1, ?5, ?6)
                 ON CONFLICT (channel, device) DO UPDATE SET
                    first_seen = MIN(devices.first_seen, excluded.first_seen),
                    last_seen = MAX(devices.last_seen, excluded.last_seen),
                    last_message = CASE WHEN excluded.last_seen >= devices.last_seen
                                        THEN excluded.last_message ELSE devices.last_message END,
                    messages = devices.messages + 1,
                    silence_after_ms = excluded.silence_after_ms,
                    identity = excluded.identity",
                params![
                    sighting.channel.as_str(),
                    sighting.device.as_str(),
                    at,
                    sighting.message.to_string(),
                    window,
                    identity
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Every device seen, plus the `declared` devices never seen, with their
    /// status at `now`. A declaration's silence window applies to devices
    /// seen before it was configured.
    pub fn snapshot(
        &self,
        declared: &[Declaration],
        now: Timestamp,
    ) -> RegistryResult<Vec<DeviceState>> {
        let mut states = self.with(|conn| {
            let mut statement = conn.prepare(
                "SELECT channel, device, first_seen, last_seen, last_message, messages, silence_after_ms, identity
                 FROM devices ORDER BY device, channel",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|(channel, device, first, last, message, count, window, identity)| {
                    let corrupt = |e: String| RegistryError::Corrupt(e);
                    let last_seen = Timestamp::from_unix_nanos(last);
                    let silence_after = window.map(|ms| Duration::from_millis(u64::try_from(ms).unwrap_or(0)));
                    Ok(DeviceState {
                        device: DeviceId::new(device).map_err(|e| corrupt(e.to_string()))?,
                        channel: Some(ChannelId::new(channel).map_err(|e| corrupt(e.to_string()))?),
                        status: status(Some(last_seen), silence_after, now),
                        first_seen: Some(Timestamp::from_unix_nanos(first)),
                        last_seen: Some(last_seen),
                        last_message: message.parse().ok(),
                        messages: u64::try_from(count).unwrap_or(0),
                        silence_after,
                        identity: serde_json::from_str(&identity).map_err(|e| corrupt(e.to_string()))?,
                    })
                })
                .collect::<RegistryResult<Vec<_>>>()
        })?;
        let windows: BTreeMap<&DeviceId, Option<Duration>> = declared
            .iter()
            .map(|d| (&d.device, d.silence_after))
            .collect();
        for state in &mut states {
            if let Some(window) = windows.get(&state.device)
                && state.silence_after.is_none()
            {
                state.silence_after = *window;
                state.status = status(state.last_seen, state.silence_after, now);
            }
        }
        for declaration in declared {
            if !states.iter().any(|s| s.device == declaration.device) {
                states.push(DeviceState {
                    device: declaration.device.clone(),
                    channel: None,
                    status: DeviceStatus::NeverSeen,
                    first_seen: None,
                    last_seen: None,
                    last_message: None,
                    messages: 0,
                    silence_after: declaration.silence_after,
                    identity: DeviceIdentity::default(),
                });
            }
        }
        states.sort_by(|a, b| a.device.cmp(&b.device).then(a.channel.cmp(&b.channel)));
        Ok(states)
    }

    /// The devices past their silence window at `now`: what a silence alarm
    /// reports.
    pub fn silent(
        &self,
        declared: &[Declaration],
        now: Timestamp,
    ) -> RegistryResult<Vec<DeviceState>> {
        Ok(self
            .snapshot(declared, now)?
            .into_iter()
            .filter(|state| state.status == DeviceStatus::Silent)
            .collect())
    }

    /// Removes a device from the registry. Returns whether it was known.
    pub fn forget(&self, channel: &ChannelId, device: &DeviceId) -> RegistryResult<bool> {
        self.with(|conn| {
            Ok(conn.execute(
                "DELETE FROM devices WHERE channel = ?1 AND device = ?2",
                params![channel.as_str(), device.as_str()],
            )? > 0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_unix_nanos(seconds * 1_000_000_000)
    }

    fn id(n: u64) -> MessageId {
        MessageId::from_parts(1_790_000_000_000, u128::from(n))
    }

    #[test]
    fn tracks_sightings_identity_and_silence() {
        let registry = DeviceRegistry::open_in_memory().unwrap();
        let chem = DeviceId::new("chem-1").unwrap();
        let hema = DeviceId::new("hema-1").unwrap();
        let poct = DeviceId::new("poct-1").unwrap();
        let channel = ChannelId::new("lab").unwrap();
        let identity = DeviceIdentity {
            name: Some("ChemAnalyzer".into()),
            software_version: Some("2.1".into()),
            ..DeviceIdentity::default()
        };
        let window = Some(Duration::from_secs(60));
        for (n, seconds) in [(1, 10), (2, 20)] {
            registry
                .record(&Sighting {
                    device: &chem,
                    channel: &channel,
                    message: id(n),
                    at: at(seconds),
                    silence_after: window,
                    identity: &identity,
                })
                .unwrap();
        }
        registry
            .record(&Sighting {
                device: &chem,
                channel: &channel,
                message: id(3),
                at: at(15),
                silence_after: window,
                identity: &DeviceIdentity {
                    serial_number: Some("SN1".into()),
                    ..DeviceIdentity::default()
                },
            })
            .unwrap();
        registry
            .record(&Sighting {
                device: &hema,
                channel: &channel,
                message: id(4),
                at: at(5),
                silence_after: None,
                identity: &DeviceIdentity::default(),
            })
            .unwrap();
        let declared = [
            Declaration {
                device: poct.clone(),
                silence_after: None,
            },
            Declaration {
                device: hema.clone(),
                silence_after: Some(Duration::from_secs(30)),
            },
        ];

        let states = registry.snapshot(&declared, at(50)).unwrap();
        assert_eq!(states.len(), 3);
        let chem_state = &states[0];
        assert_eq!(chem_state.device, chem);
        assert_eq!(chem_state.status, DeviceStatus::Online);
        assert_eq!(chem_state.messages, 3);
        assert_eq!(chem_state.first_seen, Some(at(10)));
        assert_eq!(chem_state.last_seen, Some(at(20)));
        assert_eq!(chem_state.last_message, Some(id(2)));
        assert_eq!(chem_state.identity.name.as_deref(), Some("ChemAnalyzer"));
        assert_eq!(chem_state.identity.serial_number.as_deref(), Some("SN1"));
        // hema-1 uses the declared window: 45 s of silence > 30 s.
        assert_eq!(states[1].status, DeviceStatus::Silent);
        assert_eq!(states[2].device, poct);
        assert_eq!(states[2].status, DeviceStatus::NeverSeen);

        let silent: Vec<_> = registry
            .silent(&declared, at(100))
            .unwrap()
            .into_iter()
            .map(|s| s.device)
            .collect();
        assert_eq!(silent, [chem.clone(), hema]);
        assert!(registry.forget(&channel, &chem).unwrap());
        assert!(!registry.forget(&channel, &chem).unwrap());
        let json = serde_json::to_value(&states[0]).unwrap();
        assert_eq!(json["status"], "online");
        assert_eq!(json["silence_after"], 60.0);
    }
}
