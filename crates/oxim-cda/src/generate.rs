//! A CDA R2 laboratory report written from normalized results.
//!
//! The report is a minimal, schema-conformant CDA R2 document:
//!
//! - header: `typeId` (`POCD_HD000040`), `id` (a UUID derived from the
//!   OXIM message identifier), `code` LOINC `11502-2` (Laboratory report),
//!   `title`, `effectiveTime` (the receive time), `confidentialityCode`,
//!   `languageCode`, `recordTarget` (the patient), `author` (the device, or
//!   OXIM), `custodian`;
//! - one section, LOINC `30954-2`, with a narrative table and one
//!   `organizer` (`BATTERY`) per result group holding the specimen and one
//!   `observation` per result.
//!
//! Values are written as reported: quantities as `PQ` with the exact
//! decimal text (limits such as `<0.5` as a one-sided `IVL_PQ`), ranges as
//! `IVL_PQ`, ratios as `RTO_PQ_PQ`, coded results as `CD`, text as `ST`,
//! yes/no as `BL`, times as `TS`, attachments as base64 `ED`.
//! Interpretation codes without a system are written in HL7 v3
//! ObservationInterpretation. Codes whose system has no OID keep the system
//! URI in `codeSystemName`.

use std::fmt::Write as _;

use oxim_formats::{XmlDocument, XmlOptions};
use oxim_model::{
    ClinicalContent, ClinicalDateTime, CodeableConcept, Coding, Comparator, HumanName, Identifier,
    MessageId, Observation, ObservationStatus, ObservationValue, Patient, Quantity, ReferenceRange,
    ResultGroup, Timestamp,
};

use crate::codes::{
    CONFIDENTIALITY_OID, GENDER_OID, INTERPRETATION_OID, LOINC_OID, oid_from_system,
    root_from_system,
};
use crate::error::{CdaError, CdaResult};

/// Settings for [`encode_lab_report`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdaEncoding {
    /// The document title.
    pub title: String,
    /// `languageCode`.
    pub language: String,
    /// `confidentialityCode` (HL7 v3 Confidentiality: `N`, `R`, `V`).
    pub confidentiality: String,
    /// The custodian organization's name.
    pub custodian_name: Option<String>,
    /// The custodian organization's identifier root (an OID or UUID).
    pub custodian_id_root: Option<String>,
    /// The root for patient identifiers whose system is not an OID or UUID.
    pub patient_id_root: Option<String>,
    /// The root for specimen identifiers whose system is not an OID or UUID.
    pub specimen_id_root: Option<String>,
    /// The software name used as author when the results name no device.
    pub software_name: String,
    /// UTC offset in minutes for the document time.
    pub utc_offset_minutes: i16,
}

impl Default for CdaEncoding {
    fn default() -> Self {
        Self {
            title: "Laboratory Report".into(),
            language: "en-US".into(),
            confidentiality: "N".into(),
            custodian_name: None,
            custodian_id_root: None,
            patient_id_root: None,
            specimen_id_root: None,
            software_name: "OXIM".into(),
            utc_offset_minutes: 0,
        }
    }
}

/// The UUID form of a message identifier.
pub fn message_uuid(id: MessageId) -> String {
    let hex = format!("{:032x}", id.to_u128());
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\t' | '\n' | '\r' => c,
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => ' ',
            c => c,
        })
        .collect()
}

fn escape(text: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in clean(text).chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\n' if attribute => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' if attribute => out.push_str("&#9;"),
            c => out.push(c),
        }
    }
    out
}

type Attributes<'a> = Vec<(&'a str, Option<String>)>;

/// An indenting XML writer.
struct Writer {
    out: String,
    depth: usize,
}

impl Writer {
    fn start(&mut self, name: &str, attributes: &[(&str, Option<String>)], empty: bool) {
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
        self.out.push('<');
        self.out.push_str(name);
        for (key, value) in attributes {
            if let Some(value) = value {
                let _ = write!(self.out, " {key}=\"{}\"", escape(value, true));
            }
        }
        self.out.push_str(if empty { "/>\n" } else { ">\n" });
        if !empty {
            self.depth += 1;
        }
    }

    fn open(&mut self, name: &str, attributes: &[(&str, Option<String>)]) {
        self.start(name, attributes, false);
    }

