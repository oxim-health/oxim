//! HL7 v2 and the normalized model.
//!
//! # Messages to model
//!
//! | Message | Content |
//! |---|---|
//! | `ORU`, `OUL` | `Results` |
//! | `ORM`, `OML` | `Orders` |
//! | `QRY` with `QRD` (QRD-8 subject filter), `QBP` with `QPD` (QPD-3, IHE LAW work order query) | `Query` |
//! | anything else | not supported |
//!
//! | Segment | Model |
//! |---|---|
//! | MSH-3 sending application | `Device.name` of results |
//! | PID-3 (repeated CX: id^^^assigning authority^type) | `Patient.identifiers` (value, assigner, kind) |
//! | PID-5 (first XPN: family^given^middle^suffix^prefix) | `Patient.name` |
//! | PID-7, PID-8 | `birth_date`, `sex` |
//! | PID-35, PID-36 | `species`, `breed` |
//! | ORC-1 (`NW`, `CA`/`OC`, `XO`), ORC-2, ORC-3 | `Order.control`, `placer_id`, `filler_id` |
//! | OBR-2, OBR-3, OBR-4 | `Order.placer_id`, `filler_id`, `tests` |
//! | OBR-7, OBR-15 | `Specimen.collected_at`, `Specimen.kind` (when no SPM) |
//! | TQ1-9 priority | `Order.priority` |
//! | SPM-2 (placer, else filler id), SPM-4, SPM-17, SPM-18 | `Specimen` identifier, kind, collected, received |
//! | OBX-1 | `Observation.sequence` |
//! | OBX-3 (CWE) | `Observation.code` |
//! | OBX-2 + OBX-5 | `Observation.value`: `NM` quantity; `SN` quantity with comparator, range (`-`) or ratio (`:`, `/`); `ST`/`TX`/`FT` text (repetitions joined by line breaks); `CE`/`CWE`/`CNE` coded; `DT`/`DTM`/`TS` date/time; `ED` attachment |
//! | OBX-6 (CWE) | `Quantity.unit`; UCUM units also fill `system` and `code` |
//! | OBX-7 | `reference_range` |
//! | OBX-8 (repeated) | `interpretation`, codes unchanged |
//! | OBX-11 | `status` (see [`status_from_hl7`]) |
//! | OBX-14, else OBX-19 | `effective_at` |
//! | OBX-16 (first of id or family name) | `operator` |
//! | OBX-17 | `method` |
//! | OBX-18 | `device_id` |
//! | NTE-3 | notes of the preceding observation, else of the order |
//!
//! Coded fields use the first triplet (code^text^system) and the alternate
//! triplet (components 4 to 6); system names are translated as described
//! in [`crate::codes`].
//!
//! # Model to messages
//!
//! [`encode_results`] writes `ORU^R01`: MSH, then per result group PID (when
//! the patient changes), ORC, OBR, SPM, OBX and NTE, using the positions
//! above. [`encode_orders`] writes `OML^O21`: per ordered test ORC, TQ1
//! (when a priority is set) and OBR, with PID and SPM per group.
//! [`encode_work_orders`] writes the specimen-centric IHE LAW work order
//! `OML^O33` (SPM and SAC, then ORC, TQ1 and OBR per test), and
//! [`encode_query_response`] answers a `QBP` host query with `RSP^K11`.
//!
//! In specimen-first layouts (`OML^O33`, `OUL^R22`) an SPM that precedes
//! the orders applies to every order and result after it, up to the next
//! SPM or PID.

use std::collections::BTreeMap;

use encoding_rs::{Encoding, UTF_8};
use oxim_hl7::{SegmentRef, Value};
use oxim_model::{
    AdministrativeSex, ClinicalContent, ClinicalDateTime, CodeableConcept, Coding, Comparator,
    Decimal, Device, HumanName, Identifier, MessageId, Observation, ObservationStatus,
    ObservationValue, Order, OrderControl, OrderGroup, Patient, Precision, Quantity, ResultGroup,
    Specimen, SpecimenQuery, Timestamp,
};

use crate::astm::kind_name;
use crate::codes::{ASTM_LOCAL, UCUM, coding, pick, system_from_hl7, system_to_hl7};
use crate::error::{MappingError, MappingResult};
use crate::values::{
    datetime, hl7_datetime, nonempty, priority_code, priority_from_code, range_text,
    reference_range, status_from_hl7, status_to_hl7, value_from_text,
};

struct Reader {
    encoding: &'static Encoding,
}

impl Reader {
    fn text(&self, value: Option<Value<'_>>) -> Option<String> {
        value.and_then(|v| nonempty(v.to_text(self.encoding)))
    }

    fn get(&self, segment: &SegmentRef<'_>, path: &str) -> Option<String> {
        self.text(segment.get(path))
    }

    fn concept(&self, value: Option<Value<'_>>) -> Option<CodeableConcept> {
        let value = value?;
        let part = |i: usize| self.text(value.component(i));
        let mut codings = Vec::new();
        for start in [1, 4] {
            if let Some(code) = part(start) {
                codings.extend(coding(
                    &code,
                    part(start + 1).as_deref(),
                    part(start + 2).and_then(|s| system_from_hl7(&s)),
                ));
            }
        }
        let text = part(9).or_else(|| part(2));
        (!codings.is_empty() || text.is_some()).then_some(CodeableConcept { codings, text })
    }

    fn patient(&self, segment: &SegmentRef<'_>) -> Patient {
        let mut identifiers = Vec::new();
        for field in [3, 2, 4] {
            let Some(list) = segment.field(field) else {
                continue;
            };
            for cx in list.repetitions() {
                if let Some(value) = self.text(cx.component(1)) {
                    identifiers.push(Identifier {
                        value,
                        assigner: self.text(cx.component(4).and_then(|hd| hd.subcomponent(1))),
                        kind: self.text(cx.component(5)),
                        system: None,
                    });
                }
            }
        }
        let name = segment
            .field(5)
            .and_then(|names| names.repetition(1))
            .map(|xpn| HumanName {
                family: self.text(xpn.component(1).and_then(|fn_| fn_.subcomponent(1))),
                given: [self.text(xpn.component(2)), self.text(xpn.component(3))]
                    .into_iter()
                    .flatten()
                    .collect(),
                suffix: self.text(xpn.component(4)),
                prefix: self.text(xpn.component(5)),
            });
        Patient {
            identifiers,
            name: name.filter(|n| *n != HumanName::default()),
            birth_date: self.get(segment, "7.1").and_then(|t| datetime(&t)),
            sex: self
                .get(segment, "8")
                .and_then(|code| AdministrativeSex::from_hl7_code(&code)),
            species: self.concept(segment.field(35)),
            breed: self.concept(segment.field(36)),
        }
    }

