//! Settings shared by the database connectors: secrets, the polling reader
//! and the writer.

use std::collections::BTreeMap;
use std::time::Duration;

use oxim_connectors::values::ValueSource;
use oxim_core::config::DurationText;
use oxim_core::{EngineError, Settings};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::sql::{Placeholder, Prepared, prepare};

/// Deserializes settings into a typed struct.
pub(crate) fn parse<T: DeserializeOwned>(settings: Settings, what: &str) -> Result<T, EngineError> {
    serde_json::from_value(serde_json::Value::Object(settings))
        .map_err(|e| EngineError::Config(format!("{what} settings: {e}")))
}

/// Splits settings into the connection part (the keys in `keys`) and the
/// rest, so both can reject unknown keys.
pub(crate) fn split(settings: &Settings, keys: &[&str]) -> (Settings, Settings) {
    let mut connection = Settings::new();
    let mut rest = Settings::new();
    for (key, value) in settings {
        if keys.contains(&key.as_str()) {
            connection.insert(key.clone(), value.clone());
        } else {
            rest.insert(key.clone(), value.clone());
        }
    }
    (connection, rest)
}

/// A password given inline or through an environment variable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Secret {
    inline: Option<String>,
    env: Option<String>,
}

impl Secret {
    /// A secret from the `password` and `password_env` settings.
    pub(crate) fn new(inline: Option<String>, env: Option<String>) -> Result<Self, EngineError> {
        if inline.is_some() && env.is_some() {
            return Err(EngineError::Config(
                "set either password or password_env, not both".into(),
            ));
        }
        Ok(Self { inline, env })
    }

    /// The password, read when a connection is opened so a service can get
    /// it from its environment; `None` when no password is configured.
    pub(crate) fn resolve(&self) -> Result<Option<String>, String> {
        match (&self.inline, &self.env) {
            (Some(value), _) => Ok(Some(value.clone())),
            (None, Some(name)) => std::env::var(name).map(Some).map_err(|_| {
                format!("the environment variable {name} holding the password is not set")
            }),
            (None, None) => Ok(None),
        }
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.env {
            Some(name) => write!(f, "${name}"),
            None if self.inline.is_some() => f.write_str("(inline)"),
            None => f.write_str("(none)"),
        }
    }
}

/// A constant reader parameter.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub(crate) enum Scalar {
    /// Text.
    Text(String),
    /// A number.
    Number(serde_json::Number),
    /// A boolean.
    Bool(bool),
}

impl Scalar {
    fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Number(number) => number.to_string(),
            Self::Bool(flag) => flag.to_string(),
        }
    }
}

fn default_interval() -> DurationText {
    DurationText(Duration::from_secs(5))
}

fn default_max_rows() -> usize {
    500
}

/// Settings of a polling reader (source).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReaderSettings {
    /// The query that selects new rows.
    pub(crate) query: String,
    /// Constant values of the query's parameters.
    #[serde(default)]
    pub(crate) params: BTreeMap<String, Scalar>,
    /// Runs after each row is stored, with the row's columns (and the
    /// constants) as parameters; typically marks the row as processed.
    #[serde(default)]
    pub(crate) post_query: Option<String>,
    /// Time between polls.
    #[serde(default = "default_interval")]
    pub(crate) interval: DurationText,
    /// Most rows taken per poll.
    #[serde(default = "default_max_rows")]
    pub(crate) max_rows: usize,
    /// When set, the message is the raw content of this column instead of
    /// the row as JSON.
    #[serde(default)]
    pub(crate) column: Option<String>,
}

/// A prepared polling reader.
#[derive(Debug, Clone)]
pub(crate) struct Reader {
    pub(crate) query: Prepared,
    pub(crate) query_params: Vec<crate::value::Param>,
    pub(crate) post_query: Option<Prepared>,
    pub(crate) constants: BTreeMap<String, String>,
    pub(crate) interval: Duration,
    pub(crate) max_rows: usize,
    pub(crate) column: Option<String>,
}

