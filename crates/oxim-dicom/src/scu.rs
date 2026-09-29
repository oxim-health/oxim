//! The `dicom-scu` destination: a Storage SCU that sends each object with
//! C-STORE over its own association.
//!
//! The SCU proposes the object's SOP class with its own transfer syntax.
//! For objects in a native (uncompressed) transfer syntax it also proposes
//! Explicit VR Little Endian and Implicit VR Little Endian, and converts
//! the data set when the receiver accepts only those. Compressed pixel data
//! is never decompressed: such objects need a receiver that accepts their
//! transfer syntax.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use dicom_ul::association::AsyncClientAssociation;
use dicom_ul::association::client::ClientAssociationOptions;
use dicom_ul::pdu::{PDataValueType, Pdu, PresentationContextNegotiated};
use oxim_core::config::DurationText;
use oxim_core::{DestinationConfig, DestinationConnector, EngineError, SendError, async_trait};
use oxim_store::Delivery;
use serde::Deserialize;
use tokio::net::TcpStream;
use tracing::debug;

use crate::dimse::{self, Assembler, Command, StatusKind, command_field, element};
use crate::error::DicomError;
use crate::net::{settings, validate_ae_title};
use crate::object;
use crate::part10;
use crate::uids;

fn default_called_ae_title() -> String {
    "ANY-SCP".to_owned()
}

fn default_calling_ae_title() -> String {
    "OXIM".to_owned()
}

fn default_connect_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(60))
}

fn default_max_pdu_length() -> u32 {
    16_384
}

fn yes() -> bool {
    true
}

/// Settings of the `dicom-scu` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DicomScuSettings {
    /// Receiver address, `host:port`.
    pub target: String,
    /// AE title of the receiver.
    #[serde(default = "default_called_ae_title")]
    pub called_ae_title: String,
    /// AE title OXIM presents.
    #[serde(default = "default_calling_ae_title")]
    pub calling_ae_title: String,
    /// Limit for establishing the TCP connection.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: DurationText,
    /// Limit for each network read and write, including the wait for the
    /// C-STORE response.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
    /// Largest PDU OXIM accepts from the receiver, in bytes.
    #[serde(default = "default_max_pdu_length")]
    pub max_pdu_length: u32,
    /// Whether to convert native data sets to Explicit or Implicit VR
    /// Little Endian when the receiver does not accept their transfer
    /// syntax.
    #[serde(default = "yes")]
    pub transcode: bool,
}

/// The native transfer syntaxes the SCU can convert between.
const NATIVE: [&str; 3] = [
    uids::IMPLICIT_VR_LITTLE_ENDIAN,
    uids::EXPLICIT_VR_LITTLE_ENDIAN,
    uids::EXPLICIT_VR_BIG_ENDIAN,
];

/// The receiver's answer to a C-STORE request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreResponse {
    /// The DIMSE status.
    pub status: u16,
    /// The receiver's error comment, if any.
    pub comment: Option<String>,
    /// The transfer syntax the data set was sent in.
    pub transfer_syntax: String,
}

/// Whether a failure status may succeed when retried: out of resources,
/// processing failure, resource limitation, cancel and pending.
pub(crate) fn failure_is_temporary(status: u16) -> bool {
    matches!(
        status,
        0xA700
            ..=0xA7FF
                | dimse::status::PROCESSING_FAILURE
                | dimse::status::RESOURCE_LIMITATION
                | 0xFE00
                | 0xFF00
                | 0xFF01
    )
}

/// How a C-STORE response answers a delivery: success and warning statuses
/// deliver the message, `A7xx`, `0110` and `0213` are retried, other
/// failures (`A9xx`, `Cxxx`, `01xx`, ...) fail the delivery.
pub(crate) fn classify(response: &StoreResponse) -> Result<Option<Vec<u8>>, SendError> {
    let kind = StatusKind::of(response.status);
    if matches!(kind, StatusKind::Success | StatusKind::Warning) {
        let body = serde_json::json!({
            "status": format!("{:04X}", response.status),
            "result": if kind == StatusKind::Success { "success" } else { "warning" },
            "comment": response.comment,
            "transfer_syntax": response.transfer_syntax,
        });
        return Ok(Some(body.to_string().into_bytes()));
    }
    let message = match &response.comment {
        Some(comment) if !comment.is_empty() => {
            format!(
                "C-STORE failed with status {:04X}: {comment}",
                response.status
            )
        }
        _ => format!("C-STORE failed with status {:04X}", response.status),
    };
    if failure_is_temporary(response.status) {
        Err(SendError::temporary(message))
    } else {
        Err(SendError::permanent(message))
    }
}

