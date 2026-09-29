//! JSON documents.
//!
//! Object member order and number text are preserved (`5.40` stays `5.40`).
//! An unmodified document serializes to its original bytes; once edited it
//! is serialized compactly, because whitespace and string escape choices are
//! not retained.

use std::fmt;
use std::str::FromStr;

use serde_json::{Map, Value};
use thiserror::Error;

/// The largest index a path may address and the largest number of `null`
/// slots a single edit may add to an array.
pub const MAX_ARRAY_PADDING: usize = 4096;

/// Options for [`JsonDocument::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct JsonOptions {
    /// The largest accepted document, in bytes.
    pub max_len: usize,
    /// The deepest accepted nesting of arrays and objects. The JSON parser
    /// itself never accepts more than 128 levels.
    pub max_depth: usize,
}

impl Default for JsonOptions {
    fn default() -> Self {
        Self {
            max_len: 64 * 1024 * 1024,
            max_depth: 64,
        }
    }
}

/// Returned when JSON cannot be parsed or a path cannot be applied.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum JsonError {
    /// The input is not valid JSON.
    #[error("invalid JSON: {0}")]
    Syntax(String),
    /// The input is larger than [`JsonOptions::max_len`].
    #[error("JSON document exceeds {0} bytes")]
    TooLarge(usize),
    /// Arrays and objects are nested deeper than [`JsonOptions::max_depth`].
    #[error("JSON nesting exceeds {0} levels")]
    TooDeep(usize),
    /// The path text is not a valid path.
    #[error("invalid JSON path {0:?}")]
    Path(String),
    /// An edit would have to descend through a value that is not a container
    /// of the required kind.
    #[error("cannot descend into a JSON {found} at {path:?}")]
    TypeMismatch {
        /// The path being written.
        path: String,
        /// The kind of value found.
        found: &'static str,
    },
    /// An edit addresses an array index too far beyond the end of the array.
    #[error("array index {0} is more than {MAX_ARRAY_PADDING} past the end of the array")]
    IndexTooLarge(usize),
}

/// One step of a [`JsonPath`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Segment {
    /// An object member (dotted form).
    Key(String),
    /// An array element (bracket form).
    Index(usize),
    /// A JSON Pointer reference token: a member of an object, or an index
    /// (or `-` for "append") of an array.
    Token(String),
}

/// A location in a JSON document.
///
/// Two notations are accepted:
///
/// - **JSON Pointer** (RFC 6901), when the path is empty or starts with `/`:
///   `/results/2/value`, with `~1` for `/` and `~0` for `~` inside names.
///   When writing, `-` appends to an array.
/// - **Dotted form** as in JavaScript: `results[2].value`, `[0].code`. Array
///   indexes are 0-based. Member names containing `.`, `[` or `]` need the
///   pointer form.
///
/// The empty path addresses the whole document.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JsonPath {
    text: String,
    segments: Vec<Segment>,
}

impl FromStr for JsonPath {
    type Err = JsonError;

    fn from_str(s: &str) -> Result<Self, JsonError> {
        let invalid = || JsonError::Path(s.to_owned());
        let mut segments = Vec::new();
        if s.is_empty() {
        } else if let Some(pointer) = s.strip_prefix('/') {
            for token in pointer.split('/') {
                segments.push(Segment::Token(unescape_pointer(token).ok_or_else(invalid)?));
            }
        } else {
            for (i, part) in s.split('.').enumerate() {
                let (name, mut brackets) = part.split_at(part.find('[').unwrap_or(part.len()));
                if name.contains(']') || (name.is_empty() && (i > 0 || brackets.is_empty())) {
                    return Err(invalid());
                }
                if !name.is_empty() {
                    segments.push(Segment::Key(name.to_owned()));
                }
                while !brackets.is_empty() {
                    let inner = brackets.strip_prefix('[').ok_or_else(invalid)?;
                    let (digits, rest) = inner.split_once(']').ok_or_else(invalid)?;
                    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(invalid());
                    }
                    let index = digits.parse::<usize>().map_err(|_| invalid())?;
                    segments.push(Segment::Index(index));
                    brackets = rest;
                }
            }
        }
        Ok(Self {
            text: s.to_owned(),
            segments,
        })
    }
}

