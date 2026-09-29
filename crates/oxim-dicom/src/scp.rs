//! The accepting DICOM components: `dicom-scp` (storage and storage
//! commitment), `dicom-qr-scp` (C-FIND over the instance index),
//! `dicom-mwl-scp` (modality worklist, optionally with MPPS) and
//! `dicom-mpps-scp` (modality performed procedure steps).
//!
//! All of them share one association engine: an association may carry
//! any request of the services its component offers, over plain TCP or
//! TLS. Every stored object and performed procedure step is submitted as a
//! Part 10 object; requests are answered with Success only after the
//! message is stored durably.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use dicom_core::VR;
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use dicom_ul::association::server::{AccessControl, Negotiation, ServerAssociationOptions};
use dicom_ul::pdu::{
    AssociationRJServiceUserReason, Pdu, PresentationContextResultReason, RequestorRoles,
    UserIdentity,
};
use oxim_connectors::tls::ServerTlsSettings;
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, EngineError, SourceConfig, SourceConnector, SourceContext, SubmitInfo,
    async_trait,
};
use oxim_model::MessageId;
use serde::Deserialize;
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::assoc::{Acceptor, Incoming, receive_message, send_message};
use crate::dimse::{Assembler, Command, Message, command_field, status};
use crate::environment::{CommitmentReport, DicomEnvironment};
use crate::index::{IndexedInstance, Level};
use crate::net::{serve, settings, validate_ae_title};
use crate::object::{self, DicomObject};
use crate::part10::{self, FileMeta};
use crate::query::{self, items, sequence, text, text_element};
use crate::scan::{self, Syntax};
use crate::uids;

/// The default bound for one received object: 512 MiB.
pub const DEFAULT_MAX_OBJECT_SIZE: usize = 512 * 1024 * 1024;

/// The largest data set accepted for queries and procedure steps.
const MAX_REQUEST_DATA_SET: usize = 4 * 1024 * 1024;

fn default_ae_title() -> String {
    "OXIM".to_owned()
}

fn yes() -> bool {
    true
}

fn default_max_pdu_length() -> u32 {
    16_384
}

fn default_max_object_size() -> usize {
    DEFAULT_MAX_OBJECT_SIZE
}

fn default_max_associations() -> usize {
    32
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(60))
}

fn default_max_results() -> usize {
    1000
}

/// Settings of the `dicom-scp` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DicomScpSettings {
    /// Address to listen on, for example `0.0.0.0:104`.
    pub listen: String,
    /// The AE title of this SCP.
    #[serde(default = "default_ae_title")]
    pub ae_title: String,
    /// Calling AE titles allowed to associate; empty allows any.
    #[serde(default)]
    pub calling_ae_titles: Vec<String>,
    /// Whether to reject associations whose called AE title is not
    /// `ae_title`.
    #[serde(default = "yes")]
    pub check_called_ae_title: bool,
    /// Accepted SOP classes, as UIDs or keywords such as `CTImageStorage`;
    /// `["*"]` accepts any. Verification is always accepted. Defaults to
    /// the common storage SOP classes.
    #[serde(default)]
    pub sop_classes: Option<Vec<String>>,
    /// Accepted transfer syntax UIDs, in order of preference; empty accepts
    /// any transfer syntax OXIM can read, in the order the caller proposes.
    #[serde(default)]
    pub transfer_syntaxes: Vec<String>,
    /// Largest PDU accepted, in bytes.
    #[serde(default = "default_max_pdu_length")]
    pub max_pdu_length: u32,
    /// Largest data set accepted, in bytes. Objects are held in memory.
    #[serde(default = "default_max_object_size")]
    pub max_object_size: usize,
    /// Most simultaneous associations.
    #[serde(default = "default_max_associations")]
    pub max_associations: usize,
    /// How long to wait for the next PDU of an association.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
    /// Record every stored object in the instance index.
    #[serde(default)]
    pub index: bool,
    /// Offer the Storage Commitment Push Model: commit to objects OXIM has
    /// stored, and accept commitment reports for the requests of
    /// `dicom-scu` destinations. Implies `index`.
    #[serde(default)]
    pub storage_commitment: bool,
    /// TLS, optionally with client certificates.
    #[serde(default)]
    pub tls: Option<ServerTlsSettings>,
}

/// Settings of the `dicom-qr-scp`, `dicom-mwl-scp` and `dicom-mpps-scp`
/// sources.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DicomServiceSettings {
    /// Address to listen on, for example `0.0.0.0:105`.
    pub listen: String,
    /// The AE title of this SCP.
    #[serde(default = "default_ae_title")]
    pub ae_title: String,
    /// Calling AE titles allowed to associate; empty allows any.
    #[serde(default)]
    pub calling_ae_titles: Vec<String>,
    /// Whether to reject associations whose called AE title is not
    /// `ae_title`.
    #[serde(default = "yes")]
    pub check_called_ae_title: bool,
    /// Largest PDU accepted, in bytes.
    #[serde(default = "default_max_pdu_length")]
    pub max_pdu_length: u32,
    /// Most simultaneous associations.
    #[serde(default = "default_max_associations")]
    pub max_associations: usize,
    /// How long to wait for the next PDU of an association.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
    /// Most matches returned for one query (`dicom-qr-scp`,
    /// `dicom-mwl-scp`).
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    /// Store every query as a message, for an audit trail.
    #[serde(default)]
    pub record_queries: bool,
    /// Also accept MPPS on the worklist AE (`dicom-mwl-scp`).
    #[serde(default)]
    pub mpps: bool,
    /// TLS, optionally with client certificates.
    #[serde(default)]
    pub tls: Option<ServerTlsSettings>,
}