    fn specimen(&self, segment: &SegmentRef<'_>) -> Specimen {
        let id = self
            .get(segment, "2.1.1")
            .or_else(|| self.get(segment, "2.2.1"));
        Specimen {
            identifiers: id.map(Identifier::new).into_iter().collect(),
            kind: self.concept(segment.field(4)),
            collected_at: self.get(segment, "17.1").and_then(|t| datetime(&t)),
            received_at: self.get(segment, "18").and_then(|t| datetime(&t)),
            ..Specimen::default()
        }
    }

    fn units(&self, segment: &SegmentRef<'_>) -> Option<(String, Option<String>, Option<String>)> {
        let code = self.get(segment, "6.1");
        let text = self.get(segment, "6.2");
        let system = self.get(segment, "6.3").and_then(|s| system_from_hl7(&s));
        let unit = code.clone().or(text)?;
        let ucum = system.as_deref() == Some(UCUM);
        Some((
            unit,
            ucum.then(|| UCUM.to_owned()),
            if ucum { code } else { None },
        ))
    }

    fn quantity(
        &self,
        text: &str,
        comparator: Option<Comparator>,
        units: Option<&(String, Option<String>, Option<String>)>,
    ) -> Option<Quantity> {
        let value = Decimal::new(text.trim()).ok()?;
        Some(Quantity {
            value,
            comparator,
            unit: units.map(|u| u.0.clone()),
            system: units.and_then(|u| u.1.clone()),
            code: units.and_then(|u| u.2.clone()),
        })
    }

    fn value(&self, segment: &SegmentRef<'_>) -> Option<ObservationValue> {
        let field = segment.field(5)?;
        let first = field.repetition(1)?;
        let units = self.units(segment);
        let value_type = self.get(segment, "2").unwrap_or_default();
        match value_type.as_str() {
            "SN" => {
                let part = |i: usize| self.text(first.component(i)).unwrap_or_default();
                let (sign, first_number, separator, second_number) =
                    (part(1), part(2), part(3), part(4));
                match separator.as_str() {
                    ":" | "/" => Some(ObservationValue::Ratio {
                        numerator: self.quantity(&first_number, None, None)?,
                        denominator: self.quantity(&second_number, None, None)?,
                    }),
                    "-" => Some(ObservationValue::Range {
                        low: self.quantity(&first_number, None, units.as_ref()),
                        high: self.quantity(&second_number, None, units.as_ref()),
                    }),
                    _ => {
                        let comparator = match sign.as_str() {
                            "" | "=" => None,
                            other => match Comparator::split(other) {
                                (Some(comparator), "") => Some(comparator),
                                _ => None,
                            },
                        };
                        self.quantity(&first_number, comparator, units.as_ref())
                            .map(ObservationValue::Quantity)
                            .or_else(|| self.text(Some(first)).map(ObservationValue::Text))
                    }
                }
            }
            "ST" | "TX" | "FT" => {
                let lines: Vec<String> = field
                    .repetitions()
                    .map(|line| line.to_text(self.encoding).into_owned())
                    .collect();
                nonempty(lines.join("\n")).map(|_| ObservationValue::Text(lines.join("\n")))
            }
            "CE" | "CWE" | "CNE" | "CF" => self.concept(Some(first)).map(ObservationValue::Coded),
            "DT" | "DTM" | "TS" => {
                let text = self.text(first.component(1))?;
                Some(match datetime(&text) {
                    Some(value) => ObservationValue::DateTime(value),
                    None => ObservationValue::Text(text),
                })
            }
            "ED" => {
                let part = |i: usize| self.text(first.component(i));
                let data = part(5)?;
                let content_type = match (part(2).as_deref(), part(3)) {
                    (Some("IM"), Some(subtype)) => {
                        Some(format!("image/{}", subtype.to_ascii_lowercase()))
                    }
                    (Some("TX"), Some(subtype)) => {
                        Some(format!("text/{}", subtype.to_ascii_lowercase()))
                    }
                    (_, Some(subtype)) => {
                        Some(format!("application/{}", subtype.to_ascii_lowercase()))
                    }
                    _ => None,
                };
                Some(ObservationValue::Attachment {
                    content_type,
                    data,
                    title: part(1),
                })
            }
            _ => {
                let text = first.to_text(self.encoding);
                match value_from_text(&text, None)? {
                    ObservationValue::Quantity(q) => {
                        let (comparator, number) = (q.comparator, q.value);
                        self.quantity(number.as_str(), comparator, units.as_ref())
                            .map(ObservationValue::Quantity)
                    }
                    other => Some(other),
                }
            }
        }
    }

    fn observation(&self, segment: &SegmentRef<'_>, specimen_id: Option<&str>) -> Observation {
        Observation {
            sequence: self.get(segment, "1").and_then(|s| s.parse().ok()),
            code: self.concept(segment.field(3)).unwrap_or_default(),
            value: self.value(segment),
            reference_range: self.get(segment, "7").and_then(|r| reference_range(&r)),
            interpretation: segment
                .field(8)
                .map(|flags| {
                    flags
                        .repetitions()
                        .filter_map(|flag| self.text(Some(flag)))
                        .map(Coding::new)
                        .collect()
                })
                .unwrap_or_default(),
            status: status_from_hl7(&self.get(segment, "11").unwrap_or_default()),
            effective_at: self
                .get(segment, "14.1")
                .or_else(|| self.get(segment, "19.1"))
                .and_then(|t| datetime(&t)),
            method: self.concept(segment.field(17)),
            device_id: self.get(segment, "18.1"),
            operator: self
                .get(segment, "16.1")
                .or_else(|| self.get(segment, "16.2")),
            specimen_id: specimen_id.map(str::to_owned),
            ..Observation::default()
        }
    }

    fn apply_orc(&self, order: &mut Order, segment: &SegmentRef<'_>) {
        order.control = match self.get(segment, "1").as_deref() {
            Some("NW") => Some(OrderControl::New),
            Some("CA" | "OC") => Some(OrderControl::Cancel),
            Some("XO") => Some(OrderControl::Replace),
            _ => order.control,
        };
        order.placer_id = order.placer_id.take().or_else(|| self.get(segment, "2.1"));
        order.filler_id = order.filler_id.take().or_else(|| self.get(segment, "3.1"));
        order.requested_at = order
            .requested_at
            .take()
            .or_else(|| self.get(segment, "9.1").and_then(|t| datetime(&t)));
        order.priority = order.priority.or_else(|| {
            self.get(segment, "7.6")
                .and_then(|p| priority_from_code(&p))
        });
    }

