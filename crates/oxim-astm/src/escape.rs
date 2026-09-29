//! ASTM E1394 escape sequences.

use std::borrow::Cow;

use memchr::memchr;

use crate::delimiters::Delimiters;

/// Escapes `text` so it can be stored as a single value.
///
/// Delimiters become `&F&` (field), `&S&` (component), `&R&` (repeat) and
/// `&E&` (escape); carriage return and line feed become `&X0D&` and `&X0A&`
/// so the value can neither end a record nor carry a character LIS01
/// forbids. Returns the input unchanged when nothing needs escaping.
pub fn escape<'a>(text: &'a [u8], delimiters: &Delimiters) -> Cow<'a, [u8]> {
    if !text.iter().any(|&b| delimiters.is_special(b)) {
        return Cow::Borrowed(text);
    }
    let esc = delimiters.escape;
    let mut out = Vec::with_capacity(text.len() + 8);
    for &b in text {
        let sequence: &[u8] = if b == delimiters.field {
            b"F"
        } else if b == delimiters.component {
            b"S"
        } else if b == delimiters.repeat {
            b"R"
        } else if b == esc {
            b"E"
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
    Cow::Owned(out)
}

/// Resolves escape sequences in a raw value.
///
/// Delimiter sequences (`&F&`, `&S&`, `&R&`, `&E&`) and hexadecimal data
/// (`&Xhh...&`) are decoded. Other sequences (highlighting, local `&Z...&`
/// sequences) and malformed or unterminated sequences are kept verbatim.
pub fn unescape<'a>(raw: &'a [u8], delimiters: &Delimiters) -> Cow<'a, [u8]> {
    let esc = delimiters.escape;
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
        b"F" => d.field,
        b"S" => d.component,
        b"R" => d.repeat,
        b"E" => d.escape,
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
        _ => return false,
    };
    out.push(single);
    true
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
        assert_eq!(
            &*escape(b"a|b^c\\d&e\rf\ng", &d()),
            b"a&F&b&S&c&R&d&E&e&X0D&f&X0A&g"
        );
        assert!(matches!(escape(b"plain", &d()), Cow::Borrowed(_)));
    }

    #[test]
    fn unescapes_known_sequences() {
        assert_eq!(&*unescape(b"&F&&S&&R&&E&&X4142&", &d()), b"|^\\&AB");
        assert!(matches!(unescape(b"plain", &d()), Cow::Borrowed(_)));
    }

    #[test]
    fn keeps_unknown_and_malformed_sequences() {
        for raw in [
            &b"&H&bold&N&"[..],
            b"&Zlocal&",
            b"&X4&",
            b"&XZZ&",
            b"&&",
            b"tail &F",
        ] {
            assert_eq!(&*unescape(raw, &d()), raw);
        }
    }
}
