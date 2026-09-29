//! Removes protected health information from messages and captures, so
//! recordings of real devices can be shared, attached to device profiles
//! and committed to repositories.
//!
//! | Input | What changes |
//! |---|---|
//! | HL7 v2 | patient, next of kin, visit, insurance and guarantor segments (see [`hl7`]); order, specimen and query identifiers; every date |
//! | ASTM E1394 | patient records, specimen and query identifiers, every date (see [`astm`]); comment records on request |
//! | POCT1-A | patient and operator identifiers and names, specimen identifiers, birth dates and every `_dttm` time (see [`poct1a`]) |
//! | JSON, XML | the paths listed in the configuration (see [`paths`]) |
//! | `.oximcap` captures | the messages inside MLLP frames, LIS01 frames (checksums recomputed), unframed ASTM and POCT1-A documents, keeping the conversation's rhythm (see [`capture`]) |
//!
//! Every value is handled by one of these actions:
//!
//! - **pseudonymize**: identifiers become different identifiers of the same
//!   shape (digits stay digits, letters stay letters), names become
//!   invented names. The same value always gets the same pseudonym within a
//!   run, so a patient's messages still belong together. Pseudonyms come
//!   from HMAC-SHA-256 under a key that is random per run unless supplied.
//! - **shift**: every date moves by the same number of days (random per run
//!   unless supplied), so intervals such as age at collection survive.
//! - **remove**: addresses, telephone numbers, national identifiers and
//!   similar values are emptied.
//! - **redact**: free text (HL7 NTE and text OBX, ASTM comments) becomes
//!   `REDACTED` when [`Options::free_text`] is set, because clinical notes
//!   may name people.
//!
//! Results, units, reference ranges and flags are never changed. The
//! [`Report`] counts changes per location without recording any value.
//! Anonymization is a tool, not a guarantee: review the output before
//! sharing it.

pub mod astm;
pub mod capture;
mod dates;
pub mod hl7;
pub mod paths;
pub mod poct1a;
mod pseudonym;
mod report;

use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use thiserror::Error;

use crate::dates::DateShift;
pub use crate::paths::{PathAction, PathRule};
use crate::pseudonym::Pseudonymizer;
pub use crate::report::{Action, Counts, Report};

/// Errors of an anonymization run.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AnonymizeError {
    /// The input is not a valid message of its type.
    #[error("cannot parse the input: {0}")]
    Parse(String),
    /// The configuration is invalid.
    #[error("invalid configuration: {0}")]
    Config(String),
    /// The system random source failed.
    #[error("the system random source failed")]
    Random,
}

/// The kind of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataKind {
    /// HL7 v2 (ER7), one message or several.
    Hl7v2,
    /// ASTM E1394 records.
    Astm,
    /// POCT1-A XML.
    Poct1a,
    /// JSON, by the configured paths.
    Json,
    /// XML, by the configured paths.
    Xml,
}

/// Guesses the kind of a message from its first bytes.
pub fn detect(bytes: &[u8]) -> Option<DataKind> {
    let text = bytes.trim_ascii_start();
    let text = text.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(text);
    if text.starts_with(b"MSH") || text.starts_with(b"FHS") || text.starts_with(b"BHS") {
        return Some(DataKind::Hl7v2);
    }
    if text.len() > 5 && text[0] == b'H' && !text[1].is_ascii_alphanumeric() {
        return Some(DataKind::Astm);
    }
    if text.starts_with(b"{") || text.starts_with(b"[") {
        return Some(DataKind::Json);
    }
    if text.starts_with(b"<") {
        let head = String::from_utf8_lossy(&text[..text.len().min(512)]).into_owned();
        let poct = [
            "HEL.R01", "DST.R01", "OBS.R01", "EVS.R01", "EOT.R01", "END.R01", "ACK.R01", "REQ.R01",
            "OPL.R01", "DTV.R01",
        ];
        return Some(
            if poct.iter().any(|root| head.contains(&format!("<{root}"))) {
                DataKind::Poct1a
            } else {
                DataKind::Xml
            },
        );
    }
    None
}

fn default_true() -> bool {
    true
}

