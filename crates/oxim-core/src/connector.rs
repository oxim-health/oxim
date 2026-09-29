//! Interfaces implemented by source and destination connectors.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use oxim_model::{
    ChannelId, ConnectorId, DataType, DeviceId, Envelope, MessageId, MessageIdGenerator,
    MessageStatus, Timestamp,
};
use oxim_store::{Delivery, Stage};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::clock::Clock;
use crate::config::{ResponseConfig, ResponseMode};
use crate::error::{ConnectorError, EngineError, SendError};
use crate::pipeline::CompiledPipeline;
use crate::store_actor::StoreHandle;

/// Receives messages from devices or systems, for example an MLLP listener,
/// a serial port or a directory.
///
/// `run` is called when the channel starts. It hands every complete message
/// to [`SourceContext::submit`], which returns once the message is stored
/// durably; only then may the connector acknowledge the sender. `run`
/// returns when [`SourceContext::cancelled`] completes. If it returns an
/// error, the engine logs it and calls `run` again after a delay.
#[async_trait]
pub trait SourceConnector: Send + Sync + fmt::Debug {
    /// Receives messages until the channel stops.
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError>;
}

/// Sends messages to a device or system, for example over MLLP, HTTP or to
/// a file.
///
/// `send` is called once per delivery attempt, in queue order. It returns
/// the receiver's response (such as an HL7 ACK) when there is one. A
/// temporary [`SendError`] schedules a retry according to the destination's
/// retry policy; a permanent one fails the delivery.
#[async_trait]
pub trait DestinationConnector: Send + Sync + fmt::Debug {
    /// Delivers one message.
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError>;
}

/// Details a source connector knows about a received message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubmitInfo {
    /// The remote peer: network address, serial port or file path.
    pub peer: Option<String>,
    /// The registered device the message came from, if identified.
    pub device: Option<DeviceId>,
    /// Links related messages, such as a query and its response.
    pub correlation_id: Option<String>,
    /// Connector-specific metadata.
    pub metadata: BTreeMap<String, String>,
}

/// Work for a channel's processor.
#[derive(Debug)]
pub(crate) enum Job {
    /// A message that was just received.
    Fresh(Envelope),
    /// A stored message that still needs processing.
    Stored(MessageId),
}

/// What a source needs from the channel it runs in.
pub(crate) struct ChannelShared {
    pub(crate) channel: ChannelId,
    pub(crate) connector: ConnectorId,
    pub(crate) data_type: DataType,
    pub(crate) store: StoreHandle,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) ids: Arc<Mutex<MessageIdGenerator>>,
    pub(crate) jobs: mpsc::Sender<Job>,
    pub(crate) pipeline: Arc<CompiledPipeline>,
    pub(crate) notifiers: Arc<BTreeMap<ConnectorId, Arc<Notify>>>,
    pub(crate) watches: Arc<DeliveryWatches>,
    pub(crate) response: Option<ResponseConfig>,
}

/// The final result of delivering one message to one destination, reported
/// to a source waiting for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveryReport {
    pub(crate) delivered: bool,
    pub(crate) response: Option<Vec<u8>>,
    pub(crate) error: Option<String>,
}

/// Sources waiting for the final delivery result of a message.
#[derive(Debug, Default)]
pub(crate) struct DeliveryWatches {
    waiting: Mutex<HashMap<(MessageId, ConnectorId), oneshot::Sender<DeliveryReport>>>,
}

impl DeliveryWatches {
    pub(crate) fn watch(
        &self,
        id: MessageId,
        destination: ConnectorId,
    ) -> oneshot::Receiver<DeliveryReport> {
        let (sender, receiver) = oneshot::channel();
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.insert((id, destination), sender);
        }
        receiver
    }

    pub(crate) fn complete(
        &self,
        id: MessageId,
        destination: &ConnectorId,
        report: DeliveryReport,
    ) {
        let sender = self
            .waiting
            .lock()
            .ok()
            .and_then(|mut waiting| waiting.remove(&(id, destination.clone())));
        if let Some(sender) = sender {
            let _ = sender.send(report);
        }
    }

    pub(crate) fn forget(&self, id: MessageId, destination: &ConnectorId) {
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.remove(&(id, destination.clone()));
        }
    }
}

