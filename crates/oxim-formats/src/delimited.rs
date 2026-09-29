//! Delimited text: CSV, TSV and custom separators.
//!
//! Every field is kept exactly as written, including its quotes, so
//! unmodified input serializes byte for byte. The parser is lenient: any
//! input is accepted (subject to the limits), and malformed quoting such as
//! text after a closing quote is kept verbatim. Written values are quoted
//! only when they need it.

use encoding_rs::{Encoding, UTF_8};
use thiserror::Error;

use crate::text::{LineEnding, decode, encode, is_byte_safe, preferred_ending, split_table_path};

/// Options for [`DelimitedDocument::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DelimitedOptions {
    /// Field separator.
    pub delimiter: u8,
    /// Quote character, or `None` when fields are never quoted.
    pub quote: Option<u8>,
    /// Escape character used inside quoted fields. `None` means a quote is
    /// escaped by doubling it (RFC 4180).
    pub escape: Option<u8>,
    /// Whether the first record holds column names.
    pub has_header: bool,
    /// Line ending for records added to a document whose existing records
    /// have none.
    pub line_ending: LineEnding,
    /// Text encoding of the fields.
    pub encoding: &'static Encoding,
    /// The largest accepted number of records.
    pub max_records: usize,
    /// The largest accepted number of fields in one record.
    pub max_fields_per_record: usize,
}

impl Default for DelimitedOptions {
    /// Comma separated values as in RFC 4180, UTF-8, without a header row.
    fn default() -> Self {
        Self {
            delimiter: b',',
            quote: Some(b'"'),
            escape: None,
            has_header: false,
            line_ending: LineEnding::CrLf,
            encoding: UTF_8,
            max_records: 1_000_000,
            max_fields_per_record: 10_000,
        }
    }
}

impl DelimitedOptions {
    /// RFC 4180 comma separated values.
    pub fn csv() -> Self {
        Self::default()
    }

    /// Tab separated values.
    pub fn tsv() -> Self {
        Self {
            delimiter: b'\t',
            ..Self::default()
        }
    }

    /// Sets whether the first record holds column names.
    pub fn with_header(mut self, has_header: bool) -> Self {
        self.has_header = has_header;
        self
    }

    /// Sets the field separator.
    pub fn with_delimiter(mut self, delimiter: u8) -> Self {
        self.delimiter = delimiter;
        self
    }

    /// Sets the quote character.
    pub fn with_quote(mut self, quote: Option<u8>) -> Self {
        self.quote = quote;
        self
    }

    /// Sets the escape character used inside quoted fields.
    pub fn with_escape(mut self, escape: Option<u8>) -> Self {
        self.escape = escape;
        self
    }

