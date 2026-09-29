//! The `dicom-scp` source: a Storage SCP that receives objects with
//! C-STORE and answers C-ECHO.
//!
//! Every received instance is submitted as a Part 10 object: file meta
//! information built from the C-STORE request and the negotiated transfer
//! syntax, followed by the data set exactly as received. The SCP answers
//! Success (`0000`) only after the object is stored durably; when it
//! cannot be stored it answers Refused: Out of Resources (`A700`), and
//! Processing Failure (`0110`) for requests it cannot turn into an object.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dicom_ul::association::server::{AccessControl, DefaultNegotiation, ServerAssociationOptions};
use dicom_ul::association::{Association, AsyncServerAssociation};
use dicom_ul::pdu::{
    AssociationRJServiceUserReason, PDataValue, PDataValueType, Pdu,
    PresentationContextResultReason, UserIdentity,
};
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, EngineError, SourceConfig, SourceConnector, SourceContext, SubmitInfo,
    async_trait,
};
use serde::Deserialize;
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::dimse::{Assembler, Command, Message, command_field, element, status};
use crate::net::{serve, settings, validate_ae_title};
use crate::part10::{self, FileMeta};
use crate::scan::{self, Syntax};
use crate::uids;

/// The default bound for one received object: 512 MiB.
pub const DEFAULT_MAX_OBJECT_SIZE: usize = 512 * 1024 * 1024;

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

type Options = ServerAssociationOptions<'static, AeTitles, DefaultNegotiation>;

/// Top-level attributes copied into the message metadata.
const METADATA_TAGS: &[((u16, u16), &str)] = &[
    ((0x0020, 0x000D), "dicom.study_instance_uid"),
    ((0x0020, 0x000E), "dicom.series_instance_uid"),
    ((0x0008, 0x0060), "dicom.modality"),
];

struct Shared {
    options: Options,
    ae_title: String,
    max_object_size: usize,
}

/// A Storage SCP source.
pub struct DicomScp {
    listen: String,
    max_associations: usize,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for DicomScp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DicomScp")
            .field("listen", &self.listen)
            .field("ae_title", &self.shared.ae_title)
            .finish_non_exhaustive()
    }
}

