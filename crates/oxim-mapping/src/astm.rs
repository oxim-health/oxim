//! ASTM E1394 (CLSI LIS02) and the normalized model.
//!
//! Field numbers follow ASTM, where the record type is field 1.
//!
//! # Records to model
//!
//! | ASTM | Model |
//! |---|---|
//! | H-5 (sender: name^software version^serial number) | `Device` name, software version, serial number |
//! | P-3 practice-assigned patient ID | `Patient.identifiers`, kind `PI` |
//! | P-4 laboratory-assigned patient ID | `Patient.identifiers`, kind `LR` |
//! | P-5 patient ID number 3 | `Patient.identifiers`, no kind |
//! | P-6 name (last^first^middle^suffix^title) | `Patient.name` |
//! | P-8 birth date | `Patient.birth_date` |
//! | P-9 sex (`M`, `F`, `U`) | `Patient.sex` |
//! | O-3 specimen ID (id^further components) | `Specimen.identifiers[0]`; further components become `Specimen.container` |
//! | O-4 instrument specimen ID | `Specimen.identifiers`, kind `instrument` |
//! | O-5 universal test IDs (repeated) | `Order.tests` |
//! | O-6 priority (`S`, `A`, `R`) | `Order.priority` |
//! | O-7 requested date/time | `Order.requested_at` |
//! | O-8 collection date/time | `Specimen.collected_at` |
//! | O-12 action code (`N`, `A`, `C`, `Q`) | `Order.control` New/Add/Cancel; `Q` marks quality control |
//! | O-16 specimen descriptor | `Specimen.kind` |
//! | R-2 sequence | `Observation.sequence` |
//! | R-3 universal test ID | `Observation.code` |
//! | R-4 value (first component) | `Observation.value`: a `Quantity` when numeric (with `<`/`>` comparators), otherwise text |
//! | R-5 units | `Quantity.unit` |
//! | R-6 reference range | `Observation.reference_range` |
//! | R-7 abnormal flags (repeated) | `Observation.interpretation`, codes unchanged |
//! | R-9 result status | `Observation.status` (see [`status_from_astm`]) |
//! | R-11 operator (first component) | `Observation.operator` |
//! | R-13 completed, else R-12 started | `Observation.effective_at` |
//! | R-14 instrument | `Observation.device_id` |
//! | C-4 comment text | `notes` of the preceding patient, order or result |
//! | Q-3 starting range ID (patient^specimen), Q-4 ending range ID | `SpecimenQuery.specimen_ids` (specimen component, else first component) |
//! | Q-5 tests (`ALL` or empty means all) | `SpecimenQuery.tests`, `all_tests` |
//! | Q-7, Q-8 begin/end date/time | `SpecimenQuery.begin_at`, `end_at` |
//!
//! A universal test ID `universal^name^type^local code^...` becomes a
//! concept whose codings are the local code ([`ASTM_LOCAL`]), the universal
//! ID when present (LOINC when component 3 is `LN` or `LOINC`), and the
//! complete field ([`ASTM_TEST_ID`]) so instrument-specific components such
//! as dilution survive re-encoding. Component 2 becomes the concept text.
//!
//! A message with `Q` records maps to `Query`; one with `R` records to
//! `Results`, or to `QualityControl` when every order with results has
//! action code `Q` (mixed messages stay `Results`); one with only `O`
//! records to `Orders`.
//!
//! # Model to records
//!
//! [`encode_orders`] writes `H`, then `P` and `O` records per order group,
//! then `L`, using the same field positions. The raw universal test ID is
//! reused when present, otherwise `^^^local code`. Date/times are written
//! as `YYYYMMDDHHMMSS` at the recorded precision.

use encoding_rs::{Encoding, UTF_8};
use oxim_astm::{Delimiters, RecordRef, Value};
use oxim_model::{
    AdministrativeSex, ClinicalContent, CodeableConcept, Coding, Device, HumanName, Identifier,
    Observation, Order, OrderControl, OrderGroup, Patient, QcResult, ResultGroup, Specimen,
    SpecimenQuery, Timestamp,
};

