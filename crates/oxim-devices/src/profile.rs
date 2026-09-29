//! Device profiles: what OXIM knows about a device model, in a versioned
//! YAML file.
//!
//! ```yaml
//! profile_version: 1
//! id: generic-astm-analyzer
//! vendor: Generic
//! model: ASTM LIS02 analyzer
//! firmware: {min: "1.0", max: "2.9"}
//! connection:
//!   transport: tcp            # tcp, serial or file
//!   protocol: astm-lis01      # astm-lis01, astm-raw, hl7v2-mllp, poct1a or other
//!   device_role: client       # tcp: the device connects to OXIM (client) or waits (server)
//!   dialect:                  # deviations from the standard, in the device's terms
//!     frame_text_max: 240
//! channel:                    # the OXIM channel that talks to the device
//!   source:
//!     type: astm-tcp
//!     data_type: astm
//!     normalize: true
//!     settings: {listen: 0.0.0.0:5100}
//! mappings:
//!   - {table: tables/device-to-loinc.csv, direction: device-to-lis}
//! quirks:
//!   - id: no-final-cr
//!     description: The terminator record lacks its final CR.
//!     workaround: Accepted by the OXIM ASTM parser; nothing to configure.
//! setup:
//!   - title: Set the host address
//!     text: Menu *Setup > Communication*: host IP of the OXIM server, port 5100.
//! verification:
//!   level: simulated          # unverified, documented, simulated, lab-verified, field-verified
//!   evidence:
//!     - note: Passes the recorded fixtures below.
//! fixtures:
//!   - name: two-results
//!     capture: fixtures/two-results.oximcap
//!     expect:
//!       messages: 1
//!       status: completed
//!       normalized: fixtures/two-results.expected.json
//! ```
//!
//! [`Profile::load`] parses a file and checks every rule below, reporting
//! each problem with its location (`fixtures[0].capture: ...`).
//!
//! | Rule | Why |
//! |---|---|
//! | `profile_version` is 1 | later versions may change meanings |
//! | `id` is 1 to 64 lowercase letters, digits, `-` or `_` | used as a channel and file name |
//! | the channel's source type matches `transport` and `protocol` | the connection description and the channel must agree |
//! | mapping tables, captures, message files and expected outputs exist | fixtures must be runnable |
//! | mapping tables are CSV with `from` and `to` columns | the `map-observations` format |
//! | `documented` needs evidence; `simulated` needs fixtures; `lab-verified` and `field-verified` need fixtures and dated evidence | the level is a claim that must be backed |
//! | a fixture has either a capture or message files | one input per fixture |

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use oxim_capture::Capture;
use oxim_core::{ChannelConfig, DestinationConfig, SourceConfig, StepConfig};
use oxim_model::{ChannelId, DataType, MessageStatus};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The profile format version this crate reads.
pub const PROFILE_VERSION: u32 = 1;

/// How the device is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// TCP/IP.
    Tcp,
    /// A serial line (RS-232, RS-485, USB serial).
    Serial,
    /// Files in a shared directory.
    File,
}

/// The protocol the device speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProtocolKind {
    /// ASTM E1394 records in CLSI LIS01 (E1381) frames.
    #[serde(rename = "astm-lis01")]
    AstmLis01,
    /// ASTM E1394 records without LIS01 framing.
    #[serde(rename = "astm-raw")]
    AstmRaw,
    /// HL7 v2 over MLLP.
    #[serde(rename = "hl7v2-mllp")]
    Hl7v2Mllp,
    /// CLSI POCT1-A.
    #[serde(rename = "poct1a")]
    Poct1a,
    /// Anything else.
    #[serde(rename = "other")]
    Other,
}

/// Which side opens a TCP connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    /// The device connects to OXIM.
    Client,
    /// The device listens and OXIM connects to it.
    Server,
}

/// A supported firmware range. Versions are vendor text, compared by
/// people, not parsed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareRange {
    /// Oldest version known to work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<String>,
    /// Newest version known to work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
    /// Remarks, such as versions with known problems.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// How the device is connected and which protocol variant it speaks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    /// The transport.
    pub transport: TransportKind,
    /// The protocol.
    pub protocol: ProtocolKind,
    /// For TCP: which side opens the connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_role: Option<DeviceRole>,
    /// Protocol variations in the device's terms, for example
    /// `frame_text_max: 240` or `encoding: windows-1252`. Documentation for
    /// people; the channel below holds the matching OXIM settings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dialect: BTreeMap<String, serde_json::Value>,
}

