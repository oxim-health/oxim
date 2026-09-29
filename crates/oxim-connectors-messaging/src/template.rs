//! Text with `{value}` placeholders filled from a delivery, for topics,
//! subjects, routing keys and message keys: `lab/{MSH-3}/results`,
//! `{$message_id}`.

use oxim_connectors::values::{DeliveryValues, ValueSource};
use oxim_core::EngineError;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Value(ValueSource),
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Template {
    parts: Vec<Part>,
}

impl Template {
    /// Parses `{...}` placeholders; `{{` and `}}` are literal braces.
    pub(crate) fn parse(text: &str, what: &str) -> Result<Self, EngineError> {
        let error = |message: String| EngineError::Config(format!("{what}: {message}"));
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    literal.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    literal.push('}');
                }
                '{' => {
                    let mut inner = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) => inner.push(c),
                            None => return Err(error(format!("unclosed {{ in {text:?}"))),
                        }
                    }
                    if !literal.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut literal)));
                    }
                    parts.push(Part::Value(ValueSource::parse(&inner).map_err(error)?));
                }
                '}' => return Err(error(format!("unmatched }} in {text:?}"))),
                c => literal.push(c),
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Text(literal));
        }
        Ok(Self { parts })
    }

    /// Fills the placeholders; a value that is missing is an error.
    pub(crate) fn render(&self, values: &mut DeliveryValues<'_>) -> Result<String, String> {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => out.push_str(text),
                Part::Value(source) => match values.get(source)? {
                    Some(value) => out.push_str(&value),
                    None => return Err(format!("{source:?} has no value")),
                },
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use oxim_model::{ChannelId, ConnectorId, DataType, MessageId};
    use oxim_store::Delivery;

    use super::*;

    #[test]
    fn renders_placeholders() {
        let delivery = Delivery {
            message_id: MessageId::from_parts(1_790_000_000_000, 1),
            channel: ChannelId::new("lab").unwrap(),
            destination: ConnectorId::new("out").unwrap(),
            attempts: 0,
            payload: b"MSH|^~\\&|CHEM|LAB|LIS|HOSP|20260929120000||ORU^R01|C1|P|2.5.1\r".to_vec(),
            data_type: Some(DataType::Hl7V2),
        };
        let mut values = DeliveryValues::new(&delivery);
        let template = Template::parse("lab/{MSH-3}/{$channel}/{{raw}}", "topic").unwrap();
        assert_eq!(template.render(&mut values).unwrap(), "lab/CHEM/lab/{raw}");
        assert_eq!(
            Template::parse("fixed/topic", "topic")
                .unwrap()
                .render(&mut values)
                .unwrap(),
            "fixed/topic"
        );
        assert!(
            Template::parse("{PID-7}", "topic")
                .unwrap()
                .render(&mut values)
                .is_err()
        );
        for bad in ["lab/{MSH-3", "lab/}", "{$nope}", "{}"] {
            assert!(Template::parse(bad, "topic").is_err(), "{bad}");
        }
    }
}