    fn empty(&mut self, name: &str, attributes: &[(&str, Option<String>)]) {
        self.start(name, attributes, true);
    }

    fn close(&mut self, name: &str) {
        self.depth = self.depth.saturating_sub(1);
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
        let _ = writeln!(self.out, "</{name}>");
    }

    fn text(&mut self, name: &str, attributes: &[(&str, Option<String>)], text: &str) {
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
        self.out.push('<');
        self.out.push_str(name);
        for (key, value) in attributes {
            if let Some(value) = value {
                let _ = write!(self.out, " {key}=\"{}\"", escape(value, true));
            }
        }
        let _ = writeln!(self.out, ">{}</{name}>", escape(text, false));
    }
}

fn some(text: &str) -> Option<String> {
    Some(text.to_owned())
}

/// The coding written as the main code: the first with an OID system,
/// else the first.
fn primary(concept: &CodeableConcept) -> Option<&Coding> {
    concept
        .codings
        .iter()
        .find(|c| c.system.as_deref().and_then(oid_from_system).is_some())
        .or_else(|| concept.codings.first())
}

fn coding_attributes<'a>(coding: &Coding, default_oid: Option<&str>) -> Attributes<'a> {
    let oid = coding
        .system
        .as_deref()
        .and_then(oid_from_system)
        .or_else(|| {
            coding
                .system
                .is_none()
                .then(|| default_oid.map(str::to_owned))
                .flatten()
        });
    let name = match (&oid, &coding.system) {
        (Some(oid), _) if oid == LOINC_OID => some("LOINC"),
        (None, Some(system)) => Some(system.clone()),
        _ => None,
    };
    vec![
        ("code", Some(coding.code.clone())),
        ("codeSystem", oid),
        ("codeSystemName", name),
        ("displayName", coding.display.clone()),
    ]
}

fn coded(
    w: &mut Writer,
    element: &str,
    concept: &CodeableConcept,
    mut prefix: Attributes<'_>,
    default_oid: Option<&str>,
) {
    let Some(main) = primary(concept) else {
        prefix.push(("nullFlavor", some("OTH")));
        match &concept.text {
            Some(text) => {
                w.open(element, &prefix);
                w.text("originalText", &[], text);
                w.close(element);
            }
            None => w.empty(element, &prefix),
        }
        return;
    };
    prefix.extend(coding_attributes(main, default_oid));
    let others: Vec<&Coding> = concept.codings.iter().filter(|c| *c != main).collect();
    if concept.text.is_none() && others.is_empty() {
        w.empty(element, &prefix);
        return;
    }
    w.open(element, &prefix);
    if let Some(text) = &concept.text {
        w.text("originalText", &[], text);
    }
    for other in others {
        w.empty("translation", &coding_attributes(other, default_oid));
    }
    w.close(element);
}

fn instance_id(w: &mut Writer, identifier: &Identifier, fallback_root: Option<&str>) {
    let root = identifier
        .system
        .as_deref()
        .and_then(root_from_system)
        .or_else(|| fallback_root.map(str::to_owned));
    let null = root.is_none().then(|| "UNK".to_owned());
    w.empty(
        "id",
        &[
            ("root", root),
            ("extension", Some(identifier.value.clone())),
            ("assigningAuthorityName", identifier.assigner.clone()),
            ("nullFlavor", null),
        ],
    );
}

fn person_name(w: &mut Writer, name: &HumanName) {
    w.open("name", &[]);
    if let Some(prefix) = &name.prefix {
        w.text("prefix", &[], prefix);
    }
    for given in &name.given {
        w.text("given", &[], given);
    }
    if let Some(family) = &name.family {
        w.text("family", &[], family);
    }
    if let Some(suffix) = &name.suffix {
        w.text("suffix", &[], suffix);
    }
    w.close("name");
}

fn time(value: &ClinicalDateTime) -> Option<String> {
    Some(value.to_hl7())
}

fn pq_attributes(quantity: &Quantity) -> Attributes<'static> {
    vec![
        ("value", Some(quantity.value.as_str().to_owned())),
        ("unit", quantity.unit.clone()),
    ]
}