/// The OXIM channel that talks to the device: a channel file without `id`,
/// `name` and `enabled`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelTemplate {
    /// The source that receives from the device.
    pub source: SourceConfig,
    /// Channel filters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<StepConfig>,
    /// Channel transformers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transformers: Vec<StepConfig>,
    /// Destinations, for example orders sent back to the device.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<DestinationConfig>,
}

impl ChannelTemplate {
    /// The channel with identifier `id`.
    pub fn to_channel(&self, id: ChannelId) -> ChannelConfig {
        ChannelConfig {
            id,
            name: None,
            description: None,
            enabled: true,
            source: self.source.clone(),
            filters: self.filters.clone(),
            transformers: self.transformers.clone(),
            destinations: self.destinations.clone(),
        }
    }
}

/// Which way a code table translates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MappingDirection {
    /// Device codes to LIS (or LOINC) codes, for results.
    DeviceToLis,
    /// LIS codes to device codes, for orders.
    LisToDevice,
}

/// A default code table for the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    /// The CSV table (`map-observations` format), relative to the profile.
    pub table: PathBuf,
    /// Which way it translates.
    pub direction: MappingDirection,
    /// What it covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A known deviation from the standard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quirk {
    /// A short identifier.
    pub id: String,
    /// What the device does.
    pub description: String,
    /// How to live with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workaround: Option<String>,
}

/// One step of the setup guide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupStep {
    /// The step's title.
    pub title: String,
    /// Instructions, in Markdown.
    pub text: String,
}

/// How well a profile is backed by evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationLevel {
    /// The profile exists; no evidence yet.
    Unverified,
    /// Based on the vendor's interface specification.
    Documented,
    /// Passes recorded or specification-based simulated sessions.
    Simulated,
    /// Verified against a real device in a test environment.
    LabVerified,
    /// Verified in production use at a real site.
    FieldVerified,
}

impl fmt::Display for VerificationLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unverified => "unverified",
            Self::Documented => "documented",
            Self::Simulated => "simulated",
            Self::LabVerified => "lab-verified",
            Self::FieldVerified => "field-verified",
        })
    }
}

/// One piece of evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// What was checked and how.
    pub note: String,
    /// When (ISO 8601 date).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// Where the evidence comes from: a document, a test site, a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// The verification claim of a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    /// The level.
    pub level: VerificationLevel,
    /// The evidence behind it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// What a fixture must produce.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    /// How many messages the channel receives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<usize>,
    /// The final status of every message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<MessageStatus>,
    /// A JSON file holding an array with the normalized content of each
    /// message, in the order received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized: Option<PathBuf>,
    /// Text that must appear in what OXIM sent back to the device.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies_contain: Vec<String>,
}

/// A recorded or written session with its expected outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    /// A short name.
    pub name: String,
    /// What the session shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A capture replayed over TCP against the channel's source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<PathBuf>,
    /// Message files submitted to the channel directly (without transport
    /// framing).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<PathBuf>,
    /// The expected outcome.
    #[serde(default)]
    pub expect: Expectation,
}

/// A device profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Always [`PROFILE_VERSION`].
    pub profile_version: u32,
    /// Identifier, also used as the channel identifier in tests.
    pub id: String,
    /// Manufacturer.
    pub vendor: String,
    /// Model name.
    pub model: String,
    /// Free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Supported firmware.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<FirmwareRange>,
    /// Transport and protocol.
    pub connection: Connection,
    /// The OXIM channel for the device.
    pub channel: ChannelTemplate,
    /// Default code tables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mappings: Vec<Mapping>,
    /// Known deviations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quirks: Vec<Quirk>,
    /// The setup guide.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<SetupStep>,
    /// The verification claim.
    pub verification: Verification,
    /// Sessions that demonstrate the profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixtures: Vec<Fixture>,
}

