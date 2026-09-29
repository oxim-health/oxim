//! The SQLite implementation of [`MessageStore`].

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use oxim_model::{
    ChannelId, ConnectorId, DataType, DestinationStatus, DeviceId, Envelope, MessageId,
    MessageStatus, Timestamp,
};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};

use crate::MessageStore;
use crate::error::{StoreError, StoreResult};
use crate::schema;
use crate::types::{
    AuditEvent, Content, Delivery, DeliveryOutcome, DestinationState, MessageQuery, MessageRecord,
    Processed, PrunePolicy, PruneReport, QueueOrdering, QueueStats, RecoveryReport, Stage,
};

/// A message store in a single SQLite database file.
///
/// Commits are durable when they return (WAL journal, `synchronous=FULL`).
/// The store holds one connection and is meant to be owned by a single
/// thread; the engine serializes access and batches writes.
#[derive(Debug)]
pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    /// Opens or creates the database at `path` and applies migrations.
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let mut conn = Connection::open(path)?;
        schema::prepare(&mut conn)?;
        Ok(Self { conn })
    }

    /// Opens a private in-memory database, for tests and simulations.
    pub fn open_in_memory() -> StoreResult<Self> {
        let mut conn = Connection::open_in_memory()?;
        schema::prepare(&mut conn)?;
        Ok(Self { conn })
    }

    /// The schema version of the open database.
    pub fn schema_version(&self) -> StoreResult<u32> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }
}

const ACTIVE: &str = "('queued', 'sending', 'retrying')";

fn id_bytes(id: MessageId) -> [u8; 16] {
    id.to_u128().to_be_bytes()
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

fn attempts(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Columns of `messages` in the order `MESSAGE_COLUMNS` selects them.
struct MessageRow {
    id: Vec<u8>,
    channel: String,
    connector: String,
    received_at: i64,
    data_type: String,
    status: String,
    peer: Option<String>,
    device: Option<String>,
    correlation_id: Option<String>,
    metadata: String,
    error: Option<String>,
}

const MESSAGE_COLUMNS: &str = "id, channel, connector, received_at, data_type, status, peer, device, correlation_id, metadata, error";

impl MessageRow {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            channel: row.get(1)?,
            connector: row.get(2)?,
            received_at: row.get(3)?,
            data_type: row.get(4)?,
            status: row.get(5)?,
            peer: row.get(6)?,
            device: row.get(7)?,
            correlation_id: row.get(8)?,
            metadata: row.get(9)?,
            error: row.get(10)?,
        })
    }

    fn into_record(self, destinations: Vec<DestinationState>) -> StoreResult<MessageRecord> {
        let metadata: BTreeMap<String, String> =
            serde_json::from_str(&self.metadata).map_err(|e| StoreError::Corrupt {
                field: "metadata",
                detail: e.to_string(),
            })?;
        Ok(MessageRecord {
            id: id_from(&self.id)?,
            channel: parse("channel", &self.channel)?,
            connector: parse("connector", &self.connector)?,
            received_at: Timestamp::from_unix_nanos(self.received_at),
            data_type: parse("data_type", &self.data_type)?,
            status: parse("status", &self.status)?,
            peer: self.peer,
            device: optional::<DeviceId>("device", self.device)?,
            correlation_id: self.correlation_id,
            metadata,
            error: self.error,
            destinations,
        })
    }
}

