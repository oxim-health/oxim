//! Encryption of stored message contents (AES-256-GCM envelope
//! encryption).
//!
//! Every database has its own data key. The data key is stored only
//! wrapped (encrypted) with the master key that the operator supplies, so
//! the master key can be rotated by rewrapping one key instead of
//! re-encrypting every message. Each content is sealed with a random
//! nonce and bound to its message, stage and destination (associated
//! data), so encrypted contents cannot be swapped between messages
//! unnoticed.
//!
//! Sealed values start with the marker `OXE1`; values without it (written
//! before encryption was enabled) are read as they are.

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

use crate::error::{StoreError, StoreResult};

/// The marker of sealed values.
const MARKER: &[u8; 4] = b"OXE1";

/// The length of keys in bytes.
pub const KEY_LEN: usize = 32;

fn crypto_error(detail: &str) -> StoreError {
    StoreError::Corrupt {
        field: "encrypted content",
        detail: detail.to_owned(),
    }
}

/// An AES-256-GCM key.
pub struct ContentCipher {
    key: LessSafeKey,
    random: SystemRandom,
}

impl std::fmt::Debug for ContentCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContentCipher(..)")
    }
}

impl ContentCipher {
    /// A cipher with the 32-byte `key`.
    pub fn new(key: &[u8; KEY_LEN]) -> StoreResult<Self> {
        let key = UnboundKey::new(&AES_256_GCM, key).map_err(|_| crypto_error("invalid key"))?;
        Ok(Self {
            key: LessSafeKey::new(key),
            random: SystemRandom::new(),
        })
    }

    /// A new random key.
    pub fn generate_key() -> StoreResult<[u8; KEY_LEN]> {
        let mut key = [0u8; KEY_LEN];
        SystemRandom::new()
            .fill(&mut key)
            .map_err(|_| crypto_error("no randomness available"))?;
        Ok(key)
    }

    /// Encrypts `plain`, bound to `context`.
    pub fn seal(&self, plain: &[u8], context: &[u8]) -> StoreResult<Vec<u8>> {
        let mut nonce = [0u8; NONCE_LEN];
        self.random
            .fill(&mut nonce)
            .map_err(|_| crypto_error("no randomness available"))?;
        let mut sealed = plain.to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(context),
                &mut sealed,
            )
            .map_err(|_| crypto_error("encryption failed"))?;
        let mut out = Vec::with_capacity(MARKER.len() + NONCE_LEN + sealed.len());
        out.extend_from_slice(MARKER);
        out.extend_from_slice(&nonce);
        out.extend(sealed);
        Ok(out)
    }

    /// Decrypts a value sealed with [`ContentCipher::seal`] for the same
    /// `context`. Values without the marker are returned unchanged.
    pub fn open(&self, stored: Vec<u8>, context: &[u8]) -> StoreResult<Vec<u8>> {
        if !is_sealed(&stored) {
            return Ok(stored);
        }
        let nonce: [u8; NONCE_LEN] = stored[MARKER.len()..MARKER.len() + NONCE_LEN]
            .try_into()
            .map_err(|_| crypto_error("truncated value"))?;
        let mut sealed = stored[MARKER.len() + NONCE_LEN..].to_vec();
        let plain = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(context),
                &mut sealed,
            )
            .map_err(|_| crypto_error("the content cannot be decrypted with this key"))?;
        Ok(plain.to_vec())
    }
}

/// Whether a stored value is sealed.
pub fn is_sealed(stored: &[u8]) -> bool {
    stored.len() >= MARKER.len() + NONCE_LEN + AES_256_GCM.tag_len() && stored.starts_with(MARKER)
}

/// The associated data binding a content to its message, stage and
/// destination.
pub(crate) fn content_context(message: &[u8], stage: &str, destination: &str) -> Vec<u8> {
    let mut context = Vec::with_capacity(message.len() + stage.len() + destination.len() + 2);
    context.extend_from_slice(message);
    context.push(0);
    context.extend_from_slice(stage.as_bytes());
    context.push(0);
    context.extend_from_slice(destination.as_bytes());
    context
}

/// Wraps a data key with the master key.
pub fn wrap_key(master: &ContentCipher, data_key: &[u8; KEY_LEN]) -> StoreResult<Vec<u8>> {
    master.seal(data_key, b"oxim data key")
}

/// Unwraps a data key with the master key.
pub fn unwrap_key(master: &ContentCipher, wrapped: &[u8]) -> StoreResult<[u8; KEY_LEN]> {
    if !is_sealed(wrapped) {
        return Err(crypto_error("the stored data key is not wrapped"));
    }
    let plain = master
        .open(wrapped.to_vec(), b"oxim data key")
        .map_err(|_| crypto_error("the master key does not match this database"))?;
    plain
        .as_slice()
        .try_into()
        .map_err(|_| crypto_error("the stored data key has the wrong length"))
}

/// Reads a 32-byte key written as 64 hexadecimal digits or as base64.
pub fn parse_key(text: &str) -> StoreResult<[u8; KEY_LEN]> {
    let text = text.trim();
    let error = || crypto_error("a key must be 32 bytes, as 64 hexadecimal digits or base64");
    if text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut key = [0u8; KEY_LEN];
        for (i, pair) in text.as_bytes().chunks(2).enumerate() {
            let pair = std::str::from_utf8(pair).map_err(|_| error())?;
            key[i] = u8::from_str_radix(pair, 16).map_err(|_| error())?;
        }
        return Ok(key);
    }
    let decoded = base64_decode(text).ok_or_else(error)?;
    decoded.as_slice().try_into().map_err(|_| error())
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_and_opens_bound_to_context() {
        let cipher = ContentCipher::new(&ContentCipher::generate_key().unwrap()).unwrap();
        let sealed = cipher.seal(b"PID|1||42", b"m1\0raw\0").unwrap();
        assert!(is_sealed(&sealed));
        assert!(!sealed.windows(9).any(|w| w == b"PID|1||42"));
        assert_eq!(
            cipher.open(sealed.clone(), b"m1\0raw\0").unwrap(),
            b"PID|1||42"
        );
        // Another message's context does not open it.
        assert!(cipher.open(sealed.clone(), b"m2\0raw\0").is_err());
        // Another key does not open it.
        let other = ContentCipher::new(&ContentCipher::generate_key().unwrap()).unwrap();
        assert!(other.open(sealed, b"m1\0raw\0").is_err());
        // Plain values written before encryption pass through.
        assert_eq!(cipher.open(b"plain".to_vec(), b"x").unwrap(), b"plain");
    }

    #[test]
    fn wraps_data_keys_and_parses_master_keys() {
        let master = ContentCipher::new(&[7u8; KEY_LEN]).unwrap();
        let data = ContentCipher::generate_key().unwrap();
        let wrapped = wrap_key(&master, &data).unwrap();
        assert_eq!(unwrap_key(&master, &wrapped).unwrap(), data);
        let wrong = ContentCipher::new(&[8u8; KEY_LEN]).unwrap();
        assert!(unwrap_key(&wrong, &wrapped).is_err());
        let hex = "00".repeat(31) + "ff";
        assert_eq!(parse_key(&hex).unwrap()[31], 0xff);
        let base64 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        assert_eq!(parse_key(base64).unwrap(), [0u8; KEY_LEN]);
        assert!(parse_key("short").is_err());
    }
}