use crate::codes::{ASTM_LOCAL, ASTM_TEST_ID, ASTM_UNIVERSAL, LOINC, coding, pick};
use crate::error::{MappingError, MappingResult};
use crate::values::{
    astm_datetime, datetime, nonempty, priority_code, priority_from_code, reference_range,
    status_from_astm, value_from_text,
};

fn text(value: Option<Value<'_>>) -> Option<String> {
    value.and_then(|v| nonempty(v.to_string_lossy()))
}

fn get(record: &RecordRef<'_>, path: &str) -> Option<String> {
    text(record.get(path))
}

/// A concept from one universal test ID (a field repetition).
fn test_concept(value: Value<'_>) -> Option<CodeableConcept> {
    let components: Vec<String> = value
        .components()
        .map(|c| c.to_string_lossy().trim().to_owned())
        .collect();
    let part = |i: usize| components.get(i).map(String::as_str).unwrap_or_default();
    let name = nonempty(part(1));
    let mut codings = Vec::new();
    codings.extend(coding(
        part(3),
        name.as_deref(),
        Some(ASTM_LOCAL.to_owned()),
    ));
    let universal_system = match part(2).to_ascii_uppercase().as_str() {
        "LN" | "LOINC" => LOINC,
        _ => ASTM_UNIVERSAL,
    };
    codings.extend(coding(
        part(0),
        name.as_deref(),
        Some(universal_system.to_owned()),
    ));
    let raw = value.to_string_lossy().into_owned();
    if codings.is_empty() && name.is_none() {
        return None;
    }
    codings.push(Coding {
        system: Some(ASTM_TEST_ID.to_owned()),
        code: raw,
        display: None,
    });
    Some(CodeableConcept {
        codings,
        text: name,
    })
}

fn device(message: &oxim_astm::Message) -> Option<Device> {
    let header = message.header();
    let sender = header.field(5)?;
    let device = Device {
        name: text(sender.component(1)),
        software_version: text(sender.component(2)),
        serial_number: text(sender.component(3)),
        ..Device::default()
    };
    (device != Device::default()).then_some(device)
}

fn patient(record: &RecordRef<'_>) -> Patient {
    let identifier = |path: &str, kind: Option<&str>| {
        get(record, path).map(|value| Identifier {
            value,
            kind: kind.map(str::to_owned),
            ..Identifier::default()
        })
    };
    let identifiers = [
        identifier("3", Some("PI")),
        identifier("4", Some("LR")),
        identifier("5", None),
    ]
    .into_iter()
    .flatten()
    .collect();
    let name = record.field(6).map(|name| HumanName {
        family: text(name.component(1)),
        given: [text(name.component(2)), text(name.component(3))]
            .into_iter()
            .flatten()
            .collect(),
        suffix: text(name.component(4)),
        prefix: text(name.component(5)),
    });
    Patient {
        identifiers,
        name: name.filter(|n| *n != HumanName::default()),
        birth_date: get(record, "8").and_then(|t| datetime(&t)),
        sex: get(record, "9").and_then(|code| AdministrativeSex::from_hl7_code(&code)),
        ..Patient::default()
    }
}

