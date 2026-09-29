//! Script step settings.

use std::sync::Arc;
use std::time::Duration;

use oxim_core::config::DurationText;
use oxim_core::{EngineError, StepConfig};
use oxim_model::DataType;
use serde_json::Value;

use crate::environment::ScriptEnvironment;

/// Default time a script may run per message.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);
/// Default heap limit of a script's runtime.
pub(crate) const DEFAULT_MEMORY_LIMIT: usize = 32 * 1024 * 1024;
/// Default stack limit of a script.
pub(crate) const DEFAULT_MAX_STACK: usize = 512 * 1024;
const MIN_MEMORY_LIMIT: usize = 1024 * 1024;
const MIN_MAX_STACK: usize = 64 * 1024;
/// Scripts run on engine threads; a larger JavaScript stack could exceed
/// the thread's own stack.
const MAX_MAX_STACK: usize = 1024 * 1024;

/// What a script step does with its script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Filter,
    Transformer,
    Encoder,
}

/// Parsed settings of one script step.
#[derive(Debug, Clone)]
pub(crate) struct ScriptSettings {
    /// The file name, or `inline`, used in error positions.
    pub(crate) name: String,
    pub(crate) source: Arc<str>,
    pub(crate) timeout: Duration,
    pub(crate) memory_limit: usize,
    pub(crate) max_stack: usize,
    pub(crate) mirth: bool,
    /// For encoders: the data type of the produced bytes.
    pub(crate) data_type: Option<DataType>,
}

fn config_error(step: &StepConfig, message: impl std::fmt::Display) -> EngineError {
    EngineError::Config(format!("step {:?}: {message}", step.kind))
}

/// A size in bytes: a number, or text such as `64MiB`, `512KiB`, `1GiB`.
fn size(step: &StepConfig, key: &str, value: &Value) -> Result<usize, EngineError> {
    let invalid = || {
        config_error(
            step,
            format!("{key} must be a number of bytes or a size such as 512KiB or 64MiB"),
        )
    };
    match value {
        Value::Number(number) => number
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(invalid),
        Value::String(text) => {
            let text = text.trim();
            let digits = text.bytes().take_while(u8::is_ascii_digit).count();
            let number: usize = text[..digits].parse().map_err(|_| invalid())?;
            let unit = match text[digits..].trim() {
                "" | "B" => 1,
                "KiB" | "KB" | "K" => 1024,
                "MiB" | "MB" | "M" => 1024 * 1024,
                "GiB" | "GB" | "G" => 1024 * 1024 * 1024,
                _ => return Err(invalid()),
            };
            number.checked_mul(unit).ok_or_else(invalid)
        }
        _ => Err(invalid()),
    }
}

impl ScriptSettings {
    /// Reads the settings of `step`.
    pub(crate) fn from_step(
        step: &StepConfig,
        environment: &ScriptEnvironment,
        kind: Kind,
    ) -> Result<Self, EngineError> {
        let mut settings = Self {
            name: "inline".into(),
            source: Arc::from(""),
            timeout: DEFAULT_TIMEOUT,
            memory_limit: DEFAULT_MEMORY_LIMIT,
            max_stack: DEFAULT_MAX_STACK,
            mirth: false,
            data_type: None,
        };
        let mut source = None;
        let mut file = None;
        for (key, value) in &step.settings {
            match key.as_str() {
                "source" => {
                    source = Some(
                        value
                            .as_str()
                            .ok_or_else(|| config_error(step, "source must be text"))?,
                    );
                }
                "file" => {
                    file = Some(
                        value
                            .as_str()
                            .ok_or_else(|| config_error(step, "file must be text"))?,
                    );
                }
                "timeout" => {
                    let text = value
                        .as_str()
                        .ok_or_else(|| config_error(step, "timeout must be text such as 1s"))?;
                    let DurationText(timeout) = text
                        .parse::<DurationText>()
                        .map_err(|e| config_error(step, e))?;
                    if timeout.is_zero() {
                        return Err(config_error(step, "timeout must be longer than zero"));
                    }
                    settings.timeout = timeout;
                }
                "memory_limit" => {
                    settings.memory_limit = size(step, key, value)?;
                    if settings.memory_limit < MIN_MEMORY_LIMIT {
                        return Err(config_error(step, "memory_limit must be at least 1MiB"));
                    }
                }
                "max_stack" => {
                    settings.max_stack = size(step, key, value)?;
                    if !(MIN_MAX_STACK..=MAX_MAX_STACK).contains(&settings.max_stack) {
                        return Err(config_error(
                            step,
                            "max_stack must be between 64KiB and 1MiB",
                        ));
                    }
                }
                "mirth" => {
                    settings.mirth = value
                        .as_bool()
                        .ok_or_else(|| config_error(step, "mirth must be true or false"))?;
                }
                "data_type" if kind == Kind::Encoder => {
                    settings.data_type =
                        Some(serde_json::from_value(value.clone()).map_err(|_| {
                            config_error(step, format!("unknown data_type {value}"))
                        })?);
                }
                other => {
                    return Err(config_error(step, format!("unknown setting {other:?}")));
                }
            }
        }
        match (source, file) {
            (Some(source), None) => settings.source = Arc::from(source),
            (None, Some(file)) => {
                settings.source = environment.load(file)?;
                settings.name = file.to_owned();
            }
            _ => {
                return Err(config_error(
                    step,
                    "set either source (the script) or file (a script in the scripts directory)",
                ));
            }
        }
        Ok(settings)
    }
}
