use thiserror::Error;

/// Errors from authentication and user management.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The user name or password is wrong.
    #[error("invalid user name or password")]
    InvalidCredentials,
    /// The account is disabled.
    #[error("the account is disabled")]
    AccountDisabled,
    /// The session or API token is unknown, revoked or expired.
    #[error("the session or token is invalid or expired")]
    InvalidToken,
    /// The request violates a policy, for example the password policy.
    #[error("{0}")]
    Policy(String),
    /// The user name is not acceptable.
    #[error("user names are 1 to 64 letters, digits, '.', '_' or '-'")]
    InvalidUsername,
    /// A user with that name exists.
    #[error("user {0:?} already exists")]
    UserExists(String),
    /// No user with that name exists.
    #[error("user {0:?} not found")]
    UserNotFound(String),
    /// The API token does not exist.
    #[error("API token {0} not found")]
    TokenNotFound(i64),
    /// Password hashing failed.
    #[error("password hashing failed: {0}")]
    Hash(String),
    /// The operating system's random source failed.
    #[error("the random source is unavailable")]
    Random,
    /// The database failed.
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    /// The database was written by a newer OXIM version.
    #[error("auth database schema version {0} is newer than this OXIM supports")]
    SchemaTooNew(u32),
    /// Stored data could not be read.
    #[error("corrupt stored value: {0}")]
    Corrupt(String),
}
