//! Parsed messages of every supported data type behind one interface.

use encoding_rs::{Encoding, UTF_8};
use oxim_formats::{DelimitedOptions, FixedField, FixedWidthLayout, JsonOptions, XmlOptions};
use oxim_model::DataType;

use crate::config::Settings;
use crate::error::{EngineError, StepError};

/// A parsed message.
///
/// Every variant keeps the message losslessly, so a document that is not
/// edited serializes to the bytes it was parsed from.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Document {
    /// HL7 v2 (ER7).
    Hl7(oxim_hl7::Message),
    /// ASTM E1394 / CLSI LIS02 records.
    Astm(oxim_astm::Message),
    /// POCT1-A XML.
    Poct1a(oxim_poct1a::Message),
    /// JSON, XML, delimited text or fixed-width records.
    Format(oxim_formats::Document),
    /// Bytes of a data type that is not parsed.
    Raw(Vec<u8>),
}

impl Document {
    /// The data type of the document.
    pub fn data_type(&self) -> DataType {
        match self {
            Self::Hl7(_) => DataType::Hl7V2,
            Self::Astm(_) => DataType::Astm,
            Self::Poct1a(_) => DataType::Poct1a,
            #[allow(unreachable_patterns)]
            Self::Format(document) => match document {
                oxim_formats::Document::Json(_) => DataType::Json,
                oxim_formats::Document::Xml(_) => DataType::Xml,
                oxim_formats::Document::Delimited(_) => DataType::Delimited,
                oxim_formats::Document::FixedWidth(_) => DataType::FixedWidth,
                _ => DataType::Raw,
            },
            Self::Raw(_) => DataType::Raw,
        }
    }

    /// Serializes the document.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Hl7(message) => message.to_bytes(),
            Self::Astm(message) => message.to_bytes(),
            Self::Poct1a(message) => message.to_bytes(),
            Self::Format(document) => document.to_bytes(),
            Self::Raw(bytes) => bytes.clone(),
        }
    }

    /// The text at `path`, with escape sequences resolved. Path syntax
    /// depends on the data type: `PID-5.1` (HL7), `R[2]-4` (ASTM),
    /// `SVC/PT/OBS/OBS.value` (POCT1-A), `/order/test/@code` (XML),
    /// `results[0].value` (JSON), `3/name` (delimited, fixed width).
    pub fn get(&self, path: &str) -> Result<Option<String>, StepError> {
        Ok(match self {
            Self::Hl7(message) => {
                let encoding = message.declared_encoding().ok().flatten().unwrap_or(UTF_8);
                message
                    .get(path)
                    .map(|value| value.to_text(encoding).into_owned())
            }
            Self::Astm(message) => message
                .get(path)
                .map(|value| value.to_string_lossy().into_owned()),
            Self::Poct1a(message) => message.value(path).map(str::to_owned),
            Self::Format(document) => document
                .get(path)
                .map_err(|e| StepError::new("get", e.to_string()))?,
            Self::Raw(_) => {
                return Err(StepError::new(
                    "get",
                    "raw documents have no addressable values",
                ));
            }
        })
    }

    /// Stores `text` at `path`, escaping it as the data type requires.
    pub fn set(&mut self, path: &str, text: &str) -> Result<(), StepError> {
        let error = |e: &dyn std::fmt::Display| StepError::new("set", format!("{path}: {e}"));
        match self {
            Self::Hl7(message) => message.set(path, text).map_err(|e| error(&e)),
            Self::Astm(message) => message.set_text(path, text, UTF_8).map_err(|e| error(&e)),
            Self::Format(document) => document.set(path, text).map_err(|e| error(&e)),
            Self::Poct1a(_) => Err(StepError::new(
                "set",
                "POCT1-A documents are edited through the normalized model",
            )),
            Self::Raw(_) => Err(StepError::new(
                "set",
                "raw documents have no addressable values",
            )),
        }
    }
}

/// Parses messages of one data type with fixed options, prepared once when
/// a channel is deployed.
#[derive(Debug, Clone)]
pub struct DocumentParser {
    data_type: DataType,
    format: Option<oxim_formats::DataType>,
}