impl Reader {
    /// Prepares the statements for a database's placeholders.
    pub(crate) fn new(settings: ReaderSettings, style: Placeholder) -> Result<Self, EngineError> {
        let config = |what: &str, e: String| EngineError::Config(format!("{what}: {e}"));
        let query = prepare(&settings.query, style).map_err(|e| config("query", e))?;
        let constants: BTreeMap<String, String> = settings
            .params
            .iter()
            .map(|(name, value)| (name.clone(), value.text()))
            .collect();
        let query_params = query
            .names
            .iter()
            .map(|name| {
                constants
                    .get(name)
                    .map(|value| crate::value::Param::Text(value.clone()))
                    .ok_or_else(|| {
                        EngineError::Config(format!(
                            "query parameter :{name} has no value in params"
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let post_query = settings
            .post_query
            .as_deref()
            .map(|sql| prepare(sql, style).map_err(|e| config("post_query", e)))
            .transpose()?;
        if settings.max_rows == 0 {
            return Err(EngineError::Config("max_rows must be at least 1".into()));
        }
        if settings.interval.0 < Duration::from_millis(100) {
            return Err(EngineError::Config(
                "interval must be at least 100ms".into(),
            ));
        }
        Ok(Self {
            query,
            query_params,
            post_query,
            constants,
            interval: settings.interval.0,
            max_rows: settings.max_rows,
            column: settings.column,
        })
    }
}

/// Where a writer parameter comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParamSource {
    /// A value of the delivery (see [`ValueSource`]).
    Value(ValueSource),
    /// The delivered bytes as binary data (`$payload_bytes`).
    PayloadBytes,
}

impl<'de> Deserialize<'de> for ParamSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.as_str().map(str::trim) == Some("$payload_bytes") {
            return Ok(Self::PayloadBytes);
        }
        serde_json::from_value(value)
            .map(Self::Value)
            .map_err(serde::de::Error::custom)
    }
}

/// One or several statements.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub(crate) enum Statements {
    /// One statement.
    One(String),
    /// Several statements, run in order in one transaction.
    Many(Vec<String>),
}

/// Settings of a writer (destination).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriterSettings {
    /// The statement (or statements) run for each delivery.
    pub(crate) statement: Statements,
    /// Values of the statement parameters.
    #[serde(default)]
    pub(crate) params: BTreeMap<String, ParamSource>,
}

/// A prepared writer.
#[derive(Debug, Clone)]
pub(crate) struct Writer {
    pub(crate) statements: Vec<Prepared>,
    pub(crate) params: BTreeMap<String, ParamSource>,
}

impl Writer {
    /// Prepares the statements and checks that every parameter has a
    /// source and every source is used.
    pub(crate) fn new(settings: WriterSettings, style: Placeholder) -> Result<Self, EngineError> {
        let sql = match settings.statement {
            Statements::One(sql) => vec![sql],
            Statements::Many(list) => list,
        };
        if sql.is_empty() {
            return Err(EngineError::Config("statement must not be empty".into()));
        }
        let statements = sql
            .iter()
            .map(|sql| {
                prepare(sql, style).map_err(|e| EngineError::Config(format!("statement: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for name in statements.iter().flat_map(|s| &s.names) {
            if !settings.params.contains_key(name) {
                return Err(EngineError::Config(format!(
                    "statement parameter :{name} has no source in params"
                )));
            }
        }
        for name in settings.params.keys() {
            if !statements.iter().any(|s| s.names.contains(name)) {
                return Err(EngineError::Config(format!(
                    "params.{name} is not used by the statement"
                )));
            }
        }
        Ok(Self {
            statements,
            params: settings.params,
        })
    }
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
    fn prepares_readers_and_writers() {
        let reader: ReaderSettings = parse(
            settings(serde_json::json!({
                "query": "SELECT * FROM results WHERE site = :site AND done = false",
                "params": {"site": "LAB1"},
                "post_query": "UPDATE results SET done = true WHERE id = :id",
            })),
            "reader",
        )
        .unwrap();
        let reader = Reader::new(reader, Placeholder::Dollar).unwrap();
        assert_eq!(
            reader.query_params,
            [crate::value::Param::Text("LAB1".into())]
        );
        assert_eq!(reader.post_query.unwrap().names, ["id"]);

        let missing: ReaderSettings = parse(
            settings(serde_json::json!({"query": "SELECT :x"})),
            "reader",
        )
        .unwrap();
        assert!(Reader::new(missing, Placeholder::Dollar).is_err());

        let writer: WriterSettings = parse(
            settings(serde_json::json!({
                "statement": ["INSERT INTO m (id, body) VALUES (:id, :body)", "UPDATE c SET n = n + 1 WHERE k = :kind"],
                "params": {"id": "$message_id", "body": "$payload_bytes", "kind": {"value": "lab"}},
            })),
            "writer",
        )
        .unwrap();
        let writer = Writer::new(writer, Placeholder::Question).unwrap();
        assert_eq!(writer.statements.len(), 2);
        assert_eq!(writer.params["body"], ParamSource::PayloadBytes);
        assert_eq!(
            writer.params["kind"],
            ParamSource::Value(ValueSource::Constant("lab".into()))
        );

        for bad in [
            serde_json::json!({"statement": "INSERT INTO m VALUES (:a)"}),
            serde_json::json!({"statement": "INSERT INTO m VALUES (1)", "params": {"a": "$payload"}}),
            serde_json::json!({"statement": [], "params": {}}),
        ] {
            let writer: WriterSettings = parse(settings(bad.clone()), "writer").unwrap();
            assert!(Writer::new(writer, Placeholder::Dollar).is_err(), "{bad}");
        }
    }

    #[test]
    fn splits_and_resolves_secrets() {
        let all = settings(serde_json::json!({"host": "db", "query": "SELECT 1"}));
        let (connection, rest) = split(&all, &["host"]);
        assert_eq!(connection.len(), 1);
        assert!(rest.contains_key("query"));
        assert!(Secret::new(Some("a".into()), Some("B".into())).is_err());
        let secret = Secret::new(None, Some("OXIM_TEST_UNSET_DB_PASSWORD_9C1".into())).unwrap();
        assert!(
            secret
                .resolve()
                .unwrap_err()
                .contains("OXIM_TEST_UNSET_DB_PASSWORD_9C1")
        );
        assert_eq!(secret.to_string(), "$OXIM_TEST_UNSET_DB_PASSWORD_9C1");
        assert_eq!(Secret::default().resolve(), Ok(None));
    }
}
