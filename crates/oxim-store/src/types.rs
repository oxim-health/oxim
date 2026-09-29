use std::fmt;
use std::str::FromStr;

use oxim_model::{
    ChannelId, ConnectorId, DataType, DestinationStatus, DeviceId, MessageId, MessageStatus,
    Timestamp,
};

/// A processing stage whose content is stored for a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// The bytes exactly as received.
    Raw,
    /// The normalized clinical model, as JSON.
    Normalized,
    /// The message after channel transformers.
    Transformed,
    /// The bytes prepared for one destination.
    Encoded,
    /// The response a destination returned, such as an HL7 ACK.
    Response,
}

impl Stage {
    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Normalized => "normalized",
            Self::Transformed => "transformed",
            Self::Encoded => "encoded",
            Self::Response => "response",
        }
    }

    /// Whether content of this stage belongs to one destination.
    pub fn is_per_destination(self) -> bool {
        matches!(self, Self::Encoded | Self::Response)
    }
}

impl FromStr for Stage {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "raw" => Self::Raw,
            "normalized" => Self::Normalized,
            "transformed" => Self::Transformed,
            "encoded" => Self::Encoded,
            "response" => Self::Response,
            _ => return Err(()),
        })
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stored content of one stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    /// The stage.
    pub stage: Stage,
    /// The destination, for per-destination stages.
    pub destination: Option<ConnectorId>,
    /// The data type of `data`, if known.
    pub data_type: Option<DataType>,
    /// The bytes.
    pub data: Vec<u8>,
}

impl Content {
    /// Message-level content.
    pub fn new(stage: Stage, data_type: Option<DataType>, data: Vec<u8>) -> Self {
        Self {
            stage,
            destination: None,
            data_type,
            data,
        }
    }

    /// Content for one destination.
    pub fn for_destination(
        stage: Stage,
        destination: ConnectorId,
        data_type: Option<DataType>,
        data: Vec<u8>,
    ) -> Self {
        Self {
            stage,
            destination: Some(destination),
            data_type,
            data,
        }
    }
}

/// The result of running a message through its channel's filters and
/// transformers, recorded atomically by [`MessageStore::finish_processing`].
///
/// [`MessageStore::finish_processing`]: crate::MessageStore::finish_processing
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Processed {
    /// `Filtered`, `Transformed` or `Error`.
    pub status: MessageStatus,
    /// Error description when `status` is `Error`.
    pub error: Option<String>,
    /// Stage contents to store, including the encoded bytes of every queued
    /// destination.
    pub contents: Vec<Content>,
    /// Destinations whose queues receive the message.
    pub queue: Vec<ConnectorId>,
    /// Destinations that skipped the message through their own filters.
    pub filtered: Vec<ConnectorId>,
}

/// How a destination queue orders deliveries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum QueueOrdering {
    /// Strict first-in, first-out: while the oldest message waits for a
    /// retry, newer messages wait too. Required when order matters, for
    /// example corrected results that must follow the original.
    #[default]
    Strict,
    /// Messages waiting for a retry do not block newer messages.
    BestEffort,
}

/// A message claimed for delivery to one destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The message.
    pub message_id: MessageId,
    /// The channel.
    pub channel: ChannelId,
    /// The destination.
    pub destination: ConnectorId,
    /// Attempts made before this one.
    pub attempts: u32,
    /// The encoded bytes to send.
    pub payload: Vec<u8>,
    /// The data type of the payload.
    pub data_type: Option<DataType>,
}

/// The outcome of one delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// Delivered; the response is stored when present.
    Sent {
        /// The destination's response, such as an ACK.
        response: Option<Vec<u8>>,
    },
    /// The attempt failed; try again at `retry_at`.
    Retry {
        /// When the next attempt may start.
        retry_at: Timestamp,
        /// What went wrong.
        error: String,
    },
    /// Delivery gave up; an operator must requeue the message.
    Failed {
        /// What went wrong.
        error: String,
    },
}