/// Accepts associations by calling and called AE title.
#[derive(Debug, Clone)]
struct AeTitles {
    calling: Vec<String>,
    check_called: bool,
}

impl AccessControl for AeTitles {
    fn check_access(
        &self,
        this_ae_title: &str,
        calling_ae_title: &str,
        called_ae_title: &str,
        _user_identity: Option<&UserIdentity>,
    ) -> Result<(), AssociationRJServiceUserReason> {
        if self.check_called && called_ae_title.trim() != this_ae_title.trim() {
            return Err(AssociationRJServiceUserReason::CalledAETitleNotRecognized);
        }
        let calling = calling_ae_title.trim();
        if !self.calling.is_empty() && !self.calling.iter().any(|allowed| allowed == calling) {
            return Err(AssociationRJServiceUserReason::CallingAETitleNotRecognized);
        }
        Ok(())
    }
}

/// Accepts the roles a peer asks for on the Storage Commitment Push Model,
/// so an archive may open an association to send its report (acting as
/// the SCP of the class on an association it requested).
#[derive(Debug, Clone, Copy, Default)]
struct CommitmentRoles;

impl Negotiation for CommitmentRoles {
    fn negotiate_roles(
        &self,
        sop_class_uid: &str,
        scu_role: bool,
        scp_role: bool,
    ) -> Option<RequestorRoles> {
        (sop_class_uid.trim_end_matches(['\0', ' ']) == uids::STORAGE_COMMITMENT_PUSH).then_some(
            RequestorRoles {
                scu: scu_role,
                scp: scp_role,
            },
        )
    }
}

/// The services one accepting component offers.
#[derive(Debug, Clone, Default)]
struct Services {
    storage: bool,
    index: bool,
    commitment: bool,
    query: bool,
    worklist: bool,
    mpps: bool,
    max_results: usize,
    record_queries: bool,
}

impl Services {
    fn names(&self) -> String {
        let mut names = vec!["C-ECHO"];
        if self.storage {
            names.push("C-STORE");
        }
        if self.query || self.worklist {
            names.push("C-FIND");
        }
        if self.mpps {
            names.push("N-CREATE, N-SET (MPPS)");
        }
        if self.commitment {
            names.push("N-ACTION, N-EVENT-REPORT (storage commitment)");
        }
        names.join(", ")
    }
}

/// Top-level attributes copied into the message metadata.
const METADATA_TAGS: &[((u16, u16), &str)] = &[
    ((0x0020, 0x000D), "dicom.study_instance_uid"),
    ((0x0020, 0x000E), "dicom.series_instance_uid"),
    ((0x0008, 0x0060), "dicom.modality"),
];

struct Shared {
    kind: &'static str,
    acceptor: Acceptor<AeTitles, CommitmentRoles>,
    ae_title: String,
    max_object_size: usize,
    services: Services,
    environment: DicomEnvironment,
}

/// An accepting DICOM component.
pub struct DicomScp {
    listen: String,
    max_associations: usize,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for DicomScp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DicomScp")
            .field("kind", &self.shared.kind)
            .field("listen", &self.listen)
            .field("ae_title", &self.shared.ae_title)
            .field("services", &self.shared.services.names())
            .finish_non_exhaustive()
    }
}

/// What every accepting component is configured with.
struct Listener<'a> {
    kind: &'static str,
    listen: &'a str,
    ae_title: &'a str,
    calling_ae_titles: &'a [String],
    check_called_ae_title: bool,
    max_pdu_length: u32,
    max_associations: usize,
    timeout: Duration,
    tls: Option<&'a ServerTlsSettings>,
}

type Options = ServerAssociationOptions<'static, AeTitles, CommitmentRoles>;

impl DicomScp {
    /// Validates the settings of a `dicom-scp` source and creates it.
    pub fn new(settings: DicomScpSettings) -> Result<Self, EngineError> {
        Self::storage(settings, DicomEnvironment::in_memory())
    }

    /// A `dicom-scp` source sharing `environment` with other components.
    pub fn storage(
        settings: DicomScpSettings,
        environment: DicomEnvironment,
    ) -> Result<Self, EngineError> {
        let config = |message: String| EngineError::Config(format!("dicom-scp source: {message}"));
        if settings.max_object_size == 0 {
            return Err(config("max_object_size must be positive".into()));
        }
        let listener = Listener {
            kind: "dicom-scp",
            listen: &settings.listen,
            ae_title: &settings.ae_title,
            calling_ae_titles: &settings.calling_ae_titles,
            check_called_ae_title: settings.check_called_ae_title,
            max_pdu_length: settings.max_pdu_length,
            max_associations: settings.max_associations,
            timeout: settings.timeout.0,
            tls: settings.tls.as_ref(),
        };
        let services = Services {
            storage: true,
            index: settings.index || settings.storage_commitment,
            commitment: settings.storage_commitment,
            ..Services::default()
        };
        let sop_classes = settings.sop_classes.clone();
        let transfer_syntaxes = settings.transfer_syntaxes.clone();
        Self::build(
            listener,
            services,
            settings.max_object_size,
            environment,
            |mut options| {
                match sop_classes.as_deref() {
                    None => {
                        for uid in uids::DEFAULT_STORAGE_SOP_CLASSES {
                            options = options.with_abstract_syntax(*uid);
                        }
                    }
                    Some([any]) if any == "*" => options = options.promiscuous(true),
                    Some(classes) => {
                        if classes.is_empty() {
                            return Err(config("sop_classes must not be empty".into()));
                        }
                        for class in classes {
                            let uid = uids::resolve_sop_class(class)
                                .ok_or_else(|| config(format!("unknown SOP class {class:?}")))?;
                            options = options.with_abstract_syntax(uid);
                        }
                    }
                }
                if settings.storage_commitment {
                    options = options.with_abstract_syntax(uids::STORAGE_COMMITMENT_PUSH);
                }
                for ts in &transfer_syntaxes {
                    Syntax::of(ts).map_err(|e| config(format!("transfer syntax {ts:?}: {e}")))?;
                    options = options.with_transfer_syntax(ts.clone());
                }
                Ok(options)
            },
        )
    }