fn comments(records: &[RecordRef<'_>]) -> Vec<String> {
    records.iter().filter_map(|c| get(c, "4")).collect()
}

struct AstmOrder {
    specimen: Option<Specimen>,
    order: Order,
    quality_control: bool,
}

fn order(record: &RecordRef<'_>, notes: Vec<String>) -> AstmOrder {
    let mut specimen = Specimen::default();
    if let Some(field) = record.field(3) {
        if let Some(id) = text(field.component(1)) {
            specimen.identifiers.push(Identifier::new(id));
        }
        let rest: Vec<String> = field
            .components()
            .skip(1)
            .map(|c| c.to_string_lossy().into_owned())
            .collect();
        if rest.iter().any(|c| !c.trim().is_empty()) {
            specimen.container = Some(rest.join("^"));
        }
    }
    if let Some(id) = get(record, "4") {
        specimen.identifiers.push(Identifier {
            value: id,
            kind: Some("instrument".into()),
            ..Identifier::default()
        });
    }
    specimen.collected_at = get(record, "8").and_then(|t| datetime(&t));
    specimen.kind =
        text(record.get("16.1")).map(|code| CodeableConcept::from_coding(Coding::new(code)));
    let action = get(record, "12");
    let order = Order {
        tests: record
            .field(5)
            .map(|tests| tests.repetitions().filter_map(test_concept).collect())
            .unwrap_or_default(),
        priority: get(record, "6").and_then(|p| priority_from_code(&p)),
        requested_at: get(record, "7").and_then(|t| datetime(&t)),
        specimen_ids: specimen
            .identifiers
            .first()
            .map(|id| vec![id.value.clone()])
            .unwrap_or_default(),
        control: match action.as_deref() {
            Some("N") => Some(OrderControl::New),
            Some("A") => Some(OrderControl::Add),
            Some("C") => Some(OrderControl::Cancel),
            _ => None,
        },
        notes,
        ..Order::default()
    };
    AstmOrder {
        specimen: (specimen != Specimen::default()).then_some(specimen),
        order,
        quality_control: action.as_deref() == Some("Q"),
    }
}

fn observation(
    record: &RecordRef<'_>,
    specimen_id: Option<&str>,
    notes: Vec<String>,
) -> Observation {
    let unit = get(record, "5");
    Observation {
        sequence: get(record, "2").and_then(|s| s.parse().ok()),
        code: record
            .field(3)
            .and_then(|f| f.repetition(1))
            .and_then(test_concept)
            .unwrap_or_default(),
        value: record
            .get("4.1")
            .and_then(|v| value_from_text(&v.to_string_lossy(), unit.as_deref())),
        reference_range: get(record, "6").and_then(|r| reference_range(&r)),
        interpretation: record
            .field(7)
            .map(|flags| {
                flags
                    .repetitions()
                    .filter_map(|flag| nonempty(flag.to_string_lossy()))
                    .map(Coding::new)
                    .collect()
            })
            .unwrap_or_default(),
        status: status_from_astm(&get(record, "9").unwrap_or_default()),
        operator: get(record, "11.1"),
        effective_at: get(record, "13")
            .or_else(|| get(record, "12"))
            .and_then(|t| datetime(&t)),
        device_id: get(record, "14"),
        specimen_id: specimen_id.map(str::to_owned),
        notes,
        ..Observation::default()
    }
}

fn query(message: &oxim_astm::Message, record: &RecordRef<'_>) -> ClinicalContent {
    let mut query = SpecimenQuery::default();
    for field in [3, 4] {
        let Some(range) = record.field(field) else {
            continue;
        };
        for id in range.repetitions() {
            let specimen = text(id.component(2)).or_else(|| text(id.component(1)));
            if let Some(specimen) = specimen
                && !query.specimen_ids.contains(&specimen)
            {
                query.specimen_ids.push(specimen);
            }
        }
    }
    let mut tests = Vec::new();
    let mut all = false;
    if let Some(field) = record.field(5) {
        for test in field.repetitions() {
            let code = text(test.component(4)).or_else(|| text(test.component(1)));
            match code {
                Some(code) if code.eq_ignore_ascii_case("ALL") => all = true,
                Some(_) => tests.extend(test_concept(test)),
                None => {}
            }
        }
    }
    query.all_tests = all || tests.is_empty();
    query.tests = tests;
    query.begin_at = get(record, "7").and_then(|t| datetime(&t));
    query.end_at = get(record, "8").and_then(|t| datetime(&t));
    ClinicalContent::Query {
        device: device(message),
        query,
    }
}

/// Maps an ASTM message to normalized content (see the module
/// documentation for the rules).
pub fn normalize(message: &oxim_astm::Message) -> MappingResult<ClinicalContent> {
    if let Some(record) = message.record("Q", 1) {
        return Ok(query(message, &record));
    }
    let mut groups = Vec::new();
    let mut quality_control = Vec::new();
    let mut orders_only = Vec::new();
    let mut patient_results = false;
    for patient_group in message.patients() {
        let patient = patient_group.patient.as_ref().map(patient);
        // The model's patient has no notes; patient comments are attached to
        // the notes of each of the patient's orders instead.
        let patient_notes = comments(&patient_group.comments);
        for order_group in patient_group.orders {
            let mut notes = patient_notes.clone();
            notes.extend(comments(&order_group.comments));
            let parsed = order_group
                .order
                .as_ref()
                .map(|record| order(record, notes.clone()));
            let specimen_id = parsed
                .as_ref()
                .and_then(|o| o.order.specimen_ids.first().cloned());
            let observations: Vec<Observation> = order_group
                .results
                .iter()
                .map(|result| {
                    observation(
                        &result.result,
                        specimen_id.as_deref(),
                        comments(&result.comments),
                    )
                })
                .collect();
            let quality = parsed.as_ref().is_some_and(|o| o.quality_control);
            if observations.is_empty() {
                if let Some(parsed) = parsed {
                    orders_only.push(OrderGroup {
                        patient: patient.clone(),
                        specimen: parsed.specimen,
                        order: parsed.order,
                    });
                }
                continue;
            }
            if quality {
                let material = specimen_id.clone();
                quality_control.extend(observations.iter().cloned().map(|observation| QcResult {
                    material: material.clone(),
                    observation,
                    ..QcResult::default()
                }));
            } else {
                patient_results = true;
            }
            let (specimen, order) = match parsed {
                Some(parsed) => (parsed.specimen, Some(parsed.order)),
                None => (None, None),
            };
            groups.push(ResultGroup {
                patient: patient.clone(),
                specimen,
                order,
                observations,
            });
        }
    }
    if !groups.is_empty() {
        if !patient_results {
            return Ok(ClinicalContent::QualityControl {
                device: device(message),
                results: quality_control,
            });
        }
        return Ok(ClinicalContent::Results {
            device: device(message),
            groups,
        });
    }
    if !orders_only.is_empty() {
        return Ok(ClinicalContent::Orders {
            groups: orders_only,
        });
    }
    Err(MappingError::Unsupported(
        "the ASTM message has no order, result or query records".into(),
    ))
}

/// Settings for [`encode_orders`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AstmEncoding {
    /// H-5, the sender name.
    pub sender: Option<String>,
    /// H-10, the receiver ID.
    pub receiver: Option<String>,
    /// H-12, the processing ID (`P` production, `T` training, `D` debug).
    pub processing_id: String,
    /// H-13, the version.
    pub version: String,
    /// O-26 report type: `O` for orders, `Q` for a response to a query.
    pub report_type: String,
    /// UTC offset in minutes for the H-14 timestamp.
    pub utc_offset_minutes: i16,
    /// Character encoding of the text.
    pub encoding: &'static Encoding,
}