/// The state of one destination for a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationState {
    /// The destination.
    pub destination: ConnectorId,
    /// Its status.
    pub status: DestinationStatus,
    /// Attempts made.
    pub attempts: u32,
    /// The last error.
    pub last_error: Option<String>,
    /// When the next attempt may start.
    pub next_attempt_at: Option<Timestamp>,
    /// When the state last changed.
    pub updated_at: Timestamp,
}

/// Everything known about a stored message except its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRecord {
    /// Identifier.
    pub id: MessageId,
    /// Channel.
    pub channel: ChannelId,
    /// Source connector.
    pub connector: ConnectorId,
    /// Receive time.
    pub received_at: Timestamp,
    /// Data type of the raw content.
    pub data_type: DataType,
    /// Processing status.
    pub status: MessageStatus,
    /// Remote peer.
    pub peer: Option<String>,
    /// Registered device.
    pub device: Option<DeviceId>,
    /// Correlation identifier.
    pub correlation_id: Option<String>,
    /// Metadata.
    pub metadata: std::collections::BTreeMap<String, String>,
    /// Processing error.
    pub error: Option<String>,
    /// Per-destination states.
    pub destinations: Vec<DestinationState>,
}

/// Filters for [`MessageStore::list_messages`](crate::MessageStore::list_messages).
///
/// Results are ordered newest first. Pass the last returned identifier as
/// `before` to fetch the next page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageQuery {
    /// Only this channel.
    pub channel: Option<ChannelId>,
    /// Only this status.
    pub status: Option<MessageStatus>,
    /// Only messages with a destination in this status.
    pub destination_status: Option<DestinationStatus>,
    /// Only messages received at or after this time.
    pub from: Option<Timestamp>,
    /// Only messages received before this time.
    pub until: Option<Timestamp>,
    /// Only messages older than this identifier (keyset pagination).
    pub before: Option<MessageId>,
    /// Page size, at most 1000.
    pub limit: usize,
}

impl Default for MessageQuery {
    fn default() -> Self {
        Self {
            channel: None,
            status: None,
            destination_status: None,
            from: None,
            until: None,
            before: None,
            limit: 100,
        }
    }
}

/// Queue depth for one destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueStats {
    /// Waiting for a first attempt.
    pub queued: u64,
    /// Being sent.
    pub sending: u64,
    /// Waiting for a retry.
    pub retrying: u64,
    /// Given up; waiting for an operator.
    pub failed: u64,
    /// Receive time of the oldest message not yet sent.
    pub oldest_pending_at: Option<Timestamp>,
}

/// What [`MessageStore::recover`](crate::MessageStore::recover) found after
/// a restart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoveryReport {
    /// Deliveries that were in flight and were queued again. They may be
    /// delivered twice (at-least-once delivery).
    pub requeued: u64,
    /// Messages that were stored and acknowledged but not processed yet.
    pub unprocessed: Vec<MessageId>,
}

/// What to delete during pruning. Only messages that reached a final state
/// (filtered, or completed with every destination final) are touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrunePolicy {
    /// Delete stored contents (keeping the message record) of messages
    /// received before this time.
    pub contents_before: Option<Timestamp>,
    /// Delete messages entirely if received before this time.
    pub messages_before: Option<Timestamp>,
}

/// What pruning deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PruneReport {
    /// Messages whose contents were deleted.
    pub contents_pruned: u64,
    /// Messages deleted entirely.
    pub messages_pruned: u64,
}

/// An entry of the audit trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    /// When it happened.
    pub at: Timestamp,
    /// What happened, for example `message.viewed` or `channel.deployed`.
    pub action: String,
    /// Who did it: a user name, or `system`.
    pub actor: String,
    /// The message concerned.
    pub message_id: Option<MessageId>,
    /// The channel concerned.
    pub channel: Option<ChannelId>,
    /// Additional detail.
    pub detail: Option<String>,
}

/// Stored messages and deliveries counted by state, for metrics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusCounts {
    /// Messages per channel and status.
    pub messages: Vec<(ChannelId, MessageStatus, u64)>,
    /// Deliveries per channel, destination and status.
    pub deliveries: Vec<(ChannelId, ConnectorId, DestinationStatus, u64)>,
}
