//! HL7 escape sequences (HL7 v2, chapter 2 "Use of escape sequences").

use std::borrow::Cow;

use memchr::memchr;

use crate::delimiters::Delimiters;
use crate::error::EscapeError;

/// Escapes `text` so it can be stored as a single value.
///
/// Delimiters become `\F\`, `\S\`, `\R\`, `\E\`, `\T\` and `\P\`; carriage
/// return and line feed become `\X0D\` and `\X0A\` so the value cannot break
/// a segment. Returns the input unchanged when nothing needs escaping.
pub fn escape<'a>(text: &'a [u8], delimiters: &Delimiters) -> Result<Cow<'a, [u8]>, EscapeError> {
    if !text.iter().any(|&b| delimiters.is_special(b)) {
        return Ok(Cow::Borrowed(text));
    }
    let Some(esc) = delimiters.escape else {
        return Err(EscapeError::NoEscapeCharacter);
    };
    let mut out = Vec::with_capacity(text.len() + 8);
    for &b in text {
        let sequence: &[u8] = if b == delimiters.field {
            b"F"
        } else if b == delimiters.component {
            b"S"
        } else if b == delimiters.repetition {
            b"R"
        } else if b == esc {
            b"E"
        } else if Some(b) == delimiters.subcomponent {
            b"T"
        } else if Some(b) == delimiters.truncation {
            b"P"
        } else if b == b'\r' {
            b"X0D"
        } else if b == b'\n' {
            b"X0A"
        } else {
            out.push(b);
            continue;
        };
        out.push(esc);
        out.extend_from_slice(sequence);
        out.push(esc);
    }
    Ok(Cow::Owned(out))
}

/// Resolves escape sequences in a raw value.
///
/// Delimiter sequences (`\F\`, `\S\`, `\R\`, `\E\`, `\T\`, `\P\`) and
/// hexadecimal data (`\Xhh...\`) are decoded. Formatting commands (`\H\`,
/// `\N\`, `\.br\`, ...), character set switches (`\C...\`, `\M...\`), locally
/// defined sequences (`\Z...\`) and malformed or unterminated sequences are
/// kept verbatim, so no information is lost.
pub fn unescape<'a>(raw: &'a [u8], delimiters: &Delimiters) -> Cow<'a, [u8]> {
    let Some(esc) = delimiters.escape else {
        return Cow::Borrowed(raw);
    };
    if memchr(esc, raw).is_none() {
        return Cow::Borrowed(raw);
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = memchr(esc, rest) {
        out.extend_from_slice(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(len) = memchr(esc, after) else {
            out.extend_from_slice(&rest[start..]);
            return Cow::Owned(out);
        };
        let content = &after[..len];
        if !decode_sequence(content, delimiters, &mut out) {
            out.push(esc);
            out.extend_from_slice(content);
            out.push(esc);
        }
        rest = &after[len + 1..];
    }
    out.extend_from_slice(rest);
    Cow::Owned(out)
}

fn decode_sequence(content: &[u8], d: &Delimiters, out: &mut Vec<u8>) -> bool {
    let single = match content {
        b"F" => Some(d.field),
        b"S" => Some(d.component),
        b"R" => Some(d.repetition),
        b"E" => d.escape,
        b"T" => d.subcomponent,
        b"P" => d.truncation,
        [b'X', hex @ ..] if !hex.is_empty() && hex.len() % 2 == 0 => {
            let start = out.len();
            for [high, low] in hex.as_chunks::<2>().0 {
                match (hex_value(*high), hex_value(*low)) {
                    (Some(high), Some(low)) => out.push(high << 4 | low),
                    _ => {
                        out.truncate(start);
                        return false;
                    }
                }
            }
            return true;
        }
        _ => None,
    };
    match single {
        Some(b) => {
            out.push(b);
            true
        }
        None => false,
    }
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d() -> Delimiters {
        Delimiters::default()
    }

    #[test]
    fn escapes_delimiters_and_line_breaks() {
        let escaped = escape(b"a|b^c~d\\e&f\rg\nh", &d()).unwrap();
        assert_eq!(
            &*escaped,
            b"a\\F\\b\\S\\c\\R\\d\\E\\e\\T\\f\\X0D\\g\\X0A\\h"
        );
    }

    #[test]
    fn borrows_when_nothing_to_escape() {
        assert!(matches!(
            escape(b"plain text", &d()).unwrap(),
            Cow::Borrowed(_)
        ));
        assert!(matches!(unescape(b"plain text", &d()), Cow::Borrowed(_)));
    }

    #[test]
    fn requires_escape_character() {
        let mut delimiters = d();
        delimiters.escape = None;
        delimiters.subcomponent = None;
        assert_eq!(
            escape(b"a^b", &delimiters),
            Err(EscapeError::NoEscapeCharacter)
        );
        assert_eq!(&*escape(b"a&b", &delimiters).unwrap(), b"a&b");
    }

    #[test]
    fn unescapes_known_sequences() {
        let raw = b"\\F\\\\S\\\\R\\\\E\\\\T\\\\X414243\\\\X0d0A\\";
        assert_eq!(&*unescape(raw, &d()), b"|^~\\&ABC\r\n");
    }

    #[test]
    fn keeps_unknown_and_malformed_sequences() {
        for raw in [
            &b"\\H\\bold\\N\\"[..],
            b"line\\.br\\break",
            b"\\Zlocal\\",
            b"\\X4\\",
            b"\\XZZ\\",
            b"\\X\\",
            b"\\\\",
            b"unterminated \\F",
            b"\\P\\",
        ] {
            assert_eq!(
                &*unescape(raw, &d()),
                raw,
                "{:?}",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[test]
    fn truncation_sequence_uses_declared_character() {
        let mut delimiters = d();
        delimiters.truncation = Some(b'#');
        assert_eq!(&*unescape(b"a\\P\\b", &delimiters), b"a#b");
        assert_eq!(&*escape(b"a#b", &delimiters).unwrap(), b"a\\P\\b");
    }
}