fn write_value(w: &mut Writer, value: &ObservationValue) {
    let typed = |t: &str| vec![("xsi:type", some(t))];
    match value {
        ObservationValue::Quantity(quantity) => match quantity.comparator {
            None => {
                let mut attributes = typed("PQ");
                attributes.extend(pq_attributes(quantity));
                w.empty("value", &attributes);
            }
            Some(comparator) => {
                let (limit, inclusive) = match comparator {
                    Comparator::LessThan => ("high", "false"),
                    Comparator::LessOrEqual => ("high", "true"),
                    Comparator::GreaterThan => ("low", "false"),
                    Comparator::GreaterOrEqual => ("low", "true"),
                };
                w.open("value", &typed("IVL_PQ"));
                let mut attributes = pq_attributes(quantity);
                attributes.push(("inclusive", some(inclusive)));
                w.empty(limit, &attributes);
                w.close("value");
            }
        },
        ObservationValue::Range { low, high } => {
            w.open("value", &typed("IVL_PQ"));
            if let Some(low) = low {
                w.empty("low", &pq_attributes(low));
            }
            if let Some(high) = high {
                w.empty("high", &pq_attributes(high));
            }
            w.close("value");
        }
        ObservationValue::Ratio {
            numerator,
            denominator,
        } => {
            w.open("value", &typed("RTO_PQ_PQ"));
            w.empty("numerator", &pq_attributes(numerator));
            w.empty("denominator", &pq_attributes(denominator));
            w.close("value");
        }
        ObservationValue::Coded(concept) => coded(w, "value", concept, typed("CD"), None),
        ObservationValue::Text(text) => w.text("value", &typed("ST"), text),
        ObservationValue::Boolean(value) => {
            let mut attributes = typed("BL");
            attributes.push(("value", some(if *value { "true" } else { "false" })));
            w.empty("value", &attributes);
        }
        ObservationValue::DateTime(value) => {
            let mut attributes = typed("TS");
            attributes.push(("value", time(value)));
            w.empty("value", &attributes);
        }
        ObservationValue::Attachment {
            content_type, data, ..
        } => {
            let mut attributes = typed("ED");
            attributes.push(("mediaType", content_type.clone()));
            attributes.push(("representation", some("B64")));
            w.text("value", &attributes, data);
        }
    }
}

/// The `statusCode`; an unknown status is left out.
fn status_code(status: ObservationStatus) -> Option<&'static str> {
    Some(match status {
        ObservationStatus::Registered => "new",
        ObservationStatus::Preliminary => "active",
        ObservationStatus::Final | ObservationStatus::Amended | ObservationStatus::Corrected => {
            "completed"
        }
        ObservationStatus::Cancelled => "cancelled",
        ObservationStatus::EnteredInError => "nullified",
        ObservationStatus::Unknown => return None,
    })
}

fn reference_range(w: &mut Writer, range: &ReferenceRange) {
    w.open("referenceRange", &[]);
    w.open("observationRange", &[]);
    if let Some(text) = &range.text {
        w.text("text", &[], text);
    }
    if range.low.is_some() || range.high.is_some() {
        w.open("value", &[("xsi:type", some("IVL_PQ"))]);
        if let Some(low) = &range.low {
            w.empty("low", &[("value", Some(low.as_str().to_owned()))]);
        }
        if let Some(high) = &range.high {
            w.empty("high", &[("value", Some(high.as_str().to_owned()))]);
        }
        w.close("value");
    }
    w.close("observationRange");
    w.close("referenceRange");
}

fn specimen(w: &mut Writer, id: &Identifier, kind: Option<&CodeableConcept>, root: Option<&str>) {
    w.open("specimen", &[("typeCode", some("SPC"))]);
    w.open("specimenRole", &[("classCode", some("SPEC"))]);
    instance_id(w, id, root);
    if let Some(kind) = kind {
        w.open("specimenPlayingEntity", &[]);
        coded(w, "code", kind, vec![], None);
        w.close("specimenPlayingEntity");
    }
    w.close("specimenRole");
    w.close("specimen");
}