    /// A `dicom-qr-scp`, `dicom-mwl-scp` or `dicom-mpps-scp` source.
    fn service(
        kind: &'static str,
        settings: DicomServiceSettings,
        environment: DicomEnvironment,
    ) -> Result<Self, EngineError> {
        let config = |message: String| EngineError::Config(format!("{kind} source: {message}"));
        if settings.max_results == 0 {
            return Err(config("max_results must be positive".into()));
        }
        if settings.mpps && kind != "dicom-mwl-scp" {
            return Err(config("mpps is a setting of dicom-mwl-scp".into()));
        }
        let services = Services {
            query: kind == "dicom-qr-scp",
            worklist: kind == "dicom-mwl-scp",
            mpps: kind == "dicom-mpps-scp" || settings.mpps,
            max_results: settings.max_results,
            record_queries: settings.record_queries,
            ..Services::default()
        };
        let listener = Listener {
            kind,
            listen: &settings.listen,
            ae_title: &settings.ae_title,
            calling_ae_titles: &settings.calling_ae_titles,
            check_called_ae_title: settings.check_called_ae_title,
            max_pdu_length: settings.max_pdu_length,
            max_associations: settings.max_associations,
            timeout: settings.timeout.0,
            tls: settings.tls.as_ref(),
        };
        let abstract_syntaxes: Vec<&str> = [
            (services.query, uids::PATIENT_ROOT_FIND),
            (services.query, uids::STUDY_ROOT_FIND),
            (services.worklist, uids::MODALITY_WORKLIST_FIND),
            (services.mpps, uids::MPPS),
        ]
        .into_iter()
        .filter_map(|(enabled, uid)| enabled.then_some(uid))
        .collect();
        Self::build(
            listener,
            services,
            MAX_REQUEST_DATA_SET,
            environment,
            |mut options| {
                for uid in abstract_syntaxes {
                    options = options
                        .with_abstract_syntax(uid)
                        .with_transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                        .with_transfer_syntax(uids::IMPLICIT_VR_LITTLE_ENDIAN);
                }
                Ok(options)
            },
        )
    }

    fn build(
        listener: Listener<'_>,
        services: Services,
        max_object_size: usize,
        environment: DicomEnvironment,
        configure: impl FnOnce(Options) -> Result<Options, EngineError>,
    ) -> Result<Self, EngineError> {
        let kind = listener.kind;
        if listener.listen.trim().is_empty() {
            return Err(EngineError::Config(format!(
                "{kind} source: listen must not be empty"
            )));
        }
        validate_ae_title("ae_title", listener.ae_title)?;
        let ae_title = listener.ae_title.trim().to_owned();
        let mut calling = Vec::with_capacity(listener.calling_ae_titles.len());
        for title in listener.calling_ae_titles {
            validate_ae_title("calling AE title", title)?;
            calling.push(title.trim().to_owned());
        }
        let options = ServerAssociationOptions::new()
            .ae_access_control(AeTitles {
                calling,
                check_called: listener.check_called_ae_title,
            })
            .with_negotiation(CommitmentRoles)
            .ae_title(ae_title.clone())
            .max_pdu_length(listener.max_pdu_length.max(4096))
            .read_timeout(listener.timeout)
            .write_timeout(listener.timeout)
            .with_abstract_syntax(uids::VERIFICATION);
        let options = configure(options)?;
        let acceptor = Acceptor::new(options, listener.tls)?;
        Ok(Self {
            listen: listener.listen.to_owned(),
            max_associations: listener.max_associations,
            shared: Arc::new(Shared {
                kind,
                acceptor,
                ae_title,
                max_object_size,
                services,
                environment,
            }),
        })
    }
}

#[async_trait]
impl SourceConnector for DicomScp {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let shared = self.shared.clone();
        serve(
            &context,
            &self.listen,
            self.max_associations,
            |stream, peer| {
                let shared = shared.clone();
                let context = context.clone();
                async move { shared.association(context, stream, peer).await }
            },
        )
        .await
    }
}

/// The parties of an association.
struct Parties {
    calling: String,
    called: String,
    peer: SocketAddr,
    tls: bool,
}

/// A message to send.
struct Outgoing {
    command: Command,
    data: Option<Vec<u8>>,
}

impl Outgoing {
    fn command(command: Command) -> Self {
        Self {
            command,
            data: None,
        }
    }
}

