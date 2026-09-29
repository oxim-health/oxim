//! The normalized clinical model.
//!
//! Every protocol OXIM speaks is mapped to and from these types, so a result
//! from an ASTM analyzer and a result from a POCT device leave OXIM in the
//! same shape. The model is a deliberately small subset of HL7 FHIR R4
//! (`Patient`, `Specimen`, `ServiceRequest`, `Observation`, `Device`) with
//! FHIR names and value sets, so it maps to FHIR without loss and to HL7 v2
//! and ASTM with well-defined rules (ADR 0007).
//!
//! Values are carried, never interpreted: numbers stay [`Decimal`] text,
//! times keep their recorded [`ClinicalDateTime`] precision, and abnormal
//! flags are passed through as codes (ADR 0011).

use serde::{Deserialize, Serialize};

use crate::decimal::Decimal;
use crate::time::ClinicalDateTime;

fn is_false(value: &bool) -> bool {
    !*value
}

/// A code from a code system (FHIR `Coding`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Coding {
    /// The code system, for example `http://loinc.org` or a local system name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The code.
    pub code: String,
    /// A human-readable display text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

impl Coding {
    /// A code without a system or display text.
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            system: None,
            code: code.into(),
            display: None,
        }
    }

    /// Sets the code system.
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Sets the display text.
    pub fn with_display(mut self, display: impl Into<String>) -> Self {
        self.display = Some(display.into());
        self
    }
}

/// A concept expressed by one or more codes and/or text (FHIR
/// `CodeableConcept`). The first coding is the primary one, typically the
/// code the sending system used.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct CodeableConcept {
    /// Codes for the concept, primary first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codings: Vec<Coding>,
    /// Free text for the concept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl CodeableConcept {
    /// A concept with one coding.
    pub fn from_coding(coding: Coding) -> Self {
        Self {
            codings: vec![coding],
            text: None,
        }
    }

    /// A concept with only text.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            codings: Vec::new(),
            text: Some(text.into()),
        }
    }

    /// The code of the primary coding.
    pub fn primary_code(&self) -> Option<&str> {
        self.codings.first().map(|coding| coding.code.as_str())
    }

    /// The code from a specific system, if present.
    pub fn code_in(&self, system: &str) -> Option<&str> {
        self.codings
            .iter()
            .find(|coding| coding.system.as_deref() == Some(system))
            .map(|coding| coding.code.as_str())
    }
}

/// An identifier such as a medical record number, specimen barcode or order
/// number (FHIR `Identifier`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Identifier {
    /// The namespace of the identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The identifier value.
    pub value: String,
    /// The identifier type, for example `MR` (HL7 table 0203).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The organization that issued the identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigner: Option<String>,
}

impl Identifier {
    /// An identifier with only a value.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            ..Self::default()
        }
    }
}

/// A person's name (FHIR `HumanName`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct HumanName {
    /// Family name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Given names, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub given: Vec<String>,
    /// Prefix such as `Dr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Suffix such as `Jr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

/// Administrative sex (FHIR `administrative-gender`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdministrativeSex {
    /// Male.
    Male,
    /// Female.
    Female,
    /// Other.
    Other,
    /// Unknown.
    Unknown,
}

impl AdministrativeSex {
    /// Maps an HL7 v2 table 0001 or ASTM code (`M`, `F`, `O`, `U`, `A`, `N`).
    pub fn from_hl7_code(code: &str) -> Option<Self> {
        match code.trim() {
            "M" | "m" => Some(Self::Male),
            "F" | "f" => Some(Self::Female),
            "O" | "o" | "A" | "a" => Some(Self::Other),
            "U" | "u" | "N" | "n" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// The HL7 v2 table 0001 code.
    pub fn to_hl7_code(self) -> &'static str {
        match self {
            Self::Male => "M",
            Self::Female => "F",
            Self::Other => "O",
            Self::Unknown => "U",
        }
    }
}

/// The subject of a test (FHIR `Patient`). Species and breed follow the FHIR
/// `patient-animal` extension for veterinary use.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Patient {
    /// Identifiers, primary first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<Identifier>,
    /// Name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<HumanName>,
    /// Birth date, at whatever precision is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birth_date: Option<ClinicalDateTime>,
    /// Administrative sex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sex: Option<AdministrativeSex>,
    /// Species, for veterinary patients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub species: Option<CodeableConcept>,
    /// Breed, for veterinary patients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breed: Option<CodeableConcept>,
}

