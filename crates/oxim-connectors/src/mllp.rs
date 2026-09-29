//! HL7 v2 over MLLP: a listener that acknowledges every stored message, and
//! a client that waits for the receiver's acknowledgment.
//!
//! Source type `mllp`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `listen` | required | Address to listen on, for example `0.0.0.0:2575` |
//! | `max_connections` | `100` | Concurrent connections; more are closed |
//! | `max_frame_len` | 16 MiB | Largest accepted message |
//! | `require_trailing_cr` | `false` | Reject frames whose end block is not followed by CR |
//!
//! Every frame is stored before it is acknowledged. In original mode the
//! sender gets `AA` once the message is durable, or `AE` when it could not
//! be stored; in enhanced mode (MSH-15/16 valued) it gets a commit
//! acknowledgment (`CA`/`CE`) when MSH-15 asks for one. Payloads that are
//! not valid HL7 are stored as well, so they can be inspected, and answered
//! with `AR`.
//!
//! Destination type `mllp`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `target` | required | Receiver address, for example `10.0.0.20:2575` |
//! | `connect_timeout` | `10s` | Time to establish a connection |
//! | `ack_timeout` | `30s` | Time to wait for the acknowledgment |
//! | `ack` | `required` | `required`, or `none` for receivers that never acknowledge |
//! | `max_frame_len` | 16 MiB | Largest accepted acknowledgment |
//!
//! The connection is kept open between messages and re-established after
//! errors. `AA`/`CA` completes the delivery (the acknowledgment is stored as
//! the response), `AE`/`CE` and transport errors are retried according to
//! the destination's retry policy, and `AR`/`CR` fails the delivery. An
//! acknowledgment whose MSA-2 does not match the sent MSH-10 is treated as
//! a transport error.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConfig, DestinationConnector, EngineError, Registry, SendError,
    SourceConfig, SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_hl7::{
    AckCode, AckError, AckMode, AckOptions, Delimiters, Message, Severity, build_ack,
    requested_mode,
};
use oxim_mllp::{Decoder, DecoderOptions, Event};
use oxim_model::{MessageId, Timestamp};
use oxim_store::Delivery;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::net::{DEFAULT_MAX_MESSAGE, hl7_timestamp, serve, settings};

const READ_BUFFER: usize = 64 * 1024;

fn default_max_connections() -> usize {
    100
}

fn default_max_frame_len() -> usize {
    DEFAULT_MAX_MESSAGE
}

fn default_connect_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

fn default_ack_timeout() -> DurationText {
    DurationText(Duration::from_secs(30))
}

/// Settings of the `mllp` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MllpSourceSettings {
    /// Address to listen on.
    pub listen: String,
    /// Concurrent connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Largest accepted message.
    #[serde(default = "default_max_frame_len")]
    pub max_frame_len: usize,
    /// Whether an end block must be followed by a carriage return.
    #[serde(default)]
    pub require_trailing_cr: bool,
}

/// Listens for HL7 v2 messages over MLLP.
#[derive(Debug, Clone)]
pub struct MllpSource {
    settings: Arc<MllpSourceSettings>,
}

impl MllpSource {
    /// Creates the source from its settings.
    pub fn new(settings: MllpSourceSettings) -> Self {
        Self {
            settings: Arc::new(settings),
        }
    }

    fn decoder_options(&self) -> DecoderOptions {
        let mut options = DecoderOptions::default();
        options.max_frame_len = self.settings.max_frame_len;
        options.require_trailing_cr = self.settings.require_trailing_cr;
        options
    }
}

#[async_trait]
impl SourceConnector for MllpSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let options = self.decoder_options();
        let handler_context = context.clone();
        serve(
            &context,
            &self.settings.listen,
            self.settings.max_connections,
            move |stream, peer| connection(handler_context.clone(), options, stream, peer),
        )
        .await
    }
}

async fn connection(
    context: SourceContext,
    options: DecoderOptions,
    mut stream: TcpStream,
    peer: SocketAddr,
) {
    let mut decoder = Decoder::new(options);
    let mut buffer = vec![0; READ_BUFFER];
    loop {
        let read = tokio::select! {
            () = context.cancelled() => return,
            read = stream.read(&mut buffer) => read,
        };
        match read {
            Ok(0) => {
                if let Some(event) = decoder.finish() {
                    handle_event(&context, &mut stream, peer, event).await;
                }
                return;
            }
            Ok(n) => decoder.push(&buffer[..n]),
            Err(e) => {
                debug!(channel = %context.channel(), %peer, error = %e, "connection closed");
                return;
            }
        }
        while let Some(event) = decoder.next_event() {
            if !handle_event(&context, &mut stream, peer, event).await {
                return;
            }
        }
    }
}