    fn apply_obr(
        &self,
        order: &mut Order,
        specimen: &mut Option<Specimen>,
        segment: &SegmentRef<'_>,
    ) {
        order.placer_id = order.placer_id.take().or_else(|| self.get(segment, "2.1"));
        order.filler_id = order.filler_id.take().or_else(|| self.get(segment, "3.1"));
        order.tests.extend(self.concept(segment.field(4)));
        order.priority = order
            .priority
            .or_else(|| {
                self.get(segment, "27.6")
                    .and_then(|p| priority_from_code(&p))
            })
            .or_else(|| self.get(segment, "5").and_then(|p| priority_from_code(&p)));
        order.requested_at = order
            .requested_at
            .take()
            .or_else(|| self.get(segment, "6.1").and_then(|t| datetime(&t)));
        let collected = self.get(segment, "7.1").and_then(|t| datetime(&t));
        let kind = self.concept(segment.field(15));
        if collected.is_some() || kind.is_some() {
            let specimen = specimen.get_or_insert_with(Specimen::default);
            specimen.collected_at = specimen.collected_at.or(collected);
            if specimen.kind.is_none() {
                specimen.kind = kind;
            }
        }
    }
}

/// Where a group under construction stands.
#[derive(Default)]
struct Group {
    order: Option<Order>,
    has_obr: bool,
    specimen: Option<Specimen>,
    observations: Vec<Observation>,
}

impl Group {
    fn is_empty(&self) -> bool {
        self.order.is_none() && self.specimen.is_none() && self.observations.is_empty()
    }
}

fn walk(message: &oxim_hl7::Message, reader: &Reader) -> Vec<(Option<Patient>, Group)> {
    let mut groups = Vec::new();
    let mut patient: Option<Patient> = None;
    let mut current = Group::default();
    let mut flush = |patient: &Option<Patient>, current: &mut Group| {
        let group = std::mem::take(current);
        if !group.is_empty() {
            groups.push((patient.clone(), group));
        }
    };
    // In specimen-first layouts (OML^O33, OUL^R22) an SPM heads the orders
    // that follow it until the next SPM or PID.
    let mut specimen_first = false;
    let mut heading: Option<Specimen> = None;
    for segment in message.segments() {
        match segment.id() {
            b"PID" => {
                flush(&patient, &mut current);
                patient = Some(reader.patient(&segment));
                specimen_first = false;
                heading = None;
            }
            b"ORC" => {
                let mut order = Order::default();
                reader.apply_orc(&mut order, &segment);
                if !(specimen_first && current.order.is_none() && !current.has_obr) {
                    flush(&patient, &mut current);
                    if specimen_first {
                        current.specimen.clone_from(&heading);
                    }
                }
                current.order = Some(order);
            }
            b"OBR" => {
                if current.has_obr {
                    flush(&patient, &mut current);
                    if specimen_first {
                        current.specimen.clone_from(&heading);
                    }
                }
                let mut specimen = current.specimen.take();
                let order = current.order.get_or_insert_with(Order::default);
                reader.apply_obr(order, &mut specimen, &segment);
                current.specimen = specimen;
                current.has_obr = true;
            }
            b"TQ1" => {
                if let Some(order) = current.order.as_mut() {
                    order.priority = order.priority.or_else(|| {
                        reader
                            .get(&segment, "9.1")
                            .and_then(|p| priority_from_code(&p))
                    });
                }
            }
            b"SPM" => {
                let specimen = reader.specimen(&segment);
                if specimen_first && (current.order.is_some() || current.has_obr) {
                    flush(&patient, &mut current);
                }
                if current.order.is_none() && !current.has_obr {
                    specimen_first = true;
                }
                let target = current.specimen.get_or_insert_with(Specimen::default);
                if !specimen.identifiers.is_empty() {
                    target.identifiers = specimen.identifiers;
                }
                target.kind = specimen.kind.or(target.kind.take());
                target.collected_at = specimen.collected_at.or(target.collected_at);
                target.received_at = specimen.received_at.or(target.received_at);
                if specimen_first {
                    heading.clone_from(&current.specimen);
                }
            }
            b"OBX" => {
                let specimen_id = current
                    .specimen
                    .as_ref()
                    .and_then(|s| s.identifiers.first())
                    .map(|id| id.value.clone());
                current
                    .observations
                    .push(reader.observation(&segment, specimen_id.as_deref()));
            }
            b"NTE" => {
                if let Some(text) = reader.get(&segment, "3") {
                    if let Some(observation) = current.observations.last_mut() {
                        observation.notes.push(text);
                    } else if let Some(order) = current.order.as_mut() {
                        order.notes.push(text);
                    }
                }
            }
            _ => {}
        }
    }
    flush(&patient, &mut current);
    groups
}

fn finish_order(order: Option<Order>, specimen: Option<&Specimen>) -> Option<Order> {
    order.map(|mut order| {
        if order.specimen_ids.is_empty()
            && let Some(id) = specimen.and_then(|s| s.identifiers.first())
        {
            order.specimen_ids.push(id.value.clone());
        }
        order
    })
}

fn query(message: &oxim_hl7::Message, reader: &Reader) -> MappingResult<ClinicalContent> {
    let mut query = SpecimenQuery::default();
    if let Some(qrd) = message.segment("QRD", 1)
        && let Some(filter) = qrd.field(8)
    {
        for who in filter.repetitions() {
            query.specimen_ids.extend(reader.text(who.component(1)));
        }
    }
    if let Some(qpd) = message.segment("QPD", 1) {
        query.specimen_ids.extend(reader.get(&qpd, "3.1"));
    }
    if query.specimen_ids.is_empty() {
        return Err(MappingError::Unsupported(
            "the HL7 query names no specimen (QRD-8 or QPD-3)".into(),
        ));
    }
    query.all_tests = true;
    Ok(ClinicalContent::Query {
        device: None,
        query,
    })
}