/// A Storage SCU destination.
#[derive(Debug, Clone)]
pub struct DicomScu {
    target: String,
    called_ae_title: String,
    calling_ae_title: String,
    connect_timeout: Duration,
    timeout: Duration,
    max_pdu_length: u32,
    transcode: bool,
}

/// A failure while storing, before a status was received.
#[derive(Debug)]
enum Failure {
    Temporary(String),
    Permanent(String),
}

impl From<Failure> for SendError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Temporary(message) => SendError::temporary(message),
            Failure::Permanent(message) => SendError::permanent(message),
        }
    }
}

impl From<Failure> for DicomError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Temporary(message) | Failure::Permanent(message) => {
                DicomError::Association(message)
            }
        }
    }
}

impl DicomScu {
    /// Validates the settings and creates the destination.
    pub fn new(settings: DicomScuSettings) -> Result<Self, EngineError> {
        if settings.target.trim().is_empty() {
            return Err(EngineError::Config(
                "dicom-scu destination: target must not be empty".into(),
            ));
        }
        validate_ae_title("called_ae_title", &settings.called_ae_title)?;
        validate_ae_title("calling_ae_title", &settings.calling_ae_title)?;
        Ok(Self {
            target: settings.target.trim().to_owned(),
            called_ae_title: settings.called_ae_title.trim().to_owned(),
            calling_ae_title: settings.calling_ae_title.trim().to_owned(),
            connect_timeout: settings.connect_timeout.0,
            timeout: settings.timeout.0,
            max_pdu_length: settings.max_pdu_length.max(4096),
            transcode: settings.transcode,
        })
    }