/// Handles one decoder event; returns `false` when the connection is lost.
async fn handle_event(
    context: &SourceContext,
    stream: &mut TcpStream,
    peer: SocketAddr,
    event: Event,
) -> bool {
    match event {
        Event::Frame(payload) => {
            let Some(reply) = receive(context, payload, peer).await else {
                return true;
            };
            match oxim_mllp::encode(&reply) {
                Ok(frame) => stream.write_all(&frame).await.is_ok(),
                Err(e) => {
                    warn!(channel = %context.channel(), error = %e, "cannot frame acknowledgment");
                    true
                }
            }
        }
        Event::CommitAck | Event::CommitNak => true,
        Event::Discarded { bytes, reason } => {
            debug!(channel = %context.channel(), %peer, bytes, ?reason, "discarded bytes");
            true
        }
    }
}

/// Stores one message and returns the acknowledgment to send, if any.
async fn receive(context: &SourceContext, payload: Vec<u8>, peer: SocketAddr) -> Option<Vec<u8>> {
    let info = SubmitInfo {
        peer: Some(peer.to_string()),
        ..SubmitInfo::default()
    };
    match Message::parse(&payload) {
        Ok(message) => {
            let mode = requested_mode(&message);
            // A channel that answers requests (for example a query relayed
            // to the LIS) sends its reply instead of a plain acknowledgment.
            if context.responds() {
                match context.request(payload, info).await {
                    Ok(reply) => {
                        if let Some(data) = reply.data {
                            return Some(data);
                        }
                        if let Some(reason) = &reply.error {
                            debug!(channel = %context.channel(), %peer, %reason, "no reply; acknowledging instead");
                        }
                        return acknowledgment(
                            &message,
                            mode,
                            &Ok(reply.message_id),
                            context.now(),
                        );
                    }
                    Err(error) => {
                        let error = error.to_string();
                        warn!(channel = %context.channel(), %peer, %error, "message could not be stored");
                        return acknowledgment(&message, mode, &Err(error), context.now());
                    }
                }
            }
            let outcome = context
                .submit(payload, info)
                .await
                .map_err(|e| e.to_string());
            if let Err(error) = &outcome {
                warn!(channel = %context.channel(), %peer, %error, "message could not be stored");
            }
            acknowledgment(&message, mode, &outcome, context.now())
        }
        Err(parse_error) => {
            let reason = format!("not a valid HL7 v2 message: {parse_error}");
            let stored = context.submit(payload, info).await;
            warn!(channel = %context.channel(), %peer, stored = stored.is_ok(), %reason, "rejecting message");
            Some(reject(&reason, context.now()))
        }
    }
}

/// The acknowledgment for an inbound message whose storage produced
/// `outcome`, or `None` when the requested mode wants no acknowledgment.
pub(crate) fn acknowledgment(
    inbound: &Message,
    mode: AckMode,
    outcome: &Result<MessageId, String>,
    now: Timestamp,
) -> Option<Vec<u8>> {
    let code = match (mode, outcome) {
        (AckMode::Original, Ok(_)) => AckCode::ApplicationAccept,
        (AckMode::Original, Err(_)) => AckCode::ApplicationError,
        (AckMode::Enhanced { accept, .. }, Ok(_)) => {
            if !accept.applies(true) {
                return None;
            }
            AckCode::CommitAccept
        }
        (AckMode::Enhanced { accept, .. }, Err(_)) => {
            if !accept.applies(false) {
                return None;
            }
            AckCode::CommitError
        }
    };
    let control_id = match outcome {
        Ok(id) => id.to_string(),
        Err(_) => format!("E{}", now.unix_nanos()),
    };
    let error = outcome.as_ref().err().map(|text| AckError {
        code: "207",
        text,
        severity: Severity::Error,
        location: None,
    });
    let timestamp = hl7_timestamp(now);
    let options = AckOptions {
        code,
        control_id: &control_id,
        timestamp: &timestamp,
        text: outcome.as_ref().err().map(String::as_str),
        error,
    };
    match build_ack(inbound, &options) {
        Ok(ack) => Some(ack.to_bytes()),
        Err(e) => {
            warn!(error = %e, "cannot build acknowledgment; rejecting");
            Some(reject(&format!("cannot build acknowledgment: {e}"), now))
        }
    }
}

