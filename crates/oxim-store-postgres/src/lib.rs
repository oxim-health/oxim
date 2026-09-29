//! A PostgreSQL [`MessageStore`] for OXIM clusters.
//!
//! Several OXIM nodes can share one database: each delivery is claimed
//! with `SELECT … FOR UPDATE SKIP LOCKED`, so a message is sent by one
//! node, and every claim records the node that made it. A node that
//! restarts releases only its own claims ([`MessageStore::recover`]); the
//! claims of a node that died are released by the node that takes over
//! ([`PostgresStore::adopt`], used by `oxim-cluster`).
//!
//! Commits are durable when they return (the default
//! `synchronous_commit = on`; do not lower it for OXIM's database), which
//! is what lets OXIM acknowledge a message as soon as it is stored
//! (ADR 0004). The schema is created and migrated on connect; concurrent
//! nodes serialize migrations with an advisory lock.
//!
//! The client is synchronous (the `postgres` crate) because the engine
//! calls the store from its own thread. [`PostgresStore::connect`] must
//! therefore not be called from inside an async task; use a plain thread
//! or `tokio::task::spawn_blocking`.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};

use oxim_connectors::tls::{ClientTlsSettings, client_config};
use oxim_model::{
    ChannelId, ConnectorId, DataType, DestinationStatus, DeviceId, Envelope, MessageId,
    MessageStatus, Timestamp,
};
use oxim_store::{
    AuditEvent, Content, Delivery, DeliveryOutcome, DestinationState, MessageQuery, MessageRecord,
    MessageStore, Processed, PrunePolicy, PruneReport, QueueOrdering, QueueStats, RecoveryReport,
    Stage, StatusCounts, StoreError, StoreResult,
};
use postgres::types::ToSql;
use postgres::{Client, GenericClient, NoTls, Row};

/// Migrations in order; migration `n` (1-based) moves the schema from
/// version `n - 1` to `n`. Released migrations are never edited.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE messages (
        id BYTEA PRIMARY KEY,
        channel TEXT NOT NULL,
        connector TEXT NOT NULL,
        received_at BIGINT NOT NULL,
        data_type TEXT NOT NULL,
        status TEXT NOT NULL,
        peer TEXT,
        device TEXT,
        correlation_id TEXT,
        metadata TEXT NOT NULL,
        error TEXT,
        received_by TEXT NOT NULL
    );
    CREATE INDEX messages_by_channel ON messages (channel, id);
    CREATE INDEX messages_by_status ON messages (status, id);
    CREATE INDEX messages_by_time ON messages (received_at);
    CREATE INDEX messages_by_correlation ON messages (correlation_id)
        WHERE correlation_id IS NOT NULL;

    CREATE TABLE contents (
        message_id BYTEA NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
        stage TEXT NOT NULL,
        destination TEXT NOT NULL DEFAULT '',
        data_type TEXT,
        data BYTEA NOT NULL,
        PRIMARY KEY (message_id, stage, destination)
    );

    CREATE TABLE deliveries (
        message_id BYTEA NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
        destination TEXT NOT NULL,
        channel TEXT NOT NULL,
        status TEXT NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0,
        next_attempt_at BIGINT,
        last_error TEXT,
        updated_at BIGINT NOT NULL,
        claimed_by TEXT,
        PRIMARY KEY (message_id, destination)
    );
    CREATE INDEX deliveries_by_queue ON deliveries (channel, destination, status, message_id);
    CREATE INDEX deliveries_by_claim ON deliveries (claimed_by) WHERE status = 'sending';

    CREATE TABLE audit_events (
        id BIGSERIAL PRIMARY KEY,
        at BIGINT NOT NULL,
        action TEXT NOT NULL,
        actor TEXT NOT NULL,
        message_id BYTEA,
        channel TEXT,
        detail TEXT
    );
    CREATE INDEX audit_by_message ON audit_events (message_id) WHERE message_id IS NOT NULL;
    CREATE INDEX audit_by_time ON audit_events (at);
"];

/// The advisory lock key that serializes migrations across nodes.
const MIGRATION_LOCK: i64 = 0x6f78_696d_5f6d_6967; // "oxim_mig"

const ACTIVE: &str = "('queued', 'sending', 'retrying')";

const MESSAGE_COLUMNS: &str = "id, channel, connector, received_at, data_type, status, peer, device, correlation_id, metadata, error";

fn backend(error: postgres::Error) -> StoreError {
    StoreError::Backend(error.to_string())
}