fn observation(w: &mut Writer, observation: &Observation, settings: &CdaEncoding) {
    w.open("component", &[]);
    w.open(
        "observation",
        &[("classCode", some("OBS")), ("moodCode", some("EVN"))],
    );
    coded(w, "code", &observation.code, vec![], None);
    if let Some(status) = status_code(observation.status) {
        w.empty("statusCode", &[("code", some(status))]);
    }
    if let Some(at) = &observation.effective_at {
        w.empty("effectiveTime", &[("value", time(at))]);
    }
    if let Some(value) = &observation.value {
        write_value(w, value);
    }
    for flag in &observation.interpretation {
        coded(
            w,
            "interpretationCode",
            &CodeableConcept::from_coding(flag.clone()),
            vec![],
            Some(INTERPRETATION_OID),
        );
    }
    if let Some(method) = &observation.method {
        coded(w, "methodCode", method, vec![], None);
    }
    if let Some(id) = &observation.specimen_id {
        specimen(
            w,
            &Identifier::new(id.clone()),
            None,
            settings.specimen_id_root.as_deref(),
        );
    }
    for note in &observation.notes {
        w.open(
            "entryRelationship",
            &[("typeCode", some("SUBJ")), ("inversionInd", some("true"))],
        );
        w.open(
            "act",
            &[("classCode", some("ACT")), ("moodCode", some("EVN"))],
        );
        w.empty(
            "code",
            &[
                ("code", some("48767-8")),
                ("codeSystem", some(LOINC_OID)),
                ("codeSystemName", some("LOINC")),
                ("displayName", some("Annotation comment")),
            ],
        );
        w.text("text", &[], note);
        w.close("act");
        w.close("entryRelationship");
    }
    if let Some(range) = &observation.reference_range {
        reference_range(w, range);
    }
    w.close("observation");
    w.close("component");
}

fn display(concept: &CodeableConcept) -> String {
    concept
        .text
        .clone()
        .or_else(|| concept.codings.iter().find_map(|c| c.display.clone()))
        .or_else(|| concept.primary_code().map(str::to_owned))
        .unwrap_or_default()
}

fn value_text(value: &ObservationValue) -> (String, String) {
    let q = |q: &Quantity| q.value.as_str().to_owned();
    match value {
        ObservationValue::Quantity(quantity) => (
            format!(
                "{}{}",
                quantity
                    .comparator
                    .map(Comparator::as_str)
                    .unwrap_or_default(),
                q(quantity)
            ),
            quantity.unit.clone().unwrap_or_default(),
        ),
        ObservationValue::Range { low, high } => (
            format!(
                "{}-{}",
                low.as_ref().map(q).unwrap_or_default(),
                high.as_ref().map(q).unwrap_or_default()
            ),
            low.as_ref()
                .or(high.as_ref())
                .and_then(|x| x.unit.clone())
                .unwrap_or_default(),
        ),
        ObservationValue::Ratio {
            numerator,
            denominator,
        } => (
            format!("{}:{}", q(numerator), q(denominator)),
            String::new(),
        ),
        ObservationValue::Coded(concept) => (display(concept), String::new()),
        ObservationValue::Text(text) => (text.clone(), String::new()),
        ObservationValue::Boolean(value) => (value.to_string(), String::new()),
        ObservationValue::DateTime(value) => (value.to_iso(), String::new()),
        ObservationValue::Attachment { content_type, .. } => (
            format!(
                "[attachment{}]",
                content_type
                    .as_deref()
                    .map(|t| format!(" {t}"))
                    .unwrap_or_default()
            ),
            String::new(),
        ),
    }
}

fn range_text(range: &ReferenceRange) -> String {
    range.text.clone().unwrap_or_else(|| {
        format!(
            "{}-{}",
            range.low.as_ref().map(|d| d.as_str()).unwrap_or_default(),
            range.high.as_ref().map(|d| d.as_str()).unwrap_or_default()
        )
    })
}

