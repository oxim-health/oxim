//! A uniform view over all formats.

use thiserror::Error;

use crate::delimited::{DelimitedDocument, DelimitedError, DelimitedOptions};
use crate::fixed_width::{FixedWidthDocument, FixedWidthError, FixedWidthLayout};
use crate::json::{JsonDocument, JsonError, JsonOptions};
use crate::xml::{XmlDocument, XmlError, XmlOptions};

/// The data type of a channel payload, with its parsing options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataType {
    /// JSON.
    Json(JsonOptions),
    /// XML.
    Xml(XmlOptions),
    /// CSV, TSV or another delimited format.
    Delimited(DelimitedOptions),
    /// Fixed-width records.
    FixedWidth(FixedWidthLayout),
}

impl DataType {
    /// A short name for logs and configuration: `json`, `xml`, `delimited`
    /// or `fixed-width`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Json(_) => "json",
            Self::Xml(_) => "xml",
            Self::Delimited(_) => "delimited",
            Self::FixedWidth(_) => "fixed-width",
        }
    }
}

/// Returned by [`Document`] operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FormatError {
    /// A JSON error.
    #[error(transparent)]
    Json(#[from] JsonError),
    /// An XML error.
    #[error(transparent)]
    Xml(#[from] XmlError),
    /// A delimited text error.
    #[error(transparent)]
    Delimited(#[from] DelimitedError),
    /// A fixed-width error.
    #[error(transparent)]
    FixedWidth(#[from] FixedWidthError),
}

/// A parsed document of any supported data type, with text-based access so
/// the engine can treat every format the same way.
///
/// Paths follow the conventions of each format:
///
/// | Data type | Path | Example |
/// |---|---|---|
/// | JSON | JSON Pointer or dotted form, 0-based | `results[0].value`, `/results/0/value` |
/// | XML | XPath subset, 1-based | `/order/test[2]/@code` |
/// | Delimited | `row/column`, 0-based, column index or header name | `3/value` |
/// | Fixed width | `record/field`, 0-based | `0/sample` |
#[derive(Debug, Clone, PartialEq)]
pub enum Document {
    /// A JSON document.
    Json(JsonDocument),
    /// An XML document.
    Xml(XmlDocument),
    /// A delimited document.
    Delimited(DelimitedDocument),
    /// A fixed-width document.
    FixedWidth(FixedWidthDocument),
}

impl Document {
    /// Parses `input` as `data_type`.
    pub fn parse(input: &[u8], data_type: &DataType) -> Result<Self, FormatError> {
        Ok(match data_type {
            DataType::Json(options) => Self::Json(JsonDocument::parse(input, options)?),
            DataType::Xml(options) => Self::Xml(XmlDocument::parse(input, options)?),
            DataType::Delimited(options) => {
                Self::Delimited(DelimitedDocument::parse(input, options)?)
            }
            DataType::FixedWidth(layout) => {
                Self::FixedWidth(FixedWidthDocument::parse(input, layout)?)
            }
        })
    }

    /// The short name of the data type.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Json(_) => "json",
            Self::Xml(_) => "xml",
            Self::Delimited(_) => "delimited",
            Self::FixedWidth(_) => "fixed-width",
        }
    }

    /// The text at `path`, or `None` when it is absent. JSON `null` is
    /// reported as absent.
    pub fn get(&self, path: &str) -> Result<Option<String>, FormatError> {
        Ok(match self {
            Self::Json(doc) => doc.get_text(path)?,
            Self::Xml(doc) => doc.get(path)?,
            Self::Delimited(doc) => doc.get_path(path)?,
            Self::FixedWidth(doc) => doc.get_path(path)?,
        })
    }

    /// Stores `value` as text at `path`. In JSON it becomes a string; use
    /// [`JsonDocument::set`] for other value types.
    pub fn set(&mut self, path: &str, value: &str) -> Result<(), FormatError> {
        match self {
            Self::Json(doc) => doc.set_text(path, value)?,
            Self::Xml(doc) => doc.set(path, value)?,
            Self::Delimited(doc) => doc.set_path(path, value)?,
            Self::FixedWidth(doc) => doc.set_path(path, value)?,
        }
        Ok(())
    }

    /// Serializes the document.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Json(doc) => doc.to_bytes(),
            Self::Xml(doc) => doc.to_bytes(),
            Self::Delimited(doc) => doc.to_bytes(),
            Self::FixedWidth(doc) => doc.to_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed_width::FixedField;

    #[test]
    fn treats_every_format_alike() {
        let cases = [
            (
                DataType::Json(JsonOptions::default()),
                &b"{\"a\":{\"b\":\"1\"}}"[..],
                "a.b",
            ),
            (
                DataType::Xml(XmlOptions::default()),
                b"<a><b>1</b></a>",
                "/a/b",
            ),
            (
                DataType::Delimited(DelimitedOptions {
                    has_header: true,
                    ..DelimitedOptions::csv()
                }),
                b"a,b\r\n0,1\r\n",
                "0/b",
            ),
            (
                DataType::FixedWidth(
                    FixedWidthLayout::new(vec![
                        FixedField::new("a", 0, 2),
                        FixedField::new("b", 2, 2),
                    ])
                    .unwrap(),
                ),
                b"x 1 \r\n",
                "0/b",
            ),
        ];
        for (data_type, input, path) in cases {
            let mut doc = Document::parse(input, &data_type).unwrap();
            assert_eq!(doc.kind(), data_type.name());
            assert_eq!(doc.to_bytes(), input);
            assert_eq!(doc.get(path).unwrap().as_deref(), Some("1"));
            doc.set(path, "22").unwrap();
            let reparsed = Document::parse(&doc.to_bytes(), &data_type).unwrap();
            assert_eq!(reparsed.get(path).unwrap().as_deref(), Some("22"));
        }
    }
}