fn key(id: MessageId) -> Vec<u8> {
    id.to_u128().to_be_bytes().to_vec()
}

fn id_from(bytes: &[u8]) -> StoreResult<MessageId> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|_| StoreError::Corrupt {
        field: "message id",
        detail: format!("{} bytes", bytes.len()),
    })?;
    Ok(MessageId::from_u128(u128::from_be_bytes(bytes)))
}

fn parse<T: FromStr>(field: &'static str, text: &str) -> StoreResult<T> {
    text.parse().map_err(|_| StoreError::Corrupt {
        field,
        detail: text.to_owned(),
    })
}

fn optional<T: FromStr>(field: &'static str, text: Option<String>) -> StoreResult<Option<T>> {
    text.map(|text| parse(field, &text)).transpose()
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or_default()
}

fn attempts(value: i32) -> u32 {
    u32::try_from(value).unwrap_or_default()
}

fn record(row: &Row, destinations: Vec<DestinationState>) -> StoreResult<MessageRecord> {
    let metadata: String = row.get(9);
    let metadata: BTreeMap<String, String> =
        serde_json::from_str(&metadata).map_err(|e| StoreError::Corrupt {
            field: "metadata",
            detail: e.to_string(),
        })?;
    let id: Vec<u8> = row.get(0);
    Ok(MessageRecord {
        id: id_from(&id)?,
        channel: parse("channel", row.get::<_, &str>(1))?,
        connector: parse("connector", row.get::<_, &str>(2))?,
        received_at: Timestamp::from_unix_nanos(row.get(3)),
        data_type: parse("data_type", row.get::<_, &str>(4))?,
        status: parse("status", row.get::<_, &str>(5))?,
        peer: row.get(6),
        device: optional::<DeviceId>("device", row.get(7))?,
        correlation_id: row.get(8),
        metadata,
        error: row.get(10),
        destinations,
    })
}

fn destinations(client: &mut impl GenericClient, id: &[u8]) -> StoreResult<Vec<DestinationState>> {
    client
        .query(
            "SELECT destination, status, attempts, last_error, next_attempt_at, updated_at
             FROM deliveries WHERE message_id = $1 ORDER BY destination",
            &[&id],
        )
        .map_err(backend)?
        .iter()
        .map(|row| {
            Ok(DestinationState {
                destination: parse("destination", row.get::<_, &str>(0))?,
                status: parse("delivery status", row.get::<_, &str>(1))?,
                attempts: attempts(row.get(2)),
                last_error: row.get(3),
                next_attempt_at: row.get::<_, Option<i64>>(4).map(Timestamp::from_unix_nanos),
                updated_at: Timestamp::from_unix_nanos(row.get(5)),
            })
        })
        .collect()
}

/// Marks the message completed once every destination is final.
fn complete_if_final(client: &mut impl GenericClient, id: &[u8]) -> StoreResult<()> {
    let pending: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM deliveries
             WHERE message_id = $1 AND status NOT IN ('sent', 'filtered', 'failed')",
            &[&id],
        )
        .map_err(backend)?
        .get(0);
    if pending == 0 {
        client
            .execute(
                "UPDATE messages SET status = 'completed' WHERE id = $1 AND status = 'transformed'",
                &[&id],
            )
            .map_err(backend)?;
    }
    Ok(())
}

/// A message store in a PostgreSQL database, shareable by several nodes.
pub struct PostgresStore {
    client: Mutex<Client>,
    node: String,
}

impl std::fmt::Debug for PostgresStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresStore")
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
}

impl PostgresStore {
    /// Connects to `url` (`postgresql://user@host/db`, or key=value
    /// parameters), with TLS when `tls` is set, as the node `node`, and
    /// migrates the schema. Blocks; see the crate documentation.
    pub fn connect(url: &str, tls: Option<&ClientTlsSettings>, node: &str) -> StoreResult<Self> {
        if node.trim().is_empty() {
            return Err(StoreError::InvalidState(
                "the node name must not be empty".into(),
            ));
        }
        let mut client = match tls {
            Some(settings) => {
                let config = client_config(settings)
                    .map_err(|e| StoreError::Backend(format!("TLS: {e}")))?;
                Client::connect(url, tokio_postgres_rustls::MakeRustlsConnect::new(config))
            }
            None => Client::connect(url, NoTls),
        }
        .map_err(backend)?;
        migrate(&mut client)?;
        Ok(Self {
            client: Mutex::new(client),
            node: node.to_owned(),
        })
    }