    /// Sets the text encoding.
    pub fn with_encoding(mut self, encoding: &'static Encoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// Checks that the special characters are distinct, are not line breaks,
    /// and that the encoding can be split at byte level.
    pub fn validate(&self) -> Result<(), DelimitedError> {
        let specials = [Some(self.delimiter), self.quote, self.escape];
        for (i, special) in specials.iter().enumerate() {
            let Some(b) = *special else { continue };
            if b == b'\r' || b == b'\n' {
                return Err(DelimitedError::InvalidOptions(
                    "special characters cannot be line breaks",
                ));
            }
            if specials[..i].contains(&Some(b)) && !(i == 2 && self.quote == Some(b)) {
                return Err(DelimitedError::InvalidOptions(
                    "special characters must be distinct",
                ));
            }
        }
        if self.escape.is_some() && self.quote.is_none() {
            return Err(DelimitedError::InvalidOptions(
                "an escape character requires a quote character",
            ));
        }
        if !is_byte_safe(self.encoding) {
            return Err(DelimitedError::InvalidOptions(
                "the encoding may contain delimiter bytes inside characters",
            ));
        }
        Ok(())
    }

    /// The escape character, treating an escape equal to the quote as
    /// quote doubling.
    fn escape(&self) -> Option<u8> {
        self.escape.filter(|&e| Some(e) != self.quote)
    }
}

/// Returned when delimited text cannot be parsed or edited.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum DelimitedError {
    /// The options are inconsistent.
    #[error("invalid delimited options: {0}")]
    InvalidOptions(&'static str),
    /// The input exceeds a limit from [`DelimitedOptions`].
    #[error("delimited text exceeds the configured limit for {0}")]
    LimitExceeded(&'static str),
    /// The path text is not `row/column`.
    #[error("invalid delimited path {0:?}")]
    Path(String),
    /// The column name is not in the header row.
    #[error("unknown column {0:?}")]
    UnknownColumn(String),
    /// A row index is more than one past the last row.
    #[error("row {row} does not exist and is not the next row ({rows} rows)")]
    RowOutOfRange {
        /// The requested row.
        row: usize,
        /// The number of rows.
        rows: usize,
    },
    /// The value contains characters that require quoting, but no quote
    /// character is configured.
    #[error("value needs quoting but no quote character is configured")]
    NeedsQuoting,
    /// The value contains characters the encoding cannot represent.
    #[error("value cannot be represented in {0}")]
    Unencodable(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    /// Raw fields, including quotes, exactly as written.
    fields: Vec<Vec<u8>>,
    ending: LineEnding,
}

/// A parsed delimited document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelimitedDocument {
    options: DelimitedOptions,
    records: Vec<Record>,
}

impl DelimitedDocument {
    /// Parses a document.
    pub fn parse(input: &[u8], options: &DelimitedOptions) -> Result<Self, DelimitedError> {
        options.validate()?;
        let mut records = Vec::new();
        let mut pos = 0;
        while pos < input.len() {
            if records.len() == options.max_records {
                return Err(DelimitedError::LimitExceeded("records"));
            }
            let mut fields = Vec::new();
            loop {
                if fields.len() == options.max_fields_per_record {
                    return Err(DelimitedError::LimitExceeded("fields per record"));
                }
                let end = field_end(input, pos, options);
                fields.push(input[pos..end].to_vec());
                pos = end;
                if input.get(pos) == Some(&options.delimiter) {
                    pos += 1;
                } else {
                    break;
                }
            }
            let ending = match (input.get(pos), input.get(pos + 1)) {
                (Some(b'\r'), Some(b'\n')) => LineEnding::CrLf,
                (Some(b'\r'), _) => LineEnding::Cr,
                (Some(b'\n'), _) => LineEnding::Lf,
                _ => LineEnding::None,
            };
            pos += ending.as_bytes().len();
            records.push(Record { fields, ending });
        }
        Ok(Self {
            options: *options,
            records,
        })
    }

    /// The options the document was parsed with.
    pub fn options(&self) -> &DelimitedOptions {
        &self.options
    }

    /// The column names, when the options declare a header row.
    pub fn headers(&self) -> Option<Vec<String>> {
        if !self.options.has_header {
            return None;
        }
        Some(self.records.first().map_or_else(Vec::new, |record| {
            record
                .fields
                .iter()
                .map(|raw| self.decode_field(raw))
                .collect()
        }))
    }

    /// The index of the column called `name` in the header row.
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.headers()?.iter().position(|header| header == name)
    }

    /// The number of data rows (the header row is not counted).
    pub fn row_count(&self) -> usize {
        self.records.len().saturating_sub(self.header_rows())
    }

    /// The number of fields in data row `row`.
    pub fn field_count(&self, row: usize) -> Option<usize> {
        self.records
            .get(row + self.header_rows())
            .map(|r| r.fields.len())
    }

    /// The unquoted bytes of a field.
    pub fn get_bytes(&self, row: usize, column: usize) -> Option<Vec<u8>> {
        let raw = self
            .records
            .get(row + self.header_rows())?
            .fields
            .get(column)?;
        Some(unquote(raw, &self.options))
    }

    /// The decoded text of a field, or `None` when the row or column does
    /// not exist.
    pub fn get(&self, row: usize, column: usize) -> Option<String> {
        Some(decode(&self.get_bytes(row, column)?, self.options.encoding))
    }

    /// The decoded text of a field in the column named `name`.
    pub fn get_by_name(&self, row: usize, name: &str) -> Result<Option<String>, DelimitedError> {
        let column = self
            .column_index(name)
            .ok_or_else(|| DelimitedError::UnknownColumn(name.to_owned()))?;
        Ok(self.get(row, column))
    }

    /// Stores `value` in a field, quoting it only when needed.
    ///
    /// `row` may be one past the last row to append a row. Missing fields
    /// before `column` are added as empty fields.
    pub fn set(&mut self, row: usize, column: usize, value: &str) -> Result<(), DelimitedError> {
        let bytes = encode(value, self.options.encoding)
            .ok_or(DelimitedError::Unencodable(self.options.encoding.name()))?;
        let raw = quote(&bytes, &self.options)?;
        let rows = self.row_count();
        if row > rows {
            return Err(DelimitedError::RowOutOfRange { row, rows });
        }
        if column >= self.options.max_fields_per_record {
            return Err(DelimitedError::LimitExceeded("fields per record"));
        }
        if row == rows {
            self.append_record();
        }
        let index = row + self.header_rows();
        let Some(record) = self.records.get_mut(index) else {
            return Err(DelimitedError::RowOutOfRange { row, rows });
        };
        if record.fields.len() <= column {
            record.fields.resize(column + 1, Vec::new());
        }
        record.fields[column] = raw;
        Ok(())
    }

    /// Stores `value` in the column named `name`.
    pub fn set_by_name(
        &mut self,
        row: usize,
        name: &str,
        value: &str,
    ) -> Result<(), DelimitedError> {
        let column = self
            .column_index(name)
            .ok_or_else(|| DelimitedError::UnknownColumn(name.to_owned()))?;
        self.set(row, column, value)
    }

    /// Appends a data row.
    pub fn push_row<S: AsRef<str>>(&mut self, values: &[S]) -> Result<(), DelimitedError> {
        let row = self.row_count();
        if values.is_empty() {
            self.append_record();
            return Ok(());
        }
        for (column, value) in values.iter().enumerate() {
            self.set(row, column, value.as_ref())?;
        }
        Ok(())
    }

    /// The field at a `row/column` path, where `row` is a 0-based data row
    /// and `column` a 0-based index or, when it is not a number, a header
    /// name.
    pub fn get_path(&self, path: &str) -> Result<Option<String>, DelimitedError> {
        let (row, column) = self.resolve(path)?;
        Ok(self.get(row, column))
    }

    /// Stores `value` at a `row/column` path.
    pub fn set_path(&mut self, path: &str, value: &str) -> Result<(), DelimitedError> {
        let (row, column) = self.resolve(path)?;
        self.set(row, column, value)
    }

    /// Serializes the document. Unmodified records are reproduced byte for
    /// byte.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for record in &self.records {
            for (i, field) in record.fields.iter().enumerate() {
                if i > 0 {
                    out.push(self.options.delimiter);
                }
                out.extend_from_slice(field);
            }
            out.extend_from_slice(record.ending.as_bytes());
        }
        out
    }

    fn header_rows(&self) -> usize {
        usize::from(self.options.has_header && !self.records.is_empty())
    }

    fn decode_field(&self, raw: &[u8]) -> String {
        decode(&unquote(raw, &self.options), self.options.encoding)
    }

    fn resolve(&self, path: &str) -> Result<(usize, usize), DelimitedError> {
        let (row, column) =
            split_table_path(path).ok_or_else(|| DelimitedError::Path(path.to_owned()))?;
        let column = if column.bytes().all(|b| b.is_ascii_digit()) {
            column
                .parse()
                .map_err(|_| DelimitedError::Path(path.to_owned()))?
        } else {
            self.column_index(column)
                .ok_or_else(|| DelimitedError::UnknownColumn(column.to_owned()))?
        };
        Ok((row, column))
    }

    /// Appends an empty record. Appended records are always terminated: an
    /// unterminated final record holding one empty field would serialize to
    /// nothing and vanish when the text is parsed again.
    fn append_record(&mut self) {
        let ending = preferred_ending(
            self.records.first().map(|r| r.ending),
            self.options.line_ending,
        );
        if let Some(last) = self.records.last_mut()
            && last.ending == LineEnding::None
        {
            last.ending = ending;
        }
        self.records.push(Record {
            fields: vec![Vec::new()],
            ending,
        });
    }
}

