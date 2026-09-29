//! Query and retrieve as a requester: `dicom-find` (C-FIND), `dicom-move`
//! (C-MOVE) and `dicom-get` (C-GET) destinations, and the
//! `dicom-retrieved` source that receives what `dicom-get` and
//! `dicomweb-wado` retrieve.
//!
//! Each delivery's payload is the query identifier (see [`crate::query`]),
//! for example produced by a `map` or `script` step from an order:
//! `{"QueryRetrieveLevel": "STUDY", "PatientID": "SYN-0001"}`. Keys of the
//! `keys` setting fill in what the payload leaves out.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use dicom_core::header::Header;
use dicom_object::InMemDicomObject;
use dicom_ul::pdu::PresentationContextResultReason;
use oxim_connectors::tls::ClientTlsSettings;
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConfig, DestinationConnector, EngineError, SendError, SourceConfig,
    SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_store::Delivery;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::assoc::{
    ClientAssociation, Failure, Peer, PeerSettings, accepted_context, expect_message, send_message,
};
use crate::dimse::{Assembler, Command, StatusKind, command_field, element, status};
use crate::environment::DicomEnvironment;
use crate::error::DicomError;
use crate::net::settings;
use crate::object;
use crate::part10::{self, FileMeta};
use crate::query::{self, parse_identifier, to_json};
use crate::scu::failure_is_temporary;
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

fn default_find_timeout() -> DurationText {
    DurationText(Duration::from_secs(60))
}

fn default_retrieve_timeout() -> DurationText {
    DurationText(Duration::from_secs(600))
}

fn default_max_pdu_length() -> u32 {
    16_384
}

fn default_max_results() -> usize {
    1000
}

fn default_max_object_size() -> usize {
    crate::scp::DEFAULT_MAX_OBJECT_SIZE
}

/// A query/retrieve information model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Model {
    /// Study Root.
    #[default]
    StudyRoot,
    /// Patient Root.
    PatientRoot,
    /// Modality Worklist (C-FIND only).
    Worklist,
}

impl Model {
    fn find(self) -> &'static str {
        match self {
            Self::StudyRoot => uids::STUDY_ROOT_FIND,
            Self::PatientRoot => uids::PATIENT_ROOT_FIND,
            Self::Worklist => uids::MODALITY_WORKLIST_FIND,
        }
    }

    fn retrieve(self, get: bool) -> Option<&'static str> {
        match (self, get) {
            (Self::StudyRoot, false) => Some(uids::STUDY_ROOT_MOVE),
            (Self::StudyRoot, true) => Some(uids::STUDY_ROOT_GET),
            (Self::PatientRoot, false) => Some(uids::PATIENT_ROOT_MOVE),
            (Self::PatientRoot, true) => Some(uids::PATIENT_ROOT_GET),
            (Self::Worklist, _) => None,
        }
    }
}

/// Settings of the `dicom-find` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DicomFindSettings {
    /// Address of the SCP, `host:port`.
    pub target: String,
    /// AE title of the SCP.
    #[serde(default = "default_called_ae_title")]
    pub called_ae_title: String,
    /// AE title OXIM presents.
    #[serde(default = "default_calling_ae_title")]
    pub calling_ae_title: String,
    /// Limit for establishing the TCP connection.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: DurationText,
    /// Limit for each network read and write.
    #[serde(default = "default_find_timeout")]
    pub timeout: DurationText,
    /// Largest PDU OXIM accepts, in bytes.
    #[serde(default = "default_max_pdu_length")]
    pub max_pdu_length: u32,
    /// The information model.
    #[serde(default)]
    pub model: Model,
    /// Keys added to every identifier, such as return keys.
    #[serde(default)]
    pub keys: serde_json::Map<String, serde_json::Value>,
    /// Most matches kept; the query is cancelled after that many.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    /// TLS, optionally with a client certificate.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
}