impl DocumentParser {
    /// Prepares a parser. Generic formats read their options from
    /// `format`: `delimiter`, `quote` and `header` for delimited text, a
    /// `fields` list of `{name, start, length}` for fixed width, and an
    /// optional `encoding` label.
    pub fn new(data_type: DataType, format: Option<&Settings>) -> Result<Self, EngineError> {
        let empty = Settings::new();
        let settings = format.unwrap_or(&empty);
        let encoding = match settings.get("encoding").and_then(serde_json::Value::as_str) {
            Some(label) => Some(
                Encoding::for_label(label.as_bytes())
                    .ok_or_else(|| EngineError::Config(format!("unknown encoding {label:?}")))?,
            ),
            None => None,
        };
        let format = match data_type {
            DataType::Json => Some(oxim_formats::DataType::Json(JsonOptions::default())),
            DataType::Xml => Some(oxim_formats::DataType::Xml(XmlOptions::default())),
            DataType::Delimited => Some(oxim_formats::DataType::Delimited(delimited_options(
                settings, encoding,
            )?)),
            DataType::FixedWidth => Some(oxim_formats::DataType::FixedWidth(fixed_width_layout(
                settings, encoding,
            )?)),
            _ => None,
        };
        Ok(Self { data_type, format })
    }

    /// The data type this parser reads.
    pub fn data_type(&self) -> DataType {
        self.data_type
    }

    /// Parses `raw`.
    pub fn parse(&self, raw: &[u8]) -> Result<Document, StepError> {
        let error = |e: &dyn std::fmt::Display| StepError::new("parse", e.to_string());
        Ok(match (self.data_type, &self.format) {
            (DataType::Hl7V2, _) => {
                Document::Hl7(oxim_hl7::Message::parse(raw).map_err(|e| error(&e))?)
            }
            (DataType::Astm, _) => {
                Document::Astm(oxim_astm::Message::parse(raw).map_err(|e| error(&e))?)
            }
            (DataType::Poct1a, _) => {
                Document::Poct1a(oxim_poct1a::Message::parse(raw).map_err(|e| error(&e))?)
            }
            (_, Some(format)) => {
                Document::Format(oxim_formats::Document::parse(raw, format).map_err(|e| error(&e))?)
            }
            _ => Document::Raw(raw.to_vec()),
        })
    }
}

fn single_byte(settings: &Settings, key: &str) -> Result<Option<u8>, EngineError> {
    match settings.get(key).and_then(serde_json::Value::as_str) {
        None => Ok(None),
        Some("\\t" | "tab") => Ok(Some(b'\t')),
        Some(text) if text.len() == 1 && text.is_ascii() => Ok(text.bytes().next()),
        Some(text) => Err(EngineError::Config(format!(
            "{key} must be a single ASCII character, not {text:?}"
        ))),
    }
}

fn delimited_options(
    settings: &Settings,
    encoding: Option<&'static Encoding>,
) -> Result<DelimitedOptions, EngineError> {
    let mut options = DelimitedOptions::csv();
    if let Some(delimiter) = single_byte(settings, "delimiter")? {
        options = options.with_delimiter(delimiter);
    }
    if settings.contains_key("quote") {
        options = options.with_quote(single_byte(settings, "quote")?);
    }
    if let Some(header) = settings.get("header").and_then(serde_json::Value::as_bool) {
        options = options.with_header(header);
    }
    if let Some(encoding) = encoding {
        options = options.with_encoding(encoding);
    }
    Ok(options)
}

