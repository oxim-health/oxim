//! Pipeline steps for DICOM channels.
//!
//! A channel with `data_type: dicom` carries Part 10 objects as raw
//! documents. These steps decode the object, and transformers encode it
//! again in its transfer syntax, with sequences and items of undefined
//! length.
//!
//! Attributes are named by keyword (`PatientID`), tag (`(0010,0020)`,
//! `0010,0020` or `00100020`) or nested path
//! (`RequestAttributesSequence[0].AccessionNumber`).

use std::collections::BTreeMap;
use std::sync::Arc;

use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_core::header::Header;
use dicom_core::ops::{
    ApplyOp, AttributeAction, AttributeOp, AttributeSelector, AttributeSelectorStep,
};
use dicom_core::value::C;
use dicom_core::{PrimitiveValue, Tag, VR};
use dicom_dictionary_std::StandardDataDictionary;
use dicom_object::InMemDicomObject;
use oxim_core::{
    Document, EngineError, Filter, MessageContext, StepConfig, StepError, Transformer,
};
use serde::Deserialize;

use crate::net::settings;
use crate::object::{DicomObject, parse_selector};

/// The raw bytes of a DICOM message.
pub(crate) fn raw<'a>(context: &'a MessageContext, step: &str) -> Result<&'a [u8], StepError> {
    match &context.document {
        Document::Raw(bytes) => Ok(bytes),
        _ => Err(StepError::new(
            step,
            "DICOM steps need a channel with data_type: dicom",
        )),
    }
}

/// Decodes the message as a DICOM object.
pub(crate) fn decode(context: &MessageContext, step: &str) -> Result<DicomObject, StepError> {
    DicomObject::parse(raw(context, step)?).map_err(|e| StepError::new(step, e.to_string()))
}

/// Replaces the message with the encoded object.
pub(crate) fn encode(
    context: &mut MessageContext,
    object: &DicomObject,
    step: &str,
) -> Result<(), StepError> {
    let bytes = object
        .to_bytes()
        .map_err(|e| StepError::new(step, e.to_string()))?;
    context.document = Document::Raw(bytes);
    Ok(())
}

fn selector(step: &str, text: &str) -> Result<AttributeSelector, EngineError> {
    parse_selector(text).map_err(|e| EngineError::Config(format!("{step} step: {e}")))
}

/// The tag an attribute selector ends with.
fn leaf(selector: &AttributeSelector) -> Tag {
    match selector.iter().last() {
        Some(AttributeSelectorStep::Tag(tag) | AttributeSelectorStep::Nested { tag, .. }) => *tag,
        None => Tag(0, 0),
    }
}

/// Settings of the `dicom-tag` filter. Exactly one of `equals`, `in` and
/// `exists` is required.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DicomTagFilterSettings {
    /// The attribute.
    pub tag: String,
    /// Accept when the value, or one of its values, equals this text.
    #[serde(default)]
    pub equals: Option<String>,
    /// Accept when the value, or one of its values, is in this list.
    #[serde(default, rename = "in")]
    pub one_of: Option<Vec<String>>,
    /// Accept when the attribute is present (`true`) or absent (`false`).
    #[serde(default)]
    pub exists: Option<bool>,
    /// Invert the result.
    #[serde(default)]
    pub negate: bool,
}

#[derive(Debug, Clone)]
enum Condition {
    OneOf(Vec<String>),
    Exists(bool),
}

/// Keeps messages by the value of one attribute.
#[derive(Debug, Clone)]
pub struct DicomTagFilter {
    selector: AttributeSelector,
    condition: Condition,
    negate: bool,
}

impl DicomTagFilter {
    /// Validates the settings and creates the filter.
    pub fn new(settings: DicomTagFilterSettings) -> Result<Self, EngineError> {
        let selector = selector("dicom-tag", &settings.tag)?;
        let condition = match (settings.equals, settings.one_of, settings.exists) {
            (Some(value), None, None) => Condition::OneOf(vec![value]),
            (None, Some(values), None) => Condition::OneOf(values),
            (None, None, Some(exists)) => Condition::Exists(exists),
            _ => {
                return Err(EngineError::Config(
                    "dicom-tag step needs exactly one of equals, in and exists".into(),
                ));
            }
        };
        Ok(Self {
            selector,
            condition,
            negate: settings.negate,
        })
    }

