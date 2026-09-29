//! The FHIR R4 resources OXIM exchanges, and [`Resource`] for any resource.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::datatypes::{
    Annotation, Attachment, CodeableConcept, Extra, HumanName, Identifier, Meta, Period, Quantity,
    Range, Ratio, Reference,
};
use crate::error::{FhirError, FhirResult};

/// A patient.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Patient {
    /// Logical id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    /// Identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifier: Vec<Identifier>,
    /// Whether the record is in active use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub name: Vec<HumanName>,
    /// `male`, `female`, `other`, `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    /// Birth date (`date`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birth_date: Option<String>,
    /// Unmodeled elements, including extensions such as `patient-animal`.
    #[serde(flatten)]
    pub extra: Extra,
}

/// Collection details of a specimen.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecimenCollection {
    /// When the specimen was collected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collected_date_time: Option<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A container of a specimen.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SpecimenContainer {
    /// Container identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifier: Vec<Identifier>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A specimen.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Specimen {
    /// Logical id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    /// Identifiers such as the tube barcode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifier: Vec<Identifier>,
    /// Laboratory accession identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accession_identifier: Option<Identifier>,
    /// `available`, `unavailable`, `unsatisfactory`, `entered-in-error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Specimen type.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<CodeableConcept>,
    /// The patient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Reference>,
    /// When the laboratory received it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_time: Option<String>,
    /// Orders it was collected for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request: Vec<Reference>,
    /// Collection details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<SpecimenCollection>,
    /// Containers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub container: Vec<SpecimenContainer>,
    /// Notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub note: Vec<Annotation>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A test order.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRequest {
    /// Logical id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    /// Identifiers (placer and filler order numbers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifier: Vec<Identifier>,
    /// `draft`, `active`, `on-hold`, `revoked`, `completed`,
    /// `entered-in-error`, `unknown` (required).
    #[serde(default)]
    pub status: String,
    /// `proposal`, `plan`, `directive`, `order`, … (required).
    #[serde(default)]
    pub intent: String,
    /// Categories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub category: Vec<CodeableConcept>,
    /// `routine`, `urgent`, `asap`, `stat`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// What is requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<CodeableConcept>,
    /// The patient (required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Reference>,
    /// When requested (dateTime).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_on: Option<String>,
    /// Specimens.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specimen: Vec<Reference>,
    /// Notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub note: Vec<Annotation>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A reference range of an observation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ObservationReferenceRange {
    /// Lower limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<Quantity>,
    /// Upper limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<Quantity>,
    /// Range meaning.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<CodeableConcept>,
    /// The range as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A measurement or finding.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    /// Logical id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    /// Identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifier: Vec<Identifier>,
    /// The orders it fulfills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub based_on: Vec<Reference>,
    /// `registered`, `preliminary`, `final`, `amended`, `corrected`,
    /// `cancelled`, `entered-in-error`, `unknown` (required).
    #[serde(default)]
    pub status: String,
    /// Categories such as `laboratory`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub category: Vec<CodeableConcept>,
    /// What was measured (required).
    #[serde(default)]
    pub code: CodeableConcept,
    /// The patient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Reference>,
    /// Clinically relevant time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_date_time: Option<String>,
    /// Clinically relevant period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_period: Option<Period>,
    /// When released (instant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued: Option<String>,
    /// Who performed it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub performer: Vec<Reference>,
    /// `valueQuantity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_quantity: Option<Quantity>,
    /// `valueCodeableConcept`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_codeable_concept: Option<CodeableConcept>,
    /// `valueString`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_string: Option<String>,
    /// `valueBoolean`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_boolean: Option<bool>,
    /// `valueRange`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_range: Option<Range>,
    /// `valueRatio`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_ratio: Option<Ratio>,
    /// `valueDateTime`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_date_time: Option<String>,
    /// Why the value is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_absent_reason: Option<CodeableConcept>,
    /// Abnormal flags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interpretation: Vec<CodeableConcept>,
    /// Notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub note: Vec<Annotation>,
    /// Method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<CodeableConcept>,
    /// Specimen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specimen: Option<Reference>,
    /// Measuring device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Reference>,
    /// Reference ranges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_range: Vec<ObservationReferenceRange>,
    /// Unmodeled elements (including `component` and other `value[x]`).
    #[serde(flatten)]
    pub extra: Extra,
}