/// The end of the field starting at `start`: a delimiter, line break or the
/// end of input outside quotes.
fn field_end(input: &[u8], start: usize, options: &DelimitedOptions) -> usize {
    let mut pos = start;
    if let Some(quote) = options.quote
        && input.get(pos) == Some(&quote)
    {
        pos += 1;
        while let Some(&b) = input.get(pos) {
            if Some(b) == options.escape() {
                pos += 2;
            } else if b == quote {
                if input.get(pos + 1) == Some(&quote) {
                    pos += 2;
                } else {
                    pos += 1;
                    break;
                }
            } else {
                pos += 1;
            }
        }
        pos = pos.min(input.len());
    }
    while let Some(&b) = input.get(pos) {
        if b == options.delimiter || b == b'\r' || b == b'\n' {
            break;
        }
        pos += 1;
    }
    pos
}

/// Removes quoting from a raw field. Text after the closing quote is kept.
fn unquote(raw: &[u8], options: &DelimitedOptions) -> Vec<u8> {
    let Some(quote) = options.quote.filter(|&q| raw.first() == Some(&q)) else {
        return raw.to_vec();
    };
    let mut out = Vec::with_capacity(raw.len());
    let mut pos = 1;
    while let Some(&b) = raw.get(pos) {
        if Some(b) == options.escape() {
            if let Some(&next) = raw.get(pos + 1) {
                out.push(next);
            }
            pos += 2;
        } else if b == quote {
            if raw.get(pos + 1) == Some(&quote) {
                out.push(quote);
                pos += 2;
            } else {
                out.extend_from_slice(raw.get(pos + 1..).unwrap_or_default());
                break;
            }
        } else {
            out.push(b);
            pos += 1;
        }
    }
    out
}

