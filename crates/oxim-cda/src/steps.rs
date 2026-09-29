//! The CDA normalizer and the `cda-lab-report` encoder.

use std::sync::Arc;

use oxim_core::{
    Document, Encoded, Encoder, EngineError, MessageContext, Normalizer, Registry, StepConfig,
    StepError,
};
use oxim_model::{ClinicalContent, DataType};

use crate::generate::{CdaEncoding, encode_lab_report};
use crate::results::lab_results;

/// Maps CDA documents (`data_type: cda`, parsed as XML) to normalized
/// results.
#[derive(Debug, Clone, Copy, Default)]
pub struct CdaNormalizer;

impl Normalizer for CdaNormalizer {
    fn normalize(&self, document: &Document) -> Result<ClinicalContent, StepError> {
        match document {
            Document::Format(oxim_formats::Document::Xml(xml)) => {
                lab_results(xml).map_err(|e| StepError::new("normalize", e.to_string()))
            }
            _ => Err(StepError::new(
                "normalize",
                "CDA documents must be parsed as XML",
            )),
        }
    }
}

/// Writes normalized results as a CDA R2 laboratory report.
#[derive(Debug, Clone, Default)]
pub struct CdaLabReportEncoder {
    /// Document settings.
    pub settings: CdaEncoding,
}

impl Encoder for CdaLabReportEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        let content = context.clinical.as_ref().ok_or_else(|| {
            StepError::new(
                "cda-lab-report",
                "the channel must normalize messages (normalize: true)",
            )
        })?;
        let document = encode_lab_report(
            content,
            &self.settings,
            context.envelope.id,
            context.envelope.received_at,
        )
        .map_err(|e| StepError::new("cda-lab-report", e.to_string()))?;
        Ok(Encoded {
            data_type: DataType::Cda,
            data: document.to_bytes(),
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        matches!(context.clinical, Some(ClinicalContent::Results { .. }))
    }
}

fn text(step: &StepConfig, key: &str) -> Result<Option<String>, EngineError> {
    match step.settings.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.clone())),
        Some(_) => Err(EngineError::Config(format!(
            "step {:?}: {key:?} must be non-empty text",
            step.kind
        ))),
    }
}

fn root(step: &StepConfig, key: &str) -> Result<Option<String>, EngineError> {
    let value = text(step, key)?;
    if let Some(root) = &value
        && crate::codes::root_from_system(root).as_deref() != Some(root.as_str())
    {
        return Err(EngineError::Config(format!(
            "step {:?}: {key:?} must be an OID or a lowercase UUID",
            step.kind
        )));
    }
    Ok(value)
}

/// Reads [`CdaEncoding`] from step settings: `title`, `language`,
/// `confidentiality`, `custodian_name`, `custodian_id_root`,
/// `patient_id_root`, `specimen_id_root` (OIDs or UUIDs),
/// `software_name` and `utc_offset` (minutes).
pub fn cda_settings(step: &StepConfig) -> Result<CdaEncoding, EngineError> {
    let mut settings = CdaEncoding::default();
    if let Some(title) = text(step, "title")? {
        settings.title = title;
    }
    if let Some(language) = text(step, "language")? {
        settings.language = language;
    }
    if let Some(confidentiality) = text(step, "confidentiality")? {
        settings.confidentiality = confidentiality;
    }
    if let Some(name) = text(step, "software_name")? {
        settings.software_name = name;
    }
    settings.custodian_name = text(step, "custodian_name")?;
    settings.custodian_id_root = root(step, "custodian_id_root")?;
    settings.patient_id_root = root(step, "patient_id_root")?;
    settings.specimen_id_root = root(step, "specimen_id_root")?;
    settings.utc_offset_minutes = match step.settings.get("utc_offset") {
        None => 0,
        Some(value) => value
            .as_i64()
            .and_then(|m| i16::try_from(m).ok())
            .filter(|m| (-720..=840).contains(m))
            .ok_or_else(|| {
                EngineError::Config(format!(
                    "step {:?}: utc_offset must be minutes between -720 and 840",
                    step.kind
                ))
            })?,
    };
    Ok(settings)
}

/// Adds the CDA normalizer (`data_type: cda` with `normalize: true`) and
/// the `cda-lab-report` encoder to an engine registry.
pub fn register(registry: &mut Registry) {
    registry
        .add_normalizer(DataType::Cda, Arc::new(CdaNormalizer))
        .add_encoder("cda-lab-report", |step| {
            Ok(Arc::new(CdaLabReportEncoder {
                settings: cda_settings(step)?,
            }) as Arc<dyn Encoder>)
        });
}