    /// The node this store claims deliveries for.
    pub fn node(&self) -> &str {
        &self.node
    }

    fn client(&self) -> StoreResult<MutexGuard<'_, Client>> {
        self.client
            .lock()
            .map_err(|_| StoreError::Backend("the database connection is unavailable".into()))
    }

    /// Takes over the work of the node `dead`: its in-flight deliveries
    /// return to their queues and its received but unprocessed messages
    /// become this node's. Returns the messages to process.
    pub fn adopt(&self, dead: &str, now: Timestamp) -> StoreResult<RecoveryReport> {
        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(backend)?;
        let requeued = tx
            .execute(
                "UPDATE deliveries SET status = 'queued', claimed_by = NULL, updated_at = $1
                 WHERE status = 'sending' AND claimed_by = $2",
                &[&now.unix_nanos(), &dead],
            )
            .map_err(backend)?;
        let rows = tx
            .query(
                "UPDATE messages SET received_by = $1
                 WHERE status = 'received' AND received_by = $2 RETURNING id",
                &[&self.node, &dead],
            )
            .map_err(backend)?;
        let mut unprocessed = rows
            .iter()
            .map(|row| id_from(row.get::<_, &[u8]>(0)))
            .collect::<StoreResult<Vec<_>>>()?;
        unprocessed.sort();
        tx.commit().map_err(backend)?;
        Ok(RecoveryReport {
            requeued,
            unprocessed,
        })
    }
}

fn migrate(client: &mut Client) -> StoreResult<()> {
    let mut tx = client.transaction().map_err(backend)?;
    tx.execute("SELECT pg_advisory_xact_lock($1)", &[&MIGRATION_LOCK])
        .map_err(backend)?;
    tx.batch_execute("CREATE TABLE IF NOT EXISTS oxim_schema (version INTEGER NOT NULL)")
        .map_err(backend)?;
    let version: i32 = match tx
        .query_opt("SELECT version FROM oxim_schema", &[])
        .map_err(backend)?
    {
        Some(row) => row.get(0),
        None => {
            tx.execute("INSERT INTO oxim_schema (version) VALUES (0)", &[])
                .map_err(backend)?;
            0
        }
    };
    let current = i32::try_from(MIGRATIONS.len()).unwrap_or(i32::MAX);
    if version > current {
        return Err(StoreError::SchemaTooNew {
            found: u32::try_from(version).unwrap_or_default(),
            supported: u32::try_from(current).unwrap_or_default(),
        });
    }
    for (index, migration) in MIGRATIONS
        .iter()
        .enumerate()
        .skip(usize::try_from(version).unwrap_or_default())
    {
        tx.batch_execute(migration).map_err(backend)?;
        let next = i32::try_from(index + 1).unwrap_or(i32::MAX);
        tx.execute("UPDATE oxim_schema SET version = $1", &[&next])
            .map_err(backend)?;
    }
    tx.commit().map_err(backend)
}

impl MessageStore for PostgresStore {
    fn receive(&mut self, envelopes: &[Envelope]) -> StoreResult<()> {
        let node = self.node.clone();
        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(backend)?;
        for envelope in envelopes {
            let id = key(envelope.id);
            let metadata =
                serde_json::to_string(&envelope.metadata).map_err(|e| StoreError::Corrupt {
                    field: "metadata",
                    detail: e.to_string(),
                })?;
            tx.execute(
                "INSERT INTO messages
                 (id, channel, connector, received_at, data_type, status, peer, device, correlation_id, metadata, received_by)
                 VALUES ($1, $2, $3, $4, $5, 'received', $6, $7, $8, $9, $10)",
                &[
                    &id,
                    &envelope.channel.as_str(),
                    &envelope.connector.as_str(),
                    &envelope.received_at.unix_nanos(),
                    &envelope.data_type.as_str(),
                    &envelope.peer,
                    &envelope.device.as_ref().map(DeviceId::as_str),
                    &envelope.correlation_id,
                    &metadata,
                    &node,
                ],
            )
            .map_err(backend)?;
            tx.execute(
                "INSERT INTO contents (message_id, stage, destination, data_type, data)
                 VALUES ($1, 'raw', '', $2, $3)",
                &[&id, &envelope.data_type.as_str(), &envelope.raw],
            )
            .map_err(backend)?;
        }
        tx.commit().map_err(backend)
    }

