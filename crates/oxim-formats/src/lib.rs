//! Lossless JSON, XML, delimited and fixed-width documents for OXIM
//! channels.
//!
//! Every format follows the same pattern: parse bytes into a typed
//! document, read and write values by path, and serialize. Unmodified
//! documents serialize to their original bytes; edits touch only what they
//! change (JSON is re-serialized compactly once edited, see [`json`]).
//! [`Document`] offers one text-based interface over all formats so an
//! engine can treat them alike.
//!
//! The crate performs no I/O, never panics on malformed input, and bounds
//! memory with configurable limits. XML document type declarations and
//! custom entities are rejected.
//!
//! JSON numbers keep their exact text (`5.40` stays `5.40`) through
//! `serde_json`'s `arbitrary_precision` feature. Cargo unifies features, so
//! any crate built together with `oxim-formats` sees `serde_json` numbers in
//! that representation.
//!
//! ```
//! use oxim_formats::{DataType, Document, DelimitedOptions, JsonOptions};
//!
//! let mut json = Document::parse(
//!     br#"{"results":[{"code":"GLU","value":5.40}]}"#,
//!     &DataType::Json(JsonOptions::default()),
//! )?;
//! assert_eq!(json.get("results[0].value")?.as_deref(), Some("5.40"));
//! json.set("results[0].unit", "mmol/L")?;
//!
//! let csv = Document::parse(
//!     b"sample,test,value\r\nS1,GLU,5.4\r\n",
//!     &DataType::Delimited(DelimitedOptions::csv().with_header(true)),
//! )?;
//! assert_eq!(csv.get("0/test")?.as_deref(), Some("GLU"));
//! # Ok::<(), oxim_formats::FormatError>(())
//! ```

pub mod delimited;
mod document;
pub mod fixed_width;
pub mod json;
mod text;
pub mod xml;

pub use delimited::{DelimitedDocument, DelimitedError, DelimitedOptions};
pub use document::{DataType, Document, FormatError};
pub use fixed_width::{
    Alignment, FixedField, FixedWidthDocument, FixedWidthError, FixedWidthLayout, Overflow,
};
pub use json::{JsonDocument, JsonError, JsonOptions, JsonPath};
pub use text::LineEnding;
pub use xml::{XmlDocument, XmlElement, XmlError, XmlOptions, XmlPath};

/// Re-exported so callers can name encodings and JSON values without adding
/// the dependencies themselves.
pub use encoding_rs::Encoding;
pub use serde_json::Value as JsonValue;