impl Shared {
    async fn association(&self, context: SourceContext, stream: TcpStream, peer: SocketAddr) {
        let channel = context.channel().clone();
        let established = tokio::select! {
            () = context.cancelled() => return,
            established = self.acceptor.accept(stream) => established,
        };
        let mut association = match established {
            Ok(association) => association,
            Err(e) => {
                info!(%channel, %peer, error = %e, "DICOM association not established");
                return;
            }
        };
        let parties = Parties {
            calling: association.peer_ae_title().trim().to_owned(),
            called: association.called_ae_title().trim().to_owned(),
            peer,
            tls: association.is_tls(),
        };
        let contexts: HashMap<u8, String> = association
            .presentation_contexts()
            .iter()
            .filter(|pc| pc.reason == PresentationContextResultReason::Acceptance)
            .map(|pc| (pc.id, crate::assoc::transfer_syntax(pc)))
            .collect();
        debug!(%channel, %peer, calling = %parties.calling, tls = parties.tls, contexts = contexts.len(), "DICOM association established");
        let message_ids = AtomicU16::new(1);
        let mut assembler = Assembler::new(self.max_object_size);
        loop {
            let received = tokio::select! {
                () = context.cancelled() => None,
                incoming = receive_message(&mut association, &mut assembler) => Some(incoming),
            };
            let message = match received {
                None => {
                    let _ = association.abort().await;
                    return;
                }
                Some(Ok(Incoming::Message(message))) => message,
                Some(Ok(Incoming::Release)) => {
                    let _ = crate::assoc::Link::send(&mut association, &Pdu::ReleaseRP).await;
                    return;
                }
                Some(Ok(Incoming::Abort)) => {
                    debug!(%channel, %peer, "DICOM association aborted by the peer");
                    return;
                }
                Some(Err(e)) => {
                    debug!(%channel, %peer, error = %e, "DICOM association ended");
                    let _ = association.abort().await;
                    return;
                }
            };
            let Some(transfer_syntax) = contexts.get(&message.context_id).cloned() else {
                warn!(%channel, %peer, context = message.context_id, "message on a presentation context that was not accepted; aborting the association");
                let _ = association.abort().await;
                return;
            };
            let context_id = message.context_id;
            let outgoing = self
                .respond(&context, message, &transfer_syntax, &parties, &message_ids)
                .await;
            for message in outgoing {
                if let Err(e) = send_message(
                    &mut association,
                    context_id,
                    &message.command,
                    message.data.as_deref(),
                )
                .await
                {
                    debug!(%channel, %peer, error = %e, "cannot send the DIMSE message");
                    return;
                }
            }
        }
    }

    /// The messages that answer a request.
    async fn respond(
        &self,
        context: &SourceContext,
        message: Message,
        transfer_syntax: &str,
        parties: &Parties,
        message_ids: &AtomicU16,
    ) -> Vec<Outgoing> {
        let Some(field) = message.command.command_field() else {
            return Vec::new();
        };
        let unsupported = |command: &Command| {
            vec![Outgoing::command(Command::response_to(
                command,
                status::UNRECOGNIZED_OPERATION,
                Some(&format!("this AE supports {}", self.services.names())),
            ))]
        };
        match field {
            command_field::C_ECHO_RQ => vec![Outgoing::command(Command::response_to(
                &message.command,
                status::SUCCESS,
                None,
            ))],
            command_field::C_STORE_RQ if self.services.storage => {
                vec![Outgoing::command(
                    self.store(context, message, transfer_syntax, parties).await,
                )]
            }
            command_field::C_FIND_RQ if self.services.query || self.services.worklist => {
                self.find(context, message, transfer_syntax, parties).await
            }
            command_field::N_CREATE_RQ | command_field::N_SET_RQ if self.services.mpps => {
                vec![Outgoing::command(
                    self.performed_step(context, message, transfer_syntax, parties)
                        .await,
                )]
            }
            command_field::N_ACTION_RQ if self.services.commitment => {
                self.commit(message, transfer_syntax, message_ids)
            }
            command_field::N_EVENT_REPORT_RQ if self.services.commitment => {
                vec![Outgoing::command(
                    self.commitment_report(&message, transfer_syntax),
                )]
            }
            command_field::C_CANCEL_RQ => Vec::new(),
            field if field & 0x8000 == 0 => unsupported(&message.command),
            // A response from the peer, such as the answer to a commitment
            // report OXIM sent.
            _ => Vec::new(),
        }
    }