    fn finish_processing(
        &mut self,
        id: MessageId,
        processed: &Processed,
        now: Timestamp,
    ) -> StoreResult<()> {
        if !matches!(
            processed.status,
            MessageStatus::Filtered | MessageStatus::Transformed | MessageStatus::Error
        ) {
            return Err(StoreError::InvalidState(format!(
                "processing cannot end in status {}",
                processed.status
            )));
        }
        for destination in &processed.queue {
            let encoded = processed.contents.iter().any(|content| {
                content.stage == Stage::Encoded && content.destination.as_ref() == Some(destination)
            });
            if !encoded {
                return Err(StoreError::InvalidState(format!(
                    "queued destination {destination} has no encoded content"
                )));
            }
        }
        if processed.contents.iter().any(|c| c.stage == Stage::Raw) {
            return Err(StoreError::InvalidState(
                "the raw content cannot be replaced".into(),
            ));
        }
        let id_key = key(id);
        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(backend)?;
        let row = tx
            .query_opt(
                "SELECT status, channel FROM messages WHERE id = $1 FOR UPDATE",
                &[&id_key],
            )
            .map_err(backend)?
            .ok_or(StoreError::MessageNotFound(id))?;
        let status: MessageStatus = parse("status", row.get::<_, &str>(0))?;
        let channel: String = row.get(1);
        if status != MessageStatus::Received {
            return Err(StoreError::InvalidState(format!(
                "message {id} is {status}, not received"
            )));
        }
        for content in &processed.contents {
            tx.execute(
                "INSERT INTO contents (message_id, stage, destination, data_type, data)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (message_id, stage, destination)
                 DO UPDATE SET data_type = EXCLUDED.data_type, data = EXCLUDED.data",
                &[
                    &id_key,
                    &content.stage.as_str(),
                    &content.destination.as_ref().map_or("", ConnectorId::as_str),
                    &content.data_type.map(DataType::as_str),
                    &content.data,
                ],
            )
            .map_err(backend)?;
        }
        for (destination, status) in processed
            .queue
            .iter()
            .map(|d| (d, DestinationStatus::Queued))
            .chain(
                processed
                    .filtered
                    .iter()
                    .map(|d| (d, DestinationStatus::Filtered)),
            )
        {
            tx.execute(
                "INSERT INTO deliveries (message_id, destination, channel, status, updated_at)
                 VALUES ($1, $2, $3, $4, $5)",
                &[
                    &id_key,
                    &destination.as_str(),
                    &channel,
                    &status.as_str(),
                    &now.unix_nanos(),
                ],
            )
            .map_err(backend)?;
        }
        tx.execute(
            "UPDATE messages SET status = $1, error = $2 WHERE id = $3",
            &[&processed.status.as_str(), &processed.error, &id_key],
        )
        .map_err(backend)?;
        if processed.status == MessageStatus::Transformed {
            complete_if_final(&mut tx, &id_key)?;
        }
        tx.commit().map_err(backend)
    }

    fn reprocess(&mut self, id: MessageId) -> StoreResult<()> {
        let id_key = key(id);
        let node = self.node.clone();
        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(backend)?;
        tx.query_opt(
            "SELECT 1 FROM messages WHERE id = $1 FOR UPDATE",
            &[&id_key],
        )
        .map_err(backend)?
        .ok_or(StoreError::MessageNotFound(id))?;
        let sending: i64 = tx
            .query_one(
                "SELECT COUNT(*) FROM deliveries WHERE message_id = $1 AND status = 'sending'",
                &[&id_key],
            )
            .map_err(backend)?
            .get(0);
        if sending > 0 {
            return Err(StoreError::InvalidState(format!(
                "message {id} is being sent"
            )));
        }
        tx.execute("DELETE FROM deliveries WHERE message_id = $1", &[&id_key])
            .map_err(backend)?;
        tx.execute(
            "DELETE FROM contents WHERE message_id = $1 AND stage <> 'raw'",
            &[&id_key],
        )
        .map_err(backend)?;
        tx.execute(
            "UPDATE messages SET status = 'received', error = NULL, received_by = $2 WHERE id = $1",
            &[&id_key, &node],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)
    }