/// One problem found in a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Where, for example `fixtures[0].capture`.
    pub path: String,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Errors loading a profile.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProfileError {
    /// The file cannot be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// The YAML does not match the profile schema.
    #[error("invalid profile: {0}")]
    Syntax(String),
    /// The profile breaks rules the schema cannot express.
    #[error("invalid profile:\n{}", .0.iter().map(|i| format!("  {i}")).collect::<Vec<_>>().join("\n"))]
    Invalid(Vec<Issue>),
}

/// A profile read from a file, with the directory its relative paths are
/// resolved against.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedProfile {
    /// The profile.
    pub profile: Profile,
    /// The file it was read from.
    pub path: PathBuf,
    /// The directory holding the file.
    pub dir: PathBuf,
}

impl LoadedProfile {
    /// A path of the profile resolved against its directory.
    pub fn resolve(&self, path: &Path) -> PathBuf {
        self.dir.join(path)
    }
}

/// The source connector types that fit a transport and protocol.
fn source_types(
    transport: TransportKind,
    protocol: ProtocolKind,
) -> Option<&'static [&'static str]> {
    use ProtocolKind as P;
    use TransportKind as T;
    Some(match (transport, protocol) {
        (T::Tcp, P::AstmLis01) => &["astm-tcp"],
        (T::Serial, P::AstmLis01) => &["astm-serial"],
        (T::Tcp, P::AstmRaw) => &["astm-raw-tcp", "tcp"],
        (T::Tcp, P::Hl7v2Mllp) => &["mllp"],
        (T::Tcp, P::Poct1a) => &["poct1a"],
        (T::File, _) => &["file"],
        (_, P::Other) => return None,
        _ => &[],
    })
}

fn data_type_of(protocol: ProtocolKind) -> Option<DataType> {
    match protocol {
        ProtocolKind::AstmLis01 | ProtocolKind::AstmRaw => Some(DataType::Astm),
        ProtocolKind::Hl7v2Mllp => Some(DataType::Hl7V2),
        ProtocolKind::Poct1a => Some(DataType::Poct1a),
        ProtocolKind::Other => None,
    }
}

impl Profile {
    /// Parses a profile without checking the rules that need files; see
    /// [`Profile::validate`].
    pub fn from_yaml(text: &str) -> Result<Self, ProfileError> {
        serde_saphyr::from_str(text).map_err(|e| ProfileError::Syntax(e.to_string()))
    }