/// A sample taken for testing (FHIR `Specimen`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Specimen {
    /// Identifiers such as the tube barcode or accession number.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<Identifier>,
    /// Specimen type, for example serum or whole blood.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CodeableConcept>,
    /// Collection time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collected_at: Option<ClinicalDateTime>,
    /// Time the laboratory received the specimen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<ClinicalDateTime>,
    /// Container or rack position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Free-text notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Request priority (FHIR `request-priority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Priority {
    /// Normal priority.
    Routine,
    /// Higher than routine.
    Urgent,
    /// As soon as possible.
    Asap,
    /// Immediately.
    Stat,
}

/// What an order message asks the receiver to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrderControl {
    /// A new order.
    New,
    /// Tests added to an existing order.
    Add,
    /// Cancel the order or the listed tests.
    Cancel,
    /// Replace a previous order.
    Replace,
}

/// A test request (FHIR `ServiceRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Order {
    /// The order number assigned by the placer (usually the LIS or HIS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placer_id: Option<String>,
    /// The order number assigned by the filler (usually the analyzer or lab).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filler_id: Option<String>,
    /// Requested tests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<CodeableConcept>,
    /// Priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// When the order was placed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_at: Option<ClinicalDateTime>,
    /// Specimens the order applies to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specimen_ids: Vec<String>,
    /// The requested action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<OrderControl>,
    /// Free-text notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Result status (FHIR `observation-status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationStatus {
    /// Registered, no result yet.
    Registered,
    /// Preliminary result.
    Preliminary,
    /// Final result.
    Final,
    /// Changed after being final.
    Amended,
    /// Corrected after being final.
    Corrected,
    /// Cancelled.
    Cancelled,
    /// Sent in error.
    EnteredInError,
    /// Status not known.
    #[default]
    Unknown,
}

/// A comparator in front of a quantity, as in `<0.5`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Comparator {
    /// `<`
    #[serde(rename = "<")]
    LessThan,
    /// `<=`
    #[serde(rename = "<=")]
    LessOrEqual,
    /// `>=`
    #[serde(rename = ">=")]
    GreaterOrEqual,
    /// `>`
    #[serde(rename = ">")]
    GreaterThan,
}

impl Comparator {
    /// The comparator symbol.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LessThan => "<",
            Self::LessOrEqual => "<=",
            Self::GreaterOrEqual => ">=",
            Self::GreaterThan => ">",
        }
    }

    /// Splits a leading comparator from `text`, as in `<0.5` or `>= 10`.
    pub fn split(text: &str) -> (Option<Self>, &str) {
        let text = text.trim_start();
        for (symbol, comparator) in [
            ("<=", Self::LessOrEqual),
            (">=", Self::GreaterOrEqual),
            ("<", Self::LessThan),
            (">", Self::GreaterThan),
        ] {
            if let Some(rest) = text.strip_prefix(symbol) {
                return (Some(comparator), rest.trim_start());
            }
        }
        (None, text)
    }
}

/// A measured amount (FHIR `Quantity`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Quantity {
    /// The number, exactly as reported.
    pub value: Decimal,
    /// A comparator such as `<` when the value is a limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparator: Option<Comparator>,
    /// The unit as reported, for example `mmol/L`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// The unit code system, usually `http://unitsofmeasure.org` (UCUM).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The coded unit in that system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl Quantity {
    /// A quantity with a unit as reported.
    pub fn new(value: Decimal, unit: Option<String>) -> Self {
        Self {
            value,
            comparator: None,
            unit,
            system: None,
            code: None,
        }
    }
}