/// Maps an HL7 v2 message to normalized content (see the module
/// documentation for the rules).
pub fn normalize(message: &oxim_hl7::Message) -> MappingResult<ClinicalContent> {
    let reader = Reader {
        encoding: message.declared_encoding().ok().flatten().unwrap_or(UTF_8),
    };
    let kind = message.message_type().unwrap_or_default();
    match kind.code.as_str() {
        "ORU" | "OUL" => {
            let device = reader
                .text(message.header().field(3).and_then(|v| v.component(1)))
                .map(|name| Device {
                    name: Some(name),
                    ..Device::default()
                });
            let groups = walk(message, &reader)
                .into_iter()
                .filter(|(_, group)| !group.observations.is_empty())
                .map(|(patient, group)| ResultGroup {
                    patient,
                    order: finish_order(group.order, group.specimen.as_ref()),
                    specimen: group.specimen,
                    observations: group.observations,
                })
                .collect::<Vec<_>>();
            if groups.is_empty() {
                return Err(MappingError::Unsupported(
                    "the HL7 result message has no OBX segment".into(),
                ));
            }
            Ok(ClinicalContent::Results { device, groups })
        }
        "ORM" | "OML" => {
            let groups = walk(message, &reader)
                .into_iter()
                .filter_map(|(patient, group)| {
                    let order = finish_order(group.order, group.specimen.as_ref())?;
                    Some(OrderGroup {
                        patient,
                        specimen: group.specimen,
                        order,
                    })
                })
                .collect::<Vec<_>>();
            if groups.is_empty() {
                return Err(MappingError::Unsupported(
                    "the HL7 order message has no ORC or OBR segment".into(),
                ));
            }
            Ok(ClinicalContent::Orders { groups })
        }
        "QRY" | "QBP" => query(message, &reader),
        // A query answer (RSP^K11) that carries the orders; "not found"
        // answers carry none.
        "RSP" => {
            let groups = walk(message, &reader)
                .into_iter()
                .filter_map(|(patient, group)| {
                    let order = finish_order(group.order, group.specimen.as_ref())?;
                    Some(OrderGroup {
                        patient,
                        specimen: group.specimen,
                        order,
                    })
                })
                .collect::<Vec<_>>();
            Ok(ClinicalContent::Orders { groups })
        }
        "" => Err(MappingError::Unsupported(
            "the HL7 message has no MSH-9 message type".into(),
        )),
        other => Err(MappingError::Unsupported(format!("HL7 {other} messages"))),
    }
}

/// Where the specimen identifier is written in encoded messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SpecimenPlacement {
    /// OBR-3 (filler order number), as many older interfaces expect.
    Obr3,
    /// An SPM segment (HL7 v2.5+).
    Spm,
    /// Both.
    #[default]
    Both,
}

/// Settings for [`encode_results`] and [`encode_orders`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Hl7Encoding {
    /// MSH-3.
    pub sending_application: Option<String>,
    /// MSH-4.
    pub sending_facility: Option<String>,
    /// MSH-5.
    pub receiving_application: Option<String>,
    /// MSH-6.
    pub receiving_facility: Option<String>,
    /// MSH-11.
    pub processing_id: String,
    /// MSH-12.
    pub version: String,
    /// MSH-18. Text is encoded in this character set; without one, text is
    /// UTF-8 and MSH-18 stays empty.
    pub charset: Option<String>,
    /// UTC offset in minutes for MSH-7.
    pub utc_offset_minutes: i16,
    /// Where the specimen identifier goes.
    pub specimen: SpecimenPlacement,
    /// OBX-11 for results whose status the source did not report.
    pub default_result_status: String,
}

impl Default for Hl7Encoding {
    fn default() -> Self {
        Self {
            sending_application: Some("OXIM".into()),
            sending_facility: None,
            receiving_application: None,
            receiving_facility: None,
            processing_id: "P".into(),
            version: "2.5.1".into(),
            charset: None,
            utc_offset_minutes: 0,
            specimen: SpecimenPlacement::Both,
            default_result_status: "F".into(),
        }
    }
}

struct Writer {
    message: oxim_hl7::Message,
    counts: BTreeMap<&'static str, usize>,
}

impl Writer {
    fn new(
        settings: &Hl7Encoding,
        message_type: &str,
        id: MessageId,
        timestamp: Timestamp,
    ) -> MappingResult<Self> {
        let message = oxim_hl7::Message::new(oxim_hl7::Delimiters::default()).map_err(|e| {
            MappingError::Write {
                path: "MSH".into(),
                detail: e.to_string(),
            }
        })?;
        let mut writer = Self {
            message,
            counts: BTreeMap::new(),
        };
        if let Some(charset) = &settings.charset {
            writer.put("MSH-18", charset)?;
            if let Err(e) = writer.message.declared_encoding() {
                return Err(MappingError::Setting {
                    name: "charset",
                    detail: e.to_string(),
                });
            }
        }
        for (path, value) in [
            ("MSH-3", &settings.sending_application),
            ("MSH-4", &settings.sending_facility),
            ("MSH-5", &settings.receiving_application),
            ("MSH-6", &settings.receiving_facility),
        ] {
            if let Some(value) = value {
                writer.put(path, value)?;
            }
        }
        let stamp = ClinicalDateTime::from_timestamp(timestamp, settings.utc_offset_minutes)
            .map(|t| {
                let text = t.to_hl7();
                match (text.find('.'), text.rfind(['+', '-'])) {
                    (Some(dot), Some(sign)) => format!("{}{}", &text[..dot], &text[sign..]),
                    _ => text,
                }
            })
            .unwrap_or_default();
        writer.put("MSH-7", &stamp)?;
        writer.raw("MSH-9", message_type.as_bytes())?;
        writer.put("MSH-10", &id.to_string())?;
        writer.put("MSH-11", &settings.processing_id)?;
        writer.put("MSH-12", &settings.version)?;
        Ok(writer)
    }

    fn put(&mut self, path: &str, text: &str) -> MappingResult<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.message
            .set(path, text)
            .map_err(|e| MappingError::Write {
                path: path.to_owned(),
                detail: e.to_string(),
            })
    }

    fn raw(&mut self, path: &str, raw: &[u8]) -> MappingResult<()> {
        self.message
            .set_raw(path, raw)
            .map_err(|e| MappingError::Write {
                path: path.to_owned(),
                detail: e.to_string(),
            })
    }

    fn segment(&mut self, id: &'static str) -> MappingResult<usize> {
        self.message
            .push_segment(id)
            .map_err(|e| MappingError::Write {
                path: id.into(),
                detail: e.to_string(),
            })?;
        let count = self.counts.entry(id).or_default();
        *count += 1;
        Ok(*count)
    }

    fn set(&mut self, id: &str, n: usize, field: &str, text: Option<&str>) -> MappingResult<()> {
        match text {
            Some(text) => self.put(&format!("{id}[{n}]-{field}"), text),
            None => Ok(()),
        }
    }

    fn coding(
        &mut self,
        id: &str,
        n: usize,
        field: &str,
        coding: &Coding,
        offset: usize,
    ) -> MappingResult<()> {
        self.set(
            id,
            n,
            &format!("{field}.{}", offset + 1),
            Some(&coding.code),
        )?;
        self.set(
            id,
            n,
            &format!("{field}.{}", offset + 2),
            coding.display.as_deref(),
        )?;
        let system = coding.system.as_deref().map(system_to_hl7);
        self.set(id, n, &format!("{field}.{}", offset + 3), system)
    }

    /// Writes a concept as CWE: the preferred coding, then an alternate.
    fn concept(
        &mut self,
        id: &str,
        n: usize,
        field: &str,
        concept: &CodeableConcept,
    ) -> MappingResult<()> {
        let Some(primary) = pick(concept, &[ASTM_LOCAL]) else {
            return self.set(id, n, &format!("{field}.2"), concept.text.as_deref());
        };
        let primary = primary.clone();
        self.coding(id, n, field, &primary, 0)?;
        if primary.display.is_none() {
            self.set(id, n, &format!("{field}.2"), concept.text.as_deref())?;
        }
        if let Some(alternate) = concept
            .codings
            .iter()
            .find(|c| **c != primary && c.system.as_deref() != Some(crate::codes::ASTM_TEST_ID))
        {
            let alternate = alternate.clone();
            self.coding(id, n, field, &alternate, 3)?;
        }
        Ok(())
    }
}