impl Default for AstmEncoding {
    fn default() -> Self {
        Self {
            sender: Some("OXIM".into()),
            receiver: None,
            processing_id: "P".into(),
            version: "LIS2-A2".into(),
            report_type: "O".into(),
            utc_offset_minutes: 0,
            encoding: UTF_8,
        }
    }
}

struct Writer {
    message: oxim_astm::Message,
    encoding: &'static Encoding,
    counts: std::collections::BTreeMap<&'static str, usize>,
}

impl Writer {
    fn new(encoding: &'static Encoding) -> MappingResult<Self> {
        let message =
            oxim_astm::Message::new(Delimiters::default()).map_err(|e| MappingError::Write {
                path: "H".into(),
                detail: e.to_string(),
            })?;
        Ok(Self {
            message,
            encoding,
            counts: std::collections::BTreeMap::new(),
        })
    }

    fn record(&mut self, kind: &'static str) -> MappingResult<usize> {
        self.message
            .push_record(kind)
            .map_err(|e| MappingError::Write {
                path: kind.into(),
                detail: e.to_string(),
            })?;
        let count = self.counts.entry(kind).or_default();
        *count += 1;
        Ok(*count)
    }

    fn path(kind: &str, occurrence: usize, field: &str) -> String {
        if kind == "H" {
            format!("H-{field}")
        } else {
            format!("{kind}[{occurrence}]-{field}")
        }
    }