/// The answer to [`SourceContext::request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The stored message.
    pub message_id: MessageId,
    /// The message status after processing.
    pub status: MessageStatus,
    /// The reply bytes for the sender, if one was produced in time.
    pub data: Option<Vec<u8>>,
    /// The data type of `data`.
    pub data_type: Option<DataType>,
    /// Why no reply is available, if none is.
    pub error: Option<String>,
}

/// The source connector's view of its channel.
#[derive(Clone)]
pub struct SourceContext {
    shared: Arc<ChannelShared>,
    cancel: CancellationToken,
}

impl fmt::Debug for SourceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceContext")
            .field("channel", &self.shared.channel)
            .field("connector", &self.shared.connector)
            .finish_non_exhaustive()
    }
}

impl SourceContext {
    pub(crate) fn new(shared: Arc<ChannelShared>, cancel: CancellationToken) -> Self {
        Self { shared, cancel }
    }

    /// The channel.
    pub fn channel(&self) -> &ChannelId {
        &self.shared.channel
    }

    /// The source connector identifier.
    pub fn connector(&self) -> &ConnectorId {
        &self.shared.connector
    }

    /// The configured data type of received messages.
    pub fn data_type(&self) -> DataType {
        self.shared.data_type
    }

    /// The current time, from the engine's clock.
    pub fn now(&self) -> Timestamp {
        self.shared.clock.now()
    }

    /// Completes when the channel stops.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }

    /// Whether the channel is stopping.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// How the channel answers requests, if it does.
    pub fn response(&self) -> Option<&ResponseConfig> {
        self.shared.response.as_ref()
    }

    /// Whether the channel produces replies, so the connector should call
    /// [`SourceContext::request`] instead of [`SourceContext::submit`].
    pub fn responds(&self) -> bool {
        self.shared.response.is_some()
    }

    fn envelope(&self, raw: Vec<u8>, info: SubmitInfo) -> Result<Envelope, EngineError> {
        let shared = &self.shared;
        let received_at = shared.clock.now();
        let id = {
            let mut ids = shared.ids.lock().map_err(|_| EngineError::ShuttingDown)?;
            let millis = u64::try_from(received_at.unix_millis()).unwrap_or_default();
            ids.next(millis, fastrand::u128(..))
        };
        Ok(Envelope {
            id,
            channel: shared.channel.clone(),
            connector: shared.connector.clone(),
            received_at,
            data_type: shared.data_type,
            raw,
            peer: info.peer,
            device: info.device,
            correlation_id: info.correlation_id,
            metadata: info.metadata,
        })
    }

    /// Stores a received message durably and schedules it for processing.
    ///
    /// When this returns `Ok`, the message survives a crash and the sender
    /// may be acknowledged. On error nothing was stored and the sender must
    /// be told to retry (for example with an HL7 `AE`/`AR` or an ASTM `NAK`).
    pub async fn submit(&self, raw: Vec<u8>, info: SubmitInfo) -> Result<MessageId, EngineError> {
        let shared = &self.shared;
        let envelope = self.envelope(raw, info)?;
        let id = envelope.id;
        shared.store.receive(envelope.clone()).await?;
        // The message is durable. If the channel is stopping, it will be
        // processed when the channel is deployed again.
        if shared.jobs.send(Job::Fresh(envelope)).await.is_err() {
            tracing::debug!(channel = %shared.channel, %id, "channel stopping; message left for the next deployment");
        }
        Ok(id)
    }

    /// Stores a message durably, processes it at once and returns the reply
    /// for the sender, as configured by the channel's `source.response`.
    ///
    /// The message is stored before processing, exactly as with
    /// [`SourceContext::submit`]; an `Err` means nothing was stored. An
    /// `Ok` without reply data means the message was stored but no reply
    /// was produced in time (see [`Reply::error`]); the sender should then
    /// get the protocol's plain acknowledgment.
    pub async fn request(&self, raw: Vec<u8>, info: SubmitInfo) -> Result<Reply, EngineError> {
        Ok(self.begin_request(raw, info).await?.reply().await)
    }