/// Quotes `value` when it contains a delimiter, quote, escape or line break.
fn quote(value: &[u8], options: &DelimitedOptions) -> Result<Vec<u8>, DelimitedError> {
    let special = |b: &u8| {
        *b == options.delimiter
            || *b == b'\r'
            || *b == b'\n'
            || Some(*b) == options.quote
            || Some(*b) == options.escape
    };
    if !value.iter().any(special) {
        return Ok(value.to_vec());
    }
    let quote = options.quote.ok_or(DelimitedError::NeedsQuoting)?;
    let mut out = Vec::with_capacity(value.len() + 2);
    out.push(quote);
    for &b in value {
        match options.escape() {
            Some(escape) if b == quote || b == escape => {
                out.push(escape);
                out.push(b);
            }
            None if b == quote => {
                out.push(quote);
                out.push(quote);
            }
            _ => out.push(b),
        }
    }
    out.push(quote);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &[u8] = b"sample,test,value,comment\r\n\
S1,GLU,5.4,\"fasting, morning\"\r\n\
S2,HGB,13.2,\"said \"\"ok\"\"\"\r\n\
S3,K,4.1,\"multi\nline\"";

    fn csv_with_header() -> DelimitedOptions {
        DelimitedOptions {
            has_header: true,
            ..DelimitedOptions::csv()
        }
    }

    #[test]
    fn round_trips_any_input() {
        for input in [CSV, b"", b",", b"a,\"b", b"\"a\"x,b\n\n", b"a\rb\r\nc\n"] {
            let doc = DelimitedDocument::parse(input, &DelimitedOptions::csv()).unwrap();
            assert_eq!(
                doc.to_bytes(),
                input,
                "{:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn reads_fields() {
        let doc = DelimitedDocument::parse(CSV, &csv_with_header()).unwrap();
        assert_eq!(
            doc.headers().unwrap(),
            ["sample", "test", "value", "comment"]
        );
        assert_eq!(doc.row_count(), 3);
        assert_eq!(doc.get(0, 3).as_deref(), Some("fasting, morning"));
        assert_eq!(doc.get(1, 3).as_deref(), Some("said \"ok\""));
        assert_eq!(doc.get(2, 3).as_deref(), Some("multi\nline"));
        assert_eq!(doc.get_path("1/test").unwrap().as_deref(), Some("HGB"));
        assert_eq!(doc.get_path("2/2").unwrap().as_deref(), Some("4.1"));
        assert_eq!(doc.get(3, 0), None);
        assert!(matches!(
            doc.get_path("0/missing"),
            Err(DelimitedError::UnknownColumn(_))
        ));
        assert!(matches!(doc.get_path("x"), Err(DelimitedError::Path(_))));
    }

    #[test]
    fn quotes_only_when_needed() {
        let mut doc = DelimitedDocument::parse(CSV, &csv_with_header()).unwrap();
        doc.set_path("0/value", "5.5").unwrap();
        doc.set_path("1/comment", "a,b").unwrap();
        doc.push_row(&["S4", "NA", "140", "line\r\nbreak \"q\""])
            .unwrap();
        let text = String::from_utf8(doc.to_bytes()).unwrap();
        assert_eq!(
            text,
            "sample,test,value,comment\r\n\
S1,GLU,5.5,\"fasting, morning\"\r\n\
S2,HGB,13.2,\"a,b\"\r\n\
S3,K,4.1,\"multi\nline\"\r\n\
S4,NA,140,\"line\r\nbreak \"\"q\"\"\"\r\n"
        );
        let reparsed = DelimitedDocument::parse(text.as_bytes(), &csv_with_header()).unwrap();
        assert_eq!(reparsed.get(3, 3).as_deref(), Some("line\r\nbreak \"q\""));
        assert!(matches!(
            doc.set(9, 0, "x"),
            Err(DelimitedError::RowOutOfRange { .. })
        ));
    }

    #[test]
    fn supports_escape_characters_and_tabs() {
        let options = DelimitedOptions {
            escape: Some(b'\\'),
            ..DelimitedOptions::tsv()
        };
        let mut doc = DelimitedDocument::parse(b"a\t\"x\\\"y\"\n", &options).unwrap();
        assert_eq!(doc.get(0, 1).as_deref(), Some("x\"y"));
        doc.set(0, 2, "q\"\\").unwrap();
        assert_eq!(doc.to_bytes(), b"a\t\"x\\\"y\"\t\"q\\\"\\\\\"\n");
        assert_eq!(doc.get(0, 2).as_deref(), Some("q\"\\"));
    }

    #[test]
    fn rejects_invalid_options_and_unquotable_values() {
        let options = DelimitedOptions {
            quote: Some(b','),
            ..DelimitedOptions::csv()
        };
        assert!(DelimitedDocument::parse(b"", &options).is_err());
        let options = DelimitedOptions {
            quote: None,
            ..DelimitedOptions::csv()
        };
        let mut doc = DelimitedDocument::parse(b"a,b", &options).unwrap();
        assert_eq!(doc.set(0, 0, "x,y"), Err(DelimitedError::NeedsQuoting));
        let options = DelimitedOptions {
            max_records: 1,
            ..DelimitedOptions::csv()
        };
        assert_eq!(
            DelimitedDocument::parse(b"a\nb", &options),
            Err(DelimitedError::LimitExceeded("records"))
        );
    }
}
