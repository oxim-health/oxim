//! Text templates with `{...}` placeholders.

use oxim_core::{EngineError, MessageContext, StepError};

use crate::path::PathSpec;
use crate::settings::config_error;

/// A message property a template can insert.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MessageField {
    Id,
    ReceivedAt,
    Channel,
    Connector,
    DataType,
    Peer,
    Device,
    CorrelationId,
    Metadata(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Path {
        path: PathSpec,
        default: Option<String>,
    },
    Variable {
        name: String,
        default: Option<String>,
    },
    Message {
        field: MessageField,
        default: Option<String>,
    },
}

/// Text with placeholders, parsed once when the channel is deployed.
///
/// | Placeholder | Inserts |
/// |---|---|
/// | `{PID-3.1}` | the value at a document path |
/// | `{OBX[*]-5}` | the value in the current wildcard occurrence |
/// | `{$name}` | a variable set by an earlier `store` operation |
/// | `{channel}` | the channel identifier |
/// | `{message.id}` | the message identifier |
/// | `{message.received_at}` | the receive time (RFC 3339, UTC) |
/// | `{message.connector}`, `{message.data_type}`, `{message.peer}`, `{message.device}`, `{message.correlation_id}` | envelope properties |
/// | `{message.metadata.KEY}` | connector metadata |
///
/// Any placeholder may end with `|default`, used when the value is missing
/// or empty: `{PID-8|U}`. Write `{{` and `}}` for literal braces. Missing
/// values without a default insert nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Part>,
}

impl Template {
    /// Parses a template.
    pub fn parse(text: &str) -> Result<Self, EngineError> {
        Self::parse_at("template", text)
    }

    pub(crate) fn parse_at(at: &str, text: &str) -> Result<Self, EngineError> {
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
                '}' => {
                    return Err(config_error(
                        at,
                        format!(
                            "unmatched '}}' in template {text:?}; write '}}}}' for a literal brace"
                        ),
                    ));
                }
                '{' => {
                    let mut inner = String::new();
                    let mut closed = false;
                    for c in chars.by_ref() {
                        if c == '}' {
                            closed = true;
                            break;
                        }
                        if c == '{' {
                            return Err(config_error(
                                at,
                                format!("nested '{{' in template {text:?}"),
                            ));
                        }
                        inner.push(c);
                    }
                    if !closed {
                        return Err(config_error(
                            at,
                            format!("unclosed '{{' in template {text:?}"),
                        ));
                    }
                    if !literal.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut literal)));
                    }
                    parts.push(Self::placeholder(at, &inner)?);
                }
                other => literal.push(other),
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Text(literal));
        }
        Ok(Self { parts })
    }

    fn placeholder(at: &str, inner: &str) -> Result<Part, EngineError> {
        let (name, default) = match inner.split_once('|') {
            Some((name, default)) => (name.trim(), Some(default.to_owned())),
            None => (inner.trim(), None),
        };
        if name.is_empty() {
            return Err(config_error(at, "empty placeholder {}"));
        }
        if let Some(variable) = name.strip_prefix('$') {
            if variable.is_empty() {
                return Err(config_error(
                    at,
                    "a variable placeholder needs a name: {$name}",
                ));
            }
            return Ok(Part::Variable {
                name: variable.to_owned(),
                default,
            });
        }
        let field = match name {
            "channel" | "message.channel" => Some(MessageField::Channel),
            "message.id" => Some(MessageField::Id),
            "message.received_at" => Some(MessageField::ReceivedAt),
            "message.connector" => Some(MessageField::Connector),
            "message.data_type" => Some(MessageField::DataType),
            "message.peer" => Some(MessageField::Peer),
            "message.device" => Some(MessageField::Device),
            "message.correlation_id" => Some(MessageField::CorrelationId),
            other => other
                .strip_prefix("message.metadata.")
                .filter(|key| !key.is_empty())
                .map(|key| MessageField::Metadata(key.to_owned())),
        };
        if let Some(field) = field {
            return Ok(Part::Message { field, default });
        }
        if name.starts_with("message.") {
            return Err(config_error(
                at,
                format!("unknown message property {{{name}}}"),
            ));
        }
        Ok(Part::Path {
            path: PathSpec::parse(at, name)?,
            default,
        })
    }

    /// Whether the template has no placeholders.
    pub fn is_literal(&self) -> bool {
        self.parts.iter().all(|part| matches!(part, Part::Text(_)))
    }

    /// The wildcard prefixes of the paths the template reads.
    pub(crate) fn wildcard_prefixes(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|part| match part {
            Part::Path { path, .. } => path.wildcard_prefix(),
            _ => None,
        })
    }

    /// Renders the template for `context`. `occurrence` selects the current
    /// occurrence of a wildcard path.
    pub fn render(
        &self,
        context: &MessageContext,
        occurrence: Option<usize>,
    ) -> Result<String, StepError> {
        let mut out = String::new();
        for part in &self.parts {
            let (value, default) = match part {
                Part::Text(text) => {
                    out.push_str(text);
                    continue;
                }
                Part::Path { path, default } => {
                    (context.document.get(&path.resolve(occurrence))?, default)
                }
                Part::Variable { name, default } => (context.variables.get(name).cloned(), default),
                Part::Message { field, default } => (message_field(context, field), default),
            };
            match (value, default) {
                (Some(value), _) if !value.is_empty() => out.push_str(&value),
                (_, Some(default)) => out.push_str(default),
                _ => {}
            }
        }
        Ok(out)
    }
}