impl DicomScp {
    /// Validates the settings and creates the source.
    pub fn new(settings: DicomScpSettings) -> Result<Self, EngineError> {
        let config = |message: String| EngineError::Config(format!("dicom-scp source: {message}"));
        if settings.listen.trim().is_empty() {
            return Err(config("listen must not be empty".into()));
        }
        validate_ae_title("ae_title", &settings.ae_title)?;
        let ae_title = settings.ae_title.trim().to_owned();
        let mut calling = Vec::with_capacity(settings.calling_ae_titles.len());
        for title in &settings.calling_ae_titles {
            validate_ae_title("calling AE title", title)?;
            calling.push(title.trim().to_owned());
        }
        if settings.max_object_size == 0 {
            return Err(config("max_object_size must be positive".into()));
        }
        let timeout = settings.timeout.0;
        let mut options = ServerAssociationOptions::new()
            .ae_access_control(AeTitles {
                calling,
                check_called: settings.check_called_ae_title,
            })
            .ae_title(ae_title.clone())
            .max_pdu_length(settings.max_pdu_length.max(4096))
            .read_timeout(timeout)
            .write_timeout(timeout)
            .with_abstract_syntax(uids::VERIFICATION);
        match settings.sop_classes.as_deref() {
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
        for ts in &settings.transfer_syntaxes {
            Syntax::of(ts).map_err(|e| config(format!("transfer syntax {ts:?}: {e}")))?;
            options = options.with_transfer_syntax(ts.clone());
        }
        Ok(Self {
            listen: settings.listen,
            max_associations: settings.max_associations,
            shared: Arc::new(Shared {
                options,
                ae_title,
                max_object_size: settings.max_object_size,
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
}

impl Shared {
    async fn association(&self, context: SourceContext, stream: TcpStream, peer: SocketAddr) {
        let channel = context.channel().clone();
        let established = tokio::select! {
            () = context.cancelled() => return,
            established = self.options.establish_async(stream) => established,
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
        };
        let contexts: HashMap<u8, String> = association
            .presentation_contexts()
            .iter()
            .filter(|pc| pc.reason == PresentationContextResultReason::Acceptance)
            .map(|pc| {
                let ts = pc.transfer_syntax.trim_end_matches(['\0', ' ']);
                (pc.id, ts.to_owned())
            })
            .collect();
        debug!(%channel, %peer, calling = %parties.calling, contexts = contexts.len(), "DICOM association established");
        let mut assembler = Assembler::new(self.max_object_size);
        loop {
            let received = tokio::select! {
                () = context.cancelled() => None,
                pdu = association.receive() => Some(pdu),
            };
            let pdu = match received {
                None => {
                    let _ = association.abort().await;
                    return;
                }
                Some(Ok(pdu)) => pdu,
                Some(Err(e)) => {
                    debug!(%channel, %peer, error = %e, "DICOM association ended");
                    return;
                }
            };
            match pdu {
                Pdu::PData { data } => {
                    for value in data {
                        let message = match assembler.push(value) {
                            Ok(Some(message)) => message,
                            Ok(None) => continue,
                            Err(e) => {
                                warn!(%channel, %peer, error = %e, "DICOM protocol error; aborting the association");
                                let _ = association.abort().await;
                                return;
                            }
                        };
                        let Some(transfer_syntax) = contexts.get(&message.context_id) else {
                            warn!(%channel, %peer, context = message.context_id, "message on a presentation context that was not accepted; aborting the association");
                            let _ = association.abort().await;
                            return;
                        };
                        let context_id = message.context_id;
                        let Some(response) = self
                            .respond(&context, message, transfer_syntax, &parties)
                            .await
                        else {
                            continue;
                        };
                        if let Err(e) = send_command(&mut association, context_id, &response).await
                        {
                            debug!(%channel, %peer, error = %e, "cannot send the DIMSE response");
                            return;
                        }
                    }
                }
                Pdu::ReleaseRQ => {
                    let _ = association.send(&Pdu::ReleaseRP).await;
                    return;
                }
                Pdu::AbortRQ { .. } => {
                    debug!(%channel, %peer, "DICOM association aborted by the peer");
                    return;
                }
                other => {
                    warn!(%channel, %peer, pdu = %other.short_description(), "unexpected PDU; aborting the association");
                    let _ = association.abort().await;
                    return;
                }
            }
        }
    }

    /// The response to a request, if it has one.
    async fn respond(
        &self,
        context: &SourceContext,
        message: Message,
        transfer_syntax: &str,
        parties: &Parties,
    ) -> Option<Command> {
        match message.command.command_field()? {
            command_field::C_ECHO_RQ => Some(Command::response_to(
                &message.command,
                status::SUCCESS,
                None,
            )),
            command_field::C_STORE_RQ => {
                Some(self.store(context, message, transfer_syntax, parties).await)
            }
            command_field::C_CANCEL_RQ => None,
            field if field & 0x8000 == 0 => Some(Command::response_to(
                &message.command,
                status::UNRECOGNIZED_OPERATION,
                Some("OXIM supports C-ECHO and C-STORE only"),
            )),
            // A response from the peer, which OXIM never asked for.
            _ => None,
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
        let sop_class_uid = command
            .text(element::AFFECTED_SOP_CLASS_UID)
            .unwrap_or_default();
        let sop_instance_uid = command
            .text(element::AFFECTED_SOP_INSTANCE_UID)
            .unwrap_or_default();
        if sop_class_uid.is_empty() || sop_instance_uid.is_empty() {
            return failure(
                status::PROCESSING_FAILURE,
                "the request has no affected SOP class or instance UID",
            );
        }
        if message.data.is_empty() {
            return failure(status::PROCESSING_FAILURE, "the data set is empty");
        }
        let mut metadata = BTreeMap::from([
            ("dicom.calling_ae".to_owned(), parties.calling.clone()),
            ("dicom.called_ae".to_owned(), parties.called.clone()),
            ("dicom.sop_class_uid".to_owned(), sop_class_uid.clone()),
            (
                "dicom.sop_instance_uid".to_owned(),
                sop_instance_uid.clone(),
            ),
            (
                "dicom.transfer_syntax".to_owned(),
                transfer_syntax.to_owned(),
            ),
        ]);
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
        let info = SubmitInfo {
            peer: Some(parties.peer.to_string()),
            metadata,
            ..SubmitInfo::default()
        };
        match context.submit(raw, info).await {
            Ok(id) => {
                debug!(%channel, %id, calling = %parties.calling, "C-STORE stored");
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
}

async fn send_command(
    association: &mut AsyncServerAssociation<TcpStream>,
    context_id: u8,
    command: &Command,
) -> Result<(), dicom_ul::association::Error> {
    association
        .send(&Pdu::PData {
            data: vec![PDataValue {
                presentation_context_id: context_id,
                value_type: PDataValueType::Command,
                is_last: true,
                data: command.encode(),
            }],
        })
        .await
}

/// Registers the `dicom-scp` source.
pub(crate) fn register(registry: &mut oxim_core::Registry) {
    registry.add_source("dicom-scp", |config: &SourceConfig| {
        let settings: DicomScpSettings = settings(&config.settings, "dicom-scp source")?;
        Ok(Arc::new(DicomScp::new(settings)?) as Arc<dyn SourceConnector>)
    });
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
}