fn time(value: Option<&ClinicalDateTime>) -> Option<String> {
    value.map(hl7_datetime)
}

fn write_patient(writer: &mut Writer, patient: &Patient) -> MappingResult<()> {
    let n = writer.segment("PID")?;
    writer.set("PID", n, "1", Some(&n.to_string()))?;
    for (index, identifier) in patient.identifiers.iter().enumerate() {
        let repetition = index + 1;
        writer.set(
            "PID",
            n,
            &format!("3[{repetition}].1"),
            Some(&identifier.value),
        )?;
        writer.set(
            "PID",
            n,
            &format!("3[{repetition}].4"),
            identifier.assigner.as_deref(),
        )?;
        writer.set(
            "PID",
            n,
            &format!("3[{repetition}].5"),
            identifier.kind.as_deref(),
        )?;
    }
    if patient.identifiers.is_empty() {
        // PID-3 is required; an empty repetition keeps the segment valid.
        writer.raw(&format!("PID[{n}]-3"), b"")?;
    }
    if let Some(name) = &patient.name {
        writer.set("PID", n, "5.1", name.family.as_deref())?;
        writer.set("PID", n, "5.2", name.given.first().map(String::as_str))?;
        writer.set("PID", n, "5.3", name.given.get(1).map(String::as_str))?;
        writer.set("PID", n, "5.4", name.suffix.as_deref())?;
        writer.set("PID", n, "5.5", name.prefix.as_deref())?;
    }
    writer.set("PID", n, "7", time(patient.birth_date.as_ref()).as_deref())?;
    writer.set(
        "PID",
        n,
        "8",
        patient.sex.map(AdministrativeSex::to_hl7_code),
    )?;
    if let Some(species) = &patient.species {
        writer.concept("PID", n, "35", species)?;
    }
    if let Some(breed) = &patient.breed {
        writer.concept("PID", n, "36", breed)?;
    }
    Ok(())
}

fn specimen_id(specimen: Option<&Specimen>, order: Option<&Order>) -> Option<String> {
    specimen
        .and_then(|s| s.identifiers.first().map(|id| id.value.clone()))
        .or_else(|| order.and_then(|o| o.specimen_ids.first().cloned()))
}

fn write_specimen(
    writer: &mut Writer,
    specimen: &Specimen,
    id: Option<&str>,
    role: Option<&str>,
) -> MappingResult<()> {
    let n = writer.segment("SPM")?;
    writer.set("SPM", n, "1", Some(&n.to_string()))?;
    writer.set("SPM", n, "2.1.1", id)?;
    if let Some(kind) = &specimen.kind {
        writer.concept("SPM", n, "4", kind)?;
    }
    writer.set("SPM", n, "11", role)?;
    writer.set(
        "SPM",
        n,
        "17.1",
        time(specimen.collected_at.as_ref()).as_deref(),
    )?;
    writer.set(
        "SPM",
        n,
        "18",
        time(specimen.received_at.as_ref()).as_deref(),
    )
}

fn content_type_parts(content_type: Option<&str>) -> (&'static str, String) {
    let content_type = content_type.unwrap_or("application/octet-stream");
    let (major, minor) = content_type
        .split_once('/')
        .unwrap_or(("application", content_type));
    let kind = match major {
        "image" => "IM",
        "text" => "TX",
        _ => "AP",
    };
    (kind, minor.to_ascii_uppercase())
}

fn write_units(writer: &mut Writer, n: usize, quantity: &Quantity) -> MappingResult<()> {
    match (&quantity.system, &quantity.code) {
        (Some(system), Some(code)) if system == UCUM => {
            writer.set("OBX", n, "6.1", Some(code))?;
            if quantity.unit.as_deref() != Some(code.as_str()) {
                writer.set("OBX", n, "6.2", quantity.unit.as_deref())?;
            }
            writer.set("OBX", n, "6.3", Some("UCUM"))
        }
        _ => writer.set("OBX", n, "6.1", quantity.unit.as_deref()),
    }
}