    /// Reads and validates a profile file.
    pub fn load(path: impl AsRef<Path>) -> Result<LoadedProfile, ProfileError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ProfileError::Io {
            path: path.to_owned(),
            source,
        })?;
        let profile = Self::from_yaml(&text)?;
        let dir = path
            .parent()
            .map(Path::to_owned)
            .unwrap_or_else(|| PathBuf::from("."));
        let issues = profile.validate(Some(&dir));
        if !issues.is_empty() {
            return Err(ProfileError::Invalid(issues));
        }
        Ok(LoadedProfile {
            profile,
            path: path.to_owned(),
            dir,
        })
    }

    /// Checks the rules the schema cannot express. With `dir`, referenced
    /// files are checked too.
    pub fn validate(&self, dir: Option<&Path>) -> Vec<Issue> {
        let mut issues = Vec::new();
        let mut issue = |path: String, message: String| issues.push(Issue { path, message });
        if self.profile_version != PROFILE_VERSION {
            issue(
                "profile_version".into(),
                format!(
                    "unsupported version {}; this OXIM reads version {PROFILE_VERSION}",
                    self.profile_version
                ),
            );
        }
        let channel_id = ChannelId::new(self.id.clone());
        if let Err(e) = &channel_id {
            issue("id".into(), e.to_string());
        }
        for (field, value) in [("vendor", &self.vendor), ("model", &self.model)] {
            if value.trim().is_empty() {
                issue(field.into(), "must not be empty".into());
            }
        }

        let connection = &self.connection;
        let source = &self.channel.source;
        if let Some(types) = source_types(connection.transport, connection.protocol)
            && !types.contains(&source.kind.as_str())
        {
            issue(
                "channel.source.type".into(),
                if types.is_empty() {
                    format!(
                        "OXIM has no source for {:?} over {:?}",
                        connection.protocol, connection.transport
                    )
                } else {
                    format!(
                        "{:?} does not match the connection ({:?} over {:?}); use {}",
                        source.kind,
                        connection.protocol,
                        connection.transport,
                        types.join(" or ")
                    )
                },
            );
        }
        if connection.transport != TransportKind::File
            && let Some(expected) = data_type_of(connection.protocol)
            && source.data_type != expected
        {
            issue(
                "channel.source.data_type".into(),
                format!(
                    "{} does not match the protocol {:?}",
                    serde_json::to_string(&source.data_type).unwrap_or_default(),
                    connection.protocol
                ),
            );
        }
        if connection.device_role.is_some() && connection.transport != TransportKind::Tcp {
            issue(
                "connection.device_role".into(),
                "only applies to TCP".into(),
            );
        }
        if let Ok(id) = channel_id
            && let Err(e) = self.channel.to_channel(id).validate()
        {
            issue("channel".into(), e.to_string());
        }

        for (index, mapping) in self.mappings.iter().enumerate() {
            if let Some(dir) = dir {
                let path = format!("mappings[{index}].table");
                match std::fs::read_to_string(dir.join(&mapping.table)) {
                    Err(e) => issue(
                        path,
                        format!("cannot read {}: {e}", mapping.table.display()),
                    ),
                    Ok(text) => {
                        let header = text
                            .trim_start_matches('\u{feff}')
                            .lines()
                            .find(|line| !line.trim().is_empty())
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        let columns: Vec<&str> = header.split(',').map(str::trim).collect();
                        if !columns.contains(&"from") || !columns.contains(&"to") {
                            issue(path, "a code table needs `from` and `to` columns".into());
                        }
                    }
                }
            }
        }
        let mut quirk_ids = std::collections::BTreeSet::new();
        for (index, quirk) in self.quirks.iter().enumerate() {
            if !quirk_ids.insert(quirk.id.as_str()) {
                issue(
                    format!("quirks[{index}].id"),
                    format!("{:?} is used twice", quirk.id),
                );
            }
        }
        for (index, step) in self.setup.iter().enumerate() {
            if step.title.trim().is_empty() {
                issue(format!("setup[{index}].title"), "must not be empty".into());
            }
        }

        let mut names = std::collections::BTreeSet::new();
        for (index, fixture) in self.fixtures.iter().enumerate() {
            let at = |field: &str| format!("fixtures[{index}].{field}");
            if !names.insert(fixture.name.as_str()) {
                issue(at("name"), format!("{:?} is used twice", fixture.name));
            }
            match (&fixture.capture, fixture.messages.is_empty()) {
                (Some(_), false) => issue(
                    format!("fixtures[{index}]"),
                    "use either `capture` or `messages`, not both".into(),
                ),
                (None, true) => issue(
                    format!("fixtures[{index}]"),
                    "needs a `capture` or `messages`".into(),
                ),
                _ => {}
            }
            let Some(dir) = dir else { continue };
            if let Some(capture) = &fixture.capture
                && let Err(e) = Capture::load(dir.join(capture))
            {
                issue(at("capture"), format!("{}: {e}", capture.display()));
            }
            for (m, message) in fixture.messages.iter().enumerate() {
                if !dir.join(message).is_file() {
                    issue(
                        format!("fixtures[{index}].messages[{m}]"),
                        format!("file not found: {}", message.display()),
                    );
                }
            }
            if let Some(normalized) = &fixture.expect.normalized {
                let path = at("expect.normalized");
                match std::fs::read(dir.join(normalized)) {
                    Err(e) => issue(path, format!("cannot read {}: {e}", normalized.display())),
                    Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                        Ok(serde_json::Value::Array(items)) => {
                            if let Some(count) = fixture.expect.messages
                                && count != items.len()
                            {
                                issue(
                                    path,
                                    format!(
                                        "holds {} entries but `messages` is {count}",
                                        items.len()
                                    ),
                                );
                            }
                        }
                        Ok(_) => issue(path, "must be a JSON array, one entry per message".into()),
                        Err(e) => issue(path, format!("invalid JSON: {e}")),
                    },
                }
            }
        }

        let verification = &self.verification;
        let dated = verification.evidence.iter().any(|e| e.date.is_some());
        match verification.level {
            VerificationLevel::Unverified => {}
            VerificationLevel::Documented if verification.evidence.is_empty() => issue(
                "verification.evidence".into(),
                "`documented` needs evidence, such as the interface specification it follows"
                    .into(),
            ),
            VerificationLevel::Documented => {}
            VerificationLevel::Simulated if self.fixtures.is_empty() => issue(
                "fixtures".into(),
                "`simulated` needs at least one fixture".into(),
            ),
            VerificationLevel::Simulated => {}
            VerificationLevel::LabVerified | VerificationLevel::FieldVerified => {
                if self.fixtures.is_empty() {
                    issue(
                        "fixtures".into(),
                        format!("`{}` needs at least one fixture", verification.level),
                    );
                }
                if !dated {
                    issue(
                        "verification.evidence".into(),
                        format!(
                            "`{}` needs evidence with a date: where and when the device was tested",
                            verification.level
                        ),
                    );
                }
            }
        }
        issues
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "profile_version: 1
id: generic-analyzer
vendor: Generic
model: Analyzer
connection: {transport: tcp, protocol: astm-lis01, device_role: client}
channel:
  source: {type: astm-tcp, data_type: astm, normalize: true, settings: {listen: 0.0.0.0:5100}}