    /// The first half of [`SourceContext::request`]: stores the message
    /// durably and returns a handle to await the reply. Protocols that must
    /// acknowledge receipt quickly (ASTM LIS01 answers every frame within
    /// 15 seconds) acknowledge between the two halves.
    pub async fn begin_request(
        &self,
        raw: Vec<u8>,
        info: SubmitInfo,
    ) -> Result<PendingReply, EngineError> {
        let shared = self.shared.clone();
        let Some(config) = shared.response.clone() else {
            let message_id = self.submit(raw, info).await?;
            return Ok(PendingReply {
                shared,
                work: Pending::Submitted(message_id),
            });
        };
        let envelope = self.envelope(raw, info)?;
        shared.store.receive(envelope.clone()).await?;
        // Register before processing so a fast delivery cannot be missed.
        let watch = match (config.mode, &config.destination) {
            (ResponseMode::Destination, Some(destination)) => Some((
                destination.clone(),
                shared.watches.watch(envelope.id, destination.clone()),
            )),
            _ => None,
        };
        Ok(PendingReply {
            shared,
            work: Pending::Process {
                envelope: Box::new(envelope),
                config,
                watch,
            },
        })
    }
}

enum Pending {
    /// The channel does not reply; the message went the normal way.
    Submitted(MessageId),
    /// The message is stored and waits for processing and its reply.
    Process {
        envelope: Box<Envelope>,
        config: ResponseConfig,
        watch: Option<(ConnectorId, oneshot::Receiver<DeliveryReport>)>,
    },
}

/// A stored message whose reply is not produced yet; see
/// [`SourceContext::begin_request`].
pub struct PendingReply {
    shared: Arc<ChannelShared>,
    work: Pending,
}

impl fmt::Debug for PendingReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingReply")
            .field("message_id", &self.message_id())
            .finish_non_exhaustive()
    }
}

impl PendingReply {
    /// The stored message.
    pub fn message_id(&self) -> MessageId {
        match &self.work {
            Pending::Submitted(id) => *id,
            Pending::Process { envelope, .. } => envelope.id,
        }
    }

    /// Processes the message and waits for its reply.
    pub async fn reply(self) -> Reply {
        let shared = self.shared;
        let (envelope, config, watch) = match self.work {
            Pending::Submitted(message_id) => {
                return Reply {
                    message_id,
                    status: MessageStatus::Received,
                    data: None,
                    data_type: None,
                    error: Some("the channel does not produce replies".into()),
                };
            }
            Pending::Process {
                envelope,
                config,
                watch,
            } => (envelope, config, watch),
        };
        let id = envelope.id;
        let processed = crate::engine::process_and_record(&shared, *envelope).await;
        // Mirror the store: a transformed message without queued
        // destinations is complete.
        let status = match processed.status {
            MessageStatus::Transformed if processed.queue.is_empty() => MessageStatus::Completed,
            other => other,
        };
        let mut reply = Reply {
            message_id: id,
            status,
            data: None,
            data_type: None,
            error: processed.error.clone(),
        };
        match watch {
            None => {
                if let Some(content) = processed
                    .contents
                    .into_iter()
                    .find(|content| content.stage == Stage::Reply)
                {
                    reply.data = Some(content.data);
                    reply.data_type = content.data_type;
                } else if reply.error.is_none() {
                    reply.error = Some("no pipeline step produced a reply".into());
                }
            }
            Some((destination, receiver)) => {
                if !processed.queue.contains(&destination) {
                    shared.watches.forget(id, &destination);
                    if reply.error.is_none() {
                        reply.error = Some(format!("the message was not queued for {destination}"));
                    }
                } else {
                    match tokio::time::timeout(config.timeout.0, receiver).await {
                        Ok(Ok(report)) if report.delivered => {
                            reply.data = report.response;
                            if reply.data.is_none() {
                                reply.error = Some(format!("{destination} returned no response"));
                            }
                        }
                        Ok(Ok(report)) => {
                            reply.error =
                                Some(report.error.unwrap_or_else(|| {
                                    format!("delivery to {destination} failed")
                                }));
                        }
                        Ok(Err(_)) => reply.error = Some("the channel stopped".into()),
                        Err(_) => {
                            shared.watches.forget(id, &destination);
                            reply.error = Some(format!(
                                "{destination} did not answer within {}",
                                config.timeout
                            ));
                        }
                    }
                }
            }
        }
        reply
    }
}