fn fixed_width_layout(
    settings: &Settings,
    encoding: Option<&'static Encoding>,
) -> Result<FixedWidthLayout, EngineError> {
    let fields = settings
        .get("fields")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| EngineError::Config("fixed-width data needs a `fields` list".into()))?;
    let mut layout_fields = Vec::with_capacity(fields.len());
    for field in fields {
        let name = field.get("name").and_then(serde_json::Value::as_str);
        let start = field.get("start").and_then(serde_json::Value::as_u64);
        let length = field.get("length").and_then(serde_json::Value::as_u64);
        let (Some(name), Some(start), Some(length)) = (name, start, length) else {
            return Err(EngineError::Config(
                "every fixed-width field needs `name`, `start` and `length`".into(),
            ));
        };
        let to_usize = |n: u64| {
            usize::try_from(n).map_err(|_| EngineError::Config("field position too large".into()))
        };
        layout_fields.push(FixedField::new(name, to_usize(start)?, to_usize(length)?));
    }
    let mut layout = FixedWidthLayout::new(layout_fields)
        .map_err(|e| EngineError::Config(format!("invalid fixed-width layout: {e}")))?;
    if let Some(encoding) = encoding {
        layout = layout
            .with_encoding(encoding)
            .map_err(|e| EngineError::Config(format!("invalid fixed-width encoding: {e}")))?;
    }
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(json: serde_json::Value) -> Settings {
        match json {
            serde_json::Value::Object(map) => map,
            _ => Settings::new(),
        }
    }

    #[test]
    fn reads_and_edits_every_data_type() {
        let hl7 = DocumentParser::new(DataType::Hl7V2, None).unwrap();
        let mut doc = hl7.parse(b"MSH|^~\\&|LAB\rPID|1||42||Doe^Jane\r").unwrap();
        assert_eq!(doc.get("PID-5.2").unwrap().as_deref(), Some("Jane"));
        doc.set("PID-5.2", "Janet^Marie").unwrap();
        assert!(doc.to_bytes().ends_with(b"Doe^Janet\\S\\Marie\r"));
        assert_eq!(doc.data_type(), DataType::Hl7V2);

        let astm = DocumentParser::new(DataType::Astm, None).unwrap();
        let doc = astm
            .parse(b"H|\\^&|||ANALYZER\rR|1|^^^GLU|5.4|mmol/L\rL|1|N\r")
            .unwrap();
        assert_eq!(doc.get("R-4").unwrap().as_deref(), Some("5.4"));

        let json = DocumentParser::new(DataType::Json, None).unwrap();
        let mut doc = json
            .parse(br#"{"results":[{"code":"GLU","value":5.40}]}"#)
            .unwrap();
        assert_eq!(doc.get("results[0].code").unwrap().as_deref(), Some("GLU"));
        doc.set("results[0].code", "1520").unwrap();
        assert_eq!(doc.get("results[0].code").unwrap().as_deref(), Some("1520"));

        let csv = DocumentParser::new(
            DataType::Delimited,
            Some(&settings(
                serde_json::json!({"delimiter": ";", "header": true}),
            )),
        )
        .unwrap();
        let doc = csv.parse(b"code;value\r\nGLU;5.4\r\n").unwrap();
        assert_eq!(doc.data_type(), DataType::Delimited);

        let raw = DocumentParser::new(DataType::Dicom, None).unwrap();
        let doc = raw.parse(b"\x00\x01").unwrap();
        assert_eq!(doc.to_bytes(), b"\x00\x01");
        assert!(doc.get("x").is_err());
    }

    #[test]
    fn reports_parse_errors() {
        let hl7 = DocumentParser::new(DataType::Hl7V2, None).unwrap();
        let error = hl7.parse(b"PID|1").unwrap_err();
        assert_eq!(error.step, "parse");
    }

    #[test]
    fn validates_format_settings() {
        assert!(DocumentParser::new(DataType::FixedWidth, None).is_err());
        assert!(
            DocumentParser::new(
                DataType::Delimited,
                Some(&settings(serde_json::json!({"delimiter": ";;"})))
            )
            .is_err()
        );
        assert!(
            DocumentParser::new(
                DataType::Json,
                Some(&settings(serde_json::json!({"encoding": "klingon"})))
            )
            .is_err()
        );
        let fixed = DocumentParser::new(
            DataType::FixedWidth,
            Some(&settings(serde_json::json!({
                "fields": [{"name": "code", "start": 0, "length": 4}, {"name": "value", "start": 4, "length": 6}]
            }))),
        )
        .unwrap();
        let doc = fixed.parse(b"GLU    5.4\n").unwrap();
        assert_eq!(doc.data_type(), DataType::FixedWidth);
    }
}