/// An `AR` acknowledgment with default delimiters, for payloads that are not
/// valid HL7.
pub(crate) fn reject(reason: &str, now: Timestamp) -> Vec<u8> {
    let timestamp = hl7_timestamp(now);
    let control_id = format!("R{}", now.unix_nanos());
    let options = AckOptions {
        code: AckCode::ApplicationReject,
        control_id: &control_id,
        timestamp: &timestamp,
        text: Some(reason),
        error: Some(AckError {
            code: "207",
            text: reason,
            severity: Severity::Error,
            location: None,
        }),
    };
    Message::new(Delimiters::default())
        .ok()
        .and_then(|inbound| build_ack(&inbound, &options).ok())
        .map(|ack| ack.to_bytes())
        .unwrap_or_else(|| {
            format!("MSH|^~\\&|||||{timestamp}||ACK|{control_id}|P|2.5\rMSA|AR|\r").into_bytes()
        })
}

/// Whether the receiver must acknowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckRequirement {
    /// Wait for an HL7 acknowledgment (or an MLLP release 2 commit
    /// acknowledgment).
    #[default]
    Required,
    /// Consider the message delivered once it is written.
    None,
}

/// Settings of the `mllp` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MllpDestinationSettings {
    /// Receiver address.
    pub target: String,
    /// Time to establish a connection.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: DurationText,
    /// Time to wait for the acknowledgment.
    #[serde(default = "default_ack_timeout")]
    pub ack_timeout: DurationText,
    /// Whether to wait for an acknowledgment.
    #[serde(default)]
    pub ack: AckRequirement,
    /// Largest accepted acknowledgment.
    #[serde(default = "default_max_frame_len")]
    pub max_frame_len: usize,
}

struct Connection {
    stream: TcpStream,
    decoder: Decoder,
}

/// Sends HL7 v2 messages over MLLP and waits for acknowledgments.
#[derive(Debug)]
pub struct MllpDestination {
    settings: MllpDestinationSettings,
    connection: Mutex<Option<Connection>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("peer", &self.stream.peer_addr().ok())
            .finish_non_exhaustive()
    }
}

/// A failed exchange, and whether the connection must be dropped.
struct Failure {
    error: SendError,
    reset: bool,
}

impl Failure {
    fn reset(error: SendError) -> Self {
        Self { error, reset: true }
    }

    fn keep(error: SendError) -> Self {
        Self {
            error,
            reset: false,
        }
    }
}

impl MllpDestination {
    /// Creates the destination from its settings.
    pub fn new(settings: MllpDestinationSettings) -> Self {
        Self {
            settings,
            connection: Mutex::new(None),
        }
    }

    async fn connect(&self) -> Result<Connection, Failure> {
        let target = &self.settings.target;
        let stream =
            tokio::time::timeout(self.settings.connect_timeout.0, TcpStream::connect(target))
                .await
                .map_err(|_| {
                    Failure::reset(SendError::temporary(format!(
                        "connecting to {target} timed out"
                    )))
                })?
                .map_err(|e| {
                    Failure::reset(SendError::temporary(format!(
                        "cannot connect to {target}: {e}"
                    )))
                })?;
        let _ = stream.set_nodelay(true);
        let mut options = DecoderOptions::default();
        options.max_frame_len = self.settings.max_frame_len;
        Ok(Connection {
            stream,
            decoder: Decoder::new(options),
        })
    }

