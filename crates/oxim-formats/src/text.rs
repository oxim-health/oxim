//! Helpers shared by the line-oriented formats.

use std::borrow::Cow;

use encoding_rs::Encoding;
use memchr::memchr2;

/// How a record (line) was terminated in the original bytes.
///
/// The original terminator of every record is kept so unmodified input
/// serializes byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// Carriage return followed by line feed, as RFC 4180 prescribes.
    CrLf,
    /// Line feed.
    Lf,
    /// Carriage return.
    Cr,
    /// No terminator (only possible for the last record).
    None,
}

impl LineEnding {
    /// The bytes written after the record.
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::CrLf => b"\r\n",
            Self::Lf => b"\n",
            Self::Cr => b"\r",
            Self::None => b"",
        }
    }
}

/// Whether printable ASCII delimiters can never occur inside a multi-byte
/// character of `encoding`, which byte-level splitting relies on.
pub(crate) fn is_byte_safe(encoding: &'static Encoding) -> bool {
    use encoding_rs::{
        BIG5, GB18030, GBK, ISO_2022_JP, REPLACEMENT, SHIFT_JIS, UTF_16BE, UTF_16LE,
    };
    ![
        BIG5,
        GB18030,
        GBK,
        ISO_2022_JP,
        REPLACEMENT,
        SHIFT_JIS,
        UTF_16BE,
        UTF_16LE,
    ]
    .contains(&encoding)
}

/// Decodes `bytes`, replacing malformed sequences with U+FFFD.
pub(crate) fn decode(bytes: &[u8], encoding: &'static Encoding) -> String {
    encoding.decode_without_bom_handling(bytes).0.into_owned()
}

/// Encodes `text`, or returns `None` when a character cannot be represented.
pub(crate) fn encode<'a>(text: &'a str, encoding: &'static Encoding) -> Option<Cow<'a, [u8]>> {
    let (bytes, _, unmappable) = encoding.encode(text);
    (!unmappable).then_some(bytes)
}

/// Splits input into lines and their terminators (CRLF, LF or CR).
pub(crate) struct Lines<'a> {
    rest: &'a [u8],
}

impl<'a> Lines<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self { rest: input }
    }
}

impl<'a> Iterator for Lines<'a> {
    type Item = (&'a [u8], LineEnding);

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let Some(at) = memchr2(b'\r', b'\n', self.rest) else {
            let line = self.rest;
            self.rest = &[];
            return Some((line, LineEnding::None));
        };
        let line = &self.rest[..at];
        let (ending, len) = match (self.rest[at], self.rest.get(at + 1)) {
            (b'\r', Some(b'\n')) => (LineEnding::CrLf, 2),
            (b'\r', _) => (LineEnding::Cr, 1),
            _ => (LineEnding::Lf, 1),
        };
        self.rest = &self.rest[at + len..];
        Some((line, ending))
    }
}

/// The ending to use for a new record: the first record's ending when it has
/// one, otherwise `fallback`.
pub(crate) fn preferred_ending(first: Option<LineEnding>, fallback: LineEnding) -> LineEnding {
    match first {
        Some(LineEnding::None) | None => fallback,
        Some(ending) => ending,
    }
}

/// Parses a `row/column` table path. The row is a 0-based index; the column
/// is returned as written.
pub(crate) fn split_table_path(path: &str) -> Option<(usize, &str)> {
    let (row, column) = path.split_once('/')?;
    if row.is_empty() || !row.bytes().all(|b| b.is_ascii_digit()) || column.is_empty() {
        return None;
    }
    Some((row.parse().ok()?, column))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_losslessly() {
        let input = b"a\r\nb\nc\rd";
        let lines: Vec<_> = Lines::new(input).collect();
        assert_eq!(
            lines,
            [
                (&b"a"[..], LineEnding::CrLf),
                (b"b", LineEnding::Lf),
                (b"c", LineEnding::Cr),
                (b"d", LineEnding::None)
            ]
        );
    }

    #[test]
    fn parses_table_paths() {
        assert_eq!(split_table_path("0/name"), Some((0, "name")));
        assert_eq!(split_table_path("12/3"), Some((12, "3")));
        assert_eq!(split_table_path("a/b"), None);
        assert_eq!(split_table_path("1/"), None);
        assert_eq!(split_table_path("/x"), None);
        assert_eq!(split_table_path("-1/x"), None);
    }
}