    fn options(&self) -> ClientAssociationOptions<'static> {
        ClientAssociationOptions::new()
            .calling_ae_title(self.calling_ae_title.clone())
            .called_ae_title(self.called_ae_title.clone())
            .max_pdu_length(self.max_pdu_length)
            .connection_timeout(self.connect_timeout)
            .read_timeout(self.timeout)
            .write_timeout(self.timeout)
    }

    async fn establish(
        &self,
        options: ClientAssociationOptions<'static>,
    ) -> Result<AsyncClientAssociation<TcpStream>, Failure> {
        let limit = self.connect_timeout + self.timeout;
        match tokio::time::timeout(limit, options.establish_async(self.target.as_str())).await {
            Ok(Ok(association)) => Ok(association),
            Ok(Err(dicom_ul::association::Error::NoAcceptedPresentationContexts { .. })) => {
                Err(Failure::Permanent(format!(
                    "{} accepted none of the proposed presentation contexts",
                    self.target
                )))
            }
            Ok(Err(e)) => Err(Failure::Temporary(format!(
                "association with {} failed: {e}",
                self.target
            ))),
            Err(_) => Err(Failure::Temporary(format!(
                "no association with {} within {:?}",
                self.target, limit
            ))),
        }
    }

    /// Checks the receiver with C-ECHO.
    pub async fn echo(&self) -> Result<(), DicomError> {
        let options = self
            .options()
            .with_presentation_context(uids::VERIFICATION, vec![uids::IMPLICIT_VR_LITTLE_ENDIAN]);
        let mut association = self.establish(options).await?;
        let context_id = association
            .presentation_contexts()
            .first()
            .map(|pc| pc.id)
            .ok_or_else(|| DicomError::Association("no presentation context".into()))?;
        let response = exchange(&mut association, context_id, &Command::c_echo_rq(1), None)
            .await
            .map_err(DicomError::from)?;
        let _ = association.release().await;
        match response.status() {
            Some(dimse::status::SUCCESS) => Ok(()),
            other => Err(DicomError::Status {
                status: other.unwrap_or(0xFFFF),
                message: response
                    .text(element::ERROR_COMMENT)
                    .unwrap_or_else(|| "C-ECHO failed".into()),
            }),
        }
    }

    /// Sends one Part 10 object with C-STORE and returns the receiver's
    /// response. An `Err` means no status was received.
    pub async fn store(&self, object: &[u8]) -> Result<StoreResponse, SendError> {
        Ok(self.store_object(object).await?)
    }

    async fn store_object(&self, object: &[u8]) -> Result<StoreResponse, Failure> {
        let parsed = part10::parse(object).map_err(|e| Failure::Permanent(e.to_string()))?;
        let meta = &parsed.meta;
        if meta.sop_class_uid.is_empty() || meta.sop_instance_uid.is_empty() {
            return Err(Failure::Permanent(
                "the object's meta information has no SOP class or instance UID".into(),
            ));
        }
        let source_ts = meta.transfer_syntax.as_str();
        let mut options = self
            .options()
            .with_presentation_context(meta.sop_class_uid.clone(), vec![source_ts.to_owned()]);
        let convertible = self.transcode && NATIVE.contains(&source_ts);
        if convertible {
            let fallback: Vec<String> = [
                uids::EXPLICIT_VR_LITTLE_ENDIAN,
                uids::IMPLICIT_VR_LITTLE_ENDIAN,
            ]
            .into_iter()
            .filter(|ts| *ts != source_ts)
            .map(str::to_owned)
            .collect();
            options = options.with_presentation_context(meta.sop_class_uid.clone(), fallback);
        }
        let mut association = self.establish(options).await.map_err(|failure| match failure {
            Failure::Permanent(message) if !convertible && !NATIVE.contains(&source_ts) => {
                Failure::Permanent(format!(
                    "{message}; the object is in {source_ts} and OXIM does not decompress pixel data"
                ))
            }
            other => other,
        })?;
        let chosen = choose(association.presentation_contexts(), source_ts).ok_or_else(|| {
            Failure::Permanent(format!(
                "{} accepted no usable presentation context for {}",
                self.target, meta.sop_class_uid
            ))
        })?;
        let data: Cow<'_, [u8]> = if chosen.transfer_syntax == source_ts {
            Cow::Borrowed(parsed.dataset)
        } else if !convertible || !NATIVE.contains(&chosen.transfer_syntax.as_str()) {
            let _ = association.abort().await;
            return Err(Failure::Permanent(format!(
                "{} accepted {} instead of a proposed transfer syntax",
                self.target, chosen.transfer_syntax
            )));
        } else {
            debug!(from = source_ts, to = %chosen.transfer_syntax, "converting the data set");
            let converted = object::read_dataset(parsed.dataset, source_ts)
                .and_then(|dataset| object::write_dataset(&dataset, &chosen.transfer_syntax))
                .map_err(|e| {
                    Failure::Permanent(format!(
                        "cannot convert the data set to {}: {e}",
                        chosen.transfer_syntax
                    ))
                })?;
            Cow::Owned(converted)
        };
        let request = Command::c_store_rq(1, &meta.sop_class_uid, &meta.sop_instance_uid);
        let response = exchange(&mut association, chosen.id, &request, Some(&data)).await;
        let response = match response {
            Ok(response) => response,
            Err(failure) => {
                let _ = association.abort().await;
                return Err(failure);
            }
        };
        if let Err(e) = association.release().await {
            debug!(receiver = %self.target, error = %e, "association release failed after the response");
        }
        if response.command_field() != Some(command_field::C_STORE_RSP) {
            return Err(Failure::Temporary(format!(
                "{} answered C-STORE with command {:#06x}",
                self.target,
                response.command_field().unwrap_or_default()
            )));
        }
        let status = response.status().ok_or_else(|| {
            Failure::Temporary(format!("{} answered without a status", self.target))
        })?;
        Ok(StoreResponse {
            status,
            comment: response.text(element::ERROR_COMMENT),
            transfer_syntax: chosen.transfer_syntax,
        })
    }
}

/// The accepted presentation context to use: the object's own transfer
/// syntax if accepted, otherwise the first accepted fallback.
fn choose(
    contexts: &[PresentationContextNegotiated],
    source_ts: &str,
) -> Option<PresentationContextNegotiated> {
    let accepted: Vec<_> = contexts
        .iter()
        .map(|pc| PresentationContextNegotiated {
            transfer_syntax: pc.transfer_syntax.trim_end_matches(['\0', ' ']).to_owned(),
            ..pc.clone()
        })
        .collect();
    accepted
        .iter()
        .find(|pc| pc.transfer_syntax == source_ts)
        .or_else(|| accepted.first())
        .cloned()
}