fn destinations(conn: &Connection, id: &[u8]) -> StoreResult<Vec<DestinationState>> {
    let mut statement = conn.prepare_cached(
        "SELECT destination, status, attempts, last_error, next_attempt_at, updated_at
         FROM deliveries WHERE message_id = ?1 ORDER BY destination",
    )?;
    let rows = statement
        .query_map([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(destination, status, tries, last_error, next, updated)| {
            Ok(DestinationState {
                destination: parse("destination", &destination)?,
                status: parse("delivery status", &status)?,
                attempts: attempts(tries),
                last_error,
                next_attempt_at: next.map(Timestamp::from_unix_nanos),
                updated_at: Timestamp::from_unix_nanos(updated),
            })
        })
        .collect()
}

fn message_status(tx: &Transaction<'_>, id: &[u8]) -> StoreResult<Option<(MessageStatus, String)>> {
    tx.query_row(
        "SELECT status, channel FROM messages WHERE id = ?1",
        [id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .optional()?
    .map(|(status, channel)| Ok((parse("status", &status)?, channel)))
    .transpose()
}

/// Marks the message completed once every destination is final.
fn complete_if_final(tx: &Transaction<'_>, id: &[u8]) -> StoreResult<()> {
    let pending: i64 = tx.query_row(
        "SELECT COUNT(*) FROM deliveries
         WHERE message_id = ?1 AND status NOT IN ('sent', 'filtered', 'failed')",
        [id],
        |row| row.get(0),
    )?;
    if pending == 0 {
        tx.execute(
            "UPDATE messages SET status = 'completed' WHERE id = ?1 AND status = 'transformed'",
            [id],
        )?;
    }
    Ok(())
}

impl MessageStore for SqliteStore {
    fn receive(&mut self, envelopes: &[Envelope]) -> StoreResult<()> {
        let tx = self.conn.transaction()?;
        {
            let mut insert_message = tx.prepare_cached(
                "INSERT INTO messages
                 (id, channel, connector, received_at, data_type, status, peer, device, correlation_id, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'received', ?6, ?7, ?8, ?9)",
            )?;
            let mut insert_raw = tx.prepare_cached(
                "INSERT INTO contents (message_id, stage, destination, data_type, data)
                 VALUES (?1, 'raw', '', ?2, ?3)",
            )?;
            for envelope in envelopes {
                let id = id_bytes(envelope.id);
                let metadata =
                    serde_json::to_string(&envelope.metadata).map_err(|e| StoreError::Corrupt {
                        field: "metadata",
                        detail: e.to_string(),
                    })?;
                insert_message.execute(params![
                    id,
                    envelope.channel.as_str(),
                    envelope.connector.as_str(),
                    envelope.received_at.unix_nanos(),
                    envelope.data_type.as_str(),
                    envelope.peer,
                    envelope.device.as_ref().map(DeviceId::as_str),
                    envelope.correlation_id,
                    metadata,
                ])?;
                insert_raw.execute(params![id, envelope.data_type.as_str(), envelope.raw])?;
            }
        }
        tx.commit()?;
        Ok(())
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
        let key = id_bytes(id);
        let tx = self.conn.transaction()?;
        let (status, channel) =
            message_status(&tx, &key)?.ok_or(StoreError::MessageNotFound(id))?;
        if status != MessageStatus::Received {
            return Err(StoreError::InvalidState(format!(
                "message {id} is {status}, not received"
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
        {
            let mut insert_content = tx.prepare_cached(
                "INSERT OR REPLACE INTO contents (message_id, stage, destination, data_type, data)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for content in &processed.contents {
                if content.stage == Stage::Raw {
                    return Err(StoreError::InvalidState(
                        "the raw content cannot be replaced".into(),
                    ));
                }
                insert_content.execute(params![
                    key,
                    content.stage.as_str(),
                    content.destination.as_ref().map_or("", ConnectorId::as_str),
                    content.data_type.map(DataType::as_str),
                    content.data,
                ])?;
            }
            let mut insert_delivery = tx.prepare_cached(
                "INSERT INTO deliveries (message_id, destination, channel, status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for destination in &processed.queue {
                insert_delivery.execute(params![
                    key,
                    destination.as_str(),
                    channel,
                    DestinationStatus::Queued.as_str(),
                    now.unix_nanos(),
                ])?;
            }
            for destination in &processed.filtered {
                insert_delivery.execute(params![
                    key,
                    destination.as_str(),
                    channel,
                    DestinationStatus::Filtered.as_str(),
                    now.unix_nanos(),
                ])?;
            }
        }
        tx.execute(
            "UPDATE messages SET status = ?1, error = ?2 WHERE id = ?3",
            params![processed.status.as_str(), processed.error, key],
        )?;
        if processed.status == MessageStatus::Transformed {
            complete_if_final(&tx, &key)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn reprocess(&mut self, id: MessageId) -> StoreResult<()> {
        let key = id_bytes(id);
        let tx = self.conn.transaction()?;
        message_status(&tx, &key)?.ok_or(StoreError::MessageNotFound(id))?;
        let sending: i64 = tx.query_row(
            "SELECT COUNT(*) FROM deliveries WHERE message_id = ?1 AND status = 'sending'",
            [&key],
            |row| row.get(0),
        )?;
        if sending > 0 {
            return Err(StoreError::InvalidState(format!(
                "message {id} is being sent"
            )));
        }
        tx.execute("DELETE FROM deliveries WHERE message_id = ?1", [&key])?;
        tx.execute(
            "DELETE FROM contents WHERE message_id = ?1 AND stage <> 'raw'",
            [&key],
        )?;
        tx.execute(
            "UPDATE messages SET status = 'received', error = NULL WHERE id = ?1",
            [&key],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn next_delivery(
        &mut self,
        channel: &ChannelId,
        destination: &ConnectorId,
        ordering: QueueOrdering,
        now: Timestamp,
    ) -> StoreResult<Option<Delivery>> {
        let tx = self.conn.transaction()?;
        let candidate = match ordering {
            QueueOrdering::Strict => {
                let head = tx
                    .query_row(
                        &format!(
                            "SELECT message_id, status, attempts, next_attempt_at FROM deliveries
                             WHERE channel = ?1 AND destination = ?2 AND status IN {ACTIVE}
                             ORDER BY message_id LIMIT 1"
                        ),
                        params![channel.as_str(), destination.as_str()],
                        |row| {
                            Ok((
                                row.get::<_, Vec<u8>>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, Option<i64>>(3)?,
                            ))
                        },
                    )
                    .optional()?;
                match head {
                    Some((key, status, tries, next)) => {
                        let status: DestinationStatus = parse("delivery status", &status)?;
                        let due = next.is_none_or(|at| at <= now.unix_nanos());
                        match status {
                            DestinationStatus::Queued => Some((key, tries)),
                            DestinationStatus::Retrying if due => Some((key, tries)),
                            _ => None,
                        }
                    }
                    None => None,
                }
            }
            QueueOrdering::BestEffort => tx
                .query_row(
                    "SELECT message_id, attempts FROM deliveries
                     WHERE channel = ?1 AND destination = ?2
                       AND (status = 'queued' OR (status = 'retrying' AND next_attempt_at <= ?3))
                     ORDER BY message_id LIMIT 1",
                    params![channel.as_str(), destination.as_str(), now.unix_nanos()],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?,
        };
        let Some((key, tries)) = candidate else {
            return Ok(None);
        };
        tx.execute(
            "UPDATE deliveries SET status = 'sending', updated_at = ?1
             WHERE message_id = ?2 AND destination = ?3",
            params![now.unix_nanos(), key, destination.as_str()],
        )?;
        let message_id = id_from(&key)?;
        let (payload, data_type) = tx
            .query_row(
                "SELECT data, data_type FROM contents
                 WHERE message_id = ?1 AND stage = 'encoded' AND destination = ?2",
                params![key, destination.as_str()],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::Corrupt {
                field: "contents",
                detail: format!("no encoded content of {message_id} for {destination}"),
            })?;
        tx.commit()?;
        Ok(Some(Delivery {
            message_id,
            channel: channel.clone(),
            destination: destination.clone(),
            attempts: attempts(tries),
            payload,
            data_type: optional("data_type", data_type)?,
        }))
    }

    fn complete_delivery(
        &mut self,
        id: MessageId,
        destination: &ConnectorId,
        outcome: &DeliveryOutcome,
        now: Timestamp,
    ) -> StoreResult<()> {
        let key = id_bytes(id);
        let tx = self.conn.transaction()?;
        let status: String = tx
            .query_row(
                "SELECT status FROM deliveries WHERE message_id = ?1 AND destination = ?2",
                params![key, destination.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::DeliveryNotFound(id, destination.clone()))?;
        if parse::<DestinationStatus>("delivery status", &status)? != DestinationStatus::Sending {
            return Err(StoreError::InvalidState(format!(
                "delivery of {id} to {destination} is {status}, not sending"
            )));
        }
        let (status, next, error) = match outcome {
            DeliveryOutcome::Sent { response } => {
                if let Some(response) = response {
                    tx.execute(
                        "INSERT OR REPLACE INTO contents (message_id, stage, destination, data_type, data)
                         VALUES (?1, 'response', ?2, NULL, ?3)",
                        params![key, destination.as_str(), response],
                    )?;
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
             SET status = ?1, attempts = attempts + 1, next_attempt_at = ?2,
                 last_error = COALESCE(?3, last_error), updated_at = ?4
             WHERE message_id = ?5 AND destination = ?6",
            params![
                status.as_str(),
                next,
                error,
                now.unix_nanos(),
                key,
                destination.as_str()
            ],
        )?;
        complete_if_final(&tx, &key)?;
        tx.commit()?;
        Ok(())
    }

    fn requeue(
        &mut self,
        id: MessageId,
        destination: &ConnectorId,
        now: Timestamp,
    ) -> StoreResult<()> {
        let key = id_bytes(id);
        let tx = self.conn.transaction()?;
        let status: String = tx
            .query_row(
                "SELECT status FROM deliveries WHERE message_id = ?1 AND destination = ?2",
                params![key, destination.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::DeliveryNotFound(id, destination.clone()))?;
        match parse::<DestinationStatus>("delivery status", &status)? {
            DestinationStatus::Failed | DestinationStatus::Retrying => {}
            other => {
                return Err(StoreError::InvalidState(format!(
                    "delivery of {id} to {destination} is {other}; only failed or retrying deliveries can be requeued"
                )));
            }
        }
        tx.execute(
            "UPDATE deliveries SET status = 'queued', next_attempt_at = NULL, updated_at = ?1
             WHERE message_id = ?2 AND destination = ?3",
            params![now.unix_nanos(), key, destination.as_str()],
        )?;
        tx.execute(
            "UPDATE messages SET status = 'transformed' WHERE id = ?1 AND status = 'completed'",
            [&key],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn recover(&mut self, now: Timestamp) -> StoreResult<RecoveryReport> {
        let tx = self.conn.transaction()?;
        let requeued = tx.execute(
            "UPDATE deliveries SET status = 'queued', updated_at = ?1 WHERE status = 'sending'",
            [now.unix_nanos()],
        )?;
        let unprocessed = {
            let mut statement =
                tx.prepare("SELECT id FROM messages WHERE status = 'received' ORDER BY id")?;
            let keys = statement
                .query_map([], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            keys.iter()
                .map(|key| id_from(key))
                .collect::<StoreResult<Vec<_>>>()?
        };
        tx.commit()?;
        Ok(RecoveryReport {
            requeued: requeued as u64,
            unprocessed,
        })
    }

    fn message(&self, id: MessageId) -> StoreResult<Option<MessageRecord>> {
        let key = id_bytes(id);
        let row = self
            .conn
            .query_row(
                &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id = ?1"),
                [&key],
                MessageRow::read,
            )
            .optional()?;
        row.map(|row| row.into_record(destinations(&self.conn, &key)?))
            .transpose()
    }

    fn content(
        &self,
        id: MessageId,
        stage: Stage,
        destination: Option<&ConnectorId>,
    ) -> StoreResult<Option<Content>> {
        let row = self
            .conn
            .query_row(
                "SELECT data_type, data FROM contents
                 WHERE message_id = ?1 AND stage = ?2 AND destination = ?3",
                params![
                    id_bytes(id),
                    stage.as_str(),
                    destination.map_or("", ConnectorId::as_str)
                ],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        row.map(|(data_type, data)| {
            Ok(Content {
                stage,
                destination: destination.cloned(),
                data_type: optional("data_type", data_type)?,
                data,
            })
        })
        .transpose()
    }

    fn list_messages(&self, query: &MessageQuery) -> StoreResult<Vec<MessageRecord>> {
        let mut sql = format!("SELECT {MESSAGE_COLUMNS} FROM messages m WHERE 1 = 1");
        let mut values: Vec<Value> = Vec::new();
        let mut bind = |sql: &mut String, clause: &str, value: Value| {
            values.push(value);
            sql.push_str(&clause.replace('?', &format!("?{}", values.len())));
        };
        if let Some(channel) = &query.channel {
            bind(
                &mut sql,
                " AND channel = ?",
                Value::Text(channel.to_string()),
            );
        }
        if let Some(status) = query.status {
            bind(
                &mut sql,
                " AND status = ?",
                Value::Text(status.as_str().into()),
            );
        }
        if let Some(from) = query.from {
            bind(
                &mut sql,
                " AND received_at >= ?",
                Value::Integer(from.unix_nanos()),
            );
        }
        if let Some(until) = query.until {
            bind(
                &mut sql,
                " AND received_at < ?",
                Value::Integer(until.unix_nanos()),
            );
        }
        if let Some(before) = query.before {
            bind(
                &mut sql,
                " AND id < ?",
                Value::Blob(id_bytes(before).to_vec()),
            );
        }
        if let Some(status) = query.destination_status {
            bind(
                &mut sql,
                " AND EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.status = ?)",
                Value::Text(status.as_str().into()),
            );
        }
        let limit = query.limit.clamp(1, 1000) as i64;
        bind(&mut sql, " ORDER BY id DESC LIMIT ?", Value::Integer(limit));

        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement
            .query_map(params_from_iter(values), MessageRow::read)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| {
                let states = destinations(&self.conn, &row.id)?;
                row.into_record(states)
            })
            .collect()
    }

    fn queue_stats(
        &self,
        channel: &ChannelId,
        destination: &ConnectorId,
    ) -> StoreResult<QueueStats> {
        let mut stats = QueueStats::default();
        let mut statement = self.conn.prepare_cached(
            "SELECT status, COUNT(*) FROM deliveries
             WHERE channel = ?1 AND destination = ?2
               AND status IN ('queued', 'sending', 'retrying', 'failed')
             GROUP BY status",
        )?;
        let counts = statement
            .query_map(params![channel.as_str(), destination.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (status, count) in counts {
            let count = u64::try_from(count).unwrap_or_default();
            match parse::<DestinationStatus>("delivery status", &status)? {
                DestinationStatus::Queued => stats.queued = count,
                DestinationStatus::Sending => stats.sending = count,
                DestinationStatus::Retrying => stats.retrying = count,
                DestinationStatus::Failed => stats.failed = count,
                _ => {}
            }
        }
        stats.oldest_pending_at = self
            .conn
            .query_row(
                &format!(
                    "SELECT MIN(m.received_at) FROM deliveries d JOIN messages m ON m.id = d.message_id
                     WHERE d.channel = ?1 AND d.destination = ?2 AND d.status IN {ACTIVE}"
                ),
                params![channel.as_str(), destination.as_str()],
                |row| row.get::<_, Option<i64>>(0),
            )?
            .map(Timestamp::from_unix_nanos);
        Ok(stats)
    }

    fn next_retry_at(
        &self,
        channel: &ChannelId,
        destination: &ConnectorId,
    ) -> StoreResult<Option<Timestamp>> {
        Ok(self
            .conn
            .query_row(
                "SELECT MIN(next_attempt_at) FROM deliveries
                 WHERE channel = ?1 AND destination = ?2 AND status = 'retrying'",
                params![channel.as_str(), destination.as_str()],
                |row| row.get::<_, Option<i64>>(0),
            )?
            .map(Timestamp::from_unix_nanos))
    }

    fn prune(&mut self, policy: &PrunePolicy) -> StoreResult<PruneReport> {
        let tx = self.conn.transaction()?;
        let mut report = PruneReport::default();
        const FINAL: &str = "status IN ('filtered', 'completed')";
        if let Some(before) = policy.contents_before {
            report.contents_pruned = tx.query_row(
                &format!(
                    "SELECT COUNT(*) FROM messages m WHERE received_at < ?1 AND {FINAL}
                     AND EXISTS (SELECT 1 FROM contents c WHERE c.message_id = m.id)"
                ),
                [before.unix_nanos()],
                |row| row.get::<_, i64>(0),
            )? as u64;
            tx.execute(
                &format!(
                    "DELETE FROM contents WHERE message_id IN
                     (SELECT id FROM messages WHERE received_at < ?1 AND {FINAL})"
                ),
                [before.unix_nanos()],
            )?;
        }
        if let Some(before) = policy.messages_before {
            report.messages_pruned = tx.execute(
                &format!("DELETE FROM messages WHERE received_at < ?1 AND {FINAL}"),
                [before.unix_nanos()],
            )? as u64;
        }
        tx.commit()?;
        Ok(report)
    }

    fn erase(&mut self, ids: &[MessageId]) -> StoreResult<u64> {
        let tx = self.conn.transaction()?;
        let mut erased = 0;
        {
            let mut delete = tx.prepare_cached("DELETE FROM messages WHERE id = ?1")?;
            for id in ids {
                erased += delete.execute([id_bytes(*id)])? as u64;
            }
        }
        tx.commit()?;
        Ok(erased)
    }

    fn record_audit(&mut self, event: &AuditEvent) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO audit_events (at, action, actor, message_id, channel, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.at.unix_nanos(),
                event.action,
                event.actor,
                event.message_id.map(|id| id_bytes(id).to_vec()),
                event.channel.as_ref().map(ChannelId::as_str),
                event.detail,
            ],
        )?;
        Ok(())
    }

    fn audit_trail(
        &self,
        message: Option<MessageId>,
        limit: usize,
    ) -> StoreResult<Vec<AuditEvent>> {
        let limit = limit.clamp(1, 10_000) as i64;
        let mut statement = self.conn.prepare_cached(
            "SELECT at, action, actor, message_id, channel, detail FROM audit_events
             WHERE ?1 IS NULL OR message_id = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement
            .query_map(
                params![message.map(|id| id_bytes(id).to_vec()), limit],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(at, action, actor, message_id, channel, detail)| {
                Ok(AuditEvent {
                    at: Timestamp::from_unix_nanos(at),
                    action,
                    actor,
                    message_id: message_id.map(|key| id_from(&key)).transpose()?,
                    channel: optional("channel", channel)?,
                    detail,
                })
            })
            .collect()
    }
}