verification: {level: unverified}
";

    #[test]
    fn accepts_a_minimal_profile() {
        let profile = Profile::from_yaml(MINIMAL).unwrap();
        assert_eq!(profile.validate(None), []);
        assert_eq!(profile.verification.level, VerificationLevel::Unverified);
        assert!(VerificationLevel::FieldVerified > VerificationLevel::Simulated);
    }

    #[test]
    fn rejects_unknown_fields_and_versions() {
        let error = Profile::from_yaml(&format!("{MINIMAL}colour: red\n")).unwrap_err();
        assert!(error.to_string().contains("colour"), "{error}");
        let profile =
            Profile::from_yaml(&MINIMAL.replace("profile_version: 1", "profile_version: 2"))
                .unwrap();
        assert_eq!(profile.validate(None)[0].path, "profile_version");
    }

    #[test]
    fn reports_every_problem_with_its_location() {
        let text = MINIMAL
            .replace("id: generic-analyzer", "id: Generic Analyzer")
            .replace("type: astm-tcp", "type: mllp")
            .replace("data_type: astm", "data_type: hl7v2")
            .replace(
                "level: unverified",
                "level: lab-verified, evidence: [{note: tried it}]",
            )
            + "fixtures:\n  - {name: a}\n  - {name: a, capture: x.oximcap, messages: [m.astm]}\n";
        let profile = Profile::from_yaml(&text).unwrap();
        let issues: Vec<String> = profile
            .validate(None)
            .iter()
            .map(ToString::to_string)
            .collect();
        let expected = [
            "id: ",
            "channel.source.type: \"mllp\" does not match the connection (AstmLis01 over Tcp); use astm-tcp",
            "channel.source.data_type: \"hl7v2\" does not match the protocol AstmLis01",
            "fixtures[0]: needs a `capture` or `messages`",
            "fixtures[1].name: \"a\" is used twice",
            "fixtures[1]: use either `capture` or `messages`, not both",
            "verification.evidence: `lab-verified` needs evidence with a date",
        ];
        assert_eq!(issues.len(), expected.len(), "{issues:#?}");
        for (issue, expected) in issues.iter().zip(expected) {
            assert!(issue.starts_with(expected), "{issue} / {expected}");
        }
    }

    #[test]
    fn checks_referenced_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codes.csv"), "code,loinc\nGLU,2345-7\n").unwrap();
        std::fs::write(dir.path().join("expected.json"), "{}").unwrap();
        let text = MINIMAL.replace("level: unverified", "level: simulated")
            + "mappings: [{table: codes.csv, direction: device-to-lis}, {table: missing.csv, direction: lis-to-device}]
fixtures:
  - {name: a, capture: none.oximcap, expect: {normalized: expected.json}}
  - {name: b, messages: [none.astm]}
";
        let profile = Profile::from_yaml(&text).unwrap();
        let issues: Vec<String> = profile
            .validate(Some(dir.path()))
            .iter()
            .map(|i| i.path.clone())
            .collect();
        assert_eq!(
            issues,
            [
                "mappings[0].table",
                "mappings[1].table",
                "fixtures[0].capture",
                "fixtures[0].expect.normalized",
                "fixtures[1].messages[0]"
            ]
        );
    }
}