fn write_observation(
    writer: &mut Writer,
    set_id: usize,
    observation: &Observation,
    settings: &Hl7Encoding,
) -> MappingResult<()> {
    let n = writer.segment("OBX")?;
    writer.set("OBX", n, "1", Some(&set_id.to_string()))?;
    writer.concept("OBX", n, "3", &observation.code)?;
    let value_type = match &observation.value {
        None => None,
        Some(ObservationValue::Quantity(quantity)) => {
            match quantity.comparator {
                None => writer.set("OBX", n, "5", Some(quantity.value.as_str()))?,
                Some(comparator) => {
                    writer.set("OBX", n, "5.1", Some(comparator.as_str()))?;
                    writer.set("OBX", n, "5.2", Some(quantity.value.as_str()))?;
                }
            }
            write_units(writer, n, quantity)?;
            Some(if quantity.comparator.is_some() {
                "SN"
            } else {
                "NM"
            })
        }
        Some(ObservationValue::Range { low, high }) => {
            writer.set("OBX", n, "5.2", low.as_ref().map(|q| q.value.as_str()))?;
            writer.set("OBX", n, "5.3", Some("-"))?;
            writer.set("OBX", n, "5.4", high.as_ref().map(|q| q.value.as_str()))?;
            if let Some(quantity) = low.as_ref().or(high.as_ref()) {
                write_units(writer, n, quantity)?;
            }
            Some("SN")
        }
        Some(ObservationValue::Ratio {
            numerator,
            denominator,
        }) => {
            writer.set("OBX", n, "5.2", Some(numerator.value.as_str()))?;
            writer.set("OBX", n, "5.3", Some(":"))?;
            writer.set("OBX", n, "5.4", Some(denominator.value.as_str()))?;
            Some("SN")
        }
        Some(ObservationValue::Text(text)) => {
            writer.set("OBX", n, "5", Some(text))?;
            Some(if text.len() <= 199 && !text.contains(['\r', '\n']) {
                "ST"
            } else {
                "TX"
            })
        }
        Some(ObservationValue::Coded(concept)) => {
            writer.concept("OBX", n, "5", concept)?;
            Some("CWE")
        }
        Some(ObservationValue::Boolean(value)) => {
            let (code, text) = if *value { ("Y", "Yes") } else { ("N", "No") };
            writer.set("OBX", n, "5.1", Some(code))?;
            writer.set("OBX", n, "5.2", Some(text))?;
            writer.set("OBX", n, "5.3", Some("HL70136"))?;
            Some("CWE")
        }
        Some(ObservationValue::DateTime(value)) => {
            writer.set("OBX", n, "5", Some(&hl7_datetime(value)))?;
            Some(if value.precision() <= Precision::Day {
                "DT"
            } else {
                "DTM"
            })
        }
        Some(ObservationValue::Attachment {
            content_type,
            data,
            title,
        }) => {
            let (kind, subtype) = content_type_parts(content_type.as_deref());
            writer.set("OBX", n, "5.1", title.as_deref())?;
            writer.set("OBX", n, "5.2", Some(kind))?;
            writer.set("OBX", n, "5.3", Some(&subtype))?;
            writer.set("OBX", n, "5.4", Some("Base64"))?;
            writer.set("OBX", n, "5.5", Some(data))?;
            Some("ED")
        }
    };
    writer.set("OBX", n, "2", value_type)?;
    if let Some(range) = &observation.reference_range {
        writer.set("OBX", n, "7", Some(&range_text(range)))?;
    }
    for (index, flag) in observation.interpretation.iter().enumerate() {
        writer.set("OBX", n, &format!("8[{}]", index + 1), Some(&flag.code))?;
    }
    let status = status_to_hl7(observation.status, &settings.default_result_status);
    writer.set("OBX", n, "11", Some(&status))?;
    writer.set(
        "OBX",
        n,
        "14",
        time(observation.effective_at.as_ref()).as_deref(),
    )?;
    writer.set("OBX", n, "16.1", observation.operator.as_deref())?;
    if let Some(method) = &observation.method {
        writer.concept("OBX", n, "17", method)?;
    }
    writer.set("OBX", n, "18.1", observation.device_id.as_deref())?;
    write_notes(writer, &observation.notes)
}

fn write_notes(writer: &mut Writer, notes: &[String]) -> MappingResult<()> {
    for (index, note) in notes.iter().enumerate() {
        let n = writer.segment("NTE")?;
        writer.set("NTE", n, "1", Some(&(index + 1).to_string()))?;
        writer.set("NTE", n, "3", Some(note))?;
    }
    Ok(())
}

fn result_status(observations: &[Observation], fallback: &str) -> String {
    let has = |status| observations.iter().any(|o| o.status == status);
    if has(ObservationStatus::Corrected) || has(ObservationStatus::Amended) {
        "C".into()
    } else if has(ObservationStatus::Preliminary) || has(ObservationStatus::Registered) {
        "P".into()
    } else if !observations.is_empty()
        && observations
            .iter()
            .all(|o| o.status == ObservationStatus::Final)
    {
        "F".into()
    } else {
        fallback.into()
    }
}

struct OrderSegments<'a> {
    order: Option<&'a Order>,
    specimen: Option<&'a Specimen>,
    observations: &'a [Observation],
    notes: Vec<String>,
    specimen_role: Option<&'static str>,
}

fn write_result_group(
    writer: &mut Writer,
    group: OrderSegments<'_>,
    settings: &Hl7Encoding,
) -> MappingResult<()> {
    let specimen_id = specimen_id(group.specimen, group.order);
    let placer = group.order.and_then(|o| o.placer_id.as_deref());
    let filler = group.order.and_then(|o| o.filler_id.clone()).or_else(|| {
        matches!(
            settings.specimen,
            SpecimenPlacement::Obr3 | SpecimenPlacement::Both
        )
        .then(|| specimen_id.clone())
        .flatten()
    });
    let orc = writer.segment("ORC")?;
    writer.set("ORC", orc, "1", Some("RE"))?;
    writer.set("ORC", orc, "2", placer)?;
    writer.set("ORC", orc, "3", filler.as_deref())?;

    let obr = writer.segment("OBR")?;
    writer.set("OBR", obr, "1", Some(&obr.to_string()))?;
    writer.set("OBR", obr, "2", placer)?;
    writer.set("OBR", obr, "3", filler.as_deref())?;
    let service = group
        .order
        .and_then(|o| o.tests.first())
        .or_else(|| group.observations.first().map(|o| &o.code));
    if let Some(service) = service {
        writer.concept("OBR", obr, "4", service)?;
    }
    let observed = group
        .specimen
        .and_then(|s| s.collected_at.as_ref())
        .or_else(|| {
            group
                .observations
                .iter()
                .find_map(|o| o.effective_at.as_ref())
        });
    writer.set("OBR", obr, "7", time(observed).as_deref())?;
    writer.set(
        "OBR",
        obr,
        "25",
        Some(&result_status(
            group.observations,
            &settings.default_result_status,
        )),
    )?;
    if let Some(priority) = group.order.and_then(|o| o.priority) {
        writer.set("OBR", obr, "27.6", Some(priority_code(priority)))?;
    }
    write_notes(writer, &group.notes)?;

    let place_spm = matches!(
        settings.specimen,
        SpecimenPlacement::Spm | SpecimenPlacement::Both
    );
    if place_spm
        && (group.specimen.is_some() || specimen_id.is_some() || group.specimen_role.is_some())
    {
        let empty = Specimen::default();
        write_specimen(
            writer,
            group.specimen.unwrap_or(&empty),
            specimen_id.as_deref(),
            group.specimen_role,
        )?;
    }
    for (index, observation) in group.observations.iter().enumerate() {
        write_observation(writer, index + 1, observation, settings)?;
    }
    Ok(())
}

