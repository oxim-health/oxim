//! Normalizers and encoders for the OXIM engine.

use std::sync::Arc;

use encoding_rs::Encoding;
use oxim_core::{
    Document, Encoded, Encoder, EngineError, MessageContext, Normalizer, Registry, StepConfig,
    StepError,
};
use oxim_model::{ClinicalContent, DataType};

use crate::astm::{self, AstmEncoding};
use crate::error::MappingError;
use crate::hl7::{self, Hl7Encoding, SpecimenPlacement};
use crate::poct1a;

fn step_error(step: &str) -> impl Fn(MappingError) -> StepError + '_ {
    move |e| StepError::new(step, e.to_string())
}

/// Normalizes ASTM messages.
#[derive(Debug, Clone, Copy, Default)]
pub struct AstmNormalizer;

impl Normalizer for AstmNormalizer {
    fn normalize(&self, document: &Document) -> Result<ClinicalContent, StepError> {
        match document {
            Document::Astm(message) => astm::normalize(message).map_err(step_error("normalize")),
            _ => Err(StepError::new("normalize", "expected an ASTM message")),
        }
    }
}

/// Normalizes HL7 v2 messages.
#[derive(Debug, Clone, Copy, Default)]
pub struct Hl7Normalizer;

impl Normalizer for Hl7Normalizer {
    fn normalize(&self, document: &Document) -> Result<ClinicalContent, StepError> {
        match document {
            Document::Hl7(message) => hl7::normalize(message).map_err(step_error("normalize")),
            _ => Err(StepError::new("normalize", "expected an HL7 v2 message")),
        }
    }
}

/// Normalizes POCT1-A messages.
#[derive(Debug, Clone, Copy, Default)]
pub struct Poct1aNormalizer;

impl Normalizer for Poct1aNormalizer {
    fn normalize(&self, document: &Document) -> Result<ClinicalContent, StepError> {
        match document {
            Document::Poct1a(message) => {
                poct1a::normalize(message).map_err(step_error("normalize"))
            }
            _ => Err(StepError::new("normalize", "expected a POCT1-A message")),
        }
    }
}

fn clinical<'a>(
    context: &'a MessageContext,
    encoder: &str,
) -> Result<&'a ClinicalContent, StepError> {
    context
        .clinical
        .as_ref()
        .ok_or_else(|| StepError::new(encoder, MappingError::NotNormalized.to_string()))
}

/// Encodes results and quality control as HL7 v2 `ORU^R01`.
#[derive(Debug, Clone, Default)]
pub struct OruEncoder {
    /// Header and layout settings.
    pub settings: Hl7Encoding,
}

impl Encoder for OruEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        let content = clinical(context, "hl7v2-oru-r01")?;
        let message = hl7::encode_results(
            content,
            &self.settings,
            context.envelope.id,
            context.envelope.received_at,
        )
        .map_err(step_error("hl7v2-oru-r01"))?;
        Ok(Encoded {
            data_type: DataType::Hl7V2,
            data: message.to_bytes(),
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        matches!(
            context.clinical,
            Some(ClinicalContent::Results { .. } | ClinicalContent::QualityControl { .. })
        )
    }
}

/// Encodes orders as HL7 v2 `OML^O21`.
#[derive(Debug, Clone, Default)]
pub struct OmlEncoder {
    /// Header settings.
    pub settings: Hl7Encoding,
}

impl Encoder for OmlEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        let content = clinical(context, "hl7v2-oml-o21")?;
        let message = hl7::encode_orders(
            content,
            &self.settings,
            context.envelope.id,
            context.envelope.received_at,
        )
        .map_err(step_error("hl7v2-oml-o21"))?;
        Ok(Encoded {
            data_type: DataType::Hl7V2,
            data: message.to_bytes(),
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        matches!(context.clinical, Some(ClinicalContent::Orders { .. }))
    }
}

/// Encodes orders as HL7 v2 `OML^O33`, the IHE LAW work order for
/// analyzers.
#[derive(Debug, Clone, Default)]
pub struct WorkOrderEncoder {
    /// Header settings.
    pub settings: Hl7Encoding,
}

impl Encoder for WorkOrderEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        let content = clinical(context, "hl7v2-oml-o33")?;
        let message = hl7::encode_work_orders(
            content,
            &self.settings,
            context.envelope.id,
            context.envelope.received_at,
        )
        .map_err(step_error("hl7v2-oml-o33"))?;
        Ok(Encoded {
            data_type: DataType::Hl7V2,
            data: message.to_bytes(),
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        matches!(context.clinical, Some(ClinicalContent::Orders { .. }))
    }
}

/// Encodes a host query as HL7 v2 `QBP^Q11`, for example to ask the LIS
/// about a tube the order cache does not know.
#[derive(Debug, Clone, Default)]
pub struct QueryEncoder {
    /// Header settings.
    pub settings: Hl7Encoding,
}

