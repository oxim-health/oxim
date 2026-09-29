//! The normalizer and encoders registered with the OXIM engine.

use std::collections::BTreeMap;
use std::sync::Arc;

use oxim_core::{
    Document, Encoded, Encoder, EngineError, MessageContext, Normalizer, Registry, StepConfig,
    StepError,
};
use oxim_model::{ClinicalContent, DataType};
use serde_json::Value;

use crate::error::FhirError;
use crate::from_fhir::normalize;
use crate::resources::{Bundle, Resource};
use crate::to_fhir::{BundleType, FhirEncoding, encode_bundle};

fn step_error(step: &str) -> impl Fn(FhirError) -> StepError + '_ {
    move |e| StepError::new(step, e.to_string())
}

/// Normalizes FHIR JSON (a Bundle, Observation, DiagnosticReport or
/// ServiceRequest); see [`normalize`].
#[derive(Debug, Clone, Copy, Default)]
pub struct FhirNormalizer;

impl Normalizer for FhirNormalizer {
    fn normalize(&self, document: &Document) -> Result<ClinicalContent, StepError> {
        let resource =
            Resource::from_json(&document.to_bytes()).map_err(step_error("normalize"))?;
        normalize(&resource).map_err(step_error("normalize"))
    }
}

fn clinical<'a>(
    context: &'a MessageContext,
    encoder: &str,
) -> Result<&'a ClinicalContent, StepError> {
    context.clinical.as_ref().ok_or_else(|| {
        StepError::new(
            encoder,
            "the channel does not normalize messages (set normalize: true)",
        )
    })
}

fn bundle(
    context: &MessageContext,
    settings: &FhirEncoding,
    step: &str,
) -> Result<Bundle, StepError> {
    encode_bundle(
        clinical(context, step)?,
        settings,
        context.envelope.id,
        context.envelope.received_at,
    )
    .map_err(step_error(step))
}

fn to_json(resource: &Resource, pretty: bool, step: &str) -> Result<Vec<u8>, StepError> {
    if pretty {
        resource.to_json_pretty()
    } else {
        resource.to_json()
    }
    .map_err(step_error(step))
}

/// Encodes normalized content as a FHIR Bundle (`application/fhir+json`).
#[derive(Debug, Clone, Default)]
pub struct FhirBundleEncoder {
    /// Mapping settings.
    pub settings: FhirEncoding,
    /// Whether to indent the JSON.
    pub pretty: bool,
}

impl Encoder for FhirBundleEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        let bundle = bundle(context, &self.settings, "fhir-bundle")?;
        Ok(Encoded {
            data_type: DataType::Fhir,
            data: to_json(&Resource::from(bundle), self.pretty, "fhir-bundle")?,
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        matches!(
            context.clinical,
            Some(
                ClinicalContent::Results { .. }
                    | ClinicalContent::QualityControl { .. }
                    | ClinicalContent::Orders { .. }
            )
        )
    }
}

/// Encodes the single resource of one type that normalized content maps
/// to, for example the one Observation of a single-result message, to
/// `POST` to `[base]/Observation`.
///
/// References to other entries of the bundle [`encode_bundle`] would
/// build become logical references (the target's first identifier and its
/// type); references to targets without an identifier are dropped. Content
/// that maps to no resource or to several resources of the type is an
/// error: use the `fhir-bundle` encoder for it.
#[derive(Debug, Clone, Default)]
pub struct FhirResourceEncoder {
    /// Mapping settings; the bundle type is ignored.
    pub settings: FhirEncoding,
    /// The resource type to send, for example `Observation`.
    pub resource_type: String,
    /// Whether to indent the JSON.
    pub pretty: bool,
}