/// Writes results or quality control results as an `ORU^R01` message.
///
/// MSH-10 is the OXIM message identifier and MSH-7 the receive time, so the
/// output is fully determined by the input.
pub fn encode_results(
    content: &ClinicalContent,
    settings: &Hl7Encoding,
    id: MessageId,
    timestamp: Timestamp,
) -> MappingResult<oxim_hl7::Message> {
    let mut writer = Writer::new(settings, "ORU^R01^ORU_R01", id, timestamp)?;
    match content {
        ClinicalContent::Results { groups, .. } => {
            let mut last_patient: Option<&Option<Patient>> = None;
            for group in groups {
                if last_patient != Some(&group.patient) {
                    if let Some(patient) = &group.patient {
                        write_patient(&mut writer, patient)?;
                    }
                    last_patient = Some(&group.patient);
                }
                write_result_group(
                    &mut writer,
                    OrderSegments {
                        order: group.order.as_ref(),
                        specimen: group.specimen.as_ref(),
                        observations: &group.observations,
                        notes: group
                            .order
                            .as_ref()
                            .map(|o| o.notes.clone())
                            .unwrap_or_default(),
                        specimen_role: None,
                    },
                    settings,
                )?;
            }
        }
        ClinicalContent::QualityControl { results, .. } => {
            for result in results {
                let notes = [
                    result
                        .material
                        .as_ref()
                        .map(|m| format!("Control material: {m}")),
                    result.lot.as_ref().map(|l| format!("Lot: {l}")),
                    result.level.as_ref().map(|l| format!("Level: {l}")),
                    result.expires_at.as_ref().map(|e| format!("Expires: {e}")),
                ]
                .into_iter()
                .flatten()
                .collect();
                let specimen = Specimen {
                    identifiers: result
                        .material
                        .iter()
                        .cloned()
                        .map(Identifier::new)
                        .collect(),
                    ..Specimen::default()
                };
                write_result_group(
                    &mut writer,
                    OrderSegments {
                        order: None,
                        specimen: Some(&specimen),
                        observations: std::slice::from_ref(&result.observation),
                        notes,
                        specimen_role: Some("Q"),
                    },
                    settings,
                )?;
            }
        }
        other => {
            return Err(MappingError::WrongContent {
                encoder: "hl7v2-oru-r01",
                found: kind_name(other),
            });
        }
    }
    Ok(writer.message)
}

/// Writes orders as an `OML^O21` message.
pub fn encode_orders(
    content: &ClinicalContent,
    settings: &Hl7Encoding,
    id: MessageId,
    timestamp: Timestamp,
) -> MappingResult<oxim_hl7::Message> {
    let ClinicalContent::Orders { groups } = content else {
        return Err(MappingError::WrongContent {
            encoder: "hl7v2-oml-o21",
            found: kind_name(content),
        });
    };
    let mut writer = Writer::new(settings, "OML^O21^OML_O21", id, timestamp)?;
    let mut last_patient: Option<&Option<Patient>> = None;
    for group in groups {
        if last_patient != Some(&group.patient) {
            if let Some(patient) = &group.patient {
                write_patient(&mut writer, patient)?;
            }
            last_patient = Some(&group.patient);
        }
        let order = &group.order;
        let control = match order.control {
            Some(OrderControl::Cancel) => "CA",
            Some(OrderControl::Replace) => "XO",
            _ => "NW",
        };
        let specimen_id = specimen_id(group.specimen.as_ref(), Some(order));
        let tests: Vec<Option<&CodeableConcept>> = if order.tests.is_empty() {
            vec![None]
        } else {
            order.tests.iter().map(Some).collect()
        };
        for (index, test) in tests.into_iter().enumerate() {
            let orc = writer.segment("ORC")?;
            writer.set("ORC", orc, "1", Some(control))?;
            writer.set("ORC", orc, "2", order.placer_id.as_deref())?;
            writer.set("ORC", orc, "3", order.filler_id.as_deref())?;
            writer.set(
                "ORC",
                orc,
                "9",
                time(order.requested_at.as_ref()).as_deref(),
            )?;
            if let Some(priority) = order.priority {
                let tq1 = writer.segment("TQ1")?;
                writer.set("TQ1", tq1, "1", Some("1"))?;
                writer.set("TQ1", tq1, "9", Some(priority_code(priority)))?;
            }
            let obr = writer.segment("OBR")?;
            writer.set("OBR", obr, "1", Some(&(index + 1).to_string()))?;
            writer.set("OBR", obr, "2", order.placer_id.as_deref())?;
            writer.set("OBR", obr, "3", order.filler_id.as_deref())?;
            if let Some(test) = test {
                writer.concept("OBR", obr, "4", test)?;
            }
            if index == 0 {
                write_notes(&mut writer, &order.notes)?;
                if group.specimen.is_some() || specimen_id.is_some() {
                    let empty = Specimen::default();
                    write_specimen(
                        &mut writer,
                        group.specimen.as_ref().unwrap_or(&empty),
                        specimen_id.as_deref(),
                        None,
                    )?;
                }
            }
        }
    }
    Ok(writer.message)
}

fn order_control(order: &Order) -> &'static str {
    match order.control {
        Some(OrderControl::Cancel) => "CA",
        Some(OrderControl::Replace) => "XO",
        _ => "NW",
    }
}

/// Writes one specimen-centric work order: SPM, SAC (container identifier
/// = specimen identifier) and per test ORC, TQ1 (when a priority is set)
/// and OBR, as in `OML^O33`.
fn write_work_order(writer: &mut Writer, group: &OrderGroup) -> MappingResult<()> {
    let order = &group.order;
    let specimen_id = specimen_id(group.specimen.as_ref(), Some(order));
    let empty = Specimen::default();
    let specimen = group.specimen.as_ref().unwrap_or(&empty);
    write_specimen(writer, specimen, specimen_id.as_deref(), None)?;
    if specimen_id.is_some() || specimen.container.is_some() {
        let sac = writer.segment("SAC")?;
        writer.set("SAC", sac, "3.1", specimen_id.as_deref())?;
        writer.set("SAC", sac, "9.2", specimen.container.as_deref())?;
    }
    write_notes(writer, &order.notes)?;
    let tests: Vec<Option<&CodeableConcept>> = if order.tests.is_empty() {
        vec![None]
    } else {
        order.tests.iter().map(Some).collect()
    };
    for (index, test) in tests.into_iter().enumerate() {
        let orc = writer.segment("ORC")?;
        writer.set("ORC", orc, "1", Some(order_control(order)))?;
        writer.set("ORC", orc, "2", order.placer_id.as_deref())?;
        writer.set("ORC", orc, "3", order.filler_id.as_deref())?;
        writer.set(
            "ORC",
            orc,
            "9",
            time(order.requested_at.as_ref()).as_deref(),
        )?;
        if let Some(priority) = order.priority {
            let tq1 = writer.segment("TQ1")?;
            writer.set("TQ1", tq1, "1", Some("1"))?;
            writer.set("TQ1", tq1, "9", Some(priority_code(priority)))?;
        }
        let obr = writer.segment("OBR")?;
        writer.set("OBR", obr, "1", Some(&(index + 1).to_string()))?;
        writer.set("OBR", obr, "2", order.placer_id.as_deref())?;
        writer.set("OBR", obr, "3", order.filler_id.as_deref())?;
        if let Some(test) = test {
            writer.concept("OBR", obr, "4", test)?;
        }
    }
    Ok(())
}

