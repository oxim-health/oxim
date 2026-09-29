//! Consistent pseudonyms: the same input always gives the same pseudonym
//! within a run (one key), so identifiers still link messages together.
//!
//! Pseudonyms are derived with HMAC-SHA-256 under a secret key. Without the
//! key they cannot be reversed or recomputed from a guessed original value.

use ring::hmac;

/// Invented family names used as pseudonyms.
const FAMILY: [&str; 32] = [
    "Adler", "Ashby", "Baxter", "Brandt", "Calder", "Corbett", "Dalton", "Delaney", "Ellery",
    "Everly", "Fairfax", "Fenwick", "Garland", "Gresham", "Hartley", "Hollis", "Ingram", "Jarvis",
    "Keating", "Lowell", "Marlow", "Norcross", "Oakley", "Prescott", "Quinlan", "Radley", "Sutton",
    "Thorne", "Upton", "Vance", "Whitlock", "Yardley",
];

/// Invented given names used as pseudonyms.
const GIVEN: [&str; 32] = [
    "Alex", "Avery", "Blair", "Casey", "Charlie", "Dana", "Devon", "Eden", "Emery", "Finley",
    "Frankie", "Gray", "Harper", "Hayden", "Jamie", "Jordan", "Kai", "Kendall", "Lane", "Logan",
    "Morgan", "Noel", "Parker", "Quinn", "Reese", "Riley", "Rowan", "Sage", "Skyler", "Taylor",
    "Tatum", "Winter",
];

/// Makes pseudonyms under one key.
pub(crate) struct Pseudonymizer {
    key: hmac::Key,
}

impl std::fmt::Debug for Pseudonymizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pseudonymizer { key: <secret> }")
    }
}

fn keep_case(original: &str, pseudonym: &str) -> String {
    let letters = original.chars().filter(|c| c.is_alphabetic());
    let mut any = false;
    let mut upper = true;
    for c in letters {
        any = true;
        upper &= c.is_uppercase();
    }
    if any && upper {
        pseudonym.to_uppercase()
    } else {
        pseudonym.to_owned()
    }
}

impl Pseudonymizer {
    /// Pseudonyms under `key`.
    pub(crate) fn new(key: &[u8]) -> Self {
        Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, key),
        }
    }

    /// `count` pseudo-random bytes for `value` in `category`.
    fn bytes(&self, category: &str, value: &[u8], count: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(count + 32);
        let mut block = 0u32;
        while out.len() < count {
            let mut context = hmac::Context::with_key(&self.key);
            context.update(category.as_bytes());
            context.update(&[0]);
            context.update(&block.to_be_bytes());
            context.update(&[0]);
            context.update(value);
            out.extend_from_slice(context.sign().as_ref());
            block += 1;
        }
        out.truncate(count);
        out
    }

    /// An identifier with the same shape: digits stay digits, letters stay
    /// letters of the same case, punctuation (`-`, `.`) stays in place, and
    /// other bytes become letters. Keeping the shape keeps barcodes and
    /// fixed-width fields valid.
    pub(crate) fn identifier(&self, value: &[u8]) -> Vec<u8> {
        let random = self.bytes("identifier", value, value.len());
        value
            .iter()
            .zip(random)
            .map(|(&c, r)| match c {
                b'0'..=b'9' => b'0' + r % 10,
                b'a'..=b'z' => b'a' + r % 26,
                b'A'..=b'Z' | 0x80.. => b'A' + r % 26,
                other => other,
            })
            .collect()
    }

    /// An invented family name.
    pub(crate) fn family(&self, value: &[u8]) -> String {
        let text = String::from_utf8_lossy(value);
        let index = self.bytes("family", text.to_lowercase().as_bytes(), 1)[0];
        keep_case(&text, FAMILY[usize::from(index) % FAMILY.len()])
    }

    /// An invented given name.
    pub(crate) fn given(&self, value: &[u8]) -> String {
        let text = String::from_utf8_lossy(value);
        let index = self.bytes("given", text.to_lowercase().as_bytes(), 1)[0];
        keep_case(&text, GIVEN[usize::from(index) % GIVEN.len()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pseudonyms_are_consistent_and_keep_their_shape() {
        let a = Pseudonymizer::new(b"key one");
        let b = Pseudonymizer::new(b"key two");
        let id = a.identifier(b"SMP-000101.a");
        assert_eq!(id, a.identifier(b"SMP-000101.a"));
        assert_ne!(id, b.identifier(b"SMP-000101.a"));
        assert_ne!(id, b"SMP-000101.a");
        let shape: String = id
            .iter()
            .map(|&c| match c {
                b'A'..=b'Z' => 'A',
                b'a'..=b'z' => 'a',
                b'0'..=b'9' => '9',
                other => char::from(other),
            })
            .collect();
        assert_eq!(shape, "AAA-999999.a");
        assert_eq!(a.identifier(b""), b"");
        assert!(a.identifier("Ç1".as_bytes()).is_ascii());
        assert_eq!(a.family(b"DOE"), a.family(b"doe").to_uppercase());
        assert!(FAMILY.contains(&a.family(b"Doe").as_str()));
        assert!(GIVEN.contains(&a.given(b"Jane").as_str()));
        assert_eq!(format!("{a:?}"), "Pseudonymizer { key: <secret> }");
    }
}