fn message_field(context: &MessageContext, field: &MessageField) -> Option<String> {
    let envelope = &context.envelope;
    match field {
        MessageField::Id => Some(envelope.id.to_string()),
        MessageField::ReceivedAt => Some(envelope.received_at.to_string()),
        MessageField::Channel => Some(envelope.channel.to_string()),
        MessageField::Connector => Some(envelope.connector.to_string()),
        MessageField::DataType => Some(envelope.data_type.to_string()),
        MessageField::Peer => envelope.peer.clone(),
        MessageField::Device => envelope.device.as_ref().map(ToString::to_string),
        MessageField::CorrelationId => envelope.correlation_id.clone(),
        MessageField::Metadata(key) => envelope.metadata.get(key).cloned(),
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::context;

    use super::*;

    #[test]
    fn renders_paths_variables_and_message_fields() {
        let mut ctx = context(b"MSH|^~\\&|LAB|HOSP\rPID|1||12345^^^HOSP||Doe^Jane\rOBX|1|NM|GLU||5.4\rOBX|2|NM|HGB||13.2\r");
        ctx.variables.insert("site".into(), "Ankara".into());
        ctx.envelope.metadata.insert("port".into(), "5100".into());
        let template = Template::parse(
            "{PID-5.2} {PID-5.1} ({PID-3.1}) at {$site} via {channel}/{message.metadata.port}, sex {PID-8|U}, {{literal}}",
        )
        .unwrap();
        assert_eq!(
            template.render(&ctx, None).unwrap(),
            "Jane Doe (12345) at Ankara via lab/5100, sex U, {literal}"
        );
        let per_result = Template::parse("{OBX[*]-3}={OBX[*]-5}").unwrap();
        assert_eq!(per_result.render(&ctx, Some(2)).unwrap(), "HGB=13.2");
        assert_eq!(
            per_result.wildcard_prefixes().collect::<Vec<_>>(),
            ["OBX", "OBX"]
        );
        let id = Template::parse("{message.id}")
            .unwrap()
            .render(&ctx, None)
            .unwrap();
        assert_eq!(id.len(), 26);
        assert!(Template::parse("plain").unwrap().is_literal());
    }

    #[test]
    fn rejects_malformed_templates() {
        for bad in [
            "{",
            "}",
            "{PID-5",
            "{}",
            "{$}",
            "{a{b}}",
            "{message.unknown}",
        ] {
            assert!(Template::parse(bad).is_err(), "{bad:?}");
        }
    }
}
