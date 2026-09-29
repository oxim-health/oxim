//! The `dicom-scu` destination: a Storage SCU that sends each object with
//! C-STORE over its own association, optionally followed by a storage
//! commitment request.
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

use dicom_core::VR;
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use dicom_ul::pdu::PresentationContextNegotiated;
use oxim_connectors::tls::ClientTlsSettings;
use oxim_core::config::DurationText;
use oxim_core::{DestinationConfig, DestinationConnector, EngineError, SendError, async_trait};
use oxim_store::Delivery;
use serde::Deserialize;
use tracing::debug;

use crate::assoc::{
    ClientAssociation, Failure, Peer, PeerSettings, accepted_context, expect_message, send_message,
};
use crate::dimse::{self, Assembler, Command, StatusKind, command_field, element};
use crate::environment::{CommitmentReport, DicomEnvironment};
use crate::error::DicomError;
use crate::net::settings;
use crate::object;
use crate::part10;
use crate::query::{sequence, text_element};
use crate::scp::parse_commitment_report;
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

fn default_commitment_timeout() -> DurationText {
    DurationText(Duration::from_secs(60))
}

fn default_association_wait() -> DurationText {
    DurationText(Duration::from_secs(5))
}

/// Storage commitment after each C-STORE.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitmentSettings {
    /// How long to wait for the report in all.
    #[serde(default = "default_commitment_timeout")]
    pub timeout: DurationText,
    /// How long to keep the association open for a report on it; after
    /// that the association is released and the report is expected on an
    /// association the archive opens to a `dicom-scp` source with
    /// `storage_commitment: true`.
    #[serde(default = "default_association_wait")]
    pub association_wait: DurationText,
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
    /// Request storage commitment after each C-STORE and deliver only
    /// once the receiver commits to the instance.
    #[serde(default)]
    pub storage_commitment: Option<CommitmentSettings>,
    /// TLS, optionally with a client certificate.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
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
    /// The SOP instance stored.
    pub sop_instance_uid: String,
    /// The storage commitment report, when commitment was requested.
    pub commitment: Option<CommitmentReport>,
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
/// failures (`A9xx`, `Cxxx`, `01xx`, ...) fail the delivery. With storage
/// commitment the instance must also be committed; a failure reason is
/// classified the same way.
pub(crate) fn classify(response: &StoreResponse) -> Result<Option<Vec<u8>>, SendError> {
    let kind = StatusKind::of(response.status);
    if matches!(kind, StatusKind::Success | StatusKind::Warning) {
        let mut body = serde_json::json!({
            "status": format!("{:04X}", response.status),
            "result": if kind == StatusKind::Success { "success" } else { "warning" },
            "comment": response.comment,
            "transfer_syntax": response.transfer_syntax,
        });
        if let Some(report) = &response.commitment {
            let instance = &response.sop_instance_uid;
            if let Some((_, reason)) = report.failed.iter().find(|(uid, _)| uid == instance) {
                let message = format!(
                    "the receiver did not commit to the instance (failure reason {reason:04X})"
                );
                return Err(if failure_is_temporary(*reason) {
                    SendError::temporary(message)
                } else {
                    SendError::permanent(message)
                });
            }
            if !report.committed.contains(instance) {
                return Err(SendError::temporary(
                    "the storage commitment report does not mention the instance",
                ));
            }
            body["committed"] = serde_json::Value::Bool(true);
        }
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
    peer: Peer,
    transcode: bool,
    commitment: Option<(Duration, Duration)>,
    environment: DicomEnvironment,
}

impl DicomScu {
    /// Validates the settings and creates the destination.
    pub fn new(settings: DicomScuSettings) -> Result<Self, EngineError> {
        Self::with_environment(settings, DicomEnvironment::in_memory())
    }

    /// A destination that receives storage commitment reports sent to the
    /// `dicom-scp` sources of `environment`.
    pub fn with_environment(
        settings: DicomScuSettings,
        environment: DicomEnvironment,
    ) -> Result<Self, EngineError> {
        let peer = Peer::new(PeerSettings {
            what: "dicom-scu destination",
            target: &settings.target,
            called_ae_title: &settings.called_ae_title,
            calling_ae_title: &settings.calling_ae_title,
            connect_timeout: settings.connect_timeout.0,
            timeout: settings.timeout.0,
            max_pdu_length: settings.max_pdu_length,
            tls: settings.tls.as_ref(),
        })?;
        Ok(Self {
            peer,
            transcode: settings.transcode,
            commitment: settings
                .storage_commitment
                .map(|commitment| (commitment.timeout.0, commitment.association_wait.0)),
            environment,
        })
    }

    /// Checks the receiver with C-ECHO.
    pub async fn echo(&self) -> Result<(), DicomError> {
        let options = self
            .peer
            .options()
            .with_presentation_context(uids::VERIFICATION, vec![uids::IMPLICIT_VR_LITTLE_ENDIAN]);
        let mut association = self.peer.establish(options).await?;
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
        let target = &self.peer.target;
        let source_ts = meta.transfer_syntax.as_str();
        let mut options = self
            .peer
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
        if self.commitment.is_some() {
            options = options.with_presentation_context(
                uids::STORAGE_COMMITMENT_PUSH,
                vec![
                    uids::EXPLICIT_VR_LITTLE_ENDIAN,
                    uids::IMPLICIT_VR_LITTLE_ENDIAN,
                ],
            );
        }
        let mut association = self.peer.establish(options).await.map_err(|failure| match failure {
            Failure::Permanent(message) if !convertible && !NATIVE.contains(&source_ts) => {
                Failure::Permanent(format!(
                    "{message}; the object is in {source_ts} and OXIM does not decompress pixel data"
                ))
            }
            other => other,
        })?;
        let storage_contexts: Vec<PresentationContextNegotiated> = association
            .presentation_contexts()
            .iter()
            .filter(|pc| {
                pc.abstract_syntax.trim_end_matches(['\0', ' ']) != uids::STORAGE_COMMITMENT_PUSH
            })
            .cloned()
            .collect();
        let Some(chosen) = choose(&storage_contexts, source_ts) else {
            let _ = association.abort().await;
            return Err(Failure::Permanent(format!(
                "{target} accepted no usable presentation context for {}",
                meta.sop_class_uid
            )));
        };
        let data: Cow<'_, [u8]> = if chosen.transfer_syntax == source_ts {
            Cow::Borrowed(parsed.dataset)
        } else if !convertible || !NATIVE.contains(&chosen.transfer_syntax.as_str()) {
            let _ = association.abort().await;
            return Err(Failure::Permanent(format!(
                "{target} accepted {} instead of a proposed transfer syntax",
                chosen.transfer_syntax
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
                });
            match converted {
                Ok(converted) => Cow::Owned(converted),
                Err(failure) => {
                    let _ = association.abort().await;
                    return Err(failure);
                }
            }
        };
        let request = Command::c_store_rq(1, &meta.sop_class_uid, &meta.sop_instance_uid);
        let response = match exchange(&mut association, chosen.id, &request, Some(&data)).await {
            Ok(response) => response,
            Err(failure) => {
                let _ = association.abort().await;
                return Err(failure);
            }
        };
        if response.command_field() != Some(command_field::C_STORE_RSP) {
            let _ = association.abort().await;
            return Err(Failure::Temporary(format!(
                "{target} answered C-STORE with command {:#06x}",
                response.command_field().unwrap_or_default()
            )));
        }
        let Some(status) = response.status() else {
            let _ = association.abort().await;
            return Err(Failure::Temporary(format!(
                "{target} answered without a status"
            )));
        };
        let mut result = StoreResponse {
            status,
            comment: response.text(element::ERROR_COMMENT),
            transfer_syntax: chosen.transfer_syntax,
            sop_instance_uid: meta.sop_instance_uid.clone(),
            commitment: None,
        };
        let stored = matches!(
            StatusKind::of(status),
            StatusKind::Success | StatusKind::Warning
        );
        if let (Some((timeout, association_wait)), true) = (self.commitment, stored) {
            let report = self
                .commit(
                    association,
                    &meta.sop_class_uid,
                    &meta.sop_instance_uid,
                    timeout,
                    association_wait,
                )
                .await?;
            result.commitment = Some(report);
            return Ok(result);
        }
        if let Err(e) = association.release().await {
            debug!(receiver = %target, error = %e, "association release failed after the response");
        }
        Ok(result)
    }

    /// Requests storage commitment for one instance and waits for the
    /// report: first on this association, for `association_wait`, then,
    /// after releasing it, through a `dicom-scp` source with
    /// `storage_commitment: true` that receives the archive's report on an
    /// association the archive opens. The association is released or
    /// aborted before this returns.
    async fn commit(
        &self,
        mut association: ClientAssociation,
        sop_class_uid: &str,
        sop_instance_uid: &str,
        timeout: Duration,
        association_wait: Duration,
    ) -> Result<CommitmentReport, Failure> {
        let target = &self.peer.target;
        let Some(context) = accepted_context(
            association.presentation_contexts(),
            uids::STORAGE_COMMITMENT_PUSH,
        ) else {
            let _ = association.abort().await;
            return Err(Failure::Permanent(format!(
                "{target} does not accept storage commitment requests"
            )));
        };
        let transaction = uids::generate();
        let request = InMemDicomObject::from_element_iter([
            text_element(tags::TRANSACTION_UID, VR::UI, &transaction),
            sequence(
                tags::REFERENCED_SOP_SEQUENCE,
                vec![InMemDicomObject::from_element_iter([
                    text_element(tags::REFERENCED_SOP_CLASS_UID, VR::UI, sop_class_uid),
                    text_element(tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, sop_instance_uid),
                ])],
            ),
        ]);
        let data = match object::write_dataset(&request, &context.transfer_syntax) {
            Ok(data) => data,
            Err(e) => {
                let _ = association.abort().await;
                return Err(Failure::Permanent(e.to_string()));
            }
        };
        let mut waiting = match self.environment.await_commitment(&transaction) {
            Ok(waiting) => waiting,
            Err(e) => {
                let _ = association.abort().await;
                return Err(Failure::Temporary(e.to_string()));
            }
        };
        let deadline = tokio::time::Instant::now() + timeout;
        let on_association = async {
            let action = Command::n_action_rq(
                2,
                uids::STORAGE_COMMITMENT_PUSH,
                uids::STORAGE_COMMITMENT_INSTANCE,
                1,
            );
            let response = exchange(&mut association, context.id, &action, Some(&data)).await?;
            match response.status() {
                Some(status) if StatusKind::of(status) == StatusKind::Success => {}
                Some(status) => {
                    let message =
                        format!("storage commitment request refused with status {status:04X}");
                    return Err(if failure_is_temporary(status) {
                        Failure::Temporary(message)
                    } else {
                        Failure::Permanent(message)
                    });
                }
                None => {
                    return Err(Failure::Temporary(
                        "the storage commitment response has no status".into(),
                    ));
                }
            }
            let same_association = async {
                let mut assembler = Assembler::new(4 * 1024 * 1024);
                loop {
                    let message = expect_message(&mut association, &mut assembler).await?;
                    if message.command.command_field() != Some(command_field::N_EVENT_REPORT_RQ) {
                        continue;
                    }
                    let report = object::read_dataset(&message.data, &context.transfer_syntax)
                        .ok()
                        .and_then(|dataset| parse_commitment_report(&dataset));
                    let status = if report.is_some() {
                        dimse::status::SUCCESS
                    } else {
                        dimse::status::PROCESSING_FAILURE
                    };
                    send_message(
                        &mut association,
                        message.context_id,
                        &Command::response_to(&message.command, status, None),
                        None,
                    )
                    .await
                    .map_err(|e| Failure::Temporary(format!("association failed: {e}")))?;
                    match report {
                        Some((uid, report)) if uid == transaction => return Ok(report),
                        _ => continue,
                    }
                }
            };
            let wait = association_wait.min(timeout);
            let report = tokio::select! {
                report = same_association => Some(report),
                report = &mut waiting => Some(report.map_err(|_| Failure::Temporary("the commitment request was dropped".into()))),
                () = tokio::time::sleep(wait) => None,
            };
            Ok::<_, Failure>(report)
        };
        let outcome = on_association.await;
        // Nothing more arrives on the association: release it (abort after
        // an error) and wait for a report on another association.
        let result = match outcome {
            Ok(Some(report)) => {
                let _ = association.release().await;
                report
            }
            Ok(None) => {
                let _ = association.release().await;
                match tokio::time::timeout_at(deadline, waiting).await {
                    Ok(Ok(report)) => Ok(report),
                    Ok(Err(_)) => Err(Failure::Temporary(
                        "the commitment request was dropped".into(),
                    )),
                    Err(_) => Err(Failure::Temporary(format!(
                        "no storage commitment report from {target} within {timeout:?}"
                    ))),
                }
            }
            Err(failure) => {
                let _ = association.abort().await;
                Err(failure)
            }
        };
        self.environment.forget_commitment(&transaction);
        result
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
pub(crate) async fn exchange(
    association: &mut ClientAssociation,
    context_id: u8,
    command: &Command,
    data: Option<&[u8]>,
) -> Result<Command, Failure> {
    send_message(association, context_id, command, data)
        .await
        .map_err(|e| Failure::Temporary(format!("association failed: {e}")))?;
    let mut assembler = Assembler::new(0);
    let message = expect_message(association, &mut assembler).await?;
    Ok(message.command)
}

#[async_trait]
impl DestinationConnector for DicomScu {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let response = self.store(&delivery.payload).await?;
        classify(&response)
    }
}

/// Registers the `dicom-scu` destination.
pub(crate) fn register(registry: &mut oxim_core::Registry, environment: &DicomEnvironment) {
    let environment = environment.clone();
    registry.add_destination("dicom-scu", move |config: &DestinationConfig| {
        let settings: DicomScuSettings = settings(&config.settings, "dicom-scu destination")?;
        Ok(
            Arc::new(DicomScu::with_environment(settings, environment.clone())?)
                as Arc<dyn DestinationConnector>,
        )
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
            sop_instance_uid: "1.2.3".into(),
            commitment: None,
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
    fn classifies_commitment_reports() {
        let with = |committed: &[&str], failed: &[(&str, u16)]| StoreResponse {
            commitment: Some(CommitmentReport {
                committed: committed.iter().map(|uid| (*uid).to_owned()).collect(),
                failed: failed
                    .iter()
                    .map(|(uid, reason)| ((*uid).to_owned(), *reason))
                    .collect(),
            }),
            ..response(0)
        };
        let body = classify(&with(&["1.2.3"], &[])).unwrap().unwrap();
        assert!(
            String::from_utf8(body)
                .unwrap()
                .contains("\"committed\":true")
        );
        assert!(
            classify(&with(&[], &[("1.2.3", 0x0112)]))
                .unwrap_err()
                .permanent
        );
        assert!(
            !classify(&with(&[], &[("1.2.3", 0x0110)]))
                .unwrap_err()
                .permanent
        );
        assert!(!classify(&with(&["9.9"], &[])).unwrap_err().permanent);
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
            storage_commitment: None,
            tls: None,
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