    async fn store(
        &self,
        context: &SourceContext,
        message: Message,
        transfer_syntax: &str,
        parties: &Parties,
    ) -> Command {
        let channel = context.channel();
        let command = message.command;
        let failure = |status: u16, comment: &str| {
            warn!(%channel, peer = %parties.peer, calling = %parties.calling, status = format_args!("{status:#06x}"), comment, "C-STORE refused");
            Command::response_to(&command, status, Some(comment))
        };
        if message.oversized {
            return failure(
                status::OUT_OF_RESOURCES,
                &format!("the object exceeds {} bytes", self.max_object_size),
            );
        }
        let sop_class_uid = command.sop_class_uid().unwrap_or_default();
        let sop_instance_uid = command.sop_instance_uid().unwrap_or_default();
        if sop_class_uid.is_empty() || sop_instance_uid.is_empty() {
            return failure(
                status::PROCESSING_FAILURE,
                "the request has no affected SOP class or instance UID",
            );
        }
        if message.data.is_empty() {
            return failure(status::PROCESSING_FAILURE, "the data set is empty");
        }
        let mut metadata = self.metadata(parties, "C-STORE", &sop_class_uid, &sop_instance_uid);
        metadata.insert(
            "dicom.transfer_syntax".to_owned(),
            transfer_syntax.to_owned(),
        );
        // Best effort: the object is stored as received even if it cannot
        // be walked.
        let tags: Vec<(u16, u16)> = METADATA_TAGS.iter().map(|(tag, _)| *tag).collect();
        if let Ok(values) = Syntax::of(transfer_syntax)
            .and_then(|syntax| scan::top_level_text(&message.data, syntax, &tags))
        {
            for (tag, value) in values {
                if let Some((_, key)) = METADATA_TAGS.iter().find(|(known, _)| *known == tag) {
                    metadata.insert((*key).to_owned(), value);
                }
            }
        }
        let meta = FileMeta {
            sop_class_uid,
            sop_instance_uid,
            transfer_syntax: transfer_syntax.to_owned(),
            source_ae_title: Some(self.ae_title.clone()),
            sending_ae_title: Some(parties.calling.clone()),
            receiving_ae_title: Some(parties.called.clone()),
        };
        let raw = part10::encode(&meta, &message.data);
        drop(message.data);
        // The index needs the decoded attributes; decode before the bytes
        // move into the store.
        let indexed = self
            .services
            .index
            .then(|| DicomObject::parse(&raw).map(|object| IndexedInstance::from_object(&object)));
        match self.submit(context, raw, parties, metadata).await {
            Ok(id) => {
                debug!(%channel, %id, calling = %parties.calling, "C-STORE stored");
                if let Some(indexed) = indexed {
                    self.index(indexed, id, &meta, context);
                }
                Command::response_to(&command, status::SUCCESS, None)
            }
            Err(e @ (EngineError::Store(_) | EngineError::ShuttingDown)) => failure(
                status::OUT_OF_RESOURCES,
                &format!("the object could not be stored: {e}"),
            ),
            Err(e) => failure(
                status::PROCESSING_FAILURE,
                &format!("the object could not be accepted: {e}"),
            ),
        }
    }

    /// Records a stored object in the instance index. A failure is logged:
    /// the object is stored, only the index misses it.
    fn index(
        &self,
        indexed: Result<IndexedInstance, crate::error::DicomError>,
        id: MessageId,
        meta: &FileMeta,
        context: &SourceContext,
    ) {
        let recorded = indexed.and_then(|mut instance| {
            instance.message_id = Some(id.to_string());
            self.environment
                .index()
                .and_then(|index| index.record(&instance, context.now()))
        });
        if let Err(e) = recorded {
            warn!(channel = %context.channel(), %id, instance = %meta.sop_instance_uid, error = %e, "the stored object could not be indexed");
        }
    }