/// The answer to a C-FIND.
#[derive(Debug, Clone, PartialEq)]
pub struct FindResult {
    /// The final status.
    pub status: u16,
    /// The matches.
    pub matches: Vec<InMemDicomObject>,
    /// Whether OXIM cancelled the query after `max_results` matches.
    pub truncated: bool,
}

fn merge(identifier: &mut InMemDicomObject, keys: &InMemDicomObject) {
    for key in keys.iter() {
        if identifier.get(key.header().tag()).is_none() {
            identifier.put(key.clone());
        }
    }
}

fn peer(settings: PeerSettings<'_>) -> Result<Peer, EngineError> {
    Peer::new(settings)
}

/// A C-FIND SCU.
#[derive(Debug, Clone)]
pub struct DicomFind {
    peer: Peer,
    model: Model,
    keys: InMemDicomObject,
    max_results: usize,
}

impl DicomFind {
    /// Validates the settings and creates the SCU.
    pub fn new(settings: DicomFindSettings) -> Result<Self, EngineError> {
        let peer = peer(PeerSettings {
            what: "dicom-find destination",
            target: &settings.target,
            called_ae_title: &settings.called_ae_title,
            calling_ae_title: &settings.calling_ae_title,
            connect_timeout: settings.connect_timeout.0,
            timeout: settings.timeout.0,
            max_pdu_length: settings.max_pdu_length,
            tls: settings.tls.as_ref(),
        })?;
        let keys = query::from_keywords(&settings.keys)
            .map_err(|e| EngineError::Config(format!("dicom-find destination: keys: {e}")))?;
        if settings.max_results == 0 {
            return Err(EngineError::Config(
                "dicom-find destination: max_results must be positive".into(),
            ));
        }
        Ok(Self {
            peer,
            model: settings.model,
            keys,
            max_results: settings.max_results,
        })
    }

    /// Sends a C-FIND with `identifier` (plus the configured keys) and
    /// collects the matches.
    pub async fn find(&self, identifier: &InMemDicomObject) -> Result<FindResult, DicomError> {
        Ok(self.run(identifier.clone()).await?)
    }

    async fn run(&self, mut identifier: InMemDicomObject) -> Result<FindResult, Failure> {
        merge(&mut identifier, &self.keys);
        let sop_class = self.model.find();
        if self.model != Model::Worklist && query::level(&identifier).is_none() {
            return Err(Failure::Permanent(
                "the identifier needs a QueryRetrieveLevel".into(),
            ));
        }
        let options = self.peer.options().with_presentation_context(
            sop_class,
            vec![
                uids::EXPLICIT_VR_LITTLE_ENDIAN,
                uids::IMPLICIT_VR_LITTLE_ENDIAN,
            ],
        );
        let mut association = self.peer.establish(options).await?;
        let result = self
            .exchange(&mut association, sop_class, &identifier)
            .await;
        match &result {
            Ok(_) => {
                let _ = association.release().await;
            }
            Err(_) => {
                let _ = association.abort().await;
            }
        }
        result
    }

