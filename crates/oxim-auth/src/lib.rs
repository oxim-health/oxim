//! Users, roles, sessions, API tokens and login throttling for OXIM.
//!
//! - **Roles:** `admin`, `operator` and `viewer`, each a fixed set of
//!   [`Permission`]s. Viewers see patient-identifying values masked.
//! - **Passwords:** Argon2id (19 MiB, 2 iterations, parallelism 1) with a
//!   random salt; at least 12 characters and different from the user name.
//! - **Sessions:** random 256-bit tokens of which only a SHA-256 hash is
//!   stored, with idle and absolute timeouts and a CSRF token per session.
//! - **API tokens:** named, hashed, optionally expiring, revocable, with a
//!   role of their own.
//! - **Throttling:** failed logins are limited per user name and per client
//!   address, in memory and bounded.
//!
//! Everything lives in its own SQLite database (`auth.db`) with the same
//! durability settings as the message store.

mod error;
mod role;
mod secret;
mod store;
mod throttle;

pub use error::AuthError;
pub use role::{Permission, Role, UnknownRole};
pub use secret::{
    ARGON2_ITERATIONS, ARGON2_MEMORY_KIB, ARGON2_PARALLELISM, MIN_PASSWORD_LEN,
    check_password_policy, constant_time_eq, hash_password, new_token, token_hash, verify_password,
};
pub use store::{
    API_TOKEN_PREFIX, ApiToken, AuthStore, NewUser, Principal, PrincipalKind, SESSION_PREFIX,
    Session, SessionGrant, SessionPolicy, User, UserUpdate,
};
pub use throttle::LoginThrottle;
