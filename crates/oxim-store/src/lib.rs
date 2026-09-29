//! Durable message storage and per-destination delivery queues.
//!
//! The store is where OXIM's delivery guarantee lives (ADR 0004, ADR 0005):
//!
//! 1. [`MessageStore::receive`] stores incoming messages durably; only then
//!    does the source connector acknowledge the sender.
//! 2. [`MessageStore::finish_processing`] records the result of filters and
//!    transformers and puts the encoded message in the queue of every
//!    destination, atomically.
//! 3. Delivery workers take the head of their queue with
//!    [`MessageStore::next_delivery`] and report the outcome with
//!    [`MessageStore::complete_delivery`]. A message is completed when every
//!    destination is final.
//! 4. After a crash, [`MessageStore::recover`] puts in-flight deliveries back
//!    in their queues and lists messages that still need processing.
//!    Delivery is therefore at-least-once.
//!
//! Every stage of a message (raw, normalized, transformed, encoded,
//! response) is kept until a [`PrunePolicy`] removes it, so messages can be
//! inspected and reprocessed.
//!
//! [`SqliteStore`] is the single-node implementation. Clustered deployments
//! use a PostgreSQL implementation of the same trait.

mod error;
mod schema;
mod sqlite;
mod types;

use oxim_model::{ChannelId, ConnectorId, Envelope, MessageId, Timestamp};

pub use error::{StoreError, StoreResult};
pub use sqlite::SqliteStore;
pub use types::{
    AuditEvent, Content, Delivery, DeliveryOutcome, DestinationState, MessageQuery, MessageRecord,
    Processed, PrunePolicy, PruneReport, QueueOrdering, QueueStats, RecoveryReport, Stage,
    StatusCounts,
};

/// Durable storage for messages, their contents, delivery queues and the
/// audit trail.
///
/// Methods are blocking. The engine calls them from a dedicated thread and
/// batches writes, so implementations need not be thread-safe. Times are
/// supplied by the caller.
pub trait MessageStore: Send {
    /// Stores newly received messages and their raw content in one durable
    /// transaction. When this returns `Ok`, the messages survive a crash and
    /// the sender may be acknowledged.
    fn receive(&mut self, envelopes: &[Envelope]) -> StoreResult<()>;

    /// Records the outcome of processing a received message: its status,
    /// stage contents, queued destinations and filtered destinations.
    fn finish_processing(
        &mut self,
        id: MessageId,
        processed: &Processed,
        now: Timestamp,
    ) -> StoreResult<()>;

    /// Discards everything derived from a message (contents other than raw,
    /// and deliveries) and marks it received again, so it is processed anew.
    fn reprocess(&mut self, id: MessageId) -> StoreResult<()>;

    /// Claims the next message due for `destination`, marking it as being
    /// sent, or returns `None` when nothing is due.
    fn next_delivery(
        &mut self,
        channel: &ChannelId,
        destination: &ConnectorId,
        ordering: QueueOrdering,
        now: Timestamp,
    ) -> StoreResult<Option<Delivery>>;

    /// Records the outcome of a delivery attempt claimed with
    /// [`MessageStore::next_delivery`].
    fn complete_delivery(
        &mut self,
        id: MessageId,
        destination: &ConnectorId,
        outcome: &DeliveryOutcome,
        now: Timestamp,
    ) -> StoreResult<()>;

    /// Puts a failed or retrying delivery back in its queue for an immediate
    /// attempt.
    fn requeue(
        &mut self,
        id: MessageId,
        destination: &ConnectorId,
        now: Timestamp,
    ) -> StoreResult<()>;

    /// Prepares the store after a restart: in-flight deliveries return to
    /// their queues, and messages still waiting for processing are listed.
    fn recover(&mut self, now: Timestamp) -> StoreResult<RecoveryReport>;

    /// Returns the in-flight deliveries of one channel to their queues, for
    /// example when the channel is redeployed after its workers stopped.
    /// Returns how many deliveries were released.
    fn release_in_flight(&mut self, channel: &ChannelId, now: Timestamp) -> StoreResult<u64>;

    /// A message record with its destination states.
    fn message(&self, id: MessageId) -> StoreResult<Option<MessageRecord>>;

    /// Stored content of one stage.
    fn content(
        &self,
        id: MessageId,
        stage: Stage,
        destination: Option<&ConnectorId>,
    ) -> StoreResult<Option<Content>>;

    /// Messages matching `query`, newest first.
    fn list_messages(&self, query: &MessageQuery) -> StoreResult<Vec<MessageRecord>>;

    /// Queue depth of one destination.
    fn queue_stats(
        &self,
        channel: &ChannelId,
        destination: &ConnectorId,
    ) -> StoreResult<QueueStats>;

    /// The earliest retry time among retrying deliveries of a destination.
    fn next_retry_at(
        &self,
        channel: &ChannelId,
        destination: &ConnectorId,
    ) -> StoreResult<Option<Timestamp>>;

    /// Deletes contents or whole messages that reached a final state.
    fn prune(&mut self, policy: &PrunePolicy) -> StoreResult<PruneReport>;

    /// Deletes messages entirely, for example to honor an erasure request.
    /// Returns the number of messages deleted.
    fn erase(&mut self, ids: &[MessageId]) -> StoreResult<u64>;

    /// Appends an event to the audit trail.
    fn record_audit(&mut self, event: &AuditEvent) -> StoreResult<()>;

    /// Adds or replaces one content of a processed message, for content
    /// produced after processing, such as a reply relayed from a
    /// destination. The raw content cannot be replaced. The default
    /// implementation keeps nothing; stores should override it.
    fn record_content(&mut self, id: MessageId, content: &Content) -> StoreResult<()> {
        let _ = (id, content);
        Ok(())
    }

    /// Messages and deliveries counted by state, for metrics. The default
    /// implementation reports nothing; stores should override it.
    fn status_counts(&self) -> StoreResult<StatusCounts> {
        Ok(StatusCounts::default())
    }

    /// Audit events, newest first, optionally only for one message.
    fn audit_trail(&self, message: Option<MessageId>, limit: usize)
    -> StoreResult<Vec<AuditEvent>>;
}