    fn text(
        &mut self,
        kind: &str,
        occurrence: usize,
        field: &str,
        value: Option<&str>,
    ) -> MappingResult<()> {
        let Some(value) = value.filter(|v| !v.is_empty()) else {
            return Ok(());
        };
        let path = Self::path(kind, occurrence, field);
        self.message
            .set_text(&path, value, self.encoding)
            .map_err(|e| MappingError::Write {
                path,
                detail: e.to_string(),
            })
    }

    fn raw(&mut self, kind: &str, occurrence: usize, field: &str, raw: &[u8]) -> MappingResult<()> {
        if raw.is_empty() {
            return Ok(());
        }
        let path = Self::path(kind, occurrence, field);
        self.message
            .set_raw(&path, raw)
            .map_err(|e| MappingError::Write {
                path,
                detail: e.to_string(),
            })
    }
}

/// The raw O-5 repetition for a test concept.
fn test_field(
    concept: &CodeableConcept,
    delimiters: &Delimiters,
    encoding: &'static Encoding,
) -> Option<Vec<u8>> {
    if let Some(raw) = concept
        .codings
        .iter()
        .find(|c| c.system.as_deref() == Some(ASTM_TEST_ID))
        && !raw
            .code
            .bytes()
            .any(|b| b == delimiters.field || b == delimiters.repeat || b == delimiters.escape)
    {
        return Some(encoding.encode(&raw.code).0.into_owned());
    }
    let code = pick(concept, &[ASTM_LOCAL])?;
    let (bytes, _, _) = encoding.encode(&code.code);
    let escaped = oxim_astm::escape(&bytes, delimiters);
    let mut out = vec![delimiters.component; 3];
    out.extend_from_slice(&escaped);
    Some(out)
}

fn write_patient(
    writer: &mut Writer,
    sequence: usize,
    patient: Option<&Patient>,
) -> MappingResult<()> {
    let n = writer.record("P")?;
    writer.text("P", n, "2", Some(&sequence.to_string()))?;
    let Some(patient) = patient else {
        return Ok(());
    };
    let by_kind = |kind: Option<&str>| {
        patient
            .identifiers
            .iter()
            .find(|id| id.kind.as_deref() == kind)
            .map(|id| id.value.as_str())
    };
    let practice =
        by_kind(Some("PI")).or_else(|| patient.identifiers.first().map(|id| id.value.as_str()));
    writer.text("P", n, "3", practice)?;
    writer.text("P", n, "4", by_kind(Some("LR")))?;
    if let Some(name) = &patient.name {
        writer.text("P", n, "6.1", name.family.as_deref())?;
        writer.text("P", n, "6.2", name.given.first().map(String::as_str))?;
        writer.text("P", n, "6.3", name.given.get(1).map(String::as_str))?;
    }
    let birth = patient.birth_date.as_ref().map(astm_datetime);
    writer.text("P", n, "8", birth.as_deref())?;
    writer.text("P", n, "9", patient.sex.map(AdministrativeSex::to_hl7_code))?;
    Ok(())
}

