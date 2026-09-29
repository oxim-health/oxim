//! Users, sessions and API tokens in `auth.db`.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use oxim_model::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::error::AuthError;
use crate::role::{Permission, Role};
use crate::secret::{
    DUMMY_HASH, check_password_policy, hash_password, new_token, token_hash, verify_password,
};

const MIGRATIONS: &[&str] = &[
    // 1: users, sessions and API tokens.
    "
    CREATE TABLE users (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        username TEXT NOT NULL UNIQUE COLLATE NOCASE,
        display_name TEXT NOT NULL,
        password_hash TEXT NOT NULL,
        role TEXT NOT NULL,
        disabled INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        last_login_at INTEGER
    );
    CREATE TABLE sessions (
        token_hash TEXT PRIMARY KEY NOT NULL,
        user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
        csrf_hash TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        last_seen_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        idle_nanos INTEGER NOT NULL,
        client TEXT
    ) WITHOUT ROWID;
    CREATE INDEX sessions_by_user ON sessions (user_id);
    CREATE TABLE api_tokens (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        token_hash TEXT NOT NULL UNIQUE,
        role TEXT NOT NULL,
        created_by TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER,
        last_used_at INTEGER,
        revoked INTEGER NOT NULL DEFAULT 0
    );
    ",
];

/// Prefix of session tokens.
pub const SESSION_PREFIX: &str = "oxs_";
/// Prefix of API tokens.
pub const API_TOKEN_PREFIX: &str = "oxt_";

/// How often "last seen" and "last used" timestamps are written at most.
const TOUCH_INTERVAL_NANOS: i64 = 60_000_000_000;

/// A user account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct User {
    /// Internal identifier.
    pub id: i64,
    /// Login name (case-insensitive).
    pub username: String,
    /// Name shown in the UI and audit trail.
    pub display_name: String,
    /// Role.
    pub role: Role,
    /// Whether logins are refused.
    pub disabled: bool,
    /// Creation time.
    pub created_at: Timestamp,
    /// Last change.
    pub updated_at: Timestamp,
    /// Last successful login.
    pub last_login_at: Option<Timestamp>,
}

/// A user to create.
#[derive(Debug, Clone)]
pub struct NewUser<'a> {
    /// Login name.
    pub username: &'a str,
    /// Display name; the login name when empty.
    pub display_name: &'a str,
    /// Initial password.
    pub password: &'a str,
    /// Role.
    pub role: Role,
}

/// Changes to a user. `None` keeps the current value.
#[derive(Debug, Clone, Default)]
pub struct UserUpdate {
    /// New display name.
    pub display_name: Option<String>,
    /// New role.
    pub role: Option<Role>,
    /// Disable or enable the account. Disabling ends its sessions.
    pub disabled: Option<bool>,
}

/// How long sessions last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPolicy {
    /// A session ends after this long without requests.
    pub idle: Duration,
    /// A session ends this long after login, however active.
    pub max: Duration,
}

impl Default for SessionPolicy {
    /// 30 minutes idle, 12 hours at most.
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(30 * 60),
            max: Duration::from_secs(12 * 3600),
        }
    }
}

/// A new session, returned once at login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGrant {
    /// The session token (bearer token or cookie value).
    pub token: String,
    /// The CSRF token that cookie-authenticated changes must send back.
    pub csrf_token: String,
    /// When the session ends at the latest.
    pub expires_at: Timestamp,
}

/// How a request authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    /// A login session.
    Session,
    /// An API token.
    ApiToken,
}

/// Who is making a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Principal {
    /// The user name, or `token:<name>` for API tokens.
    pub username: String,
    /// Display name.
    pub display_name: String,
    /// Effective role.
    pub role: Role,
    /// How the request authenticated.
    pub kind: PrincipalKind,
    /// The user, for sessions.
    pub user_id: Option<i64>,
    /// The API token, for token requests.
    pub token_id: Option<i64>,
}

impl Principal {
    /// Whether the principal holds `permission`.
    pub fn can(&self, permission: Permission) -> bool {
        self.role.allows(permission)
    }
}

/// An authenticated session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Who the session belongs to.
    pub principal: Principal,
    csrf_hash: String,
}

