use oxim_model::{ConnectorId, MessageId};
use thiserror::Error;

/// Errors returned by a message store.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The database reported an error.
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    /// The database was written by a newer OXIM version.
    #[error("database schema version {found} is newer than the supported version {supported}")]
    SchemaTooNew {
        /// Version found in the database.
        found: u32,
        /// Newest version this build understands.
        supported: u32,
    },
    /// A stored value could not be decoded.
    #[error("corrupt stored value in {field}: {detail}")]
    Corrupt {
        /// The column or field.
        field: &'static str,
        /// What was wrong.
        detail: String,
    },
    /// The message does not exist.
    #[error("message {0} not found")]
    MessageNotFound(MessageId),
    /// The message has no delivery to that destination.
    #[error("message {0} has no delivery to {1}")]
    DeliveryNotFound(MessageId, ConnectorId),
    /// The requested operation does not fit the current state.
    #[error("invalid state: {0}")]
    InvalidState(String),
    /// Another database backend (such as PostgreSQL) reported an error.
    #[error("database error: {0}")]
    Backend(String),
}

/// Result alias for store operations.
pub type StoreResult<T> = Result<T, StoreError>;