    fn metadata(
        &self,
        parties: &Parties,
        command: &str,
        sop_class_uid: &str,
        sop_instance_uid: &str,
    ) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("dicom.command".to_owned(), command.to_owned()),
            ("dicom.calling_ae".to_owned(), parties.calling.clone()),
            ("dicom.called_ae".to_owned(), parties.called.clone()),
            ("dicom.sop_class_uid".to_owned(), sop_class_uid.to_owned()),
            (
                "dicom.sop_instance_uid".to_owned(),
                sop_instance_uid.to_owned(),
            ),
        ])
    }

    async fn submit(
        &self,
        context: &SourceContext,
        raw: Vec<u8>,
        parties: &Parties,
        metadata: BTreeMap<String, String>,
    ) -> Result<MessageId, EngineError> {
        context
            .submit(
                raw,
                SubmitInfo {
                    peer: Some(parties.peer.to_string()),
                    metadata,
                    ..SubmitInfo::default()
                },
            )
            .await
    }

    /// Answers a C-FIND with one pending response per match, then Success.
    async fn find(
        &self,
        context: &SourceContext,
        message: Message,
        transfer_syntax: &str,
        parties: &Parties,
    ) -> Vec<Outgoing> {
        let command = &message.command;
        let failed = |status: u16, comment: &str| {
            warn!(channel = %context.channel(), peer = %parties.peer, status = format_args!("{status:#06x}"), comment, "C-FIND failed");
            vec![Outgoing::command(Command::response_to(
                command,
                status,
                Some(comment),
            ))]
        };
        if message.oversized {
            return failed(status::UNABLE_TO_PROCESS, "the identifier is too large");
        }
        let sop_class = command.sop_class_uid().unwrap_or_default();
        let identifier = match object::read_dataset(&message.data, transfer_syntax) {
            Ok(identifier) => identifier,
            Err(e) => return failed(status::UNABLE_TO_PROCESS, &e.to_string()),
        };
        let now = context.now();
        let max = self.services.max_results;
        let found = match sop_class.as_str() {
            uids::MODALITY_WORKLIST_FIND if self.services.worklist => self
                .environment
                .worklist()
                .and_then(|worklist| worklist.find(&identifier, now, max)),
            uids::STUDY_ROOT_FIND | uids::PATIENT_ROOT_FIND if self.services.query => {
                let level = query::level(&identifier)
                    .and_then(|level| Level::parse(&level))
                    .filter(|level| {
                        sop_class == uids::PATIENT_ROOT_FIND || *level != Level::Patient
                    });
                let Some(level) = level else {
                    return failed(
                        status::IDENTIFIER_DOES_NOT_MATCH_SOP_CLASS,
                        "the identifier needs a Query/Retrieve Level valid for the information model",
                    );
                };
                self.environment
                    .index()
                    .and_then(|index| index.find(level, &identifier, max))
            }
            _ => {
                return failed(
                    status::SOP_CLASS_NOT_SUPPORTED,
                    &format!("C-FIND of {sop_class} is not supported by this AE"),
                );
            }
        };
        let matches = match found {
            Ok(matches) => matches,
            Err(e) => return failed(status::UNABLE_TO_PROCESS, &e.to_string()),
        };
        debug!(channel = %context.channel(), peer = %parties.peer, matches = matches.len(), "C-FIND answered");
        if self.services.record_queries {
            self.record_query(context, parties, &sop_class, &identifier, matches.len())
                .await;
        }
        let mut outgoing = Vec::with_capacity(matches.len() + 1);
        for mut response in matches {
            response.put(text_element(
                tags::SPECIFIC_CHARACTER_SET,
                VR::CS,
                "ISO_IR 192",
            ));
            match object::write_dataset(&response, transfer_syntax) {
                Ok(data) => outgoing.push(Outgoing {
                    command: Command::response_to(command, status::PENDING, None).with_data_set(),
                    data: Some(data),
                }),
                Err(e) => return failed(status::UNABLE_TO_PROCESS, &e.to_string()),
            }
        }
        outgoing.push(Outgoing::command(Command::response_to(
            command,
            status::SUCCESS,
            None,
        )));
        outgoing
    }

    async fn record_query(
        &self,
        context: &SourceContext,
        parties: &Parties,
        sop_class: &str,
        identifier: &InMemDicomObject,
        matches: usize,
    ) {
        let uid = uids::generate();
        let Ok(data) = object::write_dataset(identifier, uids::EXPLICIT_VR_LITTLE_ENDIAN) else {
            return;
        };
        let meta = FileMeta {
            sop_class_uid: sop_class.to_owned(),
            sop_instance_uid: uid.clone(),
            transfer_syntax: uids::EXPLICIT_VR_LITTLE_ENDIAN.to_owned(),
            source_ae_title: Some(self.ae_title.clone()),
            sending_ae_title: Some(parties.calling.clone()),
            receiving_ae_title: Some(parties.called.clone()),
        };
        let mut metadata = self.metadata(parties, "C-FIND", sop_class, &uid);
        metadata.insert("dicom.matches".to_owned(), matches.to_string());
        if let Err(e) = self
            .submit(context, part10::encode(&meta, &data), parties, metadata)
            .await
        {
            warn!(channel = %context.channel(), error = %e, "the query could not be recorded");
        }
    }

    /// N-CREATE and N-SET of a modality performed procedure step.
    async fn performed_step(
        &self,
        context: &SourceContext,
        message: Message,
        transfer_syntax: &str,
        parties: &Parties,
    ) -> Command {
        let command = &message.command;
        let field = command.command_field().unwrap_or_default();
        let name = if field == command_field::N_CREATE_RQ {
            "N-CREATE"
        } else {
            "N-SET"
        };
        let refuse = |status: u16, comment: &str| {
            warn!(channel = %context.channel(), peer = %parties.peer, status = format_args!("{status:#06x}"), comment, "MPPS {name} refused");
            Command::response_to(command, status, Some(comment))
        };
        if command.sop_class_uid().as_deref() != Some(uids::MPPS) {
            return refuse(
                status::SOP_CLASS_NOT_SUPPORTED,
                "only Modality Performed Procedure Step is supported",
            );
        }
        if message.oversized {
            return refuse(
                status::PROCESSING_FAILURE,
                "the attribute list is too large",
            );
        }
        let attributes = match object::read_dataset(&message.data, transfer_syntax) {
            Ok(attributes) => attributes,
            Err(e) => return refuse(status::PROCESSING_FAILURE, &e.to_string()),
        };
        let worklist = match self.environment.worklist() {
            Ok(worklist) => worklist,
            Err(e) => return refuse(status::RESOURCE_LIMITATION, &e.to_string()),
        };
        let (uid, checked) = if field == command_field::N_CREATE_RQ {
            // The SCU may leave the instance UID to the SCP.
            let uid = command.sop_instance_uid().unwrap_or_else(uids::generate);
            let checked = worklist.check_create(&uid, &attributes);
            (uid, checked)
        } else {
            let Some(uid) = command.sop_instance_uid() else {
                return refuse(status::NO_SUCH_SOP_INSTANCE, "no SOP instance named");
            };
            let checked = worklist.check_set(&uid, &attributes);
            (uid, checked)
        };
        let dataset = match checked {
            Ok(dataset) => dataset,
            Err(refusal) => return refuse(refusal.status, &refusal.comment),
        };
        let data = match object::write_dataset(&dataset, uids::EXPLICIT_VR_LITTLE_ENDIAN) {
            Ok(data) => data,
            Err(e) => return refuse(status::PROCESSING_FAILURE, &e.to_string()),
        };
        let meta = FileMeta {
            sop_class_uid: uids::MPPS.to_owned(),
            sop_instance_uid: uid.clone(),
            transfer_syntax: uids::EXPLICIT_VR_LITTLE_ENDIAN.to_owned(),
            source_ae_title: Some(self.ae_title.clone()),
            sending_ae_title: Some(parties.calling.clone()),
            receiving_ae_title: Some(parties.called.clone()),
        };
        let mut metadata = self.metadata(parties, name, uids::MPPS, &uid);
        if let Some(state) = crate::worklist::performed_status(&dataset) {
            metadata.insert("dicom.mpps_status".to_owned(), state);
        }
        let accessions: Vec<String> = items(&dataset, tags::SCHEDULED_STEP_ATTRIBUTES_SEQUENCE)
            .iter()
            .filter_map(|item| text(item, tags::ACCESSION_NUMBER))
            .collect();
        if !accessions.is_empty() {
            metadata.insert("dicom.accession_number".to_owned(), accessions.join("\\"));
        }
        match self
            .submit(context, part10::encode(&meta, &data), parties, metadata)
            .await
        {
            Ok(id) => {
                if let Err(e) = worklist.save_performed_step(&uid, &dataset, context.now()) {
                    warn!(channel = %context.channel(), %id, error = %e, "the performed procedure step was stored but its state could not be saved");
                    return refuse(status::PROCESSING_FAILURE, &e.to_string());
                }
                debug!(channel = %context.channel(), %id, instance = %uid, "MPPS {name} stored");
                Command::response_to(command, status::SUCCESS, None)
                    .with_uid(crate::dimse::element::AFFECTED_SOP_INSTANCE_UID, &uid)
            }
            Err(e) => refuse(
                status::RESOURCE_LIMITATION,
                &format!("the performed procedure step could not be stored: {e}"),
            ),
        }
    }

    /// Storage commitment request (N-ACTION): answers it, then reports on
    /// the same association which instances OXIM holds.
    fn commit(
        &self,
        message: Message,
        transfer_syntax: &str,
        message_ids: &AtomicU16,
    ) -> Vec<Outgoing> {
        let command = &message.command;
        let refuse = |status: u16, comment: &str| {
            vec![Outgoing::command(Command::response_to(
                command,
                status,
                Some(comment),
            ))]
        };
        if command.sop_class_uid().as_deref() != Some(uids::STORAGE_COMMITMENT_PUSH) {
            return refuse(
                status::SOP_CLASS_NOT_SUPPORTED,
                "not a storage commitment request",
            );
        }
        if command.u16(crate::dimse::element::ACTION_TYPE_ID) != Some(1) {
            return refuse(
                status::NO_SUCH_ACTION_TYPE,
                "only action type 1 is supported",
            );
        }
        let request = match object::read_dataset(&message.data, transfer_syntax) {
            Ok(request) => request,
            Err(e) => return refuse(status::PROCESSING_FAILURE, &e.to_string()),
        };
        let Some(transaction) = text(&request, tags::TRANSACTION_UID) else {
            return refuse(
                status::MISSING_ATTRIBUTE_VALUE,
                "Transaction UID is missing",
            );
        };
        let index = match self.environment.index() {
            Ok(index) => index,
            Err(e) => return refuse(status::PROCESSING_FAILURE, &e.to_string()),
        };
        let mut committed = Vec::new();
        let mut failed = Vec::new();
        for item in items(&request, tags::REFERENCED_SOP_SEQUENCE) {
            let class = text(item, tags::REFERENCED_SOP_CLASS_UID).unwrap_or_default();
            let instance = text(item, tags::REFERENCED_SOP_INSTANCE_UID).unwrap_or_default();
            let reason = match index.sop_class(&instance) {
                Ok(Some(stored)) if stored == class => None,
                Ok(Some(_)) => Some(0x0119u16),
                Ok(None) => Some(status::NO_SUCH_SOP_INSTANCE),
                Err(_) => Some(status::PROCESSING_FAILURE),
            };
            let mut reference = InMemDicomObject::from_element_iter([
                text_element(tags::REFERENCED_SOP_CLASS_UID, VR::UI, &class),
                text_element(tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, &instance),
            ]);
            match reason {
                None => {
                    reference.put(text_element(
                        tags::RETRIEVE_AE_TITLE,
                        VR::AE,
                        &self.ae_title,
                    ));
                    committed.push(reference);
                }
                Some(reason) => {
                    reference.put(dicom_core::DataElement::new(
                        tags::FAILURE_REASON,
                        VR::US,
                        dicom_core::PrimitiveValue::from(reason),
                    ));
                    failed.push(reference);
                }
            }
        }
        let event_type = if failed.is_empty() { 1 } else { 2 };
        let mut report = InMemDicomObject::from_element_iter([text_element(
            tags::TRANSACTION_UID,
            VR::UI,
            &transaction,
        )]);
        if !committed.is_empty() {
            report.put(sequence(tags::REFERENCED_SOP_SEQUENCE, committed));
        }
        if !failed.is_empty() {
            report.put(sequence(tags::FAILED_SOP_SEQUENCE, failed));
        }
        let Ok(data) = object::write_dataset(&report, transfer_syntax) else {
            return refuse(status::PROCESSING_FAILURE, "cannot encode the report");
        };
        let message_id = message_ids.fetch_add(1, Ordering::Relaxed);
        vec![
            Outgoing::command(Command::response_to(command, status::SUCCESS, None)),
            Outgoing {
                command: Command::n_event_report_rq(
                    message_id,
                    uids::STORAGE_COMMITMENT_PUSH,
                    uids::STORAGE_COMMITMENT_INSTANCE,
                    event_type,
                ),
                data: Some(data),
            },
        ]
    }

    /// A storage commitment report for a request of a `dicom-scu`
    /// destination, arriving on an association the archive opened.
    fn commitment_report(&self, message: &Message, transfer_syntax: &str) -> Command {
        let command = &message.command;
        let report = object::read_dataset(&message.data, transfer_syntax)
            .map_err(|e| e.to_string())
            .and_then(|dataset| {
                parse_commitment_report(&dataset).ok_or_else(|| "Transaction UID is missing".into())
            });
        match report {
            Ok((transaction, report)) => {
                if !self.environment.complete_commitment(&transaction, report) {
                    debug!(%transaction, "storage commitment report without a waiting request");
                }
                Command::response_to(command, status::SUCCESS, None)
            }
            Err(e) => Command::response_to(command, status::PROCESSING_FAILURE, Some(&e)),
        }
    }
}