impl Encoder for FhirResourceEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        const STEP: &str = "fhir-resource-json";
        let bundle = bundle(context, &self.settings, STEP)?;
        let mut targets = BTreeMap::new();
        for entry in &bundle.entry {
            if let (Some(url), Some(resource)) = (&entry.full_url, &entry.resource) {
                let value = serde_json::to_value(resource)
                    .map_err(|e| StepError::new(STEP, e.to_string()))?;
                let identifier = value.get("identifier").and_then(|ids| ids.get(0)).cloned();
                targets.insert(
                    url.clone(),
                    (resource.resource_type().to_owned(), identifier),
                );
            }
        }
        let mut matching = bundle
            .entry
            .iter()
            .filter_map(|e| e.resource.as_ref())
            .filter(|r| r.resource_type() == self.resource_type);
        let (Some(resource), None) = (matching.next(), matching.next()) else {
            let count = bundle
                .entry
                .iter()
                .filter_map(|e| e.resource.as_ref())
                .filter(|r| r.resource_type() == self.resource_type)
                .count();
            return Err(StepError::new(
                STEP,
                format!(
                    "the message maps to {count} {} resources, not one; use the fhir-bundle encoder",
                    self.resource_type
                ),
            ));
        };
        let mut value =
            serde_json::to_value(resource).map_err(|e| StepError::new(STEP, e.to_string()))?;
        detach(&mut value, &targets);
        let resource: Resource =
            serde_json::from_value(value).map_err(|e| StepError::new(STEP, e.to_string()))?;
        Ok(Encoded {
            data_type: DataType::Fhir,
            data: to_json(&resource, self.pretty, STEP)?,
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        matches!(
            context.clinical,
            Some(
                ClinicalContent::Results { .. }
                    | ClinicalContent::QualityControl { .. }
                    | ClinicalContent::Orders { .. }
            )
        )
    }
}

/// Replaces references to bundle entries by logical references and removes
/// the elements left empty.
fn detach(value: &mut Value, targets: &BTreeMap<String, (String, Option<Value>)>) {
    match value {
        Value::Object(object) => {
            let target = object
                .get("reference")
                .and_then(Value::as_str)
                .filter(|r| r.starts_with("urn:uuid:"))
                .map(str::to_owned);
            if let Some(target) = target {
                object.remove("reference");
                if let Some((kind, identifier)) = targets.get(&target)
                    && let Some(identifier) = identifier
                {
                    object
                        .entry("type")
                        .or_insert_with(|| Value::String(kind.clone()));
                    object
                        .entry("identifier")
                        .or_insert_with(|| identifier.clone());
                }
            }
            for child in object.values_mut() {
                detach(child, targets);
            }
            object.retain(|_, child| !is_empty(child));
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                detach(item, targets);
            }
            items.retain(|item| !is_empty(item));
        }
        _ => {}
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

fn text_setting(step: &StepConfig, key: &str) -> Result<Option<String>, EngineError> {
    match step.settings.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(other) => Err(EngineError::Config(format!(
            "step {:?}: setting {key:?} must be text, not {other}",
            step.kind
        ))),
    }
}

fn bool_setting(step: &StepConfig, key: &str) -> Result<bool, EngineError> {
    match step.settings.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(other) => Err(EngineError::Config(format!(
            "step {:?}: setting {key:?} must be true or false, not {other}",
            step.kind
        ))),
    }
}