/// A report grouping observations.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReport {
    /// Logical id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    /// Identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifier: Vec<Identifier>,
    /// The orders it fulfills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub based_on: Vec<Reference>,
    /// `registered`, `partial`, `preliminary`, `final`, `amended`,
    /// `corrected`, `appended`, `cancelled`, `entered-in-error`, `unknown`
    /// (required).
    #[serde(default)]
    pub status: String,
    /// Categories such as `LAB`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub category: Vec<CodeableConcept>,
    /// Report name (required).
    #[serde(default)]
    pub code: CodeableConcept,
    /// The patient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Reference>,
    /// Clinically relevant time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_date_time: Option<String>,
    /// When released (instant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued: Option<String>,
    /// Specimens.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specimen: Vec<Reference>,
    /// Observations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub result: Vec<Reference>,
    /// Rendered reports and attachments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presented_form: Vec<Attachment>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A name of a device.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeviceName {
    /// The name.
    #[serde(default)]
    pub name: String,
    /// `udi-label-name`, `user-friendly-name`, `model-name`, …
    #[serde(default, rename = "type")]
    pub kind: String,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// A version of a device.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeviceVersion {
    /// The version text.
    #[serde(default)]
    pub value: String,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// An instrument.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// Logical id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    /// Identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifier: Vec<Identifier>,
    /// Manufacturer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Serial number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    /// Names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub device_name: Vec<DeviceName>,
    /// Model number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_number: Option<String>,
    /// Software and firmware versions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version: Vec<DeviceVersion>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// One issue of an [`OperationOutcome`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Issue {
    /// `fatal`, `error`, `warning`, `information`.
    #[serde(default)]
    pub severity: String,
    /// Issue type, for example `required`, `value`, `invalid`.
    #[serde(default)]
    pub code: String,
    /// Coded details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<CodeableConcept>,
    /// Human-readable diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<String>,
    /// FHIRPath of the element concerned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expression: Vec<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

impl Issue {
    /// An issue with severity `error`.
    pub fn error(
        code: &str,
        expression: impl Into<String>,
        diagnostics: impl Into<String>,
    ) -> Self {
        Self {
            severity: "error".into(),
            code: code.into(),
            diagnostics: Some(diagnostics.into()),
            expression: vec![expression.into()],
            ..Self::default()
        }
    }

    /// Whether the issue is `error` or `fatal`.
    pub fn is_error(&self) -> bool {
        matches!(self.severity.as_str(), "error" | "fatal")
    }
}

/// A set of issues, as returned by FHIR servers.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OperationOutcome {
    /// Logical id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The issues.
    #[serde(default)]
    pub issue: Vec<Issue>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// The request of a transaction or batch entry.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleRequest {
    /// `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`.
    #[serde(default)]
    pub method: String,
    /// Target URL relative to the base.
    #[serde(default)]
    pub url: String,
    /// Conditional create: a search query that, when it matches, makes the
    /// server reuse the existing resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_none_exist: Option<String>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// The response of a transaction or batch entry.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleResponse {
    /// HTTP status line, such as `201 Created`.
    #[serde(default)]
    pub status: String,
    /// Location of the created resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Version tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// Issues about this entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Box<Resource>>,
    /// Unmodeled elements.
    #[serde(flatten)]
    pub extra: Extra,
}