/// The transaction and the per-instance outcome of a commitment report.
pub(crate) fn parse_commitment_report(
    dataset: &InMemDicomObject,
) -> Option<(String, CommitmentReport)> {
    let transaction = text(dataset, tags::TRANSACTION_UID)?;
    let committed = items(dataset, tags::REFERENCED_SOP_SEQUENCE)
        .iter()
        .filter_map(|item| text(item, tags::REFERENCED_SOP_INSTANCE_UID))
        .collect();
    let failed = items(dataset, tags::FAILED_SOP_SEQUENCE)
        .iter()
        .filter_map(|item| {
            let instance = text(item, tags::REFERENCED_SOP_INSTANCE_UID)?;
            let reason = text(item, tags::FAILURE_REASON)
                .and_then(|reason| reason.parse().ok())
                .unwrap_or(status::PROCESSING_FAILURE);
            Some((instance, reason))
        })
        .collect();
    Some((transaction, CommitmentReport { committed, failed }))
}

/// Registers the accepting components.
pub(crate) fn register(registry: &mut oxim_core::Registry, environment: &DicomEnvironment) {
    let storage = environment.clone();
    registry.add_source("dicom-scp", move |config: &SourceConfig| {
        let settings: DicomScpSettings = settings(&config.settings, "dicom-scp source")?;
        Ok(Arc::new(DicomScp::storage(settings, storage.clone())?) as Arc<dyn SourceConnector>)
    });
    for kind in ["dicom-qr-scp", "dicom-mwl-scp", "dicom-mpps-scp"] {
        let environment = environment.clone();
        registry.add_source(kind, move |config: &SourceConfig| {
            let settings: DicomServiceSettings =
                settings(&config.settings, &format!("{kind} source"))?;
            Ok(
                Arc::new(DicomScp::service(kind, settings, environment.clone())?)
                    as Arc<dyn SourceConnector>,
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> DicomScpSettings {
        settings(
            &serde_json::json!({"listen": "127.0.0.1:0"})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            "test",
        )
        .unwrap()
    }

    #[test]
    fn validates_settings() {
        assert!(DicomScp::new(base()).is_ok());
        let mut bad = base();
        bad.ae_title = "WAY-TOO-LONG-AE-TITLE".into();
        assert!(DicomScp::new(bad).is_err());
        let mut bad = base();
        bad.calling_ae_titles = vec![String::new()];
        assert!(DicomScp::new(bad).is_err());
        let mut classes = base();
        classes.sop_classes = Some(vec!["CTImageStorage".into(), "1.2.3.4".into()]);
        assert!(DicomScp::new(classes).is_ok());
        let mut bad = base();
        bad.sop_classes = Some(vec!["NotAStorageClass".into()]);
        assert!(DicomScp::new(bad).is_err());
        let mut any = base();
        any.sop_classes = Some(vec!["*".into()]);
        assert!(DicomScp::new(any).is_ok());
        let mut bad = base();
        bad.transfer_syntaxes = vec!["1.2.840.10008.1.2.1.99".into()];
        assert!(DicomScp::new(bad).is_err());
        let mut bad = base();
        bad.listen = " ".into();
        assert!(DicomScp::new(bad).is_err());
    }

    #[test]
    fn checks_ae_titles() {
        let open = AeTitles {
            calling: Vec::new(),
            check_called: false,
        };
        assert!(open.check_access("OXIM", "ANY", "OTHER", None).is_ok());
        let strict = AeTitles {
            calling: vec!["CT1".into()],
            check_called: true,
        };
        assert!(strict.check_access("OXIM", "CT1", "OXIM ", None).is_ok());
        assert_eq!(
            strict.check_access("OXIM", "CT2", "OXIM", None),
            Err(AssociationRJServiceUserReason::CallingAETitleNotRecognized)
        );
        assert_eq!(
            strict.check_access("OXIM", "CT1", "PACS", None),
            Err(AssociationRJServiceUserReason::CalledAETitleNotRecognized)
        );
    }

    #[test]
    fn negotiates_commitment_roles_only() {
        assert_eq!(
            CommitmentRoles.negotiate_roles(uids::STORAGE_COMMITMENT_PUSH, false, true),
            Some(RequestorRoles {
                scu: false,
                scp: true
            })
        );
        assert!(
            CommitmentRoles
                .negotiate_roles(uids::VERIFICATION, true, true)
                .is_none()
        );
    }
}
