//! Reading step settings with precise configuration errors.

use serde_json::{Map, Value};

use oxim_core::EngineError;

/// A JSON object from a step configuration, read key by key.
///
/// Every object is checked against the list of keys it may contain, so a
/// typo such as `equal:` instead of `equals:` is reported when the channel
/// is deployed instead of being silently ignored.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Obj<'a> {
    /// Where the object sits, for error messages (for example `map.operations[2].set`).
    pub(crate) at: &'a str,
    pub(crate) map: &'a Map<String, Value>,
}

/// A configuration error at `at`.
pub(crate) fn config_error(at: &str, message: impl std::fmt::Display) -> EngineError {
    EngineError::Config(format!("{at}: {message}"))
}

impl<'a> Obj<'a> {
    /// Interprets `value` as an object.
    pub(crate) fn new(at: &'a str, value: &'a Value) -> Result<Self, EngineError> {
        match value {
            Value::Object(map) => Ok(Self { at, map }),
            _ => Err(config_error(at, "expected a mapping")),
        }
    }

    /// Wraps an existing map.
    pub(crate) fn from_map(at: &'a str, map: &'a Map<String, Value>) -> Self {
        Self { at, map }
    }

    /// Fails when the object has keys outside `allowed`.
    pub(crate) fn only(&self, allowed: &[&str]) -> Result<(), EngineError> {
        for key in self.map.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(config_error(
                    self.at,
                    format!(
                        "unknown setting {key:?}; expected one of {}",
                        allowed.join(", ")
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Whether `key` is present.
    pub(crate) fn has(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// The raw value of `key`.
    pub(crate) fn value(&self, key: &str) -> Option<&'a Value> {
        self.map.get(key)
    }

    /// A scalar setting as text. Numbers and booleans are accepted and
    /// written as YAML would show them; quote values whose exact text
    /// matters, such as `"5.40"`.
    pub(crate) fn text(&self, key: &str) -> Result<Option<String>, EngineError> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => scalar_text(value)
                .map(Some)
                .ok_or_else(|| config_error(self.at, format!("{key:?} must be a single value"))),
        }
    }

    /// A required scalar setting as text.
    pub(crate) fn required_text(&self, key: &str) -> Result<String, EngineError> {
        self.text(key)?
            .ok_or_else(|| config_error(self.at, format!("missing setting {key:?}")))
    }

    /// A list of scalar settings as text.
    pub(crate) fn text_list(&self, key: &str) -> Result<Vec<String>, EngineError> {
        match self.map.get(key) {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    scalar_text(item).ok_or_else(|| {
                        config_error(self.at, format!("{key:?} must list single values"))
                    })
                })
                .collect(),
            Some(_) => Err(config_error(self.at, format!("{key:?} must be a list"))),
            None => Err(config_error(self.at, format!("missing setting {key:?}"))),
        }
    }

    /// A boolean setting.
    pub(crate) fn bool(&self, key: &str) -> Result<Option<bool>, EngineError> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(config_error(
                self.at,
                format!("{key:?} must be true or false"),
            )),
        }
    }

    /// A non-negative integer setting.
    pub(crate) fn usize(&self, key: &str) -> Result<Option<usize>, EngineError> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
                .and_then(|n| usize::try_from(n).ok())
                .map(Some)
                .ok_or_else(|| config_error(self.at, format!("{key:?} must be a whole number"))),
        }
    }
}

/// The text of a scalar JSON value.
pub(crate) fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}