    fn next_delivery(
        &mut self,
        channel: &ChannelId,
        destination: &ConnectorId,
        ordering: QueueOrdering,
        now: Timestamp,
    ) -> StoreResult<Option<Delivery>> {
        let node = self.node.clone();
        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(backend)?;
        let candidate: Option<(Vec<u8>, i32)> = match ordering {
            QueueOrdering::Strict => {
                // Lock the head of the queue; a node that finds it being
                // sent by another node waits for the next check.
                let head = tx
                    .query_opt(
                        &format!(
                            "SELECT message_id, status, attempts, next_attempt_at FROM deliveries
                             WHERE channel = $1 AND destination = $2 AND status IN {ACTIVE}
                             ORDER BY message_id LIMIT 1 FOR UPDATE"
                        ),
                        &[&channel.as_str(), &destination.as_str()],
                    )
                    .map_err(backend)?;
                match head {
                    Some(row) => {
                        let status: DestinationStatus =
                            parse("delivery status", row.get::<_, &str>(1))?;
                        let due = row
                            .get::<_, Option<i64>>(3)
                            .is_none_or(|at| at <= now.unix_nanos());
                        match status {
                            DestinationStatus::Queued => Some((row.get(0), row.get(2))),
                            DestinationStatus::Retrying if due => Some((row.get(0), row.get(2))),
                            _ => None,
                        }
                    }
                    None => None,
                }
            }
            QueueOrdering::BestEffort => tx
                .query_opt(
                    "SELECT message_id, attempts FROM deliveries
                     WHERE channel = $1 AND destination = $2
                       AND (status = 'queued' OR (status = 'retrying' AND next_attempt_at <= $3))
                     ORDER BY message_id LIMIT 1 FOR UPDATE SKIP LOCKED",
                    &[&channel.as_str(), &destination.as_str(), &now.unix_nanos()],
                )
                .map_err(backend)?
                .map(|row| (row.get(0), row.get(1))),
        };
        let Some((message_key, tries)) = candidate else {
            tx.commit().map_err(backend)?;
            return Ok(None);
        };
        tx.execute(
            "UPDATE deliveries SET status = 'sending', claimed_by = $1, updated_at = $2
             WHERE message_id = $3 AND destination = $4",
            &[
                &node,
                &now.unix_nanos(),
                &message_key,
                &destination.as_str(),
            ],
        )
        .map_err(backend)?;
        let message_id = id_from(&message_key)?;
        let row = tx
            .query_opt(
                "SELECT data, data_type FROM contents
                 WHERE message_id = $1 AND stage = 'encoded' AND destination = $2",
                &[&message_key, &destination.as_str()],
            )
            .map_err(backend)?
            .ok_or_else(|| StoreError::Corrupt {
                field: "contents",
                detail: format!("no encoded content of {message_id} for {destination}"),
            })?;
        let payload: Vec<u8> = row.get(0);
        let data_type = optional("data_type", row.get(1))?;
        tx.commit().map_err(backend)?;
        Ok(Some(Delivery {
            message_id,
            channel: channel.clone(),
            destination: destination.clone(),
            attempts: attempts(tries),
            payload,
            data_type,
        }))
    }

    fn complete_delivery(
        &mut self,
        id: MessageId,
        destination: &ConnectorId,
        outcome: &DeliveryOutcome,
        now: Timestamp,
    ) -> StoreResult<()> {
        let id_key = key(id);
        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(backend)?;
        let status: String = tx
            .query_opt(
                "SELECT status FROM deliveries WHERE message_id = $1 AND destination = $2 FOR UPDATE",
                &[&id_key, &destination.as_str()],
            )
            .map_err(backend)?
            .ok_or_else(|| StoreError::DeliveryNotFound(id, destination.clone()))?
            .get(0);
        if parse::<DestinationStatus>("delivery status", &status)? != DestinationStatus::Sending {
            return Err(StoreError::InvalidState(format!(
                "delivery of {id} to {destination} is {status}, not sending"
            )));
        }
        let (status, next, error): (DestinationStatus, Option<i64>, Option<&str>) = match outcome {
            DeliveryOutcome::Sent { response } => {
                if let Some(response) = response {
                    tx.execute(
                        "INSERT INTO contents (message_id, stage, destination, data_type, data)
                         VALUES ($1, 'response', $2, NULL, $3)
                         ON CONFLICT (message_id, stage, destination)
                         DO UPDATE SET data_type = NULL, data = EXCLUDED.data",
                        &[&id_key, &destination.as_str(), response],
                    )
                    .map_err(backend)?;
                }
                (DestinationStatus::Sent, None, None)
            }
            DeliveryOutcome::Retry { retry_at, error } => (
                DestinationStatus::Retrying,
                Some(retry_at.unix_nanos()),
                Some(error.as_str()),
            ),
            DeliveryOutcome::Failed { error } => {
                (DestinationStatus::Failed, None, Some(error.as_str()))
            }
        };
        tx.execute(
            "UPDATE deliveries
             SET status = $1, attempts = attempts + 1, next_attempt_at = $2,
                 last_error = COALESCE($3, last_error), updated_at = $4, claimed_by = NULL
             WHERE message_id = $5 AND destination = $6",
            &[
                &status.as_str(),
                &next,
                &error,
                &now.unix_nanos(),
                &id_key,
                &destination.as_str(),
            ],
        )
        .map_err(backend)?;
        complete_if_final(&mut tx, &id_key)?;
        tx.commit().map_err(backend)
    }

