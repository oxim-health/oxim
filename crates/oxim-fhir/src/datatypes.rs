//! FHIR R4 data types used by the modeled resources.
//!
//! Every type keeps elements it does not model (extensions, primitive
//! extensions such as `_birthDate`, elements of later profiles) in `extra`,
//! so they survive a parse → serialize round trip.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::decimal::FhirDecimal;

/// Elements a type does not model, kept verbatim.
pub type Extra = BTreeMap<String, Value>;

/// A code from a code system.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Coding {
    /// Code system URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Code system version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Display text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// Whether the user chose this coding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_selected: Option<bool>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

impl Coding {
    /// A coding with a system and code.
    pub fn new(system: Option<&str>, code: &str) -> Self {
        Self {
            system: system.map(str::to_owned),
            code: Some(code.to_owned()),
            ..Self::default()
        }
    }

    /// Sets the display text.
    pub fn with_display(mut self, display: Option<&str>) -> Self {
        self.display = display.map(str::to_owned);
        self
    }
}

/// Codes and/or text for a concept.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CodeableConcept {
    /// Codes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coding: Vec<Coding>,
    /// Plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

impl CodeableConcept {
    /// A concept with one coding.
    pub fn coding(coding: Coding) -> Self {
        Self {
            coding: vec![coding],
            ..Self::default()
        }
    }
}

/// A period of time.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Period {
    /// Start (dateTime).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// End (dateTime).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A reference to another resource.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Reference {
    /// Literal reference: relative (`Patient/1`), absolute or `urn:uuid:`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Target resource type.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Logical reference by identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<Box<Identifier>>,
    /// Display text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

impl Reference {
    /// A literal reference.
    pub fn to(reference: impl Into<String>) -> Self {
        Self {
            reference: Some(reference.into()),
            ..Self::default()
        }
    }

    /// A reference with display text only.
    pub fn display(display: impl Into<String>) -> Self {
        Self {
            display: Some(display.into()),
            ..Self::default()
        }
    }
}

/// An identifier.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Identifier {
    /// `usual`, `official`, `temp`, `secondary`, `old`.
    #[serde(default, rename = "use", skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
    /// Identifier type (for example v2 table 0203 `MR`).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<CodeableConcept>,
    /// Namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Validity period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Issuing organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigner: Option<Box<Reference>>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A person's name.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HumanName {
    /// `usual`, `official`, …
    #[serde(default, rename = "use", skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
    /// Full text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Family name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Given names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub given: Vec<String>,
    /// Prefixes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefix: Vec<String>,
    /// Suffixes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suffix: Vec<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A measured amount.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Quantity {
    /// The value, exactly as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<FhirDecimal>,
    /// `<`, `<=`, `>=`, `>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparator: Option<String>,
    /// Unit as displayed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Unit system, usually `http://unitsofmeasure.org`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Coded unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A range of quantities.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Range {
    /// Lower bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<Quantity>,
    /// Upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<Quantity>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A ratio of quantities.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Ratio {
    /// Numerator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numerator: Option<Quantity>,
    /// Denominator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denominator: Option<Quantity>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A text note.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    /// Author as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_string: Option<String>,
    /// When written (dateTime).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// The note (required).
    #[serde(default)]
    pub text: String,
    /// Unmodeled elements (including `authorReference`).
    #[serde(flatten)]
    pub extra: Extra,
}

impl Annotation {
    /// A note with text only.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

/// Binary content such as a PDF report.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// MIME type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Base64 data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// Where the data can be retrieved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// Resource metadata.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    /// Version of the resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    /// Last change (instant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    /// Source system URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Profiles the resource claims to conform to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profile: Vec<String>,
    /// Tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag: Vec<Coding>,
    /// Unmodeled elements (including `security`).
    #[serde(flatten)]
    pub extra: Extra,
}

/// An extension. Extensions of modeled types are kept in their `extra`
/// map; this type is used to build and read them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Extension {
    /// Identifies the meaning of the extension.
    pub url: String,
    /// Nested extensions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension: Vec<Extension>,
    /// The `value[x]` element and anything else.
    #[serde(flatten)]
    pub extra: Extra,
}