fn narrative(w: &mut Writer, groups: &[ResultGroup]) {
    w.open("text", &[]);
    w.open("table", &[]);
    w.open("thead", &[]);
    w.open("tr", &[]);
    for heading in [
        "Test",
        "Result",
        "Unit",
        "Reference range",
        "Flags",
        "Time",
        "Specimen",
    ] {
        w.text("th", &[], heading);
    }
    w.close("tr");
    w.close("thead");
    w.open("tbody", &[]);
    for group in groups {
        let group_specimen = group
            .specimen
            .as_ref()
            .and_then(|s| s.identifiers.first())
            .map(|id| id.value.clone());
        for observation in &group.observations {
            let (result, unit) = observation
                .value
                .as_ref()
                .map(value_text)
                .unwrap_or_default();
            let flags: Vec<&str> = observation
                .interpretation
                .iter()
                .map(|c| c.code.as_str())
                .collect();
            w.open("tr", &[]);
            for cell in [
                display(&observation.code),
                result,
                unit,
                observation
                    .reference_range
                    .as_ref()
                    .map(range_text)
                    .unwrap_or_default(),
                flags.join(" "),
                observation
                    .effective_at
                    .as_ref()
                    .map(ClinicalDateTime::to_iso)
                    .unwrap_or_default(),
                observation
                    .specimen_id
                    .clone()
                    .or_else(|| group_specimen.clone())
                    .unwrap_or_default(),
            ] {
                w.text("td", &[], &cell);
            }
            w.close("tr");
        }
    }
    w.close("tbody");
    w.close("table");
    w.close("text");
}

fn patient(w: &mut Writer, patient: Option<&Patient>, settings: &CdaEncoding) {
    w.open("recordTarget", &[]);
    w.open("patientRole", &[]);
    let identifiers = patient
        .map(|p| p.identifiers.as_slice())
        .unwrap_or_default();
    if identifiers.is_empty() {
        w.empty("id", &[("nullFlavor", some("UNK"))]);
    }
    for identifier in identifiers {
        instance_id(w, identifier, settings.patient_id_root.as_deref());
    }
    if let Some(patient) = patient {
        w.open("patient", &[]);
        if let Some(name) = &patient.name {
            person_name(w, name);
        }
        let gender = patient.sex.map(|sex| match sex.to_hl7_code() {
            "U" | "O" => "UN",
            code => code,
        });
        match gender {
            Some(code) => w.empty(
                "administrativeGenderCode",
                &[("code", some(code)), ("codeSystem", some(GENDER_OID))],
            ),
            None => w.empty("administrativeGenderCode", &[("nullFlavor", some("UNK"))]),
        }
        match &patient.birth_date {
            Some(birth) => w.empty("birthTime", &[("value", time(birth))]),
            None => w.empty("birthTime", &[("nullFlavor", some("UNK"))]),
        }
        w.close("patient");
    }
    w.close("patientRole");
    w.close("recordTarget");
}