    fn requeue(
        &mut self,
        id: MessageId,
        destination: &ConnectorId,
        now: Timestamp,
    ) -> StoreResult<()> {
        let id_key = key(id);
        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(backend)?;
        let status: String = tx
            .query_opt(
                "SELECT status FROM deliveries WHERE message_id = $1 AND destination = $2 FOR UPDATE",
                &[&id_key, &destination.as_str()],
            )
            .map_err(backend)?
            .ok_or_else(|| StoreError::DeliveryNotFound(id, destination.clone()))?
            .get(0);
        match parse::<DestinationStatus>("delivery status", &status)? {
            DestinationStatus::Failed | DestinationStatus::Retrying => {}
            other => {
                return Err(StoreError::InvalidState(format!(
                    "delivery of {id} to {destination} is {other}; only failed or retrying deliveries can be requeued"
                )));
            }
        }
        tx.execute(
            "UPDATE deliveries SET status = 'queued', next_attempt_at = NULL, updated_at = $1
             WHERE message_id = $2 AND destination = $3",
            &[&now.unix_nanos(), &id_key, &destination.as_str()],
        )
        .map_err(backend)?;
        tx.execute(
            "UPDATE messages SET status = 'transformed' WHERE id = $1 AND status = 'completed'",
            &[&id_key],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)
    }

    fn recover(&mut self, now: Timestamp) -> StoreResult<RecoveryReport> {
        // Only this node's own work: other nodes may be running.
        let node = self.node.clone();
        self.adopt(&node, now)
    }

    fn adopt_node(&mut self, node: &str, now: Timestamp) -> StoreResult<RecoveryReport> {
        self.adopt(node, now)
    }

    fn release_in_flight(&mut self, channel: &ChannelId, now: Timestamp) -> StoreResult<u64> {
        let node = self.node.clone();
        let mut client = self.client()?;
        client
            .execute(
                "UPDATE deliveries SET status = 'queued', claimed_by = NULL, updated_at = $1
                 WHERE channel = $2 AND status = 'sending' AND claimed_by = $3",
                &[&now.unix_nanos(), &channel.as_str(), &node],
            )
            .map_err(backend)
    }

    fn message(&self, id: MessageId) -> StoreResult<Option<MessageRecord>> {
        let id_key = key(id);
        let mut client = self.client()?;
        let row = client
            .query_opt(
                &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id = $1"),
                &[&id_key],
            )
            .map_err(backend)?;
        match row {
            Some(row) => {
                let states = destinations(&mut *client, &id_key)?;
                Ok(Some(record(&row, states)?))
            }
            None => Ok(None),
        }
    }

    fn content(
        &self,
        id: MessageId,
        stage: Stage,
        destination: Option<&ConnectorId>,
    ) -> StoreResult<Option<Content>> {
        let mut client = self.client()?;
        let row = client
            .query_opt(
                "SELECT data_type, data FROM contents
                 WHERE message_id = $1 AND stage = $2 AND destination = $3",
                &[
                    &key(id),
                    &stage.as_str(),
                    &destination.map_or("", ConnectorId::as_str),
                ],
            )
            .map_err(backend)?;
        row.map(|row| {
            Ok(Content {
                stage,
                destination: destination.cloned(),
                data_type: optional("data_type", row.get(0))?,
                data: row.get(1),
            })
        })
        .transpose()
    }

