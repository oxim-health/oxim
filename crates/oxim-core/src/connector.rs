//! Interfaces implemented by source and destination connectors.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use oxim_model::{
    ChannelId, ConnectorId, DataType, DeviceId, Envelope, MessageId, MessageIdGenerator, Timestamp,
};
use oxim_store::Delivery;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::clock::Clock;
use crate::error::{ConnectorError, EngineError, SendError};
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

    /// Stores a received message durably and schedules it for processing.
    ///
    /// When this returns `Ok`, the message survives a crash and the sender
    /// may be acknowledged. On error nothing was stored and the sender must
    /// be told to retry (for example with an HL7 `AE`/`AR` or an ASTM `NAK`).
    pub async fn submit(&self, raw: Vec<u8>, info: SubmitInfo) -> Result<MessageId, EngineError> {
        let shared = &self.shared;
        let received_at = shared.clock.now();
        let id = {
            let mut ids = shared.ids.lock().map_err(|_| EngineError::ShuttingDown)?;
            let millis = u64::try_from(received_at.unix_millis()).unwrap_or_default();
            ids.next(millis, fastrand::u128(..))
        };
        let envelope = Envelope {
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
        };
        shared.store.receive(envelope.clone()).await?;
        // The message is durable. If the channel is stopping, it will be
        // processed when the channel is deployed again.
        if shared.jobs.send(Job::Fresh(envelope)).await.is_err() {
            tracing::debug!(channel = %shared.channel, %id, "channel stopping; message left for the next deployment");
        }
        Ok(id)
    }
}
