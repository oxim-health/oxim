//! Passwords, random tokens and their hashes.

use std::sync::LazyLock;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use sha2::{Digest, Sha256};

use crate::error::AuthError;

/// Argon2id memory cost in KiB (19 MiB), per the OWASP password storage
/// recommendation.
pub const ARGON2_MEMORY_KIB: u32 = 19_456;
/// Argon2id iterations.
pub const ARGON2_ITERATIONS: u32 = 2;
/// Argon2id parallelism.
pub const ARGON2_PARALLELISM: u32 = 1;

/// The minimum password length.
pub const MIN_PASSWORD_LEN: usize = 12;

fn argon2() -> Result<Argon2<'static>, AuthError> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        None,
    )
    .map_err(|e| AuthError::Hash(e.to_string()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Checks the password policy: at least [`MIN_PASSWORD_LEN`] characters and
/// different from the user name.
pub fn check_password_policy(username: &str, password: &str) -> Result<(), AuthError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(AuthError::Policy(format!(
            "passwords need at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    if password.eq_ignore_ascii_case(username) {
        return Err(AuthError::Policy(
            "the password must differ from the user name".into(),
        ));
    }
    Ok(())
}

/// Hashes a password with Argon2id and a random salt, in PHC string form.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let mut salt = [0u8; 16];
    random(&mut salt)?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| AuthError::Hash(e.to_string()))?;
    argon2()?
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| AuthError::Hash(e.to_string()))
}

/// Verifies a password against a PHC string. Comparison is constant-time.
pub fn verify_password(hash: &str, password: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    argon2().is_ok_and(|argon2| argon2.verify_password(password.as_bytes(), &parsed).is_ok())
}

/// A hash to verify against when a user name does not exist, so failed
/// logins take the same time whether or not the user exists.
pub(crate) static DUMMY_HASH: LazyLock<Option<String>> =
    LazyLock::new(|| hash_password("dummy password for timing").ok());

/// Fills `bytes` from the operating system's random source.
pub(crate) fn random(bytes: &mut [u8]) -> Result<(), AuthError> {
    getrandom::getrandom(bytes).map_err(|_| AuthError::Random)
}

/// A new random token with `prefix`, carrying 256 bits of entropy.
pub fn new_token(prefix: &str) -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    random(&mut bytes)?;
    Ok(format!("{prefix}{}", hex(&bytes)))
}

/// The SHA-256 of a token in lowercase hex. Only this hash is stored, so a
/// leaked database does not reveal usable tokens.
pub fn token_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// Constant-time equality of two byte strings of the same length.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_verifies_passwords() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(verify_password(&hash, "correct horse battery"));
        assert!(!verify_password(&hash, "wrong horse battery"));
        assert!(!verify_password("not a hash", "anything"));
        assert_ne!(hash, hash_password("correct horse battery").unwrap());
    }

    #[test]
    fn enforces_the_password_policy() {
        assert!(check_password_policy("alice", "short").is_err());
        assert!(check_password_policy("alice-admin1", "ALICE-ADMIN1").is_err());
        assert!(check_password_policy("alice", "a long enough passphrase").is_ok());
    }

    #[test]
    fn tokens_are_random_and_hashed() {
        let a = new_token("oxs_").unwrap();
        let b = new_token("oxs_").unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 4 + 64);
        assert_eq!(token_hash(&a).len(), 64);
        assert_eq!(token_hash(&a), token_hash(&a));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