/// What to anonymize beyond the fixed protocol rules.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    /// Redact free text: HL7 NTE-3 and text OBX-5, ASTM comment records,
    /// POCT1-A notes.
    #[serde(default)]
    pub free_text: bool,
    /// Pseudonymize specimen, order and container identifiers (barcodes).
    /// They are not patient identifiers by themselves but link to one.
    #[serde(default = "default_true")]
    pub specimens: bool,
    /// Rules for JSON documents.
    #[serde(default)]
    pub json: Vec<PathRule>,
    /// Rules for XML documents.
    #[serde(default)]
    pub xml: Vec<PathRule>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            free_text: false,
            specimens: true,
            json: Vec::new(),
            xml: Vec::new(),
        }
    }
}

impl Options {
    /// Reads options from YAML.
    pub fn from_yaml(text: &str) -> Result<Self, AnonymizeError> {
        serde_saphyr::from_str(text).map_err(|e| AnonymizeError::Config(e.to_string()))
    }
}

/// How a field is anonymized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Component 1 of every repetition is an identifier.
    Identifier,
    /// Like [`Kind::Identifier`], for specimen and order numbers.
    Specimen,
    /// A person name: family name, given name; the rest is dropped.
    Name,
    /// A clinician: identifier, family name, given name.
    Clinician,
    /// Emptied.
    Remove,
    /// Every component that is a date is shifted.
    Date,
    /// Free text.
    Text,
    /// An ASTM query range: patient identifier, specimen identifier.
    QueryRange,
}

/// Separators of a delimited field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Separators {
    pub(crate) repetition: u8,
    pub(crate) component: u8,
}

/// Anonymizes messages and captures with one key and one date shift.
#[derive(Debug)]
pub struct Anonymizer {
    pseudonyms: Pseudonymizer,
    dates: DateShift,
    options: Options,
    key_supplied: bool,
    date_shift_supplied: bool,
}

impl Anonymizer {
    /// An anonymizer. Without a `key`, pseudonyms use a random key; without
    /// `date_shift_days`, dates move back by a random 30 to 730 days.
    pub fn new(
        options: Options,
        key: Option<&[u8]>,
        date_shift_days: Option<i64>,
    ) -> Result<Self, AnonymizeError> {
        let random = SystemRandom::new();
        let key_supplied = key.is_some();
        let key = match key {
            Some(key) if key.len() < 16 => {
                return Err(AnonymizeError::Config(
                    "the key needs at least 16 bytes".into(),
                ));
            }
            Some(key) => key.to_vec(),
            None => {
                let mut key = vec![0u8; 32];
                random.fill(&mut key).map_err(|_| AnonymizeError::Random)?;
                key
            }
        };
        let days = match date_shift_days {
            Some(days) => days,
            None => {
                let mut bytes = [0u8; 2];
                random
                    .fill(&mut bytes)
                    .map_err(|_| AnonymizeError::Random)?;
                -30 - i64::from(u16::from_be_bytes(bytes) % 701)
            }
        };
        Ok(Self {
            pseudonyms: Pseudonymizer::new(&key),
            dates: DateShift::new(days),
            options,
            key_supplied,
            date_shift_supplied: date_shift_days.is_some(),
        })
    }

    /// The options.
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// A report prepared for this anonymizer's runs.
    pub fn report(&self) -> Report {
        Report {
            key_supplied: self.key_supplied,
            date_shift_supplied: self.date_shift_supplied,
            ..Report::default()
        }
    }

    /// Anonymizes one input of a known kind.
    pub fn anonymize(
        &self,
        kind: DataKind,
        input: &[u8],
        report: &mut Report,
    ) -> Result<Vec<u8>, AnonymizeError> {
        match kind {
            DataKind::Hl7v2 => hl7::anonymize_batch(self, input, report),
            DataKind::Astm => Ok(astm::anonymize_message(self, input, report)),
            DataKind::Poct1a => Ok(poct1a::anonymize_document(self, input, report)),
            DataKind::Json => paths::anonymize_json(self, input, report),
            DataKind::Xml => paths::anonymize_xml(self, input, report),
        }
    }