    async fn exchange(
        &self,
        slot: &mut Option<Connection>,
        frame: &[u8],
        control_id: Option<&[u8]>,
    ) -> Result<Option<Vec<u8>>, Failure> {
        if slot.is_none() {
            *slot = Some(self.connect().await?);
        }
        let Some(connection) = slot.as_mut() else {
            return Err(Failure::reset(SendError::temporary("no connection")));
        };
        let timeout = self.settings.ack_timeout.0;
        tokio::time::timeout(timeout, connection.stream.write_all(frame))
            .await
            .map_err(|_| Failure::reset(SendError::temporary("sending timed out")))?
            .map_err(|e| Failure::reset(SendError::temporary(format!("sending failed: {e}"))))?;
        if self.settings.ack == AckRequirement::None {
            return Ok(None);
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let mut buffer = vec![0; READ_BUFFER];
        loop {
            while let Some(event) = connection.decoder.next_event() {
                match event {
                    Event::Frame(payload) => return classify(&payload, control_id),
                    Event::CommitAck => return Ok(Some(vec![0x06])),
                    Event::CommitNak => {
                        return Err(Failure::keep(SendError::temporary(
                            "the receiver sent a negative commit acknowledgment",
                        )));
                    }
                    Event::Discarded { bytes, reason } => {
                        debug!(target = %self.settings.target, bytes, ?reason, "discarded bytes from receiver");
                    }
                }
            }
            let read = tokio::time::timeout_at(deadline, connection.stream.read(&mut buffer))
                .await
                .map_err(|_| {
                    Failure::reset(SendError::temporary(format!(
                        "no acknowledgment within {}",
                        self.settings.ack_timeout
                    )))
                })?;
            match read {
                Ok(0) => {
                    return Err(Failure::reset(SendError::temporary(
                        "the receiver closed the connection before acknowledging",
                    )));
                }
                Ok(n) => connection.decoder.push(&buffer[..n]),
                Err(e) => {
                    return Err(Failure::reset(SendError::temporary(format!(
                        "reading the acknowledgment failed: {e}"
                    ))));
                }
            }
        }
    }
}

/// Interprets an acknowledgment received for a message with MSH-10
/// `control_id`.
fn classify(payload: &[u8], control_id: Option<&[u8]>) -> Result<Option<Vec<u8>>, Failure> {
    let ack = Message::parse(payload).map_err(|e| {
        Failure::reset(SendError::temporary(format!(
            "unreadable acknowledgment: {e}"
        )))
    })?;
    if let (Some(expected), Some(received)) = (control_id, ack.get("MSA-2"))
        && !expected.is_empty()
        && received.raw() != expected
    {
        return Err(Failure::reset(SendError::temporary(format!(
            "acknowledgment for control ID {:?} does not match the sent {:?}",
            received.to_string_lossy(),
            String::from_utf8_lossy(expected)
        ))));
    }
    let text = ["MSA-3", "ERR-3.2", "ERR-1.4.2"]
        .iter()
        .find_map(|path| ack.get(path).filter(|value| !value.is_empty()))
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let code = ack
        .get("MSA-1")
        .and_then(|value| AckCode::parse(value.raw()));
    match code {
        Some(AckCode::ApplicationAccept | AckCode::CommitAccept) => Ok(Some(payload.to_vec())),
        Some(AckCode::ApplicationError | AckCode::CommitError) => Err(Failure::keep(
            SendError::temporary(format!("the receiver reported an error: {text}")),
        )),
        Some(AckCode::ApplicationReject | AckCode::CommitReject) => Err(Failure::keep(
            SendError::permanent(format!("the receiver rejected the message: {text}")),
        )),
        None => Err(Failure::reset(SendError::temporary(
            "acknowledgment without a valid MSA-1 code",
        ))),
    }
}

#[async_trait]
impl DestinationConnector for MllpDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let frame = oxim_mllp::encode(&delivery.payload)
            .map_err(|e| SendError::permanent(format!("cannot frame the message: {e}")))?;
        let control_id = Message::parse(&delivery.payload)
            .ok()
            .and_then(|message| message.control_id().map(|value| value.raw().to_vec()));
        let mut slot = self.connection.lock().await;
        match self
            .exchange(&mut slot, &frame, control_id.as_deref())
            .await
        {
            Ok(response) => Ok(response),
            Err(failure) => {
                if failure.reset {
                    *slot = None;
                }
                Err(failure.error)
            }
        }
    }
}