    async fn exchange(
        &self,
        association: &mut ClientAssociation,
        sop_class: &str,
        identifier: &InMemDicomObject,
    ) -> Result<FindResult, Failure> {
        let context =
            accepted_context(association.presentation_contexts(), sop_class).ok_or_else(|| {
                Failure::Permanent(format!(
                    "{} does not accept C-FIND of {sop_class}",
                    self.peer.target
                ))
            })?;
        let data = object::write_dataset(identifier, &context.transfer_syntax)
            .map_err(|e| Failure::Permanent(e.to_string()))?;
        send_message(
            association,
            context.id,
            &Command::c_find_rq(1, sop_class),
            Some(&data),
        )
        .await
        .map_err(|e| Failure::Temporary(format!("association failed: {e}")))?;
        let mut assembler = Assembler::new(16 * 1024 * 1024);
        let mut matches = Vec::new();
        let mut truncated = false;
        loop {
            let message = expect_message(association, &mut assembler).await?;
            if message.command.command_field() != Some(command_field::C_FIND_RSP) {
                return Err(Failure::Temporary(format!(
                    "unexpected command {:#06x} during C-FIND",
                    message.command.command_field().unwrap_or_default()
                )));
            }
            let status = message
                .command
                .status()
                .unwrap_or(status::UNABLE_TO_PROCESS);
            if StatusKind::of(status) != StatusKind::Pending {
                return Ok(FindResult {
                    status,
                    matches,
                    truncated,
                });
            }
            if truncated {
                continue;
            }
            if !message.data.is_empty() {
                let found = object::read_dataset(&message.data, &context.transfer_syntax)
                    .map_err(|e| Failure::Temporary(format!("invalid C-FIND match: {e}")))?;
                matches.push(found);
            }
            if matches.len() >= self.max_results {
                truncated = true;
                send_message(association, context.id, &Command::c_cancel_rq(1), None)
                    .await
                    .map_err(|e| Failure::Temporary(format!("association failed: {e}")))?;
            }
        }
    }
}

/// The delivery outcome of a query or retrieve status: success, warnings
/// and cancellation after truncation deliver; `A7xx`, `0110`, `0213` and
/// `FE00` are retried; other failures fail the delivery.
fn outcome(
    operation: &str,
    status: u16,
    body: serde_json::Value,
) -> Result<Option<Vec<u8>>, SendError> {
    match StatusKind::of(status) {
        StatusKind::Success | StatusKind::Warning => Ok(Some(body.to_string().into_bytes())),
        _ => {
            let message = format!("{operation} failed with status {status:04X}");
            if failure_is_temporary(status) {
                Err(SendError::temporary(message))
            } else {
                Err(SendError::permanent(message))
            }
        }
    }
}

#[async_trait]
impl DestinationConnector for DicomFind {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let identifier =
            parse_identifier(&delivery.payload).map_err(|e| SendError::permanent(e.to_string()))?;
        let result = self.run(identifier).await?;
        let matches = result
            .matches
            .iter()
            .map(to_json)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| SendError::permanent(e.to_string()))?;
        let body = serde_json::json!({
            "status": format!("{:04X}", result.status),
            "count": matches.len(),
            "truncated": result.truncated,
            "matches": matches,
        });
        // A query OXIM cancelled after max_results ends with Cancel.
        let status = if result.truncated && result.status == status::CANCEL {
            status::SUCCESS
        } else {
            result.status
        };
        outcome("C-FIND", status, body)
    }
}

/// Settings of the `dicom-move` and `dicom-get` destinations.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DicomRetrieveSettings {
    /// Address of the SCP, `host:port`.
    pub target: String,
    /// AE title of the SCP.
    #[serde(default = "default_called_ae_title")]
    pub called_ae_title: String,
    /// AE title OXIM presents.
    #[serde(default = "default_calling_ae_title")]
    pub calling_ae_title: String,
    /// Limit for establishing the TCP connection.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: DurationText,
    /// Limit for each network read and write, including the wait between
    /// progress responses.
    #[serde(default = "default_retrieve_timeout")]
    pub timeout: DurationText,
    /// Largest PDU OXIM accepts, in bytes.
    #[serde(default = "default_max_pdu_length")]
    pub max_pdu_length: u32,
    /// The information model (`study_root` or `patient_root`).
    #[serde(default)]
    pub model: Model,
    /// Keys added to every identifier.
    #[serde(default)]
    pub keys: serde_json::Map<String, serde_json::Value>,
    /// `dicom-move`: the AE the SCP sends the instances to, usually the
    /// AE title of an OXIM `dicom-scp` source; defaults to
    /// `calling_ae_title`.
    #[serde(default)]
    pub move_destination: Option<String>,
    /// `dicom-get`: the inbox of the `dicom-retrieved` source that stores
    /// the retrieved instances.
    #[serde(default)]
    pub into: Option<String>,
    /// `dicom-get`: storage SOP classes to accept; defaults to the common
    /// storage classes.
    #[serde(default)]
    pub sop_classes: Option<Vec<String>>,
    /// `dicom-get`: largest instance accepted, in bytes.
    #[serde(default = "default_max_object_size")]
    pub max_object_size: usize,
    /// Retry the whole retrieval when some instances failed (status
    /// `B000`), instead of delivering with a warning.
    #[serde(default)]
    pub retry_partial: bool,
    /// TLS, optionally with a client certificate.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
}