/// One entry of a bundle.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleEntry {
    /// Absolute URL or `urn:uuid:` identifying the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_url: Option<String>,
    /// The resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Resource>,
    /// Transaction or batch request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<BundleRequest>,
    /// Transaction or batch response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<BundleResponse>,
    /// Unmodeled elements (including `search`).
    #[serde(flatten)]
    pub extra: Extra,
}

/// A bundle of resources.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bundle {
    /// Logical id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    /// Bundle identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<Identifier>,
    /// `transaction`, `collection`, `transaction-response`, … (required).
    #[serde(default, rename = "type")]
    pub kind: String,
    /// When assembled (instant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry: Vec<BundleEntry>,
    /// Unmodeled elements (including `total` and `link`).
    #[serde(flatten)]
    pub extra: Extra,
}

/// Any FHIR resource. The modeled resources are typed; every other
/// resource type is kept as its JSON object.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Resource {
    /// A patient.
    Patient(Box<Patient>),
    /// A specimen.
    Specimen(Box<Specimen>),
    /// A test order.
    ServiceRequest(Box<ServiceRequest>),
    /// An observation.
    Observation(Box<Observation>),
    /// A report.
    DiagnosticReport(Box<DiagnosticReport>),
    /// A device.
    Device(Box<Device>),
    /// A bundle.
    Bundle(Box<Bundle>),
    /// Issues.
    OperationOutcome(Box<OperationOutcome>),
    /// Any other resource: its JSON object, including `resourceType`.
    Other(Map<String, Value>),
}

impl Resource {
    /// The `resourceType`.
    pub fn resource_type(&self) -> &str {
        match self {
            Self::Patient(_) => "Patient",
            Self::Specimen(_) => "Specimen",
            Self::ServiceRequest(_) => "ServiceRequest",
            Self::Observation(_) => "Observation",
            Self::DiagnosticReport(_) => "DiagnosticReport",
            Self::Device(_) => "Device",
            Self::Bundle(_) => "Bundle",
            Self::OperationOutcome(_) => "OperationOutcome",
            Self::Other(map) => map
                .get("resourceType")
                .and_then(Value::as_str)
                .unwrap_or(""),
        }
    }

    /// The logical id, if set.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Patient(r) => r.id.as_deref(),
            Self::Specimen(r) => r.id.as_deref(),
            Self::ServiceRequest(r) => r.id.as_deref(),
            Self::Observation(r) => r.id.as_deref(),
            Self::DiagnosticReport(r) => r.id.as_deref(),
            Self::Device(r) => r.id.as_deref(),
            Self::Bundle(r) => r.id.as_deref(),
            Self::OperationOutcome(r) => r.id.as_deref(),
            Self::Other(map) => map.get("id").and_then(Value::as_str),
        }
    }

    /// Parses FHIR JSON.
    pub fn from_json(bytes: &[u8]) -> FhirResult<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    /// Writes compact FHIR JSON.
    pub fn to_json(&self) -> FhirResult<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Writes indented FHIR JSON.
    pub fn to_json_pretty(&self) -> FhirResult<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(self)?)
    }

    fn from_object(mut object: Map<String, Value>) -> FhirResult<Self> {
        let Some(Value::String(kind)) = object.get("resourceType").cloned() else {
            return Err(FhirError::Invalid("resourceType is missing".into()));
        };
        macro_rules! typed {
            ($variant:ident) => {{
                object.remove("resourceType");
                Self::$variant(Box::new(serde_json::from_value(Value::Object(object))?))
            }};
        }
        Ok(match kind.as_str() {
            "Patient" => typed!(Patient),
            "Specimen" => typed!(Specimen),
            "ServiceRequest" => typed!(ServiceRequest),
            "Observation" => typed!(Observation),
            "DiagnosticReport" => typed!(DiagnosticReport),
            "Device" => typed!(Device),
            "Bundle" => typed!(Bundle),
            "OperationOutcome" => typed!(OperationOutcome),
            _ => Self::Other(object),
        })
    }

    fn to_object(&self) -> Result<Map<String, Value>, serde_json::Error> {
        let body = match self {
            Self::Patient(r) => serde_json::to_value(r)?,
            Self::Specimen(r) => serde_json::to_value(r)?,
            Self::ServiceRequest(r) => serde_json::to_value(r)?,
            Self::Observation(r) => serde_json::to_value(r)?,
            Self::DiagnosticReport(r) => serde_json::to_value(r)?,
            Self::Device(r) => serde_json::to_value(r)?,
            Self::Bundle(r) => serde_json::to_value(r)?,
            Self::OperationOutcome(r) => serde_json::to_value(r)?,
            Self::Other(map) => return Ok(map.clone()),
        };
        let mut object = Map::new();
        object.insert(
            "resourceType".into(),
            Value::String(self.resource_type().into()),
        );
        if let Value::Object(fields) = body {
            object.extend(fields);
        }
        Ok(object)
    }
}