/// Sends a command, and its data set if any, and waits for the response.
async fn exchange(
    association: &mut AsyncClientAssociation<TcpStream>,
    context_id: u8,
    command: &Command,
    data: Option<&[u8]>,
) -> Result<Command, Failure> {
    let broken =
        |e: dicom_ul::association::Error| Failure::Temporary(format!("association failed: {e}"));
    let max = association.acceptor_max_pdu_length();
    let encoded = command.encode();
    for pdu in dimse::pdus(context_id, PDataValueType::Command, &encoded, max) {
        association.send(&pdu).await.map_err(broken)?;
    }
    if let Some(data) = data {
        for pdu in dimse::pdus(context_id, PDataValueType::Data, data, max) {
            association.send(&pdu).await.map_err(broken)?;
        }
    }
    let mut assembler = Assembler::new(0);
    loop {
        match association.receive().await.map_err(broken)? {
            Pdu::PData { data } => {
                for value in data {
                    if let Some(message) = assembler
                        .push(value)
                        .map_err(|e| Failure::Temporary(e.to_string()))?
                    {
                        return Ok(message.command);
                    }
                }
            }
            Pdu::AbortRQ { .. } => {
                return Err(Failure::Temporary(
                    "the receiver aborted the association".into(),
                ));
            }
            other => {
                return Err(Failure::Temporary(format!(
                    "unexpected PDU from the receiver: {}",
                    other.short_description()
                )));
            }
        }
    }
}

#[async_trait]
impl DestinationConnector for DicomScu {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let response = self.store(&delivery.payload).await?;
        classify(&response)
    }
}

/// Registers the `dicom-scu` destination.
pub(crate) fn register(registry: &mut oxim_core::Registry) {
    registry.add_destination("dicom-scu", |config: &DestinationConfig| {
        let settings: DicomScuSettings = settings(&config.settings, "dicom-scu destination")?;
        Ok(Arc::new(DicomScu::new(settings)?) as Arc<dyn DestinationConnector>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use dicom_ul::pdu::PresentationContextResultReason;

    fn response(status: u16) -> StoreResponse {
        StoreResponse {
            status,
            comment: Some("detail".into()),
            transfer_syntax: uids::EXPLICIT_VR_LITTLE_ENDIAN.into(),
        }
    }

    #[test]
    fn classifies_statuses() {
        for status in [0x0000, 0x0001, 0xB000, 0xB007] {
            assert!(classify(&response(status)).is_ok(), "{status:04X}");
        }
        for status in [0xA700, 0xA7FF, 0x0110, 0x0213] {
            let error = classify(&response(status)).unwrap_err();
            assert!(!error.permanent, "{status:04X}");
            assert!(error.message.contains("detail"));
        }
        for status in [0xA900, 0xA9FF, 0xC000, 0xCFFF, 0x0122, 0x0124, 0x0211] {
            assert!(
                classify(&response(status)).unwrap_err().permanent,
                "{status:04X}"
            );
        }
        let body = classify(&response(0)).unwrap().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "0000");
        assert_eq!(json["result"], "success");
    }

    #[test]
    fn prefers_the_source_transfer_syntax() {
        let pc = |id: u8, ts: &str| PresentationContextNegotiated {
            id,
            reason: PresentationContextResultReason::Acceptance,
            transfer_syntax: ts.to_owned(),
            abstract_syntax: "1.2".to_owned(),
        };
        let contexts = [
            pc(3, uids::EXPLICIT_VR_LITTLE_ENDIAN),
            pc(1, &format!("{}\0", uids::EXPLICIT_VR_BIG_ENDIAN)),
        ];
        assert_eq!(
            choose(&contexts, uids::EXPLICIT_VR_BIG_ENDIAN).unwrap().id,
            1
        );
        assert_eq!(choose(&contexts, "1.2.840.10008.1.2.4.50").unwrap().id, 3);
        assert!(choose(&[], uids::EXPLICIT_VR_LITTLE_ENDIAN).is_none());
    }

    #[test]
    fn validates_settings() {
        let base = || DicomScuSettings {
            target: "127.0.0.1:104".into(),
            called_ae_title: default_called_ae_title(),
            calling_ae_title: default_calling_ae_title(),
            connect_timeout: default_connect_timeout(),
            timeout: default_timeout(),
            max_pdu_length: default_max_pdu_length(),
            transcode: true,
        };
        assert!(DicomScu::new(base()).is_ok());
        let mut bad = base();
        bad.target = String::new();
        assert!(DicomScu::new(bad).is_err());
        let mut bad = base();
        bad.called_ae_title = "X".repeat(17);
        assert!(DicomScu::new(bad).is_err());
    }
}
