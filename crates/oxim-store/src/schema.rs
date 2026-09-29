//! Database schema and migrations.

use rusqlite::Connection;

use crate::error::{StoreError, StoreResult};

/// Migrations in order. Migration `n` (1-based) moves the database from
/// schema version `n - 1` to `n`. Released migrations are never edited.
const MIGRATIONS: &[&str] = &[
    // 1: messages, contents, deliveries and the audit trail.
    "
    CREATE TABLE messages (
        id BLOB PRIMARY KEY NOT NULL,
        channel TEXT NOT NULL,
        connector TEXT NOT NULL,
        received_at INTEGER NOT NULL,
        data_type TEXT NOT NULL,
        status TEXT NOT NULL,
        peer TEXT,
        device TEXT,
        correlation_id TEXT,
        metadata TEXT NOT NULL,
        error TEXT
    ) WITHOUT ROWID;
    CREATE INDEX messages_by_channel ON messages (channel, id);
    CREATE INDEX messages_by_status ON messages (status, id);
    CREATE INDEX messages_by_time ON messages (received_at);
    CREATE INDEX messages_by_correlation ON messages (correlation_id)
        WHERE correlation_id IS NOT NULL;

    CREATE TABLE contents (
        message_id BLOB NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
        stage TEXT NOT NULL,
        destination TEXT NOT NULL DEFAULT '',
        data_type TEXT,
        data BLOB NOT NULL,
        PRIMARY KEY (message_id, stage, destination)
    ) WITHOUT ROWID;

    CREATE TABLE deliveries (
        message_id BLOB NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
        destination TEXT NOT NULL,
        channel TEXT NOT NULL,
        status TEXT NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0,
        next_attempt_at INTEGER,
        last_error TEXT,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (message_id, destination)
    ) WITHOUT ROWID;
    CREATE INDEX deliveries_by_queue ON deliveries (channel, destination, status, message_id);

    CREATE TABLE audit_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        at INTEGER NOT NULL,
        action TEXT NOT NULL,
        actor TEXT NOT NULL,
        message_id BLOB,
        channel TEXT,
        detail TEXT
    );
    CREATE INDEX audit_by_message ON audit_events (message_id) WHERE message_id IS NOT NULL;
    CREATE INDEX audit_by_time ON audit_events (at);
    ",
    // 2: the wrapped data key when contents are encrypted.
    "
    CREATE TABLE content_keys (
        id INTEGER PRIMARY KEY,
        wrapped BLOB NOT NULL,
        created_at INTEGER NOT NULL
    );
    ",
];

/// The schema version this build creates and understands.
pub(crate) const CURRENT_VERSION: u32 = MIGRATIONS.len() as u32;

/// Applies connection settings and pending migrations.
pub(crate) fn prepare(conn: &mut Connection) -> StoreResult<()> {
    // WAL lets readers work while a writer commits; FULL synchronous mode
    // makes every commit durable before it returns, which is what allows
    // OXIM to acknowledge a message as soon as it is stored (ADR 0004).
    // In-memory databases answer `memory`; both answers are fine.
    let _mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;

    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > CURRENT_VERSION {
        return Err(StoreError::SchemaTooNew {
            found: version,
            supported: CURRENT_VERSION,
        });
    }
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(migration)?;
        tx.pragma_update(None, "user_version", index as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}