impl fmt::Display for JsonPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

fn unescape_pointer(token: &str) -> Option<String> {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Parses a JSON Pointer array index: digits without leading zeros.
fn pointer_index(token: &str) -> Option<usize> {
    let valid = !token.is_empty()
        && token.bytes().all(|b| b.is_ascii_digit())
        && (token == "0" || !token.starts_with('0'));
    if valid { token.parse().ok() } else { None }
}

/// A parsed JSON document.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonDocument {
    original: Vec<u8>,
    value: Value,
    modified: bool,
}

impl JsonDocument {
    /// Parses a document.
    pub fn parse(input: &[u8], options: &JsonOptions) -> Result<Self, JsonError> {
        if input.len() > options.max_len {
            return Err(JsonError::TooLarge(options.max_len));
        }
        check_depth(input, options.max_depth)?;
        let value = serde_json::from_slice(input).map_err(|e| JsonError::Syntax(e.to_string()))?;
        Ok(Self {
            original: input.to_vec(),
            value,
            modified: false,
        })
    }

    /// Creates a document from a value. It has no original bytes, so it is
    /// always serialized.
    pub fn from_value(value: Value) -> Self {
        Self {
            original: Vec::new(),
            value,
            modified: true,
        }
    }

    /// The whole document.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// The whole document, for arbitrary edits. Marks the document as
    /// modified.
    pub fn value_mut(&mut self) -> &mut Value {
        self.modified = true;
        &mut self.value
    }

    /// Whether the document was edited since it was parsed.
    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// The value at `path`, or `None` when it is absent.
    pub fn get(&self, path: &str) -> Result<Option<&Value>, JsonError> {
        Ok(self.get_path(&path.parse()?))
    }

    /// The value at `path`, or `None` when it is absent.
    pub fn get_path(&self, path: &JsonPath) -> Option<&Value> {
        let mut current = &self.value;
        for segment in &path.segments {
            current = match (segment, current) {
                (Segment::Key(key) | Segment::Token(key), Value::Object(map)) => map.get(key)?,
                (Segment::Index(index), Value::Array(items)) => items.get(*index)?,
                (Segment::Token(token), Value::Array(items)) => items.get(pointer_index(token)?)?,
                _ => return None,
            };
        }
        Some(current)
    }

    /// The value at `path` as text: strings as they are, numbers and
    /// booleans as written in JSON, arrays and objects as compact JSON.
    /// Returns `None` when the value is absent or `null`.
    pub fn get_text(&self, path: &str) -> Result<Option<String>, JsonError> {
        Ok(self.get(path)?.and_then(value_text))
    }

    /// Stores `value` at `path`, creating missing objects and arrays.
    ///
    /// A missing container is created as an array when the next step is an
    /// index (or a pointer token that is a number or `-`) and as an object
    /// otherwise. Arrays are padded with `null` up to the index, at most
    /// [`MAX_ARRAY_PADDING`] elements per edit.
    pub fn set(&mut self, path: &str, value: Value) -> Result<(), JsonError> {
        self.set_path(&path.parse()?, value)
    }

    /// Stores `text` as a JSON string at `path`.
    pub fn set_text(&mut self, path: &str, text: &str) -> Result<(), JsonError> {
        self.set(path, Value::String(text.to_owned()))
    }