/// Reads [`FhirEncoding`] from step settings: `bundle_type`
/// (`transaction` or `collection`), `patient_identifier_system`,
/// `specimen_identifier_system`, `observation_category` and `utc_offset`
/// (minutes, -720 to 840).
pub fn fhir_settings(step: &StepConfig) -> Result<FhirEncoding, EngineError> {
    let mut settings = FhirEncoding {
        bundle_type: match text_setting(step, "bundle_type")?.as_deref() {
            None | Some("transaction") => BundleType::Transaction,
            Some("collection") => BundleType::Collection,
            Some(other) => {
                return Err(EngineError::Config(format!(
                    "step {:?}: bundle_type must be transaction or collection, not {other:?}",
                    step.kind
                )));
            }
        },
        patient_identifier_system: text_setting(step, "patient_identifier_system")?,
        specimen_identifier_system: text_setting(step, "specimen_identifier_system")?,
        ..FhirEncoding::default()
    };
    if let Some(category) = text_setting(step, "observation_category")? {
        settings.observation_category = category;
    }
    if let Some(value) = step.settings.get("utc_offset") {
        settings.utc_offset_minutes = value
            .as_i64()
            .and_then(|minutes| i16::try_from(minutes).ok())
            .filter(|minutes| (-720..=840).contains(minutes))
            .ok_or_else(|| {
                EngineError::Config(format!(
                    "step {:?}: utc_offset must be minutes between -720 and 840",
                    step.kind
                ))
            })?;
    }
    Ok(settings)
}

/// The resource types [`FhirResourceEncoder`] can send.
const RESOURCE_TYPES: &[&str] = &[
    "Patient",
    "Specimen",
    "ServiceRequest",
    "Observation",
    "DiagnosticReport",
    "Device",
];

/// Registers the FHIR normalizer (for [`DataType::Fhir`]) and the
/// `fhir-bundle` and `fhir-resource-json` encoders.
pub fn register(registry: &mut Registry) {
    registry
        .add_normalizer(DataType::Fhir, Arc::new(FhirNormalizer))
        .add_encoder("fhir-bundle", |step| {
            Ok(Arc::new(FhirBundleEncoder {
                settings: fhir_settings(step)?,
                pretty: bool_setting(step, "pretty")?,
            }) as Arc<dyn Encoder>)
        })
        .add_encoder("fhir-resource-json", |step| {
            let resource_type = text_setting(step, "resource")?
                .filter(|kind| RESOURCE_TYPES.contains(&kind.as_str()))
                .ok_or_else(|| {
                    EngineError::Config(format!(
                        "step {:?}: resource must be one of {}",
                        step.kind,
                        RESOURCE_TYPES.join(", ")
                    ))
                })?;
            Ok(Arc::new(FhirResourceEncoder {
                settings: fhir_settings(step)?,
                resource_type,
                pretty: bool_setting(step, "pretty")?,
            }) as Arc<dyn Encoder>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_settings() {
        let step = StepConfig::new("fhir-bundle")
            .with("bundle_type", "collection")
            .with("patient_identifier_system", "urn:oid:1.2.3")
            .with("observation_category", "vital-signs")
            .with("utc_offset", 180);
        let settings = fhir_settings(&step).unwrap();
        assert_eq!(settings.bundle_type, BundleType::Collection);
        assert_eq!(
            settings.patient_identifier_system.as_deref(),
            Some("urn:oid:1.2.3")
        );
        assert_eq!(settings.observation_category, "vital-signs");
        assert_eq!(settings.utc_offset_minutes, 180);
        assert!(fhir_settings(&StepConfig::new("x").with("bundle_type", "batch")).is_err());
        assert!(fhir_settings(&StepConfig::new("x").with("utc_offset", 900)).is_err());
    }

    #[test]
    fn detaches_bundle_references() {
        let mut targets = BTreeMap::new();
        targets.insert(
            "urn:uuid:p".to_owned(),
            (
                "Patient".to_owned(),
                Some(serde_json::json!({"system": "urn:mrn", "value": "1"})),
            ),
        );
        let mut value = serde_json::json!({
            "subject": {"reference": "urn:uuid:p"},
            "basedOn": [{"reference": "urn:uuid:missing"}],
            "specimen": {"reference": "urn:uuid:missing", "display": "S1"}
        });
        detach(&mut value, &targets);
        assert_eq!(
            value,
            serde_json::json!({
                "subject": {"type": "Patient", "identifier": {"system": "urn:mrn", "value": "1"}},
                "specimen": {"display": "S1"}
            })
        );
    }
}