/// Writes orders as an ASTM message (`H`, `P`/`O` per group, `L`) for
/// worklist download or as the answer to a host query.
///
/// An `Orders` content without groups produces `H` and `L` with termination
/// code `I` (no information available), the reply to a query for unknown
/// specimens.
pub fn encode_orders(
    content: &ClinicalContent,
    settings: &AstmEncoding,
    timestamp: Timestamp,
) -> MappingResult<oxim_astm::Message> {
    let ClinicalContent::Orders { groups } = content else {
        return Err(MappingError::WrongContent {
            encoder: "astm-orders",
            found: kind_name(content),
        });
    };
    let delimiters = Delimiters::default();
    let mut writer = Writer::new(settings.encoding)?;
    writer.text("H", 1, "5", settings.sender.as_deref())?;
    writer.text("H", 1, "10", settings.receiver.as_deref())?;
    writer.text("H", 1, "12", Some(&settings.processing_id))?;
    writer.text("H", 1, "13", Some(&settings.version))?;
    let stamp =
        oxim_model::ClinicalDateTime::from_timestamp(timestamp, settings.utc_offset_minutes)
            .map(|t| astm_datetime(&t));
    writer.text("H", 1, "14", stamp.as_deref())?;

    let mut patient_sequence = 0;
    let mut last_patient: Option<&Option<Patient>> = None;
    let mut order_sequence = 0;
    for group in groups {
        if last_patient != Some(&group.patient) {
            patient_sequence += 1;
            order_sequence = 0;
            write_patient(&mut writer, patient_sequence, group.patient.as_ref())?;
            last_patient = Some(&group.patient);
        }
        order_sequence += 1;
        let n = writer.record("O")?;
        writer.text("O", n, "2", Some(&order_sequence.to_string()))?;
        let specimen_id = group
            .specimen
            .as_ref()
            .and_then(|s| s.identifiers.first().map(|id| id.value.clone()))
            .or_else(|| group.order.specimen_ids.first().cloned());
        writer.text("O", n, "3", specimen_id.as_deref())?;
        let mut tests = Vec::new();
        for test in &group.order.tests {
            if let Some(field) = test_field(test, &delimiters, settings.encoding) {
                if !tests.is_empty() {
                    tests.push(delimiters.repeat);
                }
                tests.extend(field);
            }
        }
        writer.raw("O", n, "5", &tests)?;
        writer.text("O", n, "6", group.order.priority.map(priority_code))?;
        let requested = group.order.requested_at.as_ref().map(astm_datetime);
        writer.text("O", n, "7", requested.as_deref())?;
        let collected = group
            .specimen
            .as_ref()
            .and_then(|s| s.collected_at.as_ref())
            .map(astm_datetime);
        writer.text("O", n, "8", collected.as_deref())?;
        let action = group.order.control.map(|control| match control {
            OrderControl::New | OrderControl::Replace => "N",
            OrderControl::Add => "A",
            OrderControl::Cancel => "C",
        });
        writer.text("O", n, "12", action)?;
        let kind = group
            .specimen
            .as_ref()
            .and_then(|s| s.kind.as_ref())
            .and_then(|k| {
                k.primary_code()
                    .map(str::to_owned)
                    .or_else(|| k.text.clone())
            });
        writer.text("O", n, "16", kind.as_deref())?;
        writer.text("O", n, "26", Some(&settings.report_type))?;
    }
    let n = writer.record("L")?;
    writer.text("L", n, "2", Some("1"))?;
    writer.text("L", n, "3", Some(if groups.is_empty() { "I" } else { "N" }))?;
    Ok(writer.message)
}

/// A short name of the content kind, for error messages.
pub(crate) fn kind_name(content: &ClinicalContent) -> &'static str {
    match content {
        ClinicalContent::Results { .. } => "results",
        ClinicalContent::Orders { .. } => "orders",
        ClinicalContent::Query { .. } => "query",
        ClinicalContent::QualityControl { .. } => "quality control",
        ClinicalContent::DeviceEvent { .. } => "device event",
    }
}