/// Writes a CDA R2 laboratory report for normalized `Results`. `id`
/// becomes the document identifier (as a UUID) and `timestamp` its time.
pub fn encode_lab_report(
    content: &ClinicalContent,
    settings: &CdaEncoding,
    id: MessageId,
    timestamp: Timestamp,
) -> CdaResult<XmlDocument> {
    let ClinicalContent::Results { device, groups } = content else {
        return Err(CdaError::Unsupported(
            "a CDA laboratory report is written from results".into(),
        ));
    };
    let groups: Vec<&ResultGroup> = groups
        .iter()
        .filter(|g| !g.observations.is_empty())
        .collect();
    if groups.is_empty() {
        return Err(CdaError::NoResults);
    }
    let mut patients: Vec<&Patient> = Vec::new();
    for patient in groups.iter().filter_map(|g| g.patient.as_ref()) {
        if !patients.contains(&patient) {
            patients.push(patient);
        }
    }
    if patients.len() > 1 {
        return Err(CdaError::Unsupported(
            "the results are about more than one patient; a CDA document is about one".into(),
        ));
    }
    let now = ClinicalDateTime::from_timestamp(timestamp, settings.utc_offset_minutes)
        .ok_or_else(|| CdaError::Unsupported("the time is out of range".into()))?;
    let mut w = Writer {
        out: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"),
        depth: 0,
    };
    w.open(
        "ClinicalDocument",
        &[
            ("xmlns", some("urn:hl7-org:v3")),
            (
                "xmlns:xsi",
                some("http://www.w3.org/2001/XMLSchema-instance"),
            ),
        ],
    );
    w.empty(
        "typeId",
        &[
            ("root", some("2.16.840.1.113883.1.3")),
            ("extension", some("POCD_HD000040")),
        ],
    );
    w.empty("id", &[("root", Some(message_uuid(id)))]);
    w.empty(
        "code",
        &[
            ("code", some("11502-2")),
            ("codeSystem", some(LOINC_OID)),
            ("codeSystemName", some("LOINC")),
            ("displayName", some("Laboratory report")),
        ],
    );
    w.text("title", &[], &settings.title);
    w.empty("effectiveTime", &[("value", time(&now))]);
    w.empty(
        "confidentialityCode",
        &[
            ("code", Some(settings.confidentiality.clone())),
            ("codeSystem", some(CONFIDENTIALITY_OID)),
        ],
    );
    w.empty("languageCode", &[("code", Some(settings.language.clone()))]);
    patient(&mut w, patients.first().copied(), settings);

    w.open("author", &[]);
    w.empty("time", &[("value", time(&now))]);
    w.open("assignedAuthor", &[]);
    let device_ids = device
        .as_ref()
        .map(|d| d.identifiers.as_slice())
        .unwrap_or_default();
    if device_ids.is_empty() {
        w.empty("id", &[("nullFlavor", some("NI"))]);
    }
    for identifier in device_ids {
        instance_id(&mut w, identifier, None);
    }
    w.open("assignedAuthoringDevice", &[]);
    let model = device.as_ref().and_then(|d| d.model.clone());
    if let Some(model) = model {
        w.text("manufacturerModelName", &[], &model);
    }
    let software = device
        .as_ref()
        .and_then(|d| d.name.clone())
        .unwrap_or_else(|| settings.software_name.clone());
    w.text("softwareName", &[], &software);
    w.close("assignedAuthoringDevice");
    w.close("assignedAuthor");
    w.close("author");

    w.open("custodian", &[]);
    w.open("assignedCustodian", &[]);
    w.open("representedCustodianOrganization", &[]);
    match &settings.custodian_id_root {
        Some(root) => w.empty("id", &[("root", Some(root.clone()))]),
        None => w.empty("id", &[("nullFlavor", some("NI"))]),
    }
    if let Some(name) = &settings.custodian_name {
        w.text("name", &[], name);
    }
    w.close("representedCustodianOrganization");
    w.close("assignedCustodian");
    w.close("custodian");

    w.open("component", &[]);
    w.open("structuredBody", &[]);
    w.open("component", &[]);
    w.open("section", &[]);
    w.empty(
        "code",
        &[
            ("code", some("30954-2")),
            ("codeSystem", some(LOINC_OID)),
            ("codeSystemName", some("LOINC")),
            (
                "displayName",
                some("Relevant diagnostic tests/laboratory data"),
            ),
        ],
    );
    w.text("title", &[], "Laboratory results");
    let owned: Vec<ResultGroup> = groups.iter().map(|g| (*g).clone()).collect();
    narrative(&mut w, &owned);
    for group in &groups {
        w.open("entry", &[("typeCode", some("DRIV"))]);
        w.open(
            "organizer",
            &[("classCode", some("BATTERY")), ("moodCode", some("EVN"))],
        );
        match group.order.as_ref().and_then(|o| o.tests.first()) {
            Some(test) => coded(&mut w, "code", test, vec![], None),
            None => w.empty("code", &[("nullFlavor", some("UNK"))]),
        }
        w.empty("statusCode", &[("code", some("completed"))]);
        let specimen_id = group
            .specimen
            .as_ref()
            .and_then(|s| s.identifiers.first().cloned())
            .or_else(|| {
                group
                    .order
                    .as_ref()
                    .and_then(|o| o.specimen_ids.first())
                    .map(|id| Identifier::new(id.clone()))
            });
        if let Some(id) = &specimen_id {
            specimen(
                &mut w,
                id,
                group.specimen.as_ref().and_then(|s| s.kind.as_ref()),
                settings.specimen_id_root.as_deref(),
            );
        }
        for result in &group.observations {
            observation(&mut w, result, settings);
        }
        w.close("organizer");
        w.close("entry");
    }
    w.close("section");
    w.close("component");
    w.close("structuredBody");
    w.close("component");
    w.close("ClinicalDocument");
    XmlDocument::parse(w.out.as_bytes(), &XmlOptions::default()).map_err(CdaError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_uuids_and_escapes() {
        assert_eq!(
            message_uuid(MessageId::from_u128(1)),
            "00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(
            escape("a<b & \"c\"\u{1}", true),
            "a&lt;b &amp; &quot;c&quot; "
        );
        assert_eq!(escape("x\ny", false), "x\ny");
    }
}
