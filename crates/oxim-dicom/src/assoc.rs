//! DICOM associations over plain TCP or TLS, and DIMSE messages over them.
//!
//! Every requesting component (`dicom-scu`, `dicom-find`, `dicom-move`,
//! `dicom-get`) opens associations through a [`Peer`], and every accepting
//! component through an [`Acceptor`]; both support TLS with the settings of
//! [`oxim_connectors::tls`] (rustls with the ring provider).

use std::sync::Arc;
use std::time::Duration;

use dicom_ul::association::client::{AsyncTlsStream as ClientTlsStream, ClientAssociationOptions};
use dicom_ul::association::server::{
    AccessControl, AsyncTlsStream as ServerTlsStream, Negotiation, ServerAssociationOptions,
};
use dicom_ul::association::{
    Association, AsyncClientAssociation, AsyncServerAssociation, Error as UlError,
};
use dicom_ul::pdu::{PDataValueType, Pdu, PresentationContextNegotiated};
use oxim_connectors::tls::{ClientTlsSettings, ServerTlsSettings, client_config, server_config};
use oxim_core::{EngineError, SendError};
use tokio::net::TcpStream;

use crate::dimse::{self, Assembler, Command, Message};
use crate::error::DicomError;
use crate::net::validate_ae_title;

/// A failure before a status was received.
#[derive(Debug)]
pub(crate) enum Failure {
    /// Trying again may succeed.
    Temporary(String),
    /// Trying again cannot succeed.
    Permanent(String),
}

impl Failure {
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Temporary(message) | Self::Permanent(message) => message,
        }
    }
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

/// Dispatches a call to the plain or the TLS association.
macro_rules! either {
    ($value:expr, $association:ident => $body:expr) => {
        match $value {
            Self::Plain($association) => $body,
            Self::Tls($association) => $body,
        }
    };
}

/// An association OXIM requested.
pub(crate) enum ClientAssociation {
    Plain(Box<AsyncClientAssociation<TcpStream>>),
    Tls(Box<AsyncClientAssociation<ClientTlsStream>>),
}

impl ClientAssociation {
    pub(crate) fn presentation_contexts(&self) -> &[PresentationContextNegotiated] {
        either!(self, association => association.presentation_contexts())
    }

    pub(crate) async fn release(self) -> Result<(), UlError> {
        either!(self, association => association.release().await)
    }

    pub(crate) async fn abort(self) -> Result<(), UlError> {
        either!(self, association => association.abort().await)
    }
}

/// An association OXIM accepted.
pub(crate) enum ServerAssociation {
    Plain(Box<AsyncServerAssociation<TcpStream>>),
    Tls(Box<AsyncServerAssociation<ServerTlsStream>>),
}

impl ServerAssociation {
    pub(crate) fn presentation_contexts(&self) -> &[PresentationContextNegotiated] {
        either!(self, association => association.presentation_contexts())
    }

    pub(crate) fn peer_ae_title(&self) -> &str {
        either!(self, association => association.peer_ae_title())
    }

    pub(crate) fn called_ae_title(&self) -> &str {
        either!(self, association => association.called_ae_title())
    }

    pub(crate) async fn abort(self) -> Result<(), UlError> {
        either!(self, association => association.abort().await)
    }

    pub(crate) fn is_tls(&self) -> bool {
        matches!(self, Self::Tls(_))
    }
}

/// Sending and receiving PDUs on either side of an association.
pub(crate) trait Link: Send {
    /// The largest PDU the peer accepts.
    fn peer_max_pdu_length(&self) -> u32;
    /// Sends one PDU.
    fn send(&mut self, pdu: &Pdu) -> impl Future<Output = Result<(), UlError>> + Send;
    /// Receives one PDU.
    fn receive(&mut self) -> impl Future<Output = Result<Pdu, UlError>> + Send;
}

impl Link for ClientAssociation {
    fn peer_max_pdu_length(&self) -> u32 {
        either!(self, association => association.acceptor_max_pdu_length())
    }

    async fn send(&mut self, pdu: &Pdu) -> Result<(), UlError> {
        either!(self, association => association.send(pdu).await)
    }

    async fn receive(&mut self) -> Result<Pdu, UlError> {
        either!(self, association => association.receive().await)
    }
}

impl Link for ServerAssociation {
    fn peer_max_pdu_length(&self) -> u32 {
        either!(self, association => association.requestor_max_pdu_length())
    }

    async fn send(&mut self, pdu: &Pdu) -> Result<(), UlError> {
        either!(self, association => association.send(pdu).await)
    }