    /// Stores `value` at `path`, creating missing objects and arrays.
    pub fn set_path(&mut self, path: &JsonPath, value: Value) -> Result<(), JsonError> {
        let mismatch = |found: &'static str| JsonError::TypeMismatch {
            path: path.text.clone(),
            found,
        };
        let mut current = &mut self.value;
        for segment in &path.segments {
            let wants_array = match segment {
                Segment::Key(_) => false,
                Segment::Index(_) => true,
                Segment::Token(token) => token == "-" || pointer_index(token).is_some(),
            };
            if current.is_null() {
                *current = if wants_array {
                    Value::Array(Vec::new())
                } else {
                    Value::Object(Map::new())
                };
            }
            current = match (segment, current) {
                (Segment::Key(key) | Segment::Token(key), Value::Object(map)) => {
                    map.entry(key.clone()).or_insert(Value::Null)
                }
                (Segment::Index(index), Value::Array(items)) => slot(items, *index)?,
                (Segment::Token(token), Value::Array(items)) => {
                    if token == "-" {
                        items.push(Value::Null);
                        let last = items.len() - 1;
                        &mut items[last]
                    } else {
                        let index = pointer_index(token).ok_or_else(|| mismatch("array"))?;
                        slot(items, index)?
                    }
                }
                (_, other) => return Err(mismatch(kind(other))),
            };
        }
        *current = value;
        self.modified = true;
        Ok(())
    }

    /// Removes the value at `path` and returns it. Array elements after it
    /// move down by one.
    pub fn remove(&mut self, path: &str) -> Result<Option<Value>, JsonError> {
        let path: JsonPath = path.parse()?;
        let Some((last, parents)) = path.segments.split_last() else {
            self.modified = true;
            return Ok(Some(std::mem::take(&mut self.value)));
        };
        let parent = JsonPath {
            text: String::new(),
            segments: parents.to_vec(),
        };
        let removed = match (last, self.get_path_mut(&parent)) {
            (Segment::Key(key) | Segment::Token(key), Some(Value::Object(map))) => {
                map.shift_remove(key)
            }
            (Segment::Index(index), Some(Value::Array(items))) if *index < items.len() => {
                Some(items.remove(*index))
            }
            (Segment::Token(token), Some(Value::Array(items))) => pointer_index(token)
                .filter(|index| *index < items.len())
                .map(|index| items.remove(index)),
            _ => None,
        };
        if removed.is_some() {
            self.modified = true;
        }
        Ok(removed)
    }

    fn get_path_mut(&mut self, path: &JsonPath) -> Option<&mut Value> {
        let mut current = &mut self.value;
        for segment in &path.segments {
            current = match (segment, current) {
                (Segment::Key(key) | Segment::Token(key), Value::Object(map)) => {
                    map.get_mut(key)?
                }
                (Segment::Index(index), Value::Array(items)) => items.get_mut(*index)?,
                (Segment::Token(token), Value::Array(items)) => {
                    items.get_mut(pointer_index(token)?)?
                }
                _ => return None,
            };
        }
        Some(current)
    }

    /// Serializes the document: the original bytes when unmodified,
    /// otherwise compact JSON.
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.modified {
            serde_json::to_vec(&self.value).unwrap_or_default()
        } else {
            self.original.clone()
        }
    }

    /// Serializes the document as indented JSON.
    pub fn to_pretty_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(&self.value).unwrap_or_default()
    }
}

fn slot(items: &mut Vec<Value>, index: usize) -> Result<&mut Value, JsonError> {
    if index >= items.len() {
        if index - items.len() > MAX_ARRAY_PADDING {
            return Err(JsonError::IndexTooLarge(index));
        }
        items.resize(index + 1, Value::Null);
    }
    Ok(&mut items[index])
}