    /// Anonymizes one field value. Returns `None` when nothing changed.
    pub(crate) fn field(
        &self,
        kind: Kind,
        raw: &[u8],
        separators: Separators,
        location: &str,
        report: &mut Report,
    ) -> Option<Vec<u8>> {
        if raw.is_empty() {
            return None;
        }
        let pseudonymize = |value: &[u8]| self.pseudonyms.identifier(value);
        let (new, action) = match kind {
            Kind::Remove => (Vec::new(), Action::Removed),
            Kind::Text => {
                if !self.options.free_text {
                    return None;
                }
                (b"REDACTED".to_vec(), Action::Redacted)
            }
            Kind::Specimen if !self.options.specimens => return None,
            Kind::Identifier | Kind::Specimen | Kind::QueryRange => {
                let new = map_repetitions(raw, separators, |components| {
                    if let Some(first) = components.first_mut()
                        && !first.is_empty()
                        && !first.eq_ignore_ascii_case(b"ALL")
                    {
                        *first = pseudonymize(first);
                    }
                    if kind == Kind::QueryRange
                        && self.options.specimens
                        && let Some(second) = components.get_mut(1)
                        && !second.is_empty()
                        && !second.eq_ignore_ascii_case(b"ALL")
                    {
                        *second = pseudonymize(second);
                    }
                });
                (new, Action::Pseudonymized)
            }
            Kind::Name => {
                let new = map_repetitions(raw, separators, |components| {
                    let family = components.first().cloned().unwrap_or_default();
                    let given = components.get(1).cloned().unwrap_or_default();
                    components.clear();
                    components.push(if family.is_empty() {
                        Vec::new()
                    } else {
                        self.pseudonyms.family(&family).into_bytes()
                    });
                    if !given.is_empty() {
                        components.push(self.pseudonyms.given(&given).into_bytes());
                    }
                });
                (new, Action::Pseudonymized)
            }
            Kind::Clinician => {
                let new = map_repetitions(raw, separators, |components| {
                    let id = components.first().cloned().unwrap_or_default();
                    let family = components.get(1).cloned().unwrap_or_default();
                    let given = components.get(2).cloned().unwrap_or_default();
                    components.clear();
                    components.push(if id.is_empty() {
                        Vec::new()
                    } else {
                        pseudonymize(&id)
                    });
                    if !family.is_empty() || !given.is_empty() {
                        components.push(if family.is_empty() {
                            Vec::new()
                        } else {
                            self.pseudonyms.family(&family).into_bytes()
                        });
                    }
                    if !given.is_empty() {
                        components.push(self.pseudonyms.given(&given).into_bytes());
                    }
                });
                (new, Action::Pseudonymized)
            }
            Kind::Date => {
                let new = map_repetitions(raw, separators, |components| {
                    for component in components.iter_mut() {
                        if let Some(shifted) = std::str::from_utf8(component)
                            .ok()
                            .and_then(|text| self.dates.compact(text))
                        {
                            *component = shifted.into_bytes();
                        }
                    }
                });
                (new, Action::Shifted)
            }
        };
        if new == raw {
            return None;
        }
        report.note(location, action);
        Some(new)
    }

    /// A pseudonym for a whole value.
    pub(crate) fn pseudonym(&self, value: &[u8]) -> Vec<u8> {
        self.pseudonyms.identifier(value)
    }

    /// An invented family name for a whole value.
    pub(crate) fn family_name(&self, value: &[u8]) -> String {
        self.pseudonyms.family(value)
    }

    /// Shifts an ISO 8601 date/time.
    pub(crate) fn shift_iso(&self, text: &str) -> Option<String> {
        self.dates.iso(text)
    }

    /// Shifts an HL7/ASTM date/time.
    pub(crate) fn shift_compact(&self, text: &str) -> Option<String> {
        self.dates.compact(text)
    }
}