/// The progress of a retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Sub-operations remaining.
    pub remaining: u16,
    /// Sub-operations completed.
    pub completed: u16,
    /// Sub-operations failed.
    pub failed: u16,
    /// Sub-operations completed with warnings.
    pub warning: u16,
}

impl Counts {
    fn update(&mut self, command: &Command) {
        let read = |element| command.u16(element);
        if let Some(value) = read(element::REMAINING_SUBOPERATIONS) {
            self.remaining = value;
        }
        if let Some(value) = read(element::COMPLETED_SUBOPERATIONS) {
            self.completed = value;
        }
        if let Some(value) = read(element::FAILED_SUBOPERATIONS) {
            self.failed = value;
        }
        if let Some(value) = read(element::WARNING_SUBOPERATIONS) {
            self.warning = value;
        }
    }
}

/// The transfer syntaxes proposed for instances received with C-GET.
const GET_TRANSFER_SYNTAXES: &[&str] = &[
    uids::EXPLICIT_VR_LITTLE_ENDIAN,
    uids::IMPLICIT_VR_LITTLE_ENDIAN,
    "1.2.840.10008.1.2.4.50",
    "1.2.840.10008.1.2.4.51",
    "1.2.840.10008.1.2.4.57",
    "1.2.840.10008.1.2.4.70",
    "1.2.840.10008.1.2.4.80",
    "1.2.840.10008.1.2.4.81",
    "1.2.840.10008.1.2.4.90",
    "1.2.840.10008.1.2.4.91",
    "1.2.840.10008.1.2.5",
];

/// A C-MOVE or C-GET SCU.
#[derive(Debug, Clone)]
pub struct DicomRetrieve {
    get: bool,
    peer: Peer,
    model: Model,
    keys: InMemDicomObject,
    move_destination: String,
    into: String,
    sop_classes: Vec<String>,
    max_object_size: usize,
    retry_partial: bool,
    environment: DicomEnvironment,
}