/// The value of an observation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ObservationValue {
    /// A number with a unit.
    Quantity(Quantity),
    /// Free text.
    Text(String),
    /// A coded result such as `POS` or an organism code.
    Coded(CodeableConcept),
    /// A range of values.
    Range {
        /// Lower bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        low: Option<Quantity>,
        /// Upper bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        high: Option<Quantity>,
    },
    /// A ratio such as a titer `1:64`.
    Ratio {
        /// Numerator.
        numerator: Quantity,
        /// Denominator.
        denominator: Quantity,
    },
    /// A yes/no result.
    Boolean(bool),
    /// A date or time result.
    DateTime(ClinicalDateTime),
    /// Binary content such as a PDF report or a histogram image.
    Attachment {
        /// MIME type.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        /// Base64-encoded content.
        data: String,
        /// Title or file name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

/// A reference range as reported with the result.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ReferenceRange {
    /// Lower limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<Decimal>,
    /// Upper limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<Decimal>,
    /// The range as text, for example `3.9-6.1` or `negative`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// One test result (FHIR `Observation`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Observation {
    /// Position of the result in the source message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u32>,
    /// What was measured.
    pub code: CodeableConcept,
    /// The result value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<ObservationValue>,
    /// Reference range reported with the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_range: Option<ReferenceRange>,
    /// Abnormal flags as reported (HL7 table 0078 codes such as `H`, `L`,
    /// `A`), passed through without interpretation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interpretation: Vec<Coding>,
    /// Result status.
    #[serde(default)]
    pub status: ObservationStatus,
    /// When the measurement applies (for example the time of analysis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_at: Option<ClinicalDateTime>,
    /// When the result was released.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<ClinicalDateTime>,
    /// Method or instrument mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<CodeableConcept>,
    /// Identifier of the device that produced the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Operator who performed or released the test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    /// Specimen the result belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specimen_id: Option<String>,
    /// Comments attached to the result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// An instrument or system (FHIR `Device`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Device {
    /// Identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<Identifier>,
    /// Manufacturer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Serial number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    /// Software or firmware version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub software_version: Option<String>,
    /// Name given by the site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Results for one patient, specimen and order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ResultGroup {
    /// The patient, if the sender identified one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patient: Option<Patient>,
    /// The specimen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specimen: Option<Specimen>,
    /// The order the results answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Order>,
    /// The results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<Observation>,
}

/// An order for one patient and specimen.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct OrderGroup {
    /// The patient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patient: Option<Patient>,
    /// The specimen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specimen: Option<Specimen>,
    /// The order.
    pub order: Order,
}

/// A device asking which tests to run (host query), usually after reading a
/// specimen barcode.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct SpecimenQuery {
    /// Specimens asked about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specimen_ids: Vec<String>,
    /// Tests asked about; empty together with `all_tests` means all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<CodeableConcept>,
    /// Whether all tests for the specimens are requested.
    #[serde(default, skip_serializing_if = "is_false")]
    pub all_tests: bool,
    /// Start of the requested time range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub begin_at: Option<ClinicalDateTime>,
    /// End of the requested time range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_at: Option<ClinicalDateTime>,
}

/// A quality control result.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct QcResult {
    /// Control material name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// Lot number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lot: Option<String>,
    /// Control level, for example `1` or `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Expiry of the lot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<ClinicalDateTime>,
    /// The measured value.
    pub observation: Observation,
}

/// Severity of a device event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventSeverity {
    /// Information.
    Information,
    /// Warning.
    Warning,
    /// Error.
    Error,
}

