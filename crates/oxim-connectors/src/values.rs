//! Values taken from a delivery, for connector settings that bind message
//! content: database statement parameters, message keys, topics and
//! headers.
//!
//! | Written as | Value |
//! |---|---|
//! | `$payload` | The delivered bytes as text (invalid UTF-8 is replaced) |
//! | `$message_id` | The OXIM message identifier |
//! | `$channel`, `$destination` | The channel and destination identifiers |
//! | `$data_type` | The data type of the delivered bytes, for example `hl7v2` |
//! | `$attempts` | Earlier delivery attempts |
//! | `{value: LAB1}` | The constant `LAB1` |
//! | anything else | A path into the delivered message, parsed with its data type: `MSH-10`, `PID-3.1`, `R[1]-3.4`, `order.id` |
//!
//! A path that has no value gives no value (for example SQL `NULL`). A
//! delivery whose payload cannot be parsed for a path fails.

use oxim_core::{Document, DocumentParser};
use oxim_model::DataType;
use oxim_store::Delivery;
use serde::Deserialize;

/// Where a value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawSource")]
pub enum ValueSource {
    /// The delivered bytes as text.
    Payload,
    /// The message identifier.
    MessageId,
    /// The channel identifier.
    Channel,
    /// The destination identifier.
    Destination,
    /// The data type of the payload.
    DataType,
    /// Earlier delivery attempts.
    Attempts,
    /// A constant.
    Constant(String),
    /// A path into the parsed payload.
    Path(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawSource {
    Text(String),
    Constant { value: serde_json::Value },
}

impl TryFrom<RawSource> for ValueSource {
    type Error = String;

    fn try_from(raw: RawSource) -> Result<Self, String> {
        match raw {
            RawSource::Text(text) => Self::parse(&text),
            RawSource::Constant { value } => match value {
                serde_json::Value::String(text) => Ok(Self::Constant(text)),
                serde_json::Value::Number(number) => Ok(Self::Constant(number.to_string())),
                serde_json::Value::Bool(flag) => Ok(Self::Constant(flag.to_string())),
                other => Err(format!("a constant value must be text, not {other}")),
            },
        }
    }
}

impl ValueSource {
    /// Parses the text form: a `$` name or a path.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if let Some(name) = text.strip_prefix('$') {
            return match name {
                "payload" => Ok(Self::Payload),
                "message_id" => Ok(Self::MessageId),
                "channel" => Ok(Self::Channel),
                "destination" => Ok(Self::Destination),
                "data_type" => Ok(Self::DataType),
                "attempts" => Ok(Self::Attempts),
                other => Err(format!(
                    "unknown value ${other}; use $payload, $message_id, $channel, $destination, $data_type or $attempts"
                )),
            };
        }
        if text.is_empty() {
            return Err("a value source must not be empty".into());
        }
        Ok(Self::Path(text.to_owned()))
    }
}

/// Resolves [`ValueSource`]s for one delivery, parsing the payload once and
/// only when a path needs it.
#[derive(Debug)]
pub struct DeliveryValues<'a> {
    delivery: &'a Delivery,
    document: Option<Result<Document, String>>,
}

impl<'a> DeliveryValues<'a> {
    /// Values of `delivery`.
    pub fn new(delivery: &'a Delivery) -> Self {
        Self {
            delivery,
            document: None,
        }
    }

    fn document(&mut self) -> Result<&Document, String> {
        if self.document.is_none() {
            let data_type = self.delivery.data_type.unwrap_or(DataType::Raw);
            let parsed = DocumentParser::new(data_type, None)
                .map_err(|e| e.to_string())
                .and_then(|parser| {
                    parser
                        .parse(&self.delivery.payload)
                        .map_err(|e| format!("the {data_type} payload cannot be parsed: {e}"))
                });
            self.document = Some(parsed);
        }
        match &self.document {
            Some(Ok(document)) => Ok(document),
            Some(Err(error)) => Err(error.clone()),
            None => Err("the payload was not parsed".into()),
        }
    }

    /// The value of `source`; `None` when a path has no value.
    pub fn get(&mut self, source: &ValueSource) -> Result<Option<String>, String> {
        let delivery = self.delivery;
        Ok(Some(match source {
            ValueSource::Payload => String::from_utf8_lossy(&delivery.payload).into_owned(),
            ValueSource::MessageId => delivery.message_id.to_string(),
            ValueSource::Channel => delivery.channel.to_string(),
            ValueSource::Destination => delivery.destination.to_string(),
            ValueSource::DataType => delivery
                .data_type
                .map_or_else(|| DataType::Raw.to_string(), |t| t.to_string()),
            ValueSource::Attempts => delivery.attempts.to_string(),
            ValueSource::Constant(value) => value.clone(),
            ValueSource::Path(path) => {
                let value = self
                    .document()?
                    .get(path)
                    .map_err(|e| format!("path {path}: {e}"))?;
                return Ok(value.filter(|value| !value.is_empty()));
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use oxim_model::{ChannelId, ConnectorId, MessageId};

    use super::*;

    fn delivery(payload: &[u8], data_type: DataType) -> Delivery {
        Delivery {
            message_id: MessageId::from_parts(1_790_000_000_000, 7),
            channel: ChannelId::new("lab").unwrap(),
            destination: ConnectorId::new("db").unwrap(),
            attempts: 2,
            payload: payload.to_vec(),
            data_type: Some(data_type),
        }
    }

    #[test]
    fn parses_sources() {
        assert_eq!(ValueSource::parse("$payload"), Ok(ValueSource::Payload));
        assert_eq!(
            ValueSource::parse(" MSH-10 "),
            Ok(ValueSource::Path("MSH-10".into()))
        );
        assert!(ValueSource::parse("$nope").is_err());
        assert!(ValueSource::parse("").is_err());
        let constant: ValueSource = serde_json::from_str(r#"{"value": 42}"#).unwrap();
        assert_eq!(constant, ValueSource::Constant("42".into()));
        let path: ValueSource = serde_json::from_str(r#""PID-3.1""#).unwrap();
        assert_eq!(path, ValueSource::Path("PID-3.1".into()));
        assert!(serde_json::from_str::<ValueSource>(r#"{"value": [1]}"#).is_err());
    }

    #[test]
    fn resolves_fields_and_paths() {
        let hl7 = delivery(
            b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01|C42|P|2.5.1\rPID|1||P1001^^^LAB||DOE^JANE\r",
            DataType::Hl7V2,
        );
        let mut values = DeliveryValues::new(&hl7);
        let get = |values: &mut DeliveryValues<'_>, text: &str| {
            values.get(&ValueSource::parse(text).unwrap()).unwrap()
        };
        assert_eq!(get(&mut values, "MSH-10").as_deref(), Some("C42"));
        assert_eq!(get(&mut values, "PID-3.1").as_deref(), Some("P1001"));
        assert_eq!(get(&mut values, "PID-7"), None);
        assert_eq!(get(&mut values, "$attempts").as_deref(), Some("2"));
        assert_eq!(get(&mut values, "$data_type").as_deref(), Some("hl7v2"));
        assert_eq!(get(&mut values, "$channel").as_deref(), Some("lab"));

        let broken = delivery(b"not hl7", DataType::Hl7V2);
        let mut values = DeliveryValues::new(&broken);
        assert!(values.get(&ValueSource::Path("MSH-10".into())).is_err());
        assert_eq!(
            values.get(&ValueSource::Payload).unwrap().as_deref(),
            Some("not hl7")
        );
    }
}