/// Registers the `mllp` source and destination.
pub fn register(registry: &mut Registry) {
    registry
        .add_source("mllp", |config: &SourceConfig| {
            let settings: MllpSourceSettings = settings(&config.settings, "mllp source")?;
            Ok(Arc::new(MllpSource::new(settings)) as Arc<dyn SourceConnector>)
        })
        .add_destination("mllp", |config: &DestinationConfig| {
            let settings: MllpDestinationSettings = settings(&config.settings, "mllp destination")?;
            if settings.target.trim().is_empty() {
                return Err(EngineError::Config(
                    "mllp destination needs a target".into(),
                ));
            }
            Ok(Arc::new(MllpDestination::new(settings)) as Arc<dyn DestinationConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbound(extra: &str) -> Message {
        Message::parse(
            format!(
                "MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01|MSG1|P|2.5.1{extra}\rPID|1\r"
            )
            .as_bytes(),
        )
        .unwrap()
    }

    fn now() -> Timestamp {
        Timestamp::from_unix_millis(1_790_067_600_000).unwrap()
    }

    #[test]
    fn acknowledges_in_original_mode() {
        let message = inbound("");
        let id = MessageId::from_parts(1, 2);
        let ack = acknowledgment(&message, requested_mode(&message), &Ok(id), now()).unwrap();
        let ack = Message::parse(&ack).unwrap();
        assert_eq!(ack.get("MSA-1").unwrap(), "AA");
        assert_eq!(ack.get("MSA-2").unwrap(), "MSG1");
        assert_eq!(ack.get("MSH-10").unwrap().raw(), id.to_string().as_bytes());
        assert_eq!(ack.get("MSH-7").unwrap(), "20260922090000+0000");

        let failed = acknowledgment(
            &message,
            requested_mode(&message),
            &Err("disk full".into()),
            now(),
        )
        .unwrap();
        let failed = Message::parse(&failed).unwrap();
        assert_eq!(failed.get("MSA-1").unwrap(), "AE");
        assert_eq!(failed.get("MSA-3").unwrap(), "disk full");
        assert_eq!(failed.get("ERR-3.1").unwrap(), "207");
    }

    #[test]
    fn follows_enhanced_mode_conditions() {
        let always = inbound("|||AL|NE");
        let ack = acknowledgment(
            &always,
            requested_mode(&always),
            &Ok(MessageId::from_parts(1, 1)),
            now(),
        )
        .unwrap();
        assert_eq!(Message::parse(&ack).unwrap().get("MSA-1").unwrap(), "CA");

        let never = inbound("|||NE|AL");
        assert!(
            acknowledgment(
                &never,
                requested_mode(&never),
                &Ok(MessageId::from_parts(1, 1)),
                now()
            )
            .is_none()
        );

        let errors_only = inbound("|||ER|NE");
        assert!(
            acknowledgment(
                &errors_only,
                requested_mode(&errors_only),
                &Ok(MessageId::from_parts(1, 1)),
                now()
            )
            .is_none()
        );
        let ack = acknowledgment(
            &errors_only,
            requested_mode(&errors_only),
            &Err("x".into()),
            now(),
        )
        .unwrap();
        assert_eq!(Message::parse(&ack).unwrap().get("MSA-1").unwrap(), "CE");
    }

    #[test]
    fn rejects_unparseable_payloads() {
        let ack = Message::parse(&reject("not a valid HL7 v2 message", now())).unwrap();
        assert_eq!(ack.get("MSA-1").unwrap(), "AR");
        assert_eq!(ack.get("MSA-3").unwrap(), "not a valid HL7 v2 message");
    }

    #[test]
    fn classifies_acknowledgments() {
        let ack = |code: &str, control: &str| {
            format!("MSH|^~\\&|LIS||LAB||20260929||ACK|A1|P|2.5.1\rMSA|{code}|{control}|reason\r")
                .into_bytes()
        };
        assert!(classify(&ack("AA", "MSG1"), Some(b"MSG1")).is_ok());
        assert!(classify(&ack("CA", "MSG1"), Some(b"MSG1")).is_ok());
        let error = classify(&ack("AE", "MSG1"), Some(b"MSG1")).unwrap_err();
        assert!(!error.error.permanent && !error.reset);
        assert!(error.error.message.contains("reason"));
        let reject = classify(&ack("AR", "MSG1"), Some(b"MSG1")).unwrap_err();
        assert!(reject.error.permanent);
        let mismatch = classify(&ack("AA", "OTHER"), Some(b"MSG1")).unwrap_err();
        assert!(mismatch.reset && !mismatch.error.permanent);
        let garbage = classify(b"hello", Some(b"MSG1")).unwrap_err();
        assert!(garbage.reset);
        let no_code = classify(&ack("ZZ", "MSG1"), Some(b"MSG1")).unwrap_err();
        assert!(no_code.reset);
    }
}