    /// Whether `object` matches, before `negate`.
    pub fn matches(&self, object: &DicomObject) -> bool {
        match &self.condition {
            Condition::Exists(expected) => object.contains(&self.selector) == *expected,
            Condition::OneOf(expected) => object.text(&self.selector).is_some_and(|value| {
                expected
                    .iter()
                    .any(|wanted| value == *wanted || value.split('\\').any(|part| part == wanted))
            }),
        }
    }
}

impl Filter for DicomTagFilter {
    fn accept(&self, context: &MessageContext) -> Result<bool, StepError> {
        let object = decode(context, "dicom-tag")?;
        Ok(self.matches(&object) != self.negate)
    }
}

/// Settings of the `dicom-set` transformer.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DicomSetSettings {
    /// Attributes to set. A value is text (multiple values separated by
    /// `\`), a number, or `{value, vr}` for attributes outside the
    /// standard dictionary.
    #[serde(default)]
    pub set: BTreeMap<String, serde_json::Value>,
    /// Attributes to remove.
    #[serde(default)]
    pub remove: Vec<String>,
    /// Remove every private attribute (odd groups), in nested sequences
    /// too.
    #[serde(default)]
    pub remove_private: bool,
}

/// The value representation of a standard attribute.
pub(crate) fn dictionary_vr(tag: Tag) -> Option<VR> {
    StandardDataDictionary
        .by_tag(tag)
        .map(|entry| entry.vr().relaxed())
}

/// Converts text to a value of `vr`.
pub(crate) fn typed_value(vr: VR, text: &str) -> Result<PrimitiveValue, String> {
    fn numbers<T: std::str::FromStr>(text: &str) -> Result<C<T>, String> {
        text.split('\\')
            .map(|part| {
                part.trim()
                    .parse()
                    .map_err(|_| format!("{part:?} is not a valid number"))
            })
            .collect()
    }
    let parts = || -> C<String> { text.split('\\').map(str::to_owned).collect() };
    Ok(match vr {
        VR::AE
        | VR::AS
        | VR::CS
        | VR::DA
        | VR::DS
        | VR::DT
        | VR::IS
        | VR::LO
        | VR::PN
        | VR::SH
        | VR::TM
        | VR::UC
        | VR::UI => {
            if text.is_empty() {
                PrimitiveValue::Empty
            } else {
                PrimitiveValue::Strs(parts())
            }
        }
        VR::LT | VR::ST | VR::UT | VR::UR => {
            if text.is_empty() {
                PrimitiveValue::Empty
            } else {
                PrimitiveValue::Str(text.to_owned())
            }
        }
        VR::US => PrimitiveValue::U16(numbers(text)?),
        VR::SS => PrimitiveValue::I16(numbers(text)?),
        VR::UL => PrimitiveValue::U32(numbers(text)?),
        VR::SL => PrimitiveValue::I32(numbers(text)?),
        VR::UV => PrimitiveValue::U64(numbers(text)?),
        VR::SV => PrimitiveValue::I64(numbers(text)?),
        VR::FL => PrimitiveValue::F32(numbers(text)?),
        VR::FD => PrimitiveValue::F64(numbers(text)?),
        VR::OB | VR::UN => {
            let mut bytes = text.as_bytes().to_vec();
            if !bytes.len().is_multiple_of(2) {
                bytes.push(0);
            }
            PrimitiveValue::U8(C::from_vec(bytes))
        }
        other => return Err(format!("values of VR {other} cannot be set from text")),
    })
}

#[derive(Debug, Clone)]
struct Assignment {
    selector: AttributeSelector,
    vr: VR,
    value: PrimitiveValue,
}

/// Sets and removes attributes.
#[derive(Debug, Clone)]
pub struct DicomSet {
    assignments: Vec<Assignment>,
    removals: Vec<AttributeSelector>,
    remove_private: bool,
}

