//! Channel version history: every saved, deleted or restored version of a
//! channel file, kept in `history.db` next to the other databases.
//!
//! Changes made through the API are recorded with the user who made them;
//! changes the file watcher of `oxim run` deploys are recorded as `file`.
//! A version identical to the channel's latest one is not recorded again,
//! so a change saved through the API and then picked up by the watcher
//! appears once.

use std::path::Path;
use std::sync::Mutex;

use oxim_model::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// Errors of the history store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HistoryError {
    /// The database reported an error.
    #[error("channel history database: {0}")]
    Database(#[from] rusqlite::Error),
    /// The store's lock was poisoned by a panic in another thread.
    #[error("the channel history is unavailable")]
    Unavailable,
}

/// What a version records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// The channel file was created or changed.
    Saved,
    /// The channel file was deleted.
    Deleted,
    /// An earlier version was restored.
    Restored,
}

impl Change {
    fn as_str(self) -> &'static str {
        match self {
            Self::Saved => "saved",
            Self::Deleted => "deleted",
            Self::Restored => "restored",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "deleted" => Self::Deleted,
            "restored" => Self::Restored,
            _ => Self::Saved,
        }
    }
}

/// One version of a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelVersion {
    /// The channel identifier.
    pub channel: String,
    /// The version number, from 1.
    pub version: u64,
    /// What happened.
    pub change: Change,
    /// Who made the change: a user name, a token name or `file`.
    pub actor: String,
    /// When it was recorded.
    pub at: Timestamp,
    /// The channel file's YAML; `None` for a deletion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaml: Option<String>,
}

/// The version history of every channel.
#[derive(Debug)]
pub struct ChannelHistory {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS channel_versions (
    channel TEXT NOT NULL,
    version INTEGER NOT NULL,
    change TEXT NOT NULL,
    yaml TEXT,
    actor TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (channel, version)
);
";

impl ChannelHistory {
    /// Opens or creates the history database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, HistoryError> {
        Self::prepare(Connection::open(path)?)
    }

    /// A private in-memory history, for tests.
    pub fn open_in_memory() -> Result<Self, HistoryError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self, HistoryError> {
        let _mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Records a version of `channel`. Returns its number, or `None` when
    /// it equals the latest version (nothing new to record).
    pub fn record(
        &self,
        channel: &str,
        change: Change,
        yaml: Option<&str>,
        actor: &str,
        at: Timestamp,
    ) -> Result<Option<u64>, HistoryError> {
        let mut conn = self.conn.lock().map_err(|_| HistoryError::Unavailable)?;
        let tx = conn.transaction()?;
        let latest: Option<(i64, String, Option<String>)> = tx
            .query_row(
                "SELECT version, change, yaml FROM channel_versions
                 WHERE channel = ?1 ORDER BY version DESC LIMIT 1",
                [channel],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((_, last_change, last_yaml)) = &latest {
            let unchanged = match change {
                Change::Deleted => last_change == "deleted",
                Change::Saved | Change::Restored => {
                    last_change != "deleted" && last_yaml.as_deref() == yaml
                }
            };
            if unchanged {
                return Ok(None);
            }
        }
        let version = latest.map_or(1, |(version, _, _)| version + 1);
        tx.execute(
            "INSERT INTO channel_versions (channel, version, change, yaml, actor, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                channel,
                version,
                change.as_str(),
                yaml,
                actor,
                at.unix_nanos()
            ],
        )?;
        tx.commit()?;
        Ok(Some(u64::try_from(version).unwrap_or(0)))
    }

    /// The versions of `channel`, newest first, without their YAML.
    pub fn list(&self, channel: &str) -> Result<Vec<ChannelVersion>, HistoryError> {
        let conn = self.conn.lock().map_err(|_| HistoryError::Unavailable)?;
        let mut statement = conn.prepare_cached(
            "SELECT version, change, actor, at FROM channel_versions
             WHERE channel = ?1 ORDER BY version DESC",
        )?;
        let versions = statement
            .query_map([channel], |row| {
                Ok(ChannelVersion {
                    channel: channel.to_owned(),
                    version: u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0),
                    change: Change::parse(&row.get::<_, String>(1)?),
                    actor: row.get(2)?,
                    at: Timestamp::from_unix_nanos(row.get(3)?),
                    yaml: None,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(versions)
    }

    /// One version of `channel` with its YAML.
    pub fn get(&self, channel: &str, version: u64) -> Result<Option<ChannelVersion>, HistoryError> {
        let conn = self.conn.lock().map_err(|_| HistoryError::Unavailable)?;
        let version = i64::try_from(version).unwrap_or(i64::MAX);
        Ok(conn
            .query_row(
                "SELECT change, yaml, actor, at FROM channel_versions
                 WHERE channel = ?1 AND version = ?2",
                params![channel, version],
                |row| {
                    Ok(ChannelVersion {
                        channel: channel.to_owned(),
                        version: u64::try_from(version).unwrap_or(0),
                        change: Change::parse(&row.get::<_, String>(0)?),
                        yaml: row.get(1)?,
                        actor: row.get(2)?,
                        at: Timestamp::from_unix_nanos(row.get(3)?),
                    })
                },
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_unix_nanos(seconds * 1_000_000_000)
    }

    #[test]
    fn records_versions_once_and_in_order() {
        let history = ChannelHistory::open_in_memory().unwrap();
        assert_eq!(
            history
                .record("lab", Change::Saved, Some("id: lab\n"), "alice", at(1))
                .unwrap(),
            Some(1)
        );
        // The watcher sees the same file: nothing new.
        assert_eq!(
            history
                .record("lab", Change::Saved, Some("id: lab\n"), "file", at(2))
                .unwrap(),
            None
        );
        assert_eq!(
            history
                .record(
                    "lab",
                    Change::Saved,
                    Some("id: lab\nenabled: false\n"),
                    "file",
                    at(3)
                )
                .unwrap(),
            Some(2)
        );
        assert_eq!(
            history
                .record("lab", Change::Deleted, None, "bob", at(4))
                .unwrap(),
            Some(3)
        );
        assert_eq!(
            history
                .record("lab", Change::Deleted, None, "file", at(5))
                .unwrap(),
            None
        );
        // Saving the first YAML again after a deletion is a new version.
        assert_eq!(
            history
                .record("lab", Change::Restored, Some("id: lab\n"), "alice", at(6))
                .unwrap(),
            Some(4)
        );
        let versions = history.list("lab").unwrap();
        assert_eq!(
            versions.iter().map(|v| v.version).collect::<Vec<_>>(),
            [4, 3, 2, 1]
        );
        assert_eq!(versions[1].change, Change::Deleted);
        assert!(versions.iter().all(|v| v.yaml.is_none()));
        let first = history.get("lab", 1).unwrap().unwrap();
        assert_eq!(first.yaml.as_deref(), Some("id: lab\n"));
        assert_eq!(first.actor, "alice");
        assert!(history.get("lab", 9).unwrap().is_none());
        assert!(history.list("other").unwrap().is_empty());
    }
}