/// Applies `edit` to the components of every repetition of a field.
fn map_repetitions(
    raw: &[u8],
    separators: Separators,
    mut edit: impl FnMut(&mut Vec<Vec<u8>>),
) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for (index, repetition) in raw.split(|&b| b == separators.repetition).enumerate() {
        if index > 0 {
            out.push(separators.repetition);
        }
        if repetition.is_empty() {
            continue;
        }
        let mut components: Vec<Vec<u8>> = repetition
            .split(|&b| b == separators.component)
            .map(<[u8]>::to_vec)
            .collect();
        edit(&mut components);
        while components.len() > 1 && components.last().is_some_and(Vec::is_empty) {
            components.pop();
        }
        out.extend_from_slice(&components.join(&separators.component));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anonymizer(options: Options) -> Anonymizer {
        Anonymizer::new(options, Some(b"0123456789abcdef"), Some(-10)).unwrap()
    }

    const SEPARATORS: Separators = Separators {
        repetition: b'~',
        component: b'^',
    };

    #[test]
    fn anonymizes_fields_by_kind() {
        let a = anonymizer(Options::default());
        let mut report = a.report();
        let field = |kind, raw: &str, report: &mut Report| {
            a.field(kind, raw.as_bytes(), SEPARATORS, "X-1", report)
                .map(|v| String::from_utf8(v).unwrap())
        };
        let id = field(Kind::Identifier, "PAT001^^^LAB^MR~555^^^NAT", &mut report).unwrap();
        assert!(
            id.ends_with("^^^LAB^MR~") || id.contains("^^^LAB^MR~"),
            "{id}"
        );
        assert!(!id.contains("PAT001") && !id.contains("555"), "{id}");
        assert_eq!(id.len(), "PAT001^^^LAB^MR~555^^^NAT".len());
        let name = field(Kind::Name, "Doe^Jane^Q^^Dr", &mut report).unwrap();
        assert_eq!(name.matches('^').count(), 1, "{name}");
        assert!(!name.contains("Doe") && !name.contains("Jane"));
        assert_eq!(
            field(Kind::Remove, "1 Main St^^Town", &mut report).as_deref(),
            Some("")
        );
        assert_eq!(
            field(Kind::Date, "20260929120000^20260930", &mut report).as_deref(),
            Some("20260919120000^20260920")
        );
        assert_eq!(field(Kind::Text, "call Dr Who", &mut report), None);
        assert_eq!(field(Kind::Identifier, "", &mut report), None);
        let range = field(Kind::QueryRange, "^SMP001^^", &mut report).unwrap();
        assert!(
            range.starts_with('^') && !range.contains("SMP001"),
            "{range}"
        );
        assert_eq!(field(Kind::QueryRange, "ALL", &mut report), None);
        let clinician = field(Kind::Clinician, "D123^House^Greg^^^Dr", &mut report).unwrap();
        assert_eq!(clinician.matches('^').count(), 2, "{clinician}");
        assert_eq!(report.changes["X-1"].pseudonymized, 4);
        assert_eq!(report.changes["X-1"].removed, 1);
        assert_eq!(report.changes["X-1"].shifted, 1);

        let a = anonymizer(Options {
            free_text: true,
            specimens: false,
            ..Options::default()
        });
        let mut report = a.report();
        assert_eq!(
            a.field(Kind::Text, b"call Dr Who", SEPARATORS, "NTE-3", &mut report)
                .as_deref(),
            Some(&b"REDACTED"[..])
        );
        assert_eq!(
            a.field(Kind::Specimen, b"SMP001", SEPARATORS, "SPM-2", &mut report),
            None
        );
        assert!(report.to_string().contains("NTE-3: 1 redacted"));
    }

    #[test]
    fn detects_kinds_and_validates_keys() {
        assert_eq!(detect(b"MSH|^~\\&|"), Some(DataKind::Hl7v2));
        assert_eq!(detect(b"H|\\^&|||"), Some(DataKind::Astm));
        assert_eq!(detect(b" {\"a\":1}"), Some(DataKind::Json));
        assert_eq!(
            detect(b"<?xml version=\"1.0\"?><OBS.R01>"),
            Some(DataKind::Poct1a)
        );
        assert_eq!(detect(b"<order/>"), Some(DataKind::Xml));
        assert_eq!(detect(b"hello"), None);
        assert!(Anonymizer::new(Options::default(), Some(b"short"), None).is_err());
        let random = Anonymizer::new(Options::default(), None, None).unwrap();
        assert!(!random.report().key_supplied);
        assert!(
            Options::from_yaml("free_text: true\nspecimens: false\n")
                .unwrap()
                .free_text
        );
        assert!(Options::from_yaml("colour: red\n").is_err());
    }
}