    fn list_messages(&self, query: &MessageQuery) -> StoreResult<Vec<MessageRecord>> {
        let mut sql = format!("SELECT {MESSAGE_COLUMNS} FROM messages m WHERE TRUE");
        let mut values: Vec<Box<dyn ToSql + Sync>> = Vec::new();
        let mut bind = |sql: &mut String, clause: &str, value: Box<dyn ToSql + Sync>| {
            values.push(value);
            sql.push_str(&clause.replace('?', &format!("${}", values.len())));
        };
        if let Some(channel) = &query.channel {
            bind(&mut sql, " AND channel = ?", Box::new(channel.to_string()));
        }
        if let Some(status) = query.status {
            bind(
                &mut sql,
                " AND status = ?",
                Box::new(status.as_str().to_owned()),
            );
        }
        if let Some(from) = query.from {
            bind(
                &mut sql,
                " AND received_at >= ?",
                Box::new(from.unix_nanos()),
            );
        }
        if let Some(until) = query.until {
            bind(
                &mut sql,
                " AND received_at < ?",
                Box::new(until.unix_nanos()),
            );
        }
        if let Some(before) = query.before {
            bind(&mut sql, " AND id < ?", Box::new(key(before)));
        }
        if let Some(status) = query.destination_status {
            bind(
                &mut sql,
                " AND EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.status = ?)",
                Box::new(status.as_str().to_owned()),
            );
        }
        let limit = i64::try_from(query.limit.clamp(1, 1000)).unwrap_or(1000);
        bind(&mut sql, " ORDER BY id DESC LIMIT ?", Box::new(limit));
        let parameters: Vec<&(dyn ToSql + Sync)> = values.iter().map(AsRef::as_ref).collect();
        let mut client = self.client()?;
        let rows = client.query(&sql, &parameters).map_err(backend)?;
        rows.iter()
            .map(|row| {
                let id: Vec<u8> = row.get(0);
                let states = destinations(&mut *client, &id)?;
                record(row, states)
            })
            .collect()
    }

    fn queue_stats(
        &self,
        channel: &ChannelId,
        destination: &ConnectorId,
    ) -> StoreResult<QueueStats> {
        let mut client = self.client()?;
        let mut stats = QueueStats::default();
        for row in client
            .query(
                "SELECT status, COUNT(*) FROM deliveries
                 WHERE channel = $1 AND destination = $2
                   AND status IN ('queued', 'sending', 'retrying', 'failed')
                 GROUP BY status",
                &[&channel.as_str(), &destination.as_str()],
            )
            .map_err(backend)?
        {
            let n = count(row.get(1));
            match parse::<DestinationStatus>("delivery status", row.get::<_, &str>(0))? {
                DestinationStatus::Queued => stats.queued = n,
                DestinationStatus::Sending => stats.sending = n,
                DestinationStatus::Retrying => stats.retrying = n,
                DestinationStatus::Failed => stats.failed = n,
                _ => {}
            }
        }
        stats.oldest_pending_at = client
            .query_one(
                &format!(
                    "SELECT MIN(m.received_at) FROM deliveries d JOIN messages m ON m.id = d.message_id
                     WHERE d.channel = $1 AND d.destination = $2 AND d.status IN {ACTIVE}"
                ),
                &[&channel.as_str(), &destination.as_str()],
            )
            .map_err(backend)?
            .get::<_, Option<i64>>(0)
            .map(Timestamp::from_unix_nanos);
        Ok(stats)
    }

    fn next_retry_at(
        &self,
        channel: &ChannelId,
        destination: &ConnectorId,
    ) -> StoreResult<Option<Timestamp>> {
        let mut client = self.client()?;
        Ok(client
            .query_one(
                "SELECT MIN(next_attempt_at) FROM deliveries
                 WHERE channel = $1 AND destination = $2 AND status = 'retrying'",
                &[&channel.as_str(), &destination.as_str()],
            )
            .map_err(backend)?
            .get::<_, Option<i64>>(0)
            .map(Timestamp::from_unix_nanos))
    }

    fn prune(&mut self, policy: &PrunePolicy) -> StoreResult<PruneReport> {
        const FINAL: &str = "status IN ('filtered', 'completed')";
        let mut client = self.client()?;
        let mut tx = client.transaction().map_err(backend)?;
        let mut report = PruneReport::default();
        if let Some(before) = policy.contents_before {
            let before = before.unix_nanos();
            report.contents_pruned = count(
                tx.query_one(
                    &format!(
                        "SELECT COUNT(*) FROM messages m WHERE received_at < $1 AND {FINAL}
                         AND EXISTS (SELECT 1 FROM contents c WHERE c.message_id = m.id)"
                    ),
                    &[&before],
                )
                .map_err(backend)?
                .get(0),
            );
            tx.execute(
                &format!(
                    "DELETE FROM contents WHERE message_id IN
                     (SELECT id FROM messages WHERE received_at < $1 AND {FINAL})"
                ),
                &[&before],
            )
            .map_err(backend)?;
        }
        if let Some(before) = policy.messages_before {
            report.messages_pruned = tx
                .execute(
                    &format!("DELETE FROM messages WHERE received_at < $1 AND {FINAL}"),
                    &[&before.unix_nanos()],
                )
                .map_err(backend)?;
        }
        tx.commit().map_err(backend)?;
        Ok(report)
    }