impl Serialize for Resource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_object()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Resource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let object = Map::deserialize(deserializer)?;
        Self::from_object(object).map_err(serde::de::Error::custom)
    }
}

macro_rules! from_resource {
    ($($variant:ident),+) => {
        $(
            impl From<$variant> for Resource {
                fn from(resource: $variant) -> Self {
                    Self::$variant(Box::new(resource))
                }
            }
        )+
    };
}

from_resource!(
    Patient,
    Specimen,
    ServiceRequest,
    Observation,
    DiagnosticReport,
    Device,
    Bundle,
    OperationOutcome
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_unknown_elements_and_precision() {
        let json = r#"{"resourceType":"Observation","id":"o1","status":"final","code":{"coding":[{"system":"http://loinc.org","code":"2345-7"}],"extension":[{"url":"urn:x","valueString":"kept"}]},"valueQuantity":{"value":5.40,"unit":"mmol/L"},"_status":{"extension":[{"url":"urn:y","valueBoolean":true}]},"component":[{"code":{"text":"c"},"valueInteger":3}]}"#;
        let resource = Resource::from_json(json.as_bytes()).unwrap();
        let Resource::Observation(observation) = &resource else {
            panic!("expected an observation");
        };
        assert_eq!(
            observation
                .value_quantity
                .as_ref()
                .unwrap()
                .value
                .as_ref()
                .unwrap()
                .as_str(),
            "5.40"
        );
        let written = String::from_utf8(resource.to_json().unwrap()).unwrap();
        let reread: Value = serde_json::from_str(&written).unwrap();
        let original: Value = serde_json::from_str(json).unwrap();
        assert_eq!(reread, original);
        assert!(written.starts_with(r#"{"resourceType":"Observation""#));
        assert!(written.contains("5.40"));
    }

    #[test]
    fn keeps_other_resources_verbatim() {
        let json =
            r#"{"resourceType":"Encounter","id":"e1","status":"finished","length":{"value":1.50}}"#;
        let resource = Resource::from_json(json.as_bytes()).unwrap();
        assert_eq!(resource.resource_type(), "Encounter");
        assert_eq!(resource.id(), Some("e1"));
        assert_eq!(
            String::from_utf8(resource.to_json().unwrap()).unwrap(),
            json
        );
    }

    #[test]
    fn reads_bundle_numbers_in_unmodeled_elements() {
        let json = r#"{"resourceType":"Bundle","type":"searchset","total":2,"entry":[{"fullUrl":"urn:uuid:1","resource":{"resourceType":"Patient","id":"p"},"search":{"score":0.80}}]}"#;
        let resource = Resource::from_json(json.as_bytes()).unwrap();
        let written = String::from_utf8(resource.to_json().unwrap()).unwrap();
        assert!(written.contains("\"total\":2"));
        assert!(written.contains("0.80"));
    }

    #[test]
    fn rejects_missing_resource_type() {
        assert!(Resource::from_json(br#"{"id":"x"}"#).is_err());
        assert!(Resource::from_json(b"[]").is_err());
    }
}