impl Encoder for QueryEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        let content = clinical(context, "hl7v2-qbp-q11")?;
        let message = hl7::encode_query(
            content,
            &self.settings,
            context.envelope.id,
            context.envelope.received_at,
        )
        .map_err(step_error("hl7v2-qbp-q11"))?;
        Ok(Encoded {
            data_type: DataType::Hl7V2,
            data: message.to_bytes(),
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        matches!(context.clinical, Some(ClinicalContent::Query { .. }))
    }
}

/// Answers an HL7 v2 host query (`QBP`) with `RSP^K11`, built from the
/// orders that a step such as `answer-query` put in place of the query.
#[derive(Debug, Clone, Default)]
pub struct QueryResponseEncoder {
    /// Header settings.
    pub settings: Hl7Encoding,
    /// Whether the orders follow the QPD segment.
    pub include_orders: bool,
}

impl Encoder for QueryResponseEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        let content = clinical(context, "hl7v2-rsp-k11")?;
        let Document::Hl7(query) = &context.document else {
            return Err(StepError::new(
                "hl7v2-rsp-k11",
                "the query being answered is not an HL7 v2 message",
            ));
        };
        let message = hl7::encode_query_response(
            content,
            query,
            &self.settings,
            self.include_orders,
            context.envelope.id,
            context.envelope.received_at,
        )
        .map_err(step_error("hl7v2-rsp-k11"))?;
        Ok(Encoded {
            data_type: DataType::Hl7V2,
            data: message.to_bytes(),
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        matches!(context.clinical, Some(ClinicalContent::Orders { .. }))
            && matches!(context.document, Document::Hl7(_))
    }
}

/// Encodes orders as ASTM records, for worklist download or as the answer
/// to a host query.
#[derive(Debug, Clone, Default)]
pub struct AstmOrdersEncoder {
    /// Header settings, including the O-26 report type.
    pub settings: AstmEncoding,
}

impl Encoder for AstmOrdersEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        let content = clinical(context, "astm-orders")?;
        let message = astm::encode_orders(content, &self.settings, context.envelope.received_at)
            .map_err(step_error("astm-orders"))?;
        Ok(Encoded {
            data_type: DataType::Astm,
            data: message.to_bytes(),
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        matches!(context.clinical, Some(ClinicalContent::Orders { .. }))
    }
}

/// Encodes the normalized content as JSON.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClinicalJsonEncoder {
    /// Whether to indent the JSON.
    pub pretty: bool,
}

impl Encoder for ClinicalJsonEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        let content = clinical(context, "clinical-json")?;
        let data = if self.pretty {
            serde_json::to_vec_pretty(content)
        } else {
            serde_json::to_vec(content)
        }
        .map_err(|e| StepError::new("clinical-json", e.to_string()))?;
        Ok(Encoded {
            data_type: DataType::Json,
            data,
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        context.clinical.is_some()
    }
}

fn text_setting(step: &StepConfig, key: &str) -> Result<Option<String>, EngineError> {
    match step.settings.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => Ok(Some(text.clone())),
        Some(serde_json::Value::Number(number)) => Ok(Some(number.to_string())),
        Some(other) => Err(EngineError::Config(format!(
            "step {:?}: setting {key:?} must be text, not {other}",
            step.kind
        ))),
    }
}

fn offset_setting(step: &StepConfig) -> Result<i16, EngineError> {
    match step.settings.get("utc_offset") {
        None => Ok(0),
        Some(value) => value
            .as_i64()
            .and_then(|minutes| i16::try_from(minutes).ok())
            .filter(|minutes| (-720..=840).contains(minutes))
            .ok_or_else(|| {
                EngineError::Config(format!(
                    "step {:?}: utc_offset must be minutes between -720 and 840",
                    step.kind
                ))
            }),
    }
}