impl DicomRetrieve {
    /// Validates the settings of `dicom-move` (`get: false`) or `dicom-get`.
    pub fn new(
        get: bool,
        settings: DicomRetrieveSettings,
        environment: DicomEnvironment,
    ) -> Result<Self, EngineError> {
        let what = if get {
            "dicom-get destination"
        } else {
            "dicom-move destination"
        };
        let config = |message: String| EngineError::Config(format!("{what}: {message}"));
        let peer = peer(PeerSettings {
            what,
            target: &settings.target,
            called_ae_title: &settings.called_ae_title,
            calling_ae_title: &settings.calling_ae_title,
            connect_timeout: settings.connect_timeout.0,
            timeout: settings.timeout.0,
            max_pdu_length: settings.max_pdu_length,
            tls: settings.tls.as_ref(),
        })?;
        if settings.model == Model::Worklist {
            return Err(config("the worklist model has no retrieve".into()));
        }
        let keys =
            query::from_keywords(&settings.keys).map_err(|e| config(format!("keys: {e}")))?;
        let move_destination = settings
            .move_destination
            .clone()
            .unwrap_or_else(|| peer.calling_ae_title.clone());
        crate::net::validate_ae_title("move_destination", &move_destination)?;
        let into = settings.into.clone().unwrap_or_default();
        match (
            get,
            into.trim().is_empty(),
            settings.move_destination.is_some(),
        ) {
            (true, true, _) => {
                return Err(config(
                    "into (the inbox of a dicom-retrieved source) is required".into(),
                ));
            }
            (true, false, true) => {
                return Err(config("move_destination is a setting of dicom-move".into()));
            }
            (false, false, _) => return Err(config("into is a setting of dicom-get".into())),
            _ => {}
        }
        let sop_classes = match &settings.sop_classes {
            None => uids::DEFAULT_STORAGE_SOP_CLASSES
                .iter()
                .map(|uid| (*uid).to_owned())
                .collect(),
            Some(classes) => classes
                .iter()
                .map(|class| {
                    uids::resolve_sop_class(class)
                        .ok_or_else(|| config(format!("unknown SOP class {class:?}")))
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        if get && sop_classes.len() > 120 {
            return Err(config(
                "at most 120 SOP classes fit in one association".into(),
            ));
        }
        Ok(Self {
            get,
            peer,
            model: settings.model,
            keys,
            move_destination: move_destination.trim().to_owned(),
            into: into.trim().to_owned(),
            sop_classes,
            max_object_size: settings.max_object_size,
            retry_partial: settings.retry_partial,
            environment,
        })
    }

    fn operation(&self) -> &'static str {
        if self.get { "C-GET" } else { "C-MOVE" }
    }

    /// Retrieves what `identifier` names; returns the final status and the
    /// counts.
    pub async fn retrieve(
        &self,
        identifier: &InMemDicomObject,
    ) -> Result<(u16, Counts), DicomError> {
        Ok(self.run(identifier.clone()).await?)
    }

    async fn run(&self, mut identifier: InMemDicomObject) -> Result<(u16, Counts), Failure> {
        merge(&mut identifier, &self.keys);
        if query::level(&identifier).is_none() {
            return Err(Failure::Permanent(
                "the identifier needs a QueryRetrieveLevel".into(),
            ));
        }
        let Some(sop_class) = self.model.retrieve(self.get) else {
            return Err(Failure::Permanent("the model has no retrieve".into()));
        };
        let native = vec![
            uids::EXPLICIT_VR_LITTLE_ENDIAN,
            uids::IMPLICIT_VR_LITTLE_ENDIAN,
        ];
        let mut options = self
            .peer
            .options()
            .with_presentation_context(sop_class, native);
        if self.get {
            let syntaxes: Vec<String> = GET_TRANSFER_SYNTAXES
                .iter()
                .map(|ts| (*ts).to_owned())
                .collect();
            for class in &self.sop_classes {
                options = options
                    .with_presentation_context(class.clone(), syntaxes.clone())
                    .with_role_selection(class.clone(), false, true);
            }
        }
        let mut association = self.peer.establish(options).await?;
        let result = self
            .exchange(&mut association, sop_class, &identifier)
            .await;
        match &result {
            Ok(_) => {
                let _ = association.release().await;
            }
            Err(_) => {
                let _ = association.abort().await;
            }
        }
        result
    }

    async fn exchange(
        &self,
        association: &mut ClientAssociation,
        sop_class: &str,
        identifier: &InMemDicomObject,
    ) -> Result<(u16, Counts), Failure> {
        let operation = self.operation();
        let context =
            accepted_context(association.presentation_contexts(), sop_class).ok_or_else(|| {
                Failure::Permanent(format!(
                    "{} does not accept {operation} of {sop_class}",
                    self.peer.target
                ))
            })?;
        let storage: BTreeMap<u8, String> = association
            .presentation_contexts()
            .iter()
            .filter(|pc| {
                pc.reason == PresentationContextResultReason::Acceptance && pc.id != context.id
            })
            .map(|pc| (pc.id, crate::assoc::transfer_syntax(pc)))
            .collect();
        let data = object::write_dataset(identifier, &context.transfer_syntax)
            .map_err(|e| Failure::Permanent(e.to_string()))?;
        let request = if self.get {
            Command::c_get_rq(1, sop_class)
        } else {
            Command::c_move_rq(1, sop_class, &self.move_destination)
        };
        send_message(association, context.id, &request, Some(&data))
            .await
            .map_err(|e| Failure::Temporary(format!("association failed: {e}")))?;
        let response_field = if self.get {
            command_field::C_GET_RSP
        } else {
            command_field::C_MOVE_RSP
        };
        let mut counts = Counts::default();
        let mut assembler = Assembler::new(self.max_object_size);
        loop {
            let message = expect_message(association, &mut assembler).await?;
            let field = message.command.command_field().unwrap_or_default();
            if field == response_field {
                counts.update(&message.command);
                let status = message
                    .command
                    .status()
                    .unwrap_or(status::UNABLE_TO_PROCESS);
                if StatusKind::of(status) != StatusKind::Pending {
                    return Ok((status, counts));
                }
                continue;
            }
            if self.get && field == command_field::C_STORE_RQ {
                let context_id = message.context_id;
                let answer = match storage.get(&context_id) {
                    Some(transfer_syntax) => self.store(message, transfer_syntax).await,
                    None => Command::response_to(
                        &message.command,
                        status::PROCESSING_FAILURE,
                        Some("unknown presentation context"),
                    ),
                };
                send_message(association, context_id, &answer, None)
                    .await
                    .map_err(|e| Failure::Temporary(format!("association failed: {e}")))?;
                continue;
            }
            return Err(Failure::Temporary(format!(
                "unexpected command {field:#06x} during {operation}"
            )));
        }
    }

    /// Stores an instance received with C-GET through the inbox and
    /// answers the C-STORE sub-operation.
    async fn store(&self, message: crate::dimse::Message, transfer_syntax: &str) -> Command {
        let command = &message.command;
        if message.oversized {
            return Command::response_to(
                command,
                status::OUT_OF_RESOURCES,
                Some("the object is too large"),
            );
        }
        let sop_class_uid = command.sop_class_uid().unwrap_or_default();
        let sop_instance_uid = command.sop_instance_uid().unwrap_or_default();
        if sop_class_uid.is_empty() || sop_instance_uid.is_empty() || message.data.is_empty() {
            return Command::response_to(
                command,
                status::PROCESSING_FAILURE,
                Some("the request has no SOP class, instance or data set"),
            );
        }
        let meta = FileMeta {
            sop_class_uid: sop_class_uid.clone(),
            sop_instance_uid: sop_instance_uid.clone(),
            transfer_syntax: transfer_syntax.to_owned(),
            source_ae_title: Some(self.peer.calling_ae_title.clone()),
            sending_ae_title: Some(self.peer.called_ae_title.clone()),
            receiving_ae_title: Some(self.peer.calling_ae_title.clone()),
        };
        let metadata = BTreeMap::from([
            ("dicom.command".to_owned(), "C-GET".to_owned()),
            (
                "dicom.calling_ae".to_owned(),
                self.peer.called_ae_title.clone(),
            ),
            ("dicom.sop_class_uid".to_owned(), sop_class_uid),
            ("dicom.sop_instance_uid".to_owned(), sop_instance_uid),
            (
                "dicom.transfer_syntax".to_owned(),
                transfer_syntax.to_owned(),
            ),
        ]);
        let raw = part10::encode(&meta, &message.data);
        match self
            .environment
            .deliver(
                &self.into,
                raw,
                metadata,
                Some(self.peer.target.clone()),
                self.peer.timeout,
            )
            .await
        {
            Ok(id) => {
                debug!(%id, inbox = %self.into, "C-GET instance stored");
                Command::response_to(command, status::SUCCESS, None)
            }
            Err(failure) => {
                warn!(inbox = %self.into, error = %failure.message(), "C-GET instance not stored");
                Command::response_to(command, status::OUT_OF_RESOURCES, Some(failure.message()))
            }
        }
    }
}

#[async_trait]
impl DestinationConnector for DicomRetrieve {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let identifier =
            parse_identifier(&delivery.payload).map_err(|e| SendError::permanent(e.to_string()))?;
        let (status, counts) = self.run(identifier).await?;
        let body = serde_json::json!({
            "status": format!("{status:04X}"),
            "completed": counts.completed,
            "failed": counts.failed,
            "warning": counts.warning,
            "remaining": counts.remaining,
        });
        if StatusKind::of(status) == StatusKind::Warning && counts.failed > 0 && self.retry_partial
        {
            return Err(SendError::temporary(format!(
                "{} left {} of {} instances unretrieved",
                self.operation(),
                counts.failed,
                counts.failed + counts.completed + counts.warning
            )));
        }
        if status == status::MOVE_DESTINATION_UNKNOWN {
            return Err(SendError::permanent(format!(
                "{} does not know the move destination {}",
                self.peer.target, self.move_destination
            )));
        }
        outcome(self.operation(), status, body)
    }
}

fn default_queue() -> usize {
    16
}

/// Settings of the `dicom-retrieved` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DicomRetrievedSettings {
    /// The inbox name `dicom-get` and `dicomweb-wado` destinations deliver
    /// to.
    pub inbox: String,
    /// How many instances may wait to be stored.
    #[serde(default = "default_queue")]
    pub queue: usize,
}

