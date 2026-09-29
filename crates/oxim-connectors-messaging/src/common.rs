//! Helpers shared by the messaging connectors.

use oxim_core::{EngineError, Settings};
use serde::de::DeserializeOwned;

/// Deserializes connector settings into a typed struct.
pub(crate) fn parse<T: DeserializeOwned>(
    settings: &Settings,
    what: &str,
) -> Result<T, EngineError> {
    serde_json::from_value(serde_json::Value::Object(settings.clone()))
        .map_err(|e| EngineError::Config(format!("{what} settings: {e}")))
}

/// A secret given inline or through an environment variable, read when a
/// connection opens so a service can take it from its environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Secret {
    inline: Option<String>,
    env: Option<String>,
    what: &'static str,
}

impl Secret {
    /// A secret from `<what>` and `<what>_env` settings.
    pub(crate) fn new(
        inline: Option<String>,
        env: Option<String>,
        what: &'static str,
    ) -> Result<Self, EngineError> {
        if inline.is_some() && env.is_some() {
            return Err(EngineError::Config(format!(
                "set either {what} or {what}_env, not both"
            )));
        }
        Ok(Self { inline, env, what })
    }

    /// Whether a secret is configured.
    pub(crate) fn is_set(&self) -> bool {
        self.inline.is_some() || self.env.is_some()
    }

    /// The secret, or `None` when not configured.
    pub(crate) fn resolve(&self) -> Result<Option<String>, String> {
        match (&self.inline, &self.env) {
            (Some(value), _) => Ok(Some(value.clone())),
            (None, Some(name)) => std::env::var(name).map(Some).map_err(|_| {
                format!(
                    "the environment variable {name} holding the {} is not set",
                    self.what
                )
            }),
            (None, None) => Ok(None),
        }
    }
}

/// Header and property values usable as message metadata: printable text
/// only, so binary header values do not end up in logs and the UI.
pub(crate) fn metadata_text(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes)
        .ok()
        .filter(|text| text.len() <= 1024 && !text.chars().any(char::is_control))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_metadata() {
        assert!(Secret::new(Some("a".into()), Some("B".into()), "password").is_err());
        let secret = Secret::new(
            None,
            Some("OXIM_TEST_UNSET_MQ_SECRET_41".into()),
            "password",
        )
        .unwrap();
        assert!(secret.is_set());
        assert!(
            secret
                .resolve()
                .unwrap_err()
                .contains("OXIM_TEST_UNSET_MQ_SECRET_41")
        );
        assert_eq!(Secret::default().resolve(), Ok(None));
        assert_eq!(metadata_text(b"LAB1").as_deref(), Some("LAB1"));
        assert_eq!(metadata_text(&[0xff]), None);
        assert_eq!(metadata_text(b"a\nb"), None);
    }
}