/// Reads [`Hl7Encoding`] from step settings: `sending_application`,
/// `sending_facility`, `receiving_application`, `receiving_facility`,
/// `version`, `processing_id`, `charset`, `utc_offset` (minutes),
/// `specimen` (`obr3`, `spm` or `both`) and `default_result_status`.
pub fn hl7_settings(step: &StepConfig) -> Result<Hl7Encoding, EngineError> {
    let mut settings = Hl7Encoding::default();
    if let Some(value) = text_setting(step, "sending_application")? {
        settings.sending_application = Some(value);
    }
    settings.sending_facility = text_setting(step, "sending_facility")?;
    settings.receiving_application = text_setting(step, "receiving_application")?;
    settings.receiving_facility = text_setting(step, "receiving_facility")?;
    if let Some(value) = text_setting(step, "version")? {
        settings.version = value;
    }
    if let Some(value) = text_setting(step, "processing_id")? {
        settings.processing_id = value;
    }
    if let Some(value) = text_setting(step, "default_result_status")? {
        settings.default_result_status = value;
    }
    settings.charset = text_setting(step, "charset")?;
    if let Some(charset) = &settings.charset {
        oxim_hl7::encoding_for_charset(charset.as_bytes())
            .map_err(|e| EngineError::Config(format!("step {:?}: {e}", step.kind)))?;
    }
    settings.utc_offset_minutes = offset_setting(step)?;
    settings.specimen = match text_setting(step, "specimen")?.as_deref() {
        None | Some("both") => SpecimenPlacement::Both,
        Some("obr3") => SpecimenPlacement::Obr3,
        Some("spm") => SpecimenPlacement::Spm,
        Some(other) => {
            return Err(EngineError::Config(format!(
                "step {:?}: specimen must be obr3, spm or both, not {other:?}",
                step.kind
            )));
        }
    };
    Ok(settings)
}

/// Reads [`AstmEncoding`] from step settings: `sender`, `receiver`,
/// `version`, `processing_id`, `utc_offset` (minutes) and `encoding` (a
/// character encoding label such as `windows-1254`).
pub fn astm_settings(step: &StepConfig, report_type: &str) -> Result<AstmEncoding, EngineError> {
    let mut settings = AstmEncoding {
        report_type: report_type.to_owned(),
        ..AstmEncoding::default()
    };
    if let Some(value) = text_setting(step, "sender")? {
        settings.sender = Some(value);
    }
    settings.receiver = text_setting(step, "receiver")?;
    if let Some(value) = text_setting(step, "version")? {
        settings.version = value;
    }
    if let Some(value) = text_setting(step, "processing_id")? {
        settings.processing_id = value;
    }
    if let Some(label) = text_setting(step, "encoding")? {
        settings.encoding = Encoding::for_label(label.as_bytes()).ok_or_else(|| {
            EngineError::Config(format!("step {:?}: unknown encoding {label:?}", step.kind))
        })?;
    }
    settings.utc_offset_minutes = offset_setting(step)?;
    Ok(settings)
}

/// Registers the normalizers for ASTM, HL7 v2 and POCT1-A and the encoders
/// `hl7v2-oru-r01`, `hl7v2-oml-o21`, `hl7v2-oml-o33`, `hl7v2-qbp-q11`, `hl7v2-rsp-k11`,
/// `astm-orders`, `astm-query-response` and `clinical-json`.
pub fn register(registry: &mut Registry) {
    registry
        .add_normalizer(DataType::Astm, Arc::new(AstmNormalizer))
        .add_normalizer(DataType::Hl7V2, Arc::new(Hl7Normalizer))
        .add_normalizer(DataType::Poct1a, Arc::new(Poct1aNormalizer))
        .add_encoder("hl7v2-oru-r01", |step| {
            Ok(Arc::new(OruEncoder {
                settings: hl7_settings(step)?,
            }) as Arc<dyn Encoder>)
        })
        .add_encoder("hl7v2-oml-o21", |step| {
            Ok(Arc::new(OmlEncoder {
                settings: hl7_settings(step)?,
            }) as Arc<dyn Encoder>)
        })
        .add_encoder("hl7v2-oml-o33", |step| {
            Ok(Arc::new(WorkOrderEncoder {
                settings: hl7_settings(step)?,
            }) as Arc<dyn Encoder>)
        })
        .add_encoder("hl7v2-qbp-q11", |step| {
            Ok(Arc::new(QueryEncoder {
                settings: hl7_settings(step)?,
            }) as Arc<dyn Encoder>)
        })
        .add_encoder("hl7v2-rsp-k11", |step| {
            let include_orders = match step.settings.get("include_orders") {
                None => false,
                Some(serde_json::Value::Bool(value)) => *value,
                Some(_) => {
                    return Err(EngineError::Config(format!(
                        "step {:?}: include_orders must be true or false",
                        step.kind
                    )));
                }
            };
            Ok(Arc::new(QueryResponseEncoder {
                settings: hl7_settings(step)?,
                include_orders,
            }) as Arc<dyn Encoder>)
        })
        .add_encoder("astm-orders", |step| {
            Ok(Arc::new(AstmOrdersEncoder {
                settings: astm_settings(step, "O")?,
            }) as Arc<dyn Encoder>)
        })
        .add_encoder("astm-query-response", |step| {
            Ok(Arc::new(AstmOrdersEncoder {
                settings: astm_settings(step, "Q")?,
            }) as Arc<dyn Encoder>)
        })
        .add_encoder("clinical-json", |step| {
            let pretty = step
                .settings
                .get("pretty")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            Ok(Arc::new(ClinicalJsonEncoder { pretty }) as Arc<dyn Encoder>)
        });
}