/// Text form of a value, `None` for `null`.
pub(crate) fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        other => serde_json::to_string(other).ok(),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Rejects nesting deeper than `max` without building the document.
fn check_depth(input: &[u8], max: usize) -> Result<(), JsonError> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &b in input {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > max {
                    return Err(JsonError::TooDeep(max));
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const RESULT: &[u8] =
        br#"{"patient":{"id":"12345"},"results":[{"code":"GLU","value":5.40},{"code":"HGB","value":13.2}]}"#;

    fn parse(input: &[u8]) -> JsonDocument {
        JsonDocument::parse(input, &JsonOptions::default()).unwrap()
    }

    #[test]
    fn keeps_unmodified_documents_byte_for_byte() {
        let input = b"{ \"b\" : 1.10,\n  \"a\": [ ] }";
        assert_eq!(parse(input).to_bytes(), input);
    }

    #[test]
    fn reads_both_path_notations() {
        let doc = parse(RESULT);
        assert_eq!(
            doc.get_text("results[1].code").unwrap().as_deref(),
            Some("HGB")
        );
        assert_eq!(
            doc.get_text("/results/0/value").unwrap().as_deref(),
            Some("5.40")
        );
        assert_eq!(
            doc.get_text("patient.id").unwrap().as_deref(),
            Some("12345")
        );
        assert_eq!(doc.get_text("results[5].code").unwrap(), None);
        assert_eq!(doc.get_text("/results/01").unwrap(), None);
        assert!(doc.get_text("results[").is_err());
        assert!(doc.get_text("a..b").is_err());
        assert!(doc.get_text("/a/~2").is_err());
    }

    #[test]
    fn edits_keep_order_and_number_text() {
        let mut doc = parse(RESULT);
        doc.set_text("results[0].unit", "mmol/L").unwrap();
        assert_eq!(
            String::from_utf8(doc.to_bytes()).unwrap(),
            r#"{"patient":{"id":"12345"},"results":[{"code":"GLU","value":5.40,"unit":"mmol/L"},{"code":"HGB","value":13.2}]}"#
        );
    }

    #[test]
    fn creates_missing_containers() {
        let mut doc = JsonDocument::from_value(Value::Null);
        doc.set_text("order.tests[2].code", "GLU").unwrap();
        doc.set("/order/priority", json!("routine")).unwrap();
        doc.set("/order/notes/-", json!("fasting")).unwrap();
        assert_eq!(
            doc.value(),
            &json!({"order": {"tests": [null, null, {"code": "GLU"}], "priority": "routine", "notes": ["fasting"]}})
        );
        assert!(matches!(
            doc.set_text("order.priority.level", "x"),
            Err(JsonError::TypeMismatch {
                found: "string",
                ..
            })
        ));
        assert_eq!(
            doc.set_text("order.tests[99999]", "x"),
            Err(JsonError::IndexTooLarge(99_999))
        );
    }

    #[test]
    fn removes_values() {
        let mut doc = parse(RESULT);
        assert_eq!(doc.remove("results[0]").unwrap().unwrap()["code"], "GLU");
        assert_eq!(
            doc.get_text("results[0].code").unwrap().as_deref(),
            Some("HGB")
        );
        assert_eq!(doc.remove("missing").unwrap(), None);
    }

    #[test]
    fn text_forms() {
        let doc = parse(br#"{"n":null,"b":true,"o":{"x":[1,"a"]}}"#);
        assert_eq!(doc.get_text("n").unwrap(), None);
        assert_eq!(doc.get_text("b").unwrap().as_deref(), Some("true"));
        assert_eq!(
            doc.get_text("o").unwrap().as_deref(),
            Some(r#"{"x":[1,"a"]}"#)
        );
        assert_eq!(
            doc.get_text("").unwrap().as_deref(),
            Some(r#"{"n":null,"b":true,"o":{"x":[1,"a"]}}"#)
        );
    }

    #[test]
    fn enforces_limits() {
        let options = JsonOptions {
            max_depth: 3,
            ..JsonOptions::default()
        };
        assert!(JsonDocument::parse(b"[[[1]]]", &options).is_ok());
        assert_eq!(
            JsonDocument::parse(b"[[[[1]]]]", &options),
            Err(JsonError::TooDeep(3))
        );
        assert!(JsonDocument::parse(br#"["[[[[["]"#, &options).is_ok());
        let options = JsonOptions {
            max_len: 4,
            ..JsonOptions::default()
        };
        assert_eq!(
            JsonDocument::parse(b"[1,2]", &options),
            Err(JsonError::TooLarge(4))
        );
        assert!(matches!(
            JsonDocument::parse(b"{", &JsonOptions::default()),
            Err(JsonError::Syntax(_))
        ));
    }
}
