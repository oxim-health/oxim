//! Acknowledgment messages (`ACK`) and acknowledgment rules.

use encoding_rs::{Encoding, UTF_8};

use crate::error::{ParseError, PathError};
use crate::message::{Message, Version};

/// Acknowledgment codes (HL7 table 0008), written to MSA-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AckCode {
    /// `AA`: the message was processed successfully.
    ApplicationAccept,
    /// `AE`: the message was processed and an error occurred.
    ApplicationError,
    /// `AR`: the message was rejected.
    ApplicationReject,
    /// `CA`: enhanced mode commit accept.
    CommitAccept,
    /// `CE`: enhanced mode commit error.
    CommitError,
    /// `CR`: enhanced mode commit reject.
    CommitReject,
}

impl AckCode {
    /// The two-letter code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApplicationAccept => "AA",
            Self::ApplicationError => "AE",
            Self::ApplicationReject => "AR",
            Self::CommitAccept => "CA",
            Self::CommitError => "CE",
            Self::CommitReject => "CR",
        }
    }

    /// Parses a two-letter code.
    pub fn parse(code: &[u8]) -> Option<Self> {
        Some(match code.trim_ascii() {
            b"AA" => Self::ApplicationAccept,
            b"AE" => Self::ApplicationError,
            b"AR" => Self::ApplicationReject,
            b"CA" => Self::CommitAccept,
            b"CE" => Self::CommitError,
            b"CR" => Self::CommitReject,
            _ => return None,
        })
    }

    /// Whether the code reports success (`AA` or `CA`).
    pub fn is_success(self) -> bool {
        matches!(self, Self::ApplicationAccept | Self::CommitAccept)
    }
}

/// When an acknowledgment must be sent (HL7 table 0155, MSH-15 and MSH-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AckCondition {
    /// `AL`: always.
    Always,
    /// `NE`: never.
    Never,
    /// `ER`: only after an error or rejection.
    ErrorOnly,
    /// `SU`: only after successful completion.
    SuccessOnly,
}

impl AckCondition {
    /// Parses a table 0155 code.
    pub fn parse(code: &[u8]) -> Option<Self> {
        Some(match code.trim_ascii() {
            b"AL" => Self::Always,
            b"NE" => Self::Never,
            b"ER" => Self::ErrorOnly,
            b"SU" => Self::SuccessOnly,
            _ => return None,
        })
    }

    /// Whether an acknowledgment is due for an outcome.
    pub fn applies(self, success: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::ErrorOnly => !success,
            Self::SuccessOnly => success,
        }
    }
}

/// The acknowledgment mode a message requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AckMode {
    /// MSH-15 and MSH-16 are empty: one application acknowledgment
    /// (`AA`/`AE`/`AR`) is always returned.
    Original,
    /// Enhanced mode: an accept (commit) acknowledgment and an application
    /// acknowledgment, each sent according to its condition.
    Enhanced {
        /// MSH-15, conditions for the accept acknowledgment.
        accept: AckCondition,
        /// MSH-16, conditions for the application acknowledgment.
        application: AckCondition,
    },
}

/// The acknowledgment mode requested by `message`.
///
/// When only one of MSH-15 and MSH-16 is valued, or a value is not a table
/// 0155 code, the missing or unknown condition is treated as `AL`, which
/// never loses an acknowledgment the sender may be waiting for.
pub fn requested_mode(message: &Message) -> AckMode {
    let header = message.header();
    let condition = |n| {
        header
            .field(n)
            .map(|value| value.raw().trim_ascii())
            .filter(|raw| !raw.is_empty())
    };
    match (condition(15), condition(16)) {
        (None, None) => AckMode::Original,
        (accept, application) => {
            let parse = |raw: Option<&[u8]>| {
                raw.and_then(AckCondition::parse)
                    .unwrap_or(AckCondition::Always)
            };
            AckMode::Enhanced {
                accept: parse(accept),
                application: parse(application),
            }
        }
    }
}

/// Error severity for ERR-4 (HL7 table 0516).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// `E`: error.
    Error,
    /// `W`: warning.
    Warning,
    /// `I`: information.
    Information,
    /// `F`: fatal error.
    Fatal,
}

impl Severity {
    /// The one-letter code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "E",
            Self::Warning => "W",
            Self::Information => "I",
            Self::Fatal => "F",
        }
    }
}

/// Where an error was found, for ERR-2 (or ERR-1 before HL7 v2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorLocation<'a> {
    /// Segment identifier, for example `PID`.
    pub segment: &'a str,
    /// 1-based occurrence of the segment.
    pub occurrence: usize,
    /// Field number, if the error concerns one field.
    pub field: Option<usize>,
}

/// Error details reported in an ERR segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AckError<'a> {
    /// HL7 table 0357 code, for example `207` (application internal error).
    pub code: &'a str,
    /// Human-readable description.
    pub text: &'a str,
    /// Severity, written to ERR-4 (HL7 v2.5+).
    pub severity: Severity,
    /// Location of the error, if known.
    pub location: Option<ErrorLocation<'a>>,
}