    async fn receive(&mut self) -> Result<Pdu, UlError> {
        either!(self, association => association.receive().await)
    }
}

/// Sends a command, and its data set if any, in fragments that fit the
/// peer's maximum PDU length.
pub(crate) async fn send_message(
    link: &mut impl Link,
    context_id: u8,
    command: &Command,
    data: Option<&[u8]>,
) -> Result<(), UlError> {
    let max = link.peer_max_pdu_length();
    let encoded = command.encode();
    for pdu in dimse::pdus(context_id, PDataValueType::Command, &encoded, max) {
        link.send(&pdu).await?;
    }
    if let Some(data) = data {
        for pdu in dimse::pdus(context_id, PDataValueType::Data, data, max) {
            link.send(&pdu).await?;
        }
    }
    Ok(())
}

/// What arrived on an association.
#[derive(Debug)]
pub(crate) enum Incoming {
    /// A complete DIMSE message.
    Message(Message),
    /// The peer asked to release the association.
    Release,
    /// The peer aborted the association.
    Abort,
}

/// Receives the next complete message, or the end of the association.
pub(crate) async fn receive_message(
    link: &mut impl Link,
    assembler: &mut Assembler,
) -> Result<Incoming, String> {
    loop {
        match link.receive().await.map_err(|e| e.to_string())? {
            Pdu::PData { data } => {
                let mut complete = None;
                for value in data {
                    if let Some(message) = assembler.push(value).map_err(|e| e.to_string())? {
                        if complete.is_some() {
                            return Err("several messages arrived in one PDU".into());
                        }
                        complete = Some(message);
                    }
                }
                if let Some(message) = complete {
                    return Ok(Incoming::Message(message));
                }
            }
            Pdu::ReleaseRQ => return Ok(Incoming::Release),
            Pdu::AbortRQ { .. } => return Ok(Incoming::Abort),
            other => {
                return Err(format!(
                    "unexpected PDU from the peer: {}",
                    other.short_description()
                ));
            }
        }
    }
}

/// Receives the next message on an association OXIM requested; a release,
/// an abort or a broken association is a temporary failure.
pub(crate) async fn expect_message(
    association: &mut ClientAssociation,
    assembler: &mut Assembler,
) -> Result<Message, Failure> {
    match receive_message(association, assembler).await {
        Ok(Incoming::Message(message)) => Ok(message),
        Ok(Incoming::Release) => Err(Failure::Temporary(
            "the peer released the association before answering".into(),
        )),
        Ok(Incoming::Abort) => Err(Failure::Temporary(
            "the peer aborted the association".into(),
        )),
        Err(e) => Err(Failure::Temporary(format!("association failed: {e}"))),
    }
}

/// The trimmed transfer syntax of a presentation context.
pub(crate) fn transfer_syntax(context: &PresentationContextNegotiated) -> String {
    context
        .transfer_syntax
        .trim_end_matches(['\0', ' '])
        .to_owned()
}

/// The first accepted presentation context for `abstract_syntax`.
pub(crate) fn accepted_context(
    contexts: &[PresentationContextNegotiated],
    abstract_syntax: &str,
) -> Option<PresentationContextNegotiated> {
    contexts
        .iter()
        .find(|pc| pc.abstract_syntax.trim_end_matches(['\0', ' ']) == abstract_syntax)
        .map(|pc| PresentationContextNegotiated {
            transfer_syntax: transfer_syntax(pc),
            ..pc.clone()
        })
}

/// The remote application entity a component requests associations with.
#[derive(Clone)]
pub(crate) struct Peer {
    pub(crate) target: String,
    pub(crate) called_ae_title: String,
    pub(crate) calling_ae_title: String,
    pub(crate) connect_timeout: Duration,
    pub(crate) timeout: Duration,
    pub(crate) max_pdu_length: u32,
    tls: Option<(Arc<rustls::ClientConfig>, String)>,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer")
            .field("target", &self.target)
            .field("called_ae_title", &self.called_ae_title)
            .field("calling_ae_title", &self.calling_ae_title)
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

/// The settings every requesting component shares.
pub(crate) struct PeerSettings<'a> {
    pub(crate) what: &'a str,
    pub(crate) target: &'a str,
    pub(crate) called_ae_title: &'a str,
    pub(crate) calling_ae_title: &'a str,
    pub(crate) connect_timeout: Duration,
    pub(crate) timeout: Duration,
    pub(crate) max_pdu_length: u32,
    pub(crate) tls: Option<&'a ClientTlsSettings>,
}