impl Session {
    /// Whether `token` is this session's CSRF token.
    pub fn csrf_matches(&self, token: &str) -> bool {
        crate::secret::constant_time_eq(token_hash(token).as_bytes(), self.csrf_hash.as_bytes())
    }
}

/// An API token as listed (never including the token itself).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiToken {
    /// Identifier.
    pub id: i64,
    /// Name, for example the integration using it.
    pub name: String,
    /// Role granted to requests with this token.
    pub role: Role,
    /// Who created it.
    pub created_by: String,
    /// Creation time.
    pub created_at: Timestamp,
    /// Expiry, if any.
    pub expires_at: Option<Timestamp>,
    /// Last use.
    pub last_used_at: Option<Timestamp>,
    /// Whether it was revoked.
    pub revoked: bool,
}

/// Users, sessions and API tokens in one SQLite database.
///
/// Methods are blocking; password hashing takes tens of milliseconds by
/// design. Async callers should use a blocking task.
#[derive(Debug)]
pub struct AuthStore {
    conn: Mutex<Connection>,
}

fn valid_username(username: &str) -> bool {
    (1..=64).contains(&username.len())
        && username
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn nanos(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}

fn role(text: &str) -> Result<Role, AuthError> {
    text.parse()
        .map_err(|_| AuthError::Corrupt(format!("role {text:?}")))
}

const USER_COLUMNS: &str =
    "id, username, display_name, role, disabled, created_at, updated_at, last_login_at";

fn read_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<(User, String)> {
    let role_text: String = row.get(3)?;
    Ok((
        User {
            id: row.get(0)?,
            username: row.get(1)?,
            display_name: row.get(2)?,
            role: Role::Viewer,
            disabled: row.get::<_, i64>(4)? != 0,
            created_at: Timestamp::from_unix_nanos(row.get(5)?),
            updated_at: Timestamp::from_unix_nanos(row.get(6)?),
            last_login_at: row
                .get::<_, Option<i64>>(7)?
                .map(Timestamp::from_unix_nanos),
        },
        role_text,
    ))
}

fn finish_user((mut user, role_text): (User, String)) -> Result<User, AuthError> {
    user.role = role(&role_text)?;
    Ok(user)
}

impl AuthStore {
    /// Opens or creates `auth.db` at `path` and applies migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AuthError> {
        Self::prepare(Connection::open(path)?)
    }

    /// A private in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self, AuthError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut conn: Connection) -> Result<Self, AuthError> {
        let _mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(Duration::from_secs(10))?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let current = MIGRATIONS.len() as u32;
        if version > current {
            return Err(AuthError::SchemaTooNew(version));
        }
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(migration)?;
            tx.pragma_update(None, "user_version", index as u32 + 1)?;
            tx.commit()?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>, AuthError> {
        self.conn
            .lock()
            .map_err(|_| AuthError::Corrupt("auth database lock poisoned".into()))
    }

    /// The number of users.
    pub fn user_count(&self) -> Result<u64, AuthError> {
        let count: i64 = self
            .conn()?
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        Ok(u64::try_from(count).unwrap_or_default())
    }

    /// Creates a user after checking the name and password policy.
    pub fn create_user(&self, new: &NewUser<'_>, now: Timestamp) -> Result<User, AuthError> {
        if !valid_username(new.username) {
            return Err(AuthError::InvalidUsername);
        }
        check_password_policy(new.username, new.password)?;
        let hash = hash_password(new.password)?;
        let display = if new.display_name.trim().is_empty() {
            new.username
        } else {
            new.display_name.trim()
        };
        let conn = self.conn()?;
        let inserted = conn.execute(
            "INSERT INTO users (username, display_name, password_hash, role, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![new.username, display, hash, new.role.as_str(), now.unix_nanos()],
        );
        match inserted {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(AuthError::UserExists(new.username.to_owned()));
            }
            Err(e) => return Err(e.into()),
        }
        drop(conn);
        self.user(new.username)?
            .ok_or_else(|| AuthError::UserNotFound(new.username.to_owned()))
    }

    /// A user by name (case-insensitive).
    pub fn user(&self, username: &str) -> Result<Option<User>, AuthError> {
        self.conn()?
            .query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE username = ?1"),
                [username],
                read_user,
            )
            .optional()?
            .map(finish_user)
            .transpose()
    }

    /// Every user, by name.
    pub fn users(&self) -> Result<Vec<User>, AuthError> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(&format!(
            "SELECT {USER_COLUMNS} FROM users ORDER BY username"
        ))?;
        let rows = statement
            .query_map([], read_user)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(finish_user).collect()
    }

    /// Changes a user. Disabling or demoting ends the user's sessions.
    pub fn update_user(
        &self,
        username: &str,
        update: &UserUpdate,
        now: Timestamp,
    ) -> Result<User, AuthError> {
        let user = self
            .user(username)?
            .ok_or_else(|| AuthError::UserNotFound(username.to_owned()))?;
        let conn = self.conn()?;
        if let Some(display) = &update.display_name {
            conn.execute(
                "UPDATE users SET display_name = ?1, updated_at = ?2 WHERE id = ?3",
                params![display.trim(), now.unix_nanos(), user.id],
            )?;
        }
        if let Some(role) = update.role {
            conn.execute(
                "UPDATE users SET role = ?1, updated_at = ?2 WHERE id = ?3",
                params![role.as_str(), now.unix_nanos(), user.id],
            )?;
            if !role.covers(user.role) {
                conn.execute("DELETE FROM sessions WHERE user_id = ?1", [user.id])?;
            }
        }
        if let Some(disabled) = update.disabled {
            conn.execute(
                "UPDATE users SET disabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![i64::from(disabled), now.unix_nanos(), user.id],
            )?;
            if disabled {
                conn.execute("DELETE FROM sessions WHERE user_id = ?1", [user.id])?;
            }
        }
        drop(conn);
        self.user(username)?
            .ok_or_else(|| AuthError::UserNotFound(username.to_owned()))
    }

    /// Sets a new password and ends the user's sessions.
    pub fn set_password(
        &self,
        username: &str,
        password: &str,
        now: Timestamp,
    ) -> Result<(), AuthError> {
        let user = self
            .user(username)?
            .ok_or_else(|| AuthError::UserNotFound(username.to_owned()))?;
        check_password_policy(&user.username, password)?;
        let hash = hash_password(password)?;
        let conn = self.conn()?;
        conn.execute(
            "UPDATE users SET password_hash = ?1, updated_at = ?2 WHERE id = ?3",
            params![hash, now.unix_nanos(), user.id],
        )?;
        conn.execute("DELETE FROM sessions WHERE user_id = ?1", [user.id])?;
        Ok(())
    }

    /// Checks a user name and password. Unknown users take as long as known
    /// ones, and a disabled account is only reported when the password is
    /// right, so failures do not reveal which accounts exist.
    pub fn authenticate(
        &self,
        username: &str,
        password: &str,
        now: Timestamp,
    ) -> Result<User, AuthError> {
        let stored: Option<(i64, String, bool)> = self
            .conn()?
            .query_row(
                "SELECT id, password_hash, disabled FROM users WHERE username = ?1",
                [username],
                |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0)),
            )
            .optional()?;
        let Some((id, hash, disabled)) = stored else {
            if let Some(dummy) = DUMMY_HASH.as_deref() {
                let _ = verify_password(dummy, password);
            }
            return Err(AuthError::InvalidCredentials);
        };
        if !verify_password(&hash, password) {
            return Err(AuthError::InvalidCredentials);
        }
        if disabled {
            return Err(AuthError::AccountDisabled);
        }
        self.conn()?.execute(
            "UPDATE users SET last_login_at = ?1 WHERE id = ?2",
            params![now.unix_nanos(), id],
        )?;
        self.user(username)?
            .ok_or_else(|| AuthError::UserNotFound(username.to_owned()))
    }

    /// Starts a session for `user`.
    pub fn create_session(
        &self,
        user: &User,
        policy: SessionPolicy,
        client: Option<&str>,
        now: Timestamp,
    ) -> Result<SessionGrant, AuthError> {
        let token = new_token(SESSION_PREFIX)?;
        let csrf_token = new_token("")?;
        let expires_at =
            Timestamp::from_unix_nanos(now.unix_nanos().saturating_add(nanos(policy.max)));
        self.conn()?.execute(
            "INSERT INTO sessions (token_hash, user_id, csrf_hash, created_at, last_seen_at, expires_at, idle_nanos, client)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7)",
            params![
                token_hash(&token),
                user.id,
                token_hash(&csrf_token),
                now.unix_nanos(),
                expires_at.unix_nanos(),
                nanos(policy.idle),
                client,
            ],
        )?;
        Ok(SessionGrant {
            token,
            csrf_token,
            expires_at,
        })
    }

    /// Looks up a session, ending it when idle or past its maximum age.
    pub fn session(&self, token: &str, now: Timestamp) -> Result<Session, AuthError> {
        if !token.starts_with(SESSION_PREFIX) {
            return Err(AuthError::InvalidToken);
        }
        let hash = token_hash(token);
        let conn = self.conn()?;
        let row = conn
            .query_row(
                "SELECT s.csrf_hash, s.last_seen_at, s.expires_at, s.idle_nanos,
                        u.id, u.username, u.display_name, u.role, u.disabled
                 FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash = ?1",
                [&hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, i64>(8)? != 0,
                    ))
                },
            )
            .optional()?
            .ok_or(AuthError::InvalidToken)?;
        let (csrf_hash, last_seen, expires, idle, user_id, username, display, role_text, disabled) =
            row;
        let now_nanos = now.unix_nanos();
        if disabled || now_nanos >= expires || now_nanos.saturating_sub(last_seen) >= idle {
            conn.execute("DELETE FROM sessions WHERE token_hash = ?1", [&hash])?;
            return Err(AuthError::InvalidToken);
        }
        if now_nanos.saturating_sub(last_seen) >= TOUCH_INTERVAL_NANOS {
            conn.execute(
                "UPDATE sessions SET last_seen_at = ?1 WHERE token_hash = ?2",
                params![now_nanos, hash],
            )?;
        }
        Ok(Session {
            principal: Principal {
                username,
                display_name: display,
                role: role(&role_text)?,
                kind: PrincipalKind::Session,
                user_id: Some(user_id),
                token_id: None,
            },
            csrf_hash,
        })
    }

    /// Ends a session.
    pub fn end_session(&self, token: &str) -> Result<(), AuthError> {
        self.conn()?.execute(
            "DELETE FROM sessions WHERE token_hash = ?1",
            [token_hash(token)],
        )?;
        Ok(())
    }

    /// Ends every session of a user. Returns how many ended.
    pub fn end_user_sessions(&self, username: &str) -> Result<u64, AuthError> {
        let ended = self.conn()?.execute(
            "DELETE FROM sessions WHERE user_id = (SELECT id FROM users WHERE username = ?1)",
            [username],
        )?;
        Ok(ended as u64)
    }

    /// Deletes expired sessions. Returns how many were deleted.
    pub fn prune_sessions(&self, now: Timestamp) -> Result<u64, AuthError> {
        let deleted = self.conn()?.execute(
            "DELETE FROM sessions WHERE expires_at <= ?1 OR ?1 - last_seen_at >= idle_nanos",
            [now.unix_nanos()],
        )?;
        Ok(deleted as u64)
    }

    /// Creates an API token. The token is returned once and never stored.
    pub fn create_api_token(
        &self,
        name: &str,
        role: Role,
        created_by: &str,
        expires_at: Option<Timestamp>,
        now: Timestamp,
    ) -> Result<(String, ApiToken), AuthError> {
        let name = name.trim();
        if name.is_empty() || name.len() > 100 {
            return Err(AuthError::Policy(
                "token names are 1 to 100 characters".into(),
            ));
        }
        let token = new_token(API_TOKEN_PREFIX)?;
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO api_tokens (name, token_hash, role, created_by, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                name,
                token_hash(&token),
                role.as_str(),
                created_by,
                now.unix_nanos(),
                expires_at.map(Timestamp::unix_nanos),
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        let listed = self
            .api_tokens()?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or(AuthError::TokenNotFound(id))?;
        Ok((token, listed))
    }

    /// Every API token, newest first.
    pub fn api_tokens(&self) -> Result<Vec<ApiToken>, AuthError> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id, name, role, created_by, created_at, expires_at, last_used_at, revoked
             FROM api_tokens ORDER BY id DESC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, i64>(7)? != 0,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(id, name, role_text, created_by, created, expires, used, revoked)| {
                    Ok(ApiToken {
                        id,
                        name,
                        role: role(&role_text)?,
                        created_by,
                        created_at: Timestamp::from_unix_nanos(created),
                        expires_at: expires.map(Timestamp::from_unix_nanos),
                        last_used_at: used.map(Timestamp::from_unix_nanos),
                        revoked,
                    })
                },
            )
            .collect()
    }

    /// Revokes an API token.
    pub fn revoke_api_token(&self, id: i64) -> Result<(), AuthError> {
        let changed = self
            .conn()?
            .execute("UPDATE api_tokens SET revoked = 1 WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(AuthError::TokenNotFound(id));
        }
        Ok(())
    }

    /// Authenticates a request made with an API token.
    pub fn api_token(&self, token: &str, now: Timestamp) -> Result<Principal, AuthError> {
        if !token.starts_with(API_TOKEN_PREFIX) {
            return Err(AuthError::InvalidToken);
        }
        let hash = token_hash(token);
        let conn = self.conn()?;
        let (id, name, role_text, expires, used, revoked) = conn
            .query_row(
                "SELECT id, name, role, expires_at, last_used_at, revoked FROM api_tokens WHERE token_hash = ?1",
                [&hash],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, i64>(5)? != 0,
                    ))
                },
            )
            .optional()?
            .ok_or(AuthError::InvalidToken)?;
        let now_nanos = now.unix_nanos();
        if revoked || expires.is_some_and(|at| now_nanos >= at) {
            return Err(AuthError::InvalidToken);
        }
        if used.is_none_or(|at| now_nanos.saturating_sub(at) >= TOUCH_INTERVAL_NANOS) {
            conn.execute(
                "UPDATE api_tokens SET last_used_at = ?1 WHERE id = ?2",
                params![now_nanos, id],
            )?;
        }
        Ok(Principal {
            username: format!("token:{name}"),
            display_name: name,
            role: role(&role_text)?,
            kind: PrincipalKind::ApiToken,
            user_id: None,
            token_id: Some(id),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000_000;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_unix_nanos(1_790_000_000 * SECOND + seconds * SECOND)
    }

    fn store_with_alice() -> (AuthStore, User) {
        let store = AuthStore::open_in_memory().unwrap();
        let user = store
            .create_user(
                &NewUser {
                    username: "alice",
                    display_name: "Alice Admin",
                    password: "a long enough passphrase",
                    role: Role::Admin,
                },
                at(0),
            )
            .unwrap();
        (store, user)
    }

    #[test]
    fn creates_and_authenticates_users() {
        let (store, user) = store_with_alice();
        assert_eq!(store.user_count().unwrap(), 1);
        assert_eq!(user.display_name, "Alice Admin");
        let logged = store
            .authenticate("ALICE", "a long enough passphrase", at(5))
            .unwrap();
        assert_eq!(logged.last_login_at, Some(at(5)));
        assert!(matches!(
            store.authenticate("alice", "wrong passphrase!!", at(6)),
            Err(AuthError::InvalidCredentials)
        ));
        assert!(matches!(
            store.authenticate("nobody", "a long enough passphrase", at(6)),
            Err(AuthError::InvalidCredentials)
        ));
        assert!(matches!(
            store.create_user(
                &NewUser {
                    username: "Alice",
                    display_name: "",
                    password: "another passphrase",
                    role: Role::Viewer
                },
                at(7)
            ),
            Err(AuthError::UserExists(_))
        ));
        assert!(matches!(
            store.create_user(
                &NewUser {
                    username: "bad name",
                    display_name: "",
                    password: "another passphrase",
                    role: Role::Viewer
                },
                at(7)
            ),
            Err(AuthError::InvalidUsername)
        ));
    }

    #[test]
    fn disabled_accounts_cannot_log_in_and_lose_sessions() {
        let (store, user) = store_with_alice();
        let grant = store
            .create_session(&user, SessionPolicy::default(), None, at(1))
            .unwrap();
        store
            .update_user(
                "alice",
                &UserUpdate {
                    disabled: Some(true),
                    ..UserUpdate::default()
                },
                at(2),
            )
            .unwrap();
        assert!(store.session(&grant.token, at(3)).is_err());
        assert!(matches!(
            store.authenticate("alice", "a long enough passphrase", at(4)),
            Err(AuthError::AccountDisabled)
        ));
    }

    #[test]
    fn sessions_expire_when_idle_or_old() {
        let (store, user) = store_with_alice();
        let policy = SessionPolicy {
            idle: Duration::from_secs(600),
            max: Duration::from_secs(3600),
        };
        let grant = store
            .create_session(&user, policy, Some("10.0.0.1"), at(0))
            .unwrap();
        let session = store.session(&grant.token, at(300)).unwrap();
        assert_eq!(session.principal.username, "alice");
        assert!(session.csrf_matches(&grant.csrf_token));
        assert!(!session.csrf_matches("forged"));
        // Activity at 300 s keeps it alive until 900 s.
        assert!(store.session(&grant.token, at(850)).is_ok());
        assert!(store.session(&grant.token, at(3600)).is_err());
        let idle = store.create_session(&user, policy, None, at(0)).unwrap();
        assert!(store.session(&idle.token, at(601)).is_err());
        assert!(store.session("oxs_unknown", at(1)).is_err());
        assert!(store.session("not-a-session", at(1)).is_err());
        let ended = store.create_session(&user, policy, None, at(0)).unwrap();
        store.end_session(&ended.token).unwrap();
        assert!(store.session(&ended.token, at(1)).is_err());
    }

    #[test]
    fn password_changes_end_sessions() {
        let (store, user) = store_with_alice();
        let grant = store
            .create_session(&user, SessionPolicy::default(), None, at(0))
            .unwrap();
        assert!(store.set_password("alice", "short", at(1)).is_err());
        store
            .set_password("alice", "a brand new passphrase", at(1))
            .unwrap();
        assert!(store.session(&grant.token, at(2)).is_err());
        assert!(
            store
                .authenticate("alice", "a brand new passphrase", at(3))
                .is_ok()
        );
    }

    #[test]
    fn api_tokens_authenticate_until_revoked_or_expired() {
        let (store, _) = store_with_alice();
        let (token, listed) = store
            .create_api_token("prometheus", Role::Viewer, "alice", Some(at(100)), at(0))
            .unwrap();
        assert!(token.starts_with(API_TOKEN_PREFIX));
        let principal = store.api_token(&token, at(10)).unwrap();
        assert_eq!(principal.username, "token:prometheus");
        assert_eq!(principal.role, Role::Viewer);
        assert!(store.api_token(&token, at(100)).is_err());
        let (other, other_listed) = store
            .create_api_token("lis", Role::Operator, "alice", None, at(0))
            .unwrap();
        store.revoke_api_token(other_listed.id).unwrap();
        assert!(store.api_token(&other, at(1)).is_err());
        assert_eq!(store.api_tokens().unwrap().len(), 2);
        assert_eq!(listed.name, "prometheus");
        assert!(store.revoke_api_token(999).is_err());
    }

    #[test]
    fn prunes_expired_sessions() {
        let (store, user) = store_with_alice();
        let policy = SessionPolicy {
            idle: Duration::from_secs(10),
            max: Duration::from_secs(100),
        };
        store.create_session(&user, policy, None, at(0)).unwrap();
        assert_eq!(store.prune_sessions(at(5)).unwrap(), 0);
        assert_eq!(store.prune_sessions(at(20)).unwrap(), 1);
    }

    #[test]
    fn persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.db");
        {
            let store = AuthStore::open(&path).unwrap();
            store
                .create_user(
                    &NewUser {
                        username: "op",
                        display_name: "",
                        password: "operator passphrase",
                        role: Role::Operator,
                    },
                    at(0),
                )
                .unwrap();
        }
        let store = AuthStore::open(&path).unwrap();
        assert_eq!(store.user("op").unwrap().unwrap().role, Role::Operator);
    }
}