impl DicomSet {
    /// Validates the settings and creates the transformer.
    pub fn new(settings: DicomSetSettings) -> Result<Self, EngineError> {
        let config = |message: String| EngineError::Config(format!("dicom-set step: {message}"));
        let mut assignments = Vec::with_capacity(settings.set.len());
        for (name, value) in settings.set {
            let selector = selector("dicom-set", &name)?;
            let tag = leaf(&selector);
            let (text, vr) = match value {
                serde_json::Value::String(text) => (text, None),
                serde_json::Value::Number(number) => (number.to_string(), None),
                serde_json::Value::Object(mut fields) => {
                    let vr = fields
                        .remove("vr")
                        .and_then(|vr| vr.as_str().map(str::to_owned))
                        .ok_or_else(|| config(format!("{name}: `vr` must be text")))?;
                    let vr: VR = vr
                        .parse()
                        .map_err(|_| config(format!("{name}: unknown VR {vr:?}")))?;
                    let text = match fields.remove("value") {
                        Some(serde_json::Value::String(text)) => text,
                        Some(serde_json::Value::Number(number)) => number.to_string(),
                        _ => {
                            return Err(config(format!(
                                "{name}: `value` must be text or a number"
                            )));
                        }
                    };
                    if let Some(extra) = fields.keys().next() {
                        return Err(config(format!("{name}: unknown field {extra:?}")));
                    }
                    (text, Some(vr))
                }
                _ => {
                    return Err(config(format!(
                        "{name}: the value must be text or a number"
                    )));
                }
            };
            let vr = vr.or_else(|| dictionary_vr(tag)).ok_or_else(|| {
                config(format!(
                    "{name} is not in the standard dictionary; give its vr"
                ))
            })?;
            if vr == VR::SQ {
                return Err(config(format!("{name} is a sequence and cannot be set")));
            }
            let value = typed_value(vr, &text).map_err(|e| config(format!("{name}: {e}")))?;
            assignments.push(Assignment {
                selector,
                vr,
                value,
            });
        }
        let removals = settings
            .remove
            .iter()
            .map(|name| selector("dicom-set", name))
            .collect::<Result<Vec<_>, _>>()?;
        if assignments.is_empty() && removals.is_empty() && !settings.remove_private {
            return Err(config("nothing to set or remove".into()));
        }
        Ok(Self {
            assignments,
            removals,
            remove_private: settings.remove_private,
        })
    }

    /// Applies the changes to a decoded object.
    pub fn apply_to(&self, object: &mut DicomObject) -> Result<(), String> {
        if self.remove_private {
            remove_private(&mut object.dataset);
        }
        for removal in &self.removals {
            // A missing attribute or sequence is already removed.
            let _ = object
                .dataset
                .apply(AttributeOp::new(removal.clone(), AttributeAction::Remove));
        }
        for assignment in &self.assignments {
            let tag = leaf(&assignment.selector);
            // Setting creates missing sequences and items; the VR then
            // follows the configuration, also for existing UN elements.
            object
                .dataset
                .apply(AttributeOp::new(
                    assignment.selector.clone(),
                    AttributeAction::Set(assignment.value.clone()),
                ))
                .and_then(|()| {
                    object.dataset.apply(AttributeOp::new(
                        assignment.selector.clone(),
                        AttributeAction::SetVr(assignment.vr),
                    ))
                })
                .map_err(|e| format!("cannot set {tag}: {e}"))?;
        }
        Ok(())
    }
}

impl Transformer for DicomSet {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let mut object = decode(context, "dicom-set")?;
        self.apply_to(&mut object)
            .map_err(|e| StepError::new("dicom-set", e))?;
        encode(context, &object, "dicom-set")
    }
}

/// Calls `f` on a data set and on every item of its sequences.
pub(crate) fn for_each_dataset(
    dataset: &mut InMemDicomObject,
    f: &mut dyn FnMut(&mut InMemDicomObject),
) {
    f(dataset);
    let sequences: Vec<Tag> = dataset
        .iter()
        .filter(|element| element.vr() == VR::SQ || element.items().is_some())
        .map(|element| element.tag())
        .collect();
    for tag in sequences {
        dataset.update_value(tag, |value| {
            if let Some(items) = value.items_mut() {
                for item in items.iter_mut() {
                    for_each_dataset(item, f);
                }
            }
        });
    }
}

/// Removes private attributes everywhere.
pub(crate) fn remove_private(dataset: &mut InMemDicomObject) {
    for_each_dataset(dataset, &mut |item| {
        item.retain(|element| element.tag().group().is_multiple_of(2));
    });
}

/// Registers the steps of this module.
pub(crate) fn register(registry: &mut oxim_core::Registry) {
    registry.add_filter("dicom-tag", |step: &StepConfig| {
        let settings = settings(&step.settings, "dicom-tag step")?;
        Ok(Arc::new(DicomTagFilter::new(settings)?) as Arc<dyn Filter>)
    });
    registry.add_transformer("dicom-set", |step: &StepConfig| {
        let settings = settings(&step.settings, "dicom-set step")?;
        Ok(Arc::new(DicomSet::new(settings)?) as Arc<dyn Transformer>)
    });
}
