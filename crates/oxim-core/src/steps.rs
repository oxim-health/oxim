//! Built-in steps that work on any parsed document through paths.
//!
//! | Type | Kind | Settings |
//! |---|---|---|
//! | `path-equals` | filter | `path`, `value`, optional `negate` |
//! | `path-in` | filter | `path`, `values` (list), optional `negate` |
//! | `path-exists` | filter | `path`, optional `negate` |
//! | `set` | transformer | `path`, `value` |
//! | `copy` | transformer | `from`, `to` |
//! | `clinical-kind` | filter | `kinds` (list of `results`, `orders`, `query`, `quality_control`, `device_event`), optional `negate` |
//! | `passthrough` | encoder | none |
//!
//! Richer mapping, code tables and scripts are provided by other crates.

use std::sync::Arc;

use crate::config::StepConfig;
use crate::error::{EngineError, StepError};
use crate::pipeline::{Encoder, Filter, MessageContext, PassthroughEncoder, Transformer};
use crate::registry::Registry;

fn negate(step: &StepConfig) -> bool {
    step.settings
        .get("negate")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Keeps messages whose value at `path` equals one of `values`.
#[derive(Debug, Clone)]
pub struct PathIn {
    path: String,
    values: Vec<String>,
    negate: bool,
}

impl Filter for PathIn {
    fn accept(&self, context: &MessageContext) -> Result<bool, StepError> {
        let value = context.document.get(&self.path)?;
        let matched = value.is_some_and(|value| self.values.contains(&value));
        Ok(matched != self.negate)
    }
}

/// Keeps messages that have a non-empty value at `path`.
#[derive(Debug, Clone)]
pub struct PathExists {
    path: String,
    negate: bool,
}

impl Filter for PathExists {
    fn accept(&self, context: &MessageContext) -> Result<bool, StepError> {
        let exists = context
            .document
            .get(&self.path)?
            .is_some_and(|value| !value.is_empty());
        Ok(exists != self.negate)
    }
}

/// Keeps messages whose normalized clinical content is one of `kinds`, for
/// example to route quality control results to a QC destination. Messages
/// without normalized content never match.
#[derive(Debug, Clone)]
pub struct ClinicalKind {
    kinds: Vec<String>,
    negate: bool,
}

/// The configuration name of a normalized content kind.
pub fn clinical_kind_name(content: &oxim_model::ClinicalContent) -> &'static str {
    use oxim_model::ClinicalContent;
    match content {
        ClinicalContent::Results { .. } => "results",
        ClinicalContent::Orders { .. } => "orders",
        ClinicalContent::Query { .. } => "query",
        ClinicalContent::QualityControl { .. } => "quality_control",
        ClinicalContent::DeviceEvent { .. } => "device_event",
    }
}

impl Filter for ClinicalKind {
    fn accept(&self, context: &MessageContext) -> Result<bool, StepError> {
        let matched = context.clinical.as_ref().is_some_and(|content| {
            self.kinds
                .iter()
                .any(|kind| kind == clinical_kind_name(content))
        });
        Ok(matched != self.negate)
    }
}

/// Writes a constant value.
#[derive(Debug, Clone)]
pub struct SetValue {
    path: String,
    value: String,
}

impl Transformer for SetValue {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        context.document.set(&self.path, &self.value)
    }
}

/// Copies a value from one path to another. A missing source writes an
/// empty value.
#[derive(Debug, Clone)]
pub struct CopyValue {
    from: String,
    to: String,
}

impl Transformer for CopyValue {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let value = context.document.get(&self.from)?.unwrap_or_default();
        context.document.set(&self.to, &value)
    }
}

fn text_list(step: &StepConfig, key: &str) -> Result<Vec<String>, EngineError> {
    step.settings
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| match value {
                    serde_json::Value::String(text) => Ok(text.clone()),
                    other => Ok(other.to_string()),
                })
                .collect()
        })
        .unwrap_or_else(|| {
            Err(EngineError::Config(format!(
                "step {:?} needs a list setting {key:?}",
                step.kind
            )))
        })
}

fn setting_text(step: &StepConfig, key: &str) -> Result<String, EngineError> {
    match step.settings.get(key) {
        Some(serde_json::Value::String(text)) => Ok(text.clone()),
        Some(serde_json::Value::Number(number)) => Ok(number.to_string()),
        Some(serde_json::Value::Bool(value)) => Ok(value.to_string()),
        _ => step.text(key).map(str::to_owned),
    }
}

/// Registers the built-in steps.
pub(crate) fn register(registry: &mut Registry) {
    registry
        .add_filter("path-equals", |step| {
            Ok(Arc::new(PathIn {
                path: step.text("path")?.to_owned(),
                values: vec![setting_text(step, "value")?],
                negate: negate(step),
            }) as Arc<dyn Filter>)
        })
        .add_filter("path-in", |step| {
            Ok(Arc::new(PathIn {
                path: step.text("path")?.to_owned(),
                values: text_list(step, "values")?,
                negate: negate(step),
            }) as Arc<dyn Filter>)
        })
        .add_filter("path-exists", |step| {
            Ok(Arc::new(PathExists {
                path: step.text("path")?.to_owned(),
                negate: negate(step),
            }) as Arc<dyn Filter>)
        })
        .add_filter("clinical-kind", |step| {
            const KNOWN: [&str; 5] = [
                "results",
                "orders",
                "query",
                "quality_control",
                "device_event",
            ];
            let kinds = text_list(step, "kinds")?;
            if let Some(unknown) = kinds.iter().find(|kind| !KNOWN.contains(&kind.as_str())) {
                return Err(EngineError::Config(format!(
                    "unknown clinical content kind {unknown:?}; use one of {}",
                    KNOWN.join(", ")
                )));
            }
            Ok(Arc::new(ClinicalKind {
                kinds,
                negate: negate(step),
            }) as Arc<dyn Filter>)
        })
        .add_transformer("set", |step| {
            Ok(Arc::new(SetValue {
                path: step.text("path")?.to_owned(),
                value: setting_text(step, "value")?,
            }) as Arc<dyn Transformer>)
        })
        .add_transformer("copy", |step| {
            Ok(Arc::new(CopyValue {
                from: step.text("from")?.to_owned(),
                to: step.text("to")?.to_owned(),
            }) as Arc<dyn Transformer>)
        })
        .add_encoder("passthrough", |_| {
            Ok(Arc::new(PassthroughEncoder) as Arc<dyn Encoder>)
        });
}