    fn erase(&mut self, ids: &[MessageId]) -> StoreResult<u64> {
        let keys: Vec<Vec<u8>> = ids.iter().map(|id| key(*id)).collect();
        let mut client = self.client()?;
        client
            .execute("DELETE FROM messages WHERE id = ANY($1)", &[&keys])
            .map_err(backend)
    }

    fn record_content(&mut self, id: MessageId, content: &Content) -> StoreResult<()> {
        if content.stage == Stage::Raw {
            return Err(StoreError::InvalidState(
                "the raw content cannot be replaced".into(),
            ));
        }
        let mut client = self.client()?;
        let changed = client
            .execute(
                "INSERT INTO contents (message_id, stage, destination, data_type, data)
                 SELECT $1, $2, $3, $4, $5 WHERE EXISTS (SELECT 1 FROM messages WHERE id = $1)
                 ON CONFLICT (message_id, stage, destination)
                 DO UPDATE SET data_type = EXCLUDED.data_type, data = EXCLUDED.data",
                &[
                    &key(id),
                    &content.stage.as_str(),
                    &content.destination.as_ref().map_or("", ConnectorId::as_str),
                    &content.data_type.map(DataType::as_str),
                    &content.data,
                ],
            )
            .map_err(backend)?;
        if changed == 0 {
            return Err(StoreError::MessageNotFound(id));
        }
        Ok(())
    }

    fn status_counts(&self) -> StoreResult<StatusCounts> {
        let mut client = self.client()?;
        let mut counts = StatusCounts::default();
        for row in client
            .query(
                "SELECT channel, status, COUNT(*) FROM messages GROUP BY channel, status",
                &[],
            )
            .map_err(backend)?
        {
            counts.messages.push((
                parse("channel", row.get::<_, &str>(0))?,
                parse("status", row.get::<_, &str>(1))?,
                count(row.get(2)),
            ));
        }
        for row in client
            .query(
                "SELECT channel, destination, status, COUNT(*) FROM deliveries
                 GROUP BY channel, destination, status",
                &[],
            )
            .map_err(backend)?
        {
            counts.deliveries.push((
                parse("channel", row.get::<_, &str>(0))?,
                parse("destination", row.get::<_, &str>(1))?,
                parse("delivery status", row.get::<_, &str>(2))?,
                count(row.get(3)),
            ));
        }
        Ok(counts)
    }

    fn record_audit(&mut self, event: &AuditEvent) -> StoreResult<()> {
        let mut client = self.client()?;
        client
            .execute(
                "INSERT INTO audit_events (at, action, actor, message_id, channel, detail)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &event.at.unix_nanos(),
                    &event.action,
                    &event.actor,
                    &event.message_id.map(key),
                    &event.channel.as_ref().map(ChannelId::as_str),
                    &event.detail,
                ],
            )
            .map_err(backend)?;
        Ok(())
    }

    fn audit_trail(
        &self,
        message: Option<MessageId>,
        limit: usize,
    ) -> StoreResult<Vec<AuditEvent>> {
        let limit = i64::try_from(limit.clamp(1, 10_000)).unwrap_or(10_000);
        let message_key = message.map(key);
        let mut client = self.client()?;
        client
            .query(
                "SELECT at, action, actor, message_id, channel, detail FROM audit_events
                 WHERE $1::BYTEA IS NULL OR message_id = $1
                 ORDER BY id DESC LIMIT $2",
                &[&message_key, &limit],
            )
            .map_err(backend)?
            .iter()
            .map(|row| {
                Ok(AuditEvent {
                    at: Timestamp::from_unix_nanos(row.get(0)),
                    action: row.get(1),
                    actor: row.get(2),
                    message_id: row
                        .get::<_, Option<Vec<u8>>>(3)
                        .map(|k| id_from(&k))
                        .transpose()?,
                    channel: optional("channel", row.get(4))?,
                    detail: row.get(5),
                })
            })
            .collect()
    }
}
