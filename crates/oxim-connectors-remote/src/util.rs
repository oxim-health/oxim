//! Helpers shared by the connectors of this crate.

use oxim_connectors::file::extension;
use oxim_core::{EngineError, Settings};
use oxim_model::{ClinicalDateTime, DataType, MessageId, Timestamp};
use serde::de::DeserializeOwned;

/// Deserializes connector settings into a typed struct.
pub(crate) fn parse<T: DeserializeOwned>(
    settings: &Settings,
    what: &str,
) -> Result<T, EngineError> {
    serde_json::from_value(serde_json::Value::Object(settings.clone()))
        .map_err(|e| EngineError::Config(format!("{what} settings: {e}")))
}

/// Splits settings into the entries named in `keys` and the rest, so two
/// groups of settings can each be checked strictly.
pub(crate) fn split(settings: &Settings, keys: &[&str]) -> (Settings, Settings) {
    let mut selected = Settings::new();
    let mut rest = Settings::new();
    for (key, value) in settings {
        if keys.contains(&key.as_str()) {
            selected.insert(key.clone(), value.clone());
        } else {
            rest.insert(key.clone(), value.clone());
        }
    }
    (selected, rest)
}

/// Reads a secret given inline or through an environment variable, when
/// the connector connects (so validating channel files does not need it).
pub(crate) fn secret(
    inline: Option<&String>,
    env: Option<&String>,
    what: &str,
) -> Result<Option<String>, String> {
    match (inline, env) {
        (Some(value), None) => Ok(Some(value.clone())),
        (None, Some(name)) => std::env::var(name)
            .map(Some)
            .map_err(|_| format!("the environment variable {name} holding the {what} is not set")),
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(format!(
            "set either the {what} or its environment variable, not both"
        )),
    }
}

/// Checks at deploy time that at most one of an inline secret and its
/// environment variable is set.
pub(crate) fn check_secret(
    inline: Option<&String>,
    env: Option<&String>,
    what: &str,
    connector: &str,
) -> Result<(), EngineError> {
    if inline.is_some() && env.is_some() {
        return Err(EngineError::Config(format!(
            "{connector}: set either the {what} or its environment variable, not both"
        )));
    }
    Ok(())
}

/// Joins a remote directory and a name with `/`.
pub(crate) fn join(directory: &str, name: &str) -> String {
    let directory = directory.trim_end_matches('/');
    if directory.is_empty() || directory == "." {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    }
}

/// An instant as `YYYYMMDDTHHMMSSZ` (UTC), for signatures and file names.
pub(crate) fn compact_utc(timestamp: Timestamp) -> String {
    ClinicalDateTime::from_timestamp(timestamp, 0)
        .map(|value| {
            format!(
                "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
                value.year(),
                value.month().unwrap_or(1),
                value.day().unwrap_or(1),
                value.hour().unwrap_or(0),
                value.minute().unwrap_or(0),
                value.second().unwrap_or(0)
            )
        })
        .unwrap_or_else(|| "19700101T000000Z".to_owned())
}

/// An instant as ISO 8601 UTC with seconds: `2026-09-29T12:00:00Z`.
pub(crate) fn iso_utc(timestamp: Timestamp) -> String {
    ClinicalDateTime::from_timestamp(timestamp, 0)
        .map(|value| {
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                value.year(),
                value.month().unwrap_or(1),
                value.day().unwrap_or(1),
                value.hour().unwrap_or(0),
                value.minute().unwrap_or(0),
                value.second().unwrap_or(0)
            )
        })
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

/// The current time.
pub(crate) fn now() -> Timestamp {
    Timestamp::from_system_time(std::time::SystemTime::now())
        .unwrap_or_else(|| Timestamp::from_unix_nanos(0))
}

/// Escapes text for XML content and attribute values.
pub(crate) fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Renders a text template with `{channel}`, `{destination}`,
/// `{message_id}`, `{timestamp}` (UTC receive time, `20260929T120000Z`) and
/// `{extension}` (from the data type).
pub(crate) fn render_text(
    template: &str,
    channel: &str,
    destination: &str,
    id: MessageId,
    data_type: Option<DataType>,
) -> Result<String, String> {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            return Err(format!("unclosed placeholder in {template:?}"));
        };
        let value = match &rest[open + 1..open + close] {
            "channel" => channel.to_owned(),
            "destination" => destination.to_owned(),
            "message_id" => id.to_string(),
            "timestamp" => {
                let millis = i64::try_from(id.timestamp_ms()).unwrap_or_default();
                compact_utc(
                    Timestamp::from_unix_millis(millis)
                        .unwrap_or_else(|| Timestamp::from_unix_nanos(0)),
                )
            }
            "extension" => extension(data_type).to_owned(),
            other => return Err(format!("unknown placeholder {{{other}}} in {template:?}")),
        };
        out.push_str(&value);
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_remote_paths() {
        assert_eq!(join("", "a.hl7"), "a.hl7");
        assert_eq!(join(".", "a.hl7"), "a.hl7");
        assert_eq!(join("in/", "a.hl7"), "in/a.hl7");
        assert_eq!(join("/data/in", "a.hl7"), "/data/in/a.hl7");
    }

    #[test]
    fn splits_settings() {
        let settings: Settings =
            serde_json::from_str(r#"{"host":"h","directory":"in","pattern":"*.hl7"}"#).unwrap();
        let (poll, rest) = split(&settings, &["directory", "pattern"]);
        assert_eq!(poll.len(), 2);
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn formats_times() {
        let ts = Timestamp::from_unix_millis(1_790_067_600_250).unwrap();
        assert_eq!(compact_utc(ts), "20260922T090000Z");
        assert_eq!(iso_utc(ts), "2026-09-22T09:00:00Z");
    }

    #[test]
    fn renders_text_templates() {
        let id = MessageId::from_parts(1_790_067_600_000, 7);
        assert_eq!(
            render_text(
                "Result {message_id} ({channel}/{destination}) {timestamp}.{extension}",
                "lab",
                "lis",
                id,
                Some(DataType::Hl7V2)
            )
            .unwrap(),
            format!("Result {id} (lab/lis) 20260922T090000Z.hl7")
        );
        assert!(render_text("{nope}", "a", "b", id, None).is_err());
        assert!(render_text("{channel", "a", "b", id, None).is_err());
    }

    #[test]
    fn escapes_xml() {
        assert_eq!(
            xml_escape("a<b & \"c\"'"),
            "a&lt;b &amp; &quot;c&quot;&apos;"
        );
    }

    #[test]
    fn reads_secrets() {
        assert_eq!(
            secret(Some(&"x".to_owned()), None, "password").unwrap(),
            Some("x".to_owned())
        );
        assert_eq!(secret(None, None, "password").unwrap(), None);
        assert!(secret(None, Some(&"OXIM_TEST_UNSET_4A1B".to_owned()), "password").is_err());
        assert!(secret(Some(&"x".to_owned()), Some(&"Y".to_owned()), "password").is_err());
    }
}