/// Inputs for [`build_ack`] that the caller controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AckOptions<'a> {
    /// MSA-1.
    pub code: AckCode,
    /// MSH-10 of the acknowledgment.
    pub control_id: &'a str,
    /// MSH-7, an HL7 date/time such as `20260929143000+0300`. Supplied by the
    /// caller so this crate never reads the clock.
    pub timestamp: &'a str,
    /// Optional MSA-3 text.
    pub text: Option<&'a str>,
    /// Optional ERR segment.
    pub error: Option<AckError<'a>>,
}

/// Returned when an acknowledgment cannot be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AckBuildError {
    /// The inbound delimiters cannot be reused.
    #[error(transparent)]
    Delimiters(#[from] ParseError),
    /// A value could not be written.
    #[error(transparent)]
    Value(#[from] PathError),
}

/// Builds an acknowledgment for `inbound`.
///
/// The acknowledgment mirrors the inbound delimiters, processing ID (MSH-11),
/// version (MSH-12) and character set (MSH-18), swaps sending and receiving
/// application and facility, echoes the trigger event in MSH-9 and the
/// inbound control ID in MSA-2. HL7 v2.5+ messages (and messages without a
/// version) get the v2.5 ERR layout; older versions get ERR-1.
pub fn build_ack(inbound: &Message, options: &AckOptions<'_>) -> Result<Message, AckBuildError> {
    let delimiters = *inbound.delimiters();
    let version = inbound.version();
    let header = inbound.header();
    let raw = |n: usize| header.field(n).map_or(&[][..], |value| value.raw());
    // An acknowledgment must be possible even for a message whose character
    // set is unknown, so text falls back to UTF-8 and unmappable characters
    // become `?` instead of failing.
    let encoding = inbound.declared_encoding().ok().flatten().unwrap_or(UTF_8);
    let put = |ack: &mut Message, path: &str, text: &str| {
        ack.set_bytes(path, &encode_lossy(text, encoding))
    };

    let mut ack = Message::new(delimiters)?;
    for (target, source) in [(3, 5), (4, 6), (5, 3), (6, 4), (11, 11), (12, 12), (18, 18)] {
        let value = raw(source);
        if !value.is_empty() {
            ack.set_raw(&format!("MSH-{target}"), value)?;
        }
    }
    put(&mut ack, "MSH-7", options.timestamp)?;

    let trigger = header
        .field(9)
        .and_then(|value| value.component(2))
        .map_or(&[][..], |value| value.raw());
    let mut message_type = b"ACK".to_vec();
    if !trigger.is_empty() {
        message_type.push(delimiters.component);
        message_type.extend_from_slice(trigger);
        if version.is_none_or(|v| v >= Version::new(2, 3, 1)) {
            message_type.push(delimiters.component);
            message_type.extend_from_slice(b"ACK");
        }
    }
    ack.set_raw("MSH-9", &message_type)?;
    put(&mut ack, "MSH-10", options.control_id)?;

    let mut msa = ack.push_segment("MSA")?;
    msa.set_bytes("1", options.code.as_str().as_bytes())?;
    msa.set_raw("2", raw(10))?;
    if let Some(text) = options.text {
        put(&mut ack, "MSA-3", text)?;
    }

    if let Some(error) = options.error {
        ack.push_segment("ERR")?;
        let modern = version.is_none_or(|v| v >= Version::new(2, 5, 0));
        let location_field = if modern { "ERR-2" } else { "ERR-1" };
        if let Some(location) = error.location {
            put(&mut ack, &format!("{location_field}.1"), location.segment)?;
            put(
                &mut ack,
                &format!("{location_field}.2"),
                &location.occurrence.to_string(),
            )?;
            if let Some(field) = location.field {
                put(&mut ack, &format!("{location_field}.3"), &field.to_string())?;
            }
        }
        if modern {
            put(&mut ack, "ERR-3.1", error.code)?;
            put(&mut ack, "ERR-3.2", error.text)?;
            put(&mut ack, "ERR-3.3", "HL70357")?;
            put(&mut ack, "ERR-4", error.severity.as_str())?;
        } else if delimiters.subcomponent.is_some() {
            put(&mut ack, "ERR-1.4.1", error.code)?;
            put(&mut ack, "ERR-1.4.2", error.text)?;
            put(&mut ack, "ERR-1.4.3", "HL70357")?;
        } else {
            put(&mut ack, "ERR-1.4", error.code)?;
        }
    }
    Ok(ack)
}

/// Encodes `text`, replacing characters the encoding cannot represent with
/// `?`.
fn encode_lossy(text: &str, encoding: &'static Encoding) -> Vec<u8> {
    let (bytes, _, unmappable) = encoding.encode(text);
    if !unmappable {
        return bytes.into_owned();
    }
    let mut out = Vec::with_capacity(text.len());
    let mut buffer = [0; 4];
    for ch in text.chars() {
        let (bytes, _, unmappable) = encoding.encode(ch.encode_utf8(&mut buffer));
        if unmappable {
            out.push(b'?');
        } else {
            out.extend_from_slice(&bytes);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbound(version: &str, extra_msh: &str) -> Message {
        let text = format!(
            "MSH|^~\\&|ANALYZER|LAB|LIS|HOSP|20260929120000||ORU^R01^ORU_R01|MSG0001|P|{version}{extra_msh}\r\
             PID|1||12345\r"
        );
        Message::parse(text.as_bytes()).unwrap()
    }

    fn options<'a>(code: AckCode) -> AckOptions<'a> {
        AckOptions {
            code,
            control_id: "ACK0001",
            timestamp: "20260929120001",
            text: None,
            error: None,
        }
    }

    #[test]
    fn builds_positive_ack() {
        let ack = build_ack(&inbound("2.5.1", ""), &options(AckCode::ApplicationAccept)).unwrap();
        assert_eq!(
            ack.to_bytes(),
            b"MSH|^~\\&|LIS|HOSP|ANALYZER|LAB|20260929120001||ACK^R01^ACK|ACK0001|P|2.5.1\r\
MSA|AA|MSG0001\r"
        );
    }

    #[test]
    fn omits_message_structure_before_2_3_1() {
        let ack = build_ack(&inbound("2.3", ""), &options(AckCode::ApplicationAccept)).unwrap();
        assert_eq!(ack.get("MSH-9").unwrap(), "ACK^R01");
    }

    #[test]
    fn reports_errors_in_v2_5_layout() {
        let mut opts = options(AckCode::ApplicationError);
        opts.text = Some("Unknown test code");
        opts.error = Some(AckError {
            code: "103",
            text: "Table value not found",
            severity: Severity::Error,
            location: Some(ErrorLocation {
                segment: "OBX",
                occurrence: 2,
                field: Some(3),
            }),
        });
        let ack = build_ack(&inbound("2.5.1", ""), &opts).unwrap();
        let text = String::from_utf8(ack.to_bytes()).unwrap();
        assert!(
            text.contains("\rMSA|AE|MSG0001|Unknown test code\r"),
            "{text}"
        );
        assert!(
            text.ends_with("\rERR||OBX^2^3|103^Table value not found^HL70357|E\r"),
            "{text}"
        );
    }

    #[test]
    fn reports_errors_in_legacy_layout() {
        let mut opts = options(AckCode::ApplicationReject);
        opts.error = Some(AckError {
            code: "200",
            text: "Unsupported message type",
            severity: Severity::Error,
            location: None,
        });
        let ack = build_ack(&inbound("2.3", ""), &opts).unwrap();
        assert!(
            ack.to_bytes()
                .ends_with(b"\rERR|^^^200&Unsupported message type&HL70357\r")
        );
    }

    #[test]
    fn mirrors_character_set_and_escapes_text() {
        let charset = format!("{}8859/9", "|".repeat(6));
        let mut opts = options(AckCode::ApplicationError);
        opts.text = Some("Hatalı ölçüm|kod");
        let ack = build_ack(&inbound("2.5", &charset), &opts).unwrap();
        let msa3 = ack.get("MSA-3").unwrap();
        assert_eq!(msa3.raw(), b"Hatal\xFD \xF6l\xE7\xFCm\\F\\kod");
        assert_eq!(msa3.to_text(encoding_rs::WINDOWS_1254), "Hatalı ölçüm|kod");
    }

    #[test]
    fn acknowledges_messages_with_unknown_or_narrow_charsets() {
        let unknown = format!("{}KLINGON", "|".repeat(6));
        let mut opts = options(AckCode::ApplicationReject);
        opts.text = Some("Unknown character set");
        let ack = build_ack(&inbound("2.5", &unknown), &opts).unwrap();
        assert_eq!(ack.get("MSH-18").unwrap(), "KLINGON");
        assert_eq!(ack.get("MSA-3").unwrap(), "Unknown character set");

        let latin1 = format!("{}8859/1", "|".repeat(6));
        opts.text = Some("Ölçüm şüpheli");
        let ack = build_ack(&inbound("2.5", &latin1), &opts).unwrap();
        assert_eq!(
            ack.get("MSA-3").unwrap().raw(),
            b"\xD6l\xE7\xFCm ?\xFCpheli"
        );
    }

    #[test]
    fn reads_requested_mode() {
        assert_eq!(requested_mode(&inbound("2.5", "")), AckMode::Original);
        assert_eq!(
            requested_mode(&inbound("2.5", "|||AL|ER")),
            AckMode::Enhanced {
                accept: AckCondition::Always,
                application: AckCondition::ErrorOnly
            }
        );
        assert_eq!(
            requested_mode(&inbound("2.5", "||||NE")),
            AckMode::Enhanced {
                accept: AckCondition::Always,
                application: AckCondition::Never
            }
        );
        assert!(AckCondition::ErrorOnly.applies(false));
        assert!(!AckCondition::ErrorOnly.applies(true));
        assert_eq!(AckCode::parse(b"CA"), Some(AckCode::CommitAccept));
        assert!(AckCode::CommitAccept.is_success());
    }
}