impl Peer {
    /// Validates the settings, reading the TLS files.
    pub(crate) fn new(settings: PeerSettings<'_>) -> Result<Self, EngineError> {
        let target = settings.target.trim();
        if target.is_empty() {
            return Err(EngineError::Config(format!(
                "{}: target must not be empty",
                settings.what
            )));
        }
        validate_ae_title("called_ae_title", settings.called_ae_title)?;
        validate_ae_title("calling_ae_title", settings.calling_ae_title)?;
        let tls = settings
            .tls
            .map(|tls| client_config(tls, target))
            .transpose()?;
        Ok(Self {
            target: target.to_owned(),
            called_ae_title: settings.called_ae_title.trim().to_owned(),
            calling_ae_title: settings.calling_ae_title.trim().to_owned(),
            connect_timeout: settings.connect_timeout,
            timeout: settings.timeout,
            max_pdu_length: settings.max_pdu_length.max(4096),
            tls,
        })
    }

    /// Association options with the AE titles, PDU length and timeouts;
    /// the caller adds presentation contexts.
    pub(crate) fn options(&self) -> ClientAssociationOptions<'static> {
        ClientAssociationOptions::new()
            .calling_ae_title(self.calling_ae_title.clone())
            .called_ae_title(self.called_ae_title.clone())
            .max_pdu_length(self.max_pdu_length)
            .connection_timeout(self.connect_timeout)
            .read_timeout(self.timeout)
            .write_timeout(self.timeout)
    }

    /// Requests an association, over TLS when configured.
    pub(crate) async fn establish(
        &self,
        options: ClientAssociationOptions<'static>,
    ) -> Result<ClientAssociation, Failure> {
        let limit = self.connect_timeout + self.timeout;
        let target = self.target.as_str();
        let established = match &self.tls {
            None => tokio::time::timeout(limit, options.establish_async(target))
                .await
                .map(|result| {
                    result.map(|association| ClientAssociation::Plain(Box::new(association)))
                }),
            Some((config, server_name)) => tokio::time::timeout(
                limit,
                options
                    .tls_config(config.clone())
                    .server_name(server_name)
                    .establish_tls_async(target),
            )
            .await
            .map(|result| result.map(|association| ClientAssociation::Tls(Box::new(association)))),
        };
        match established {
            Ok(Ok(association)) => Ok(association),
            Ok(Err(UlError::NoAcceptedPresentationContexts { .. })) => Err(Failure::Permanent(
                format!("{target} accepted none of the proposed presentation contexts"),
            )),
            Ok(Err(e)) => Err(Failure::Temporary(format!(
                "association with {target} failed: {e}"
            ))),
            Err(_) => Err(Failure::Temporary(format!(
                "no association with {target} within {limit:?}"
            ))),
        }
    }
}

/// Accepts associations for an accepting component, over TLS when
/// configured.
pub(crate) struct Acceptor<A, N> {
    options: ServerAssociationOptions<'static, A, N>,
    tls: Option<(Arc<rustls::ServerConfig>, Duration)>,
}

impl<A, N> Acceptor<A, N>
where
    A: AccessControl,
    N: Negotiation,
{
    /// An acceptor with TLS when `tls` is set, reading its files.
    pub(crate) fn new(
        options: ServerAssociationOptions<'static, A, N>,
        tls: Option<&ServerTlsSettings>,
    ) -> Result<Self, EngineError> {
        let tls = tls
            .map(|settings| {
                Ok::<_, EngineError>((server_config(settings)?, settings.handshake_timeout.0))
            })
            .transpose()?;
        let options = match &tls {
            Some((config, _)) => options.tls_config(config.clone()),
            None => options,
        };
        Ok(Self { options, tls })
    }

    /// Negotiates an association on an accepted connection.
    pub(crate) async fn accept(&self, stream: TcpStream) -> Result<ServerAssociation, String> {
        match &self.tls {
            None => self
                .options
                .establish_async(stream)
                .await
                .map(|association| ServerAssociation::Plain(Box::new(association)))
                .map_err(|e| e.to_string()),
            Some((_, handshake_timeout)) => {
                tokio::time::timeout(*handshake_timeout, self.options.establish_tls_async(stream))
                    .await
                    .map_err(|_| "the TLS handshake or association request timed out".to_owned())?
                    .map(|association| ServerAssociation::Tls(Box::new(association)))
                    .map_err(|e| e.to_string())
            }
        }
    }
}