/// A device status change, alarm or calibration event.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct DeviceEvent {
    /// Event code as reported.
    pub code: CodeableConcept,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// When the event occurred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<ClinicalDateTime>,
    /// Severity, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<EventSeverity>,
}

/// The normalized content of one message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClinicalContent {
    /// Test results.
    Results {
        /// The device that produced the results.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<Device>,
        /// Results grouped by patient, specimen and order.
        groups: Vec<ResultGroup>,
    },
    /// Test orders.
    Orders {
        /// Orders grouped by patient and specimen.
        groups: Vec<OrderGroup>,
    },
    /// A host query from a device.
    Query {
        /// The device that asked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<Device>,
        /// What it asked for.
        query: SpecimenQuery,
    },
    /// Quality control results.
    QualityControl {
        /// The device that ran the controls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<Device>,
        /// The control results.
        results: Vec<QcResult>,
    },
    /// A device event.
    DeviceEvent {
        /// The device.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<Device>,
        /// The event.
        event: DeviceEvent,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_compact_fhir_like_json() {
        let observation = Observation {
            code: CodeableConcept::from_coding(
                Coding::new("GLU")
                    .with_system("urn:oxim:local")
                    .with_display("Glucose"),
            ),
            value: Some(ObservationValue::Quantity(Quantity::new(
                Decimal::new("5.40").unwrap(),
                Some("mmol/L".into()),
            ))),
            interpretation: vec![Coding::new("N")],
            status: ObservationStatus::Final,
            ..Observation::default()
        };
        let json = serde_json::to_value(&observation).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "code": {"codings": [{"system": "urn:oxim:local", "code": "GLU", "display": "Glucose"}]},
                "value": {"type": "quantity", "value": {"value": "5.40", "unit": "mmol/L"}},
                "interpretation": [{"code": "N"}],
                "status": "final"
            })
        );
        let back: Observation = serde_json::from_value(json).unwrap();
        assert_eq!(back, observation);
    }

    #[test]
    fn content_is_tagged_by_kind() {
        let content = ClinicalContent::Query {
            device: None,
            query: SpecimenQuery {
                specimen_ids: vec!["S123".into()],
                all_tests: true,
                ..SpecimenQuery::default()
            },
        };
        let json = serde_json::to_string(&content).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"query","query":{"specimen_ids":["S123"],"all_tests":true}}"#
        );
        assert_eq!(
            serde_json::from_str::<ClinicalContent>(&json).unwrap(),
            content
        );
        assert_eq!(
            serde_json::to_string(&ObservationStatus::EnteredInError).unwrap(),
            "\"entered-in-error\""
        );
    }

    #[test]
    fn splits_comparators() {
        assert_eq!(
            Comparator::split("<0.5"),
            (Some(Comparator::LessThan), "0.5")
        );
        assert_eq!(
            Comparator::split(">= 10"),
            (Some(Comparator::GreaterOrEqual), "10")
        );
        assert_eq!(Comparator::split("5"), (None, "5"));
        assert_eq!(
            AdministrativeSex::from_hl7_code("F"),
            Some(AdministrativeSex::Female)
        );
        assert_eq!(AdministrativeSex::from_hl7_code("X"), None);
    }
}

#[cfg(test)]
mod number_tests {
    use super::*;

    /// Numbers inside the tagged `ClinicalContent` must survive JSON even
    /// when serde_json's `arbitrary_precision` feature is enabled elsewhere
    /// in the build.
    #[test]
    fn numbers_inside_tagged_content_round_trip() {
        let content = ClinicalContent::Results {
            device: None,
            groups: vec![ResultGroup {
                observations: vec![Observation {
                    sequence: Some(3),
                    code: CodeableConcept::from_coding(Coding::new("GLU")),
                    ..Observation::default()
                }],
                ..ResultGroup::default()
            }],
        };
        let json = serde_json::to_string(&content).unwrap();
        let back: ClinicalContent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, content);
    }
}