fn write_work_orders(writer: &mut Writer, groups: &[OrderGroup]) -> MappingResult<()> {
    let mut last_patient: Option<&Option<Patient>> = None;
    for group in groups {
        if last_patient != Some(&group.patient) {
            if let Some(patient) = &group.patient {
                write_patient(writer, patient)?;
            }
            last_patient = Some(&group.patient);
        }
        write_work_order(writer, group)?;
    }
    Ok(())
}

/// Writes `OML^O33`, the specimen-centric work order of IHE Laboratory
/// Analytical Workflow (LAW, transaction LAB-28) that analyzers accept:
/// per order group PID (when the patient changes), SPM, SAC (SAC-3 is the
/// specimen identifier) and per test ORC, TQ1 and OBR. Positions are those
/// of the table above.
pub fn encode_work_orders(
    content: &ClinicalContent,
    settings: &Hl7Encoding,
    id: MessageId,
    timestamp: Timestamp,
) -> MappingResult<oxim_hl7::Message> {
    let ClinicalContent::Orders { groups } = content else {
        return Err(MappingError::WrongContent {
            encoder: "hl7v2-oml-o33",
            found: kind_name(content),
        });
    };
    let mut writer = Writer::new(settings, "OML^O33^OML_O33", id, timestamp)?;
    write_work_orders(&mut writer, groups)?;
    Ok(writer.message)
}

/// Writes `QBP^Q11`, a work order query (IHE LAW LAB-27) for a `Query`,
/// for example to ask the LIS about a tube the order cache does not know:
/// QPD-1 `WOS^Work Order Step^IHE_LABTF`, QPD-2 the query tag (the message
/// identifier), QPD-3 the specimen identifier and RCP-1 `I` (immediate).
/// LAW queries name one specimen; the first queried specimen is used.
pub fn encode_query(
    content: &ClinicalContent,
    settings: &Hl7Encoding,
    id: MessageId,
    timestamp: Timestamp,
) -> MappingResult<oxim_hl7::Message> {
    let ClinicalContent::Query { query, .. } = content else {
        return Err(MappingError::WrongContent {
            encoder: "hl7v2-qbp-q11",
            found: kind_name(content),
        });
    };
    let Some(specimen) = query.specimen_ids.first() else {
        return Err(MappingError::Unsupported(
            "the query names no specimen".into(),
        ));
    };
    let mut writer = Writer::new(settings, "QBP^Q11^QBP_Q11", id, timestamp)?;
    let qpd = writer.segment("QPD")?;
    writer.raw(&format!("QPD[{qpd}]-1"), b"WOS^Work Order Step^IHE_LABTF")?;
    writer.set("QPD", qpd, "2", Some(&id.to_string()))?;
    writer.set("QPD", qpd, "3", Some(specimen))?;
    let rcp = writer.segment("RCP")?;
    writer.set("RCP", rcp, "1", Some("I"))?;
    Ok(writer.message)
}

/// Writes `RSP^K11`, the answer to a device's `QBP` host query (IHE LAW
/// LAB-27): MSA (`AA` and the query's MSH-10), QAK (QPD-2 query tag, `OK`
/// when there are orders and `NF` otherwise, QPD-1 query name, hit counts)
/// and the query's QPD. MSH-5 and MSH-6 default to the query's MSH-3 and
/// MSH-4. A `QRY` query without QPD is answered with QAK-1 from QRD-4.
///
/// IHE LAW then sends the orders separately as `OML^O33`
/// ([`encode_work_orders`]); with `include_orders` they follow the QPD
/// instead, for devices that expect the orders in the response.
pub fn encode_query_response(
    content: &ClinicalContent,
    query: &oxim_hl7::Message,
    settings: &Hl7Encoding,
    include_orders: bool,
    id: MessageId,
    timestamp: Timestamp,
) -> MappingResult<oxim_hl7::Message> {
    let ClinicalContent::Orders { groups } = content else {
        return Err(MappingError::WrongContent {
            encoder: "hl7v2-rsp-k11",
            found: kind_name(content),
        });
    };
    let reader = Reader {
        encoding: query.declared_encoding().ok().flatten().unwrap_or(UTF_8),
    };
    let header = query.header();
    let mut settings = settings.clone();
    if settings.receiving_application.is_none() {
        settings.receiving_application = reader.get(&header, "3.1");
    }
    if settings.receiving_facility.is_none() {
        settings.receiving_facility = reader.get(&header, "4.1");
    }
    let mut writer = Writer::new(&settings, "RSP^K11^RSP_K11", id, timestamp)?;
    let msa = writer.segment("MSA")?;
    writer.set("MSA", msa, "1", Some("AA"))?;
    writer.set("MSA", msa, "2", reader.get(&header, "10").as_deref())?;
    let qpd = query.segment("QPD", 1);
    let tag = match &qpd {
        Some(qpd) => reader.get(qpd, "2"),
        None => query
            .segment("QRD", 1)
            .and_then(|qrd| reader.get(&qrd, "4")),
    };
    let found = !groups.is_empty();
    let qak = writer.segment("QAK")?;
    writer.set("QAK", qak, "1", tag.as_deref())?;
    writer.set("QAK", qak, "2", Some(if found { "OK" } else { "NF" }))?;
    let count = groups.len().to_string();
    if let Some(qpd) = &qpd {
        if let Some(name) = qpd.field(1) {
            writer.raw(&format!("QAK[{qak}]-3"), name.raw())?;
        }
        writer.set("QAK", qak, "4", Some(&count))?;
        writer.set("QAK", qak, "5", Some(&count))?;
        writer.set("QAK", qak, "6", Some("0"))?;
        let copy = writer.segment("QPD")?;
        let same_delimiters = query.delimiters() == writer.message.delimiters();
        for n in 1..=qpd.field_count() {
            let Some(field) = qpd.field(n) else {
                continue;
            };
            if same_delimiters {
                writer.raw(&format!("QPD[{copy}]-{n}"), field.raw())?;
            } else {
                let text = field.to_text(reader.encoding);
                writer.set("QPD", copy, &n.to_string(), Some(&text))?;
            }
        }
    }
    if include_orders {
        write_work_orders(&mut writer, groups)?;
    }
    Ok(writer.message)
}