/// Receives the instances retrieved by `dicom-get` and `dicomweb-wado`
/// destinations and stores them as messages of its channel.
#[derive(Debug, Clone)]
pub struct DicomRetrieved {
    inbox: String,
    queue: usize,
    environment: DicomEnvironment,
}

#[async_trait]
impl SourceConnector for DicomRetrieved {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let mut inbox = self
            .environment
            .open_inbox(&self.inbox, self.queue)
            .map_err(|e| ConnectorError(e.to_string()))?;
        loop {
            let next = tokio::select! {
                () = context.cancelled() => return Ok(()),
                next = inbox.recv() => next,
            };
            let Some(retrieved) = next else {
                return Ok(());
            };
            let stored = context
                .submit(
                    retrieved.object,
                    SubmitInfo {
                        peer: retrieved.peer,
                        metadata: retrieved.metadata,
                        ..SubmitInfo::default()
                    },
                )
                .await
                .map_err(|e| e.to_string());
            let _ = retrieved.stored.send(stored);
        }
    }
}

/// Registers the query and retrieve components.
pub(crate) fn register(registry: &mut oxim_core::Registry, environment: &DicomEnvironment) {
    registry.add_destination("dicom-find", |config: &DestinationConfig| {
        let settings: DicomFindSettings = settings(&config.settings, "dicom-find destination")?;
        Ok(Arc::new(DicomFind::new(settings)?) as Arc<dyn DestinationConnector>)
    });
    for (kind, get) in [("dicom-move", false), ("dicom-get", true)] {
        let environment = environment.clone();
        registry.add_destination(kind, move |config: &DestinationConfig| {
            let settings: DicomRetrieveSettings =
                settings(&config.settings, &format!("{kind} destination"))?;
            Ok(
                Arc::new(DicomRetrieve::new(get, settings, environment.clone())?)
                    as Arc<dyn DestinationConnector>,
            )
        });
    }
    let environment = environment.clone();
    registry.add_source("dicom-retrieved", move |config: &SourceConfig| {
        let settings: DicomRetrievedSettings =
            settings(&config.settings, "dicom-retrieved source")?;
        if settings.inbox.trim().is_empty() {
            return Err(EngineError::Config(
                "dicom-retrieved source: inbox must not be empty".into(),
            ));
        }
        Ok(Arc::new(DicomRetrieved {
            inbox: settings.inbox.trim().to_owned(),
            queue: settings.queue,
            environment: environment.clone(),
        }) as Arc<dyn SourceConnector>)
    });
}
