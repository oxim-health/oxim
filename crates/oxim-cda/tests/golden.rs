//! Golden tests with a synthetic laboratory report (no real patient data).

#![allow(clippy::unwrap_used, clippy::panic)]

use oxim_cda::{CdaEncoding, CdaError, encode_lab_report, header, lab_results, sections};
use oxim_formats::{XmlDocument, XmlOptions};
use oxim_model::{
    AdministrativeSex, ClinicalContent, Comparator, MessageId, ObservationStatus, ObservationValue,
    Timestamp,
};

const REPORT: &[u8] = include_bytes!("fixtures/lab-report.xml");

fn parse(bytes: &[u8]) -> XmlDocument {
    XmlDocument::parse(bytes, &XmlOptions::default()).unwrap()
}

#[test]
fn documents_pass_through_unchanged() {
    assert_eq!(parse(REPORT).to_bytes(), REPORT);
}

#[test]
fn reads_the_header() {
    let header = header(&parse(REPORT)).unwrap();
    let id = header.id.unwrap();
    assert_eq!(
        (id.value.as_str(), id.system.as_deref()),
        ("DOC-0001", Some("urn:oid:2.999.7.1"))
    );
    let code = header.code.unwrap();
    assert_eq!(code.primary_code(), Some("11502-2"));
    assert_eq!(code.codings[0].system.as_deref(), Some("http://loinc.org"));
    assert_eq!(header.title.as_deref(), Some("Synthetic Laboratory Report"));
    assert_eq!(
        header.effective_time.unwrap().to_hl7(),
        "20260929143000+0300"
    );
    assert_eq!(header.confidentiality.as_deref(), Some("N"));
    assert_eq!(header.language.as_deref(), Some("en-US"));
    let patient = &header.patients[0];
    assert_eq!(patient.identifiers[0].value, "SYN-PAT-42");
    assert_eq!(
        patient.identifiers[0].assigner.as_deref(),
        Some("Synthetic Hospital")
    );
    let name = patient.name.as_ref().unwrap();
    assert_eq!(
        (name.family.as_deref(), name.given.as_slice()),
        (Some("Doe"), &["Jane".to_owned(), "Q".to_owned()][..])
    );
    assert_eq!(patient.sex, Some(AdministrativeSex::Female));
    assert_eq!(patient.birth_date.unwrap().to_hl7(), "19800101");
    let device = header.authors[0].device.as_ref().unwrap();
    assert_eq!(device.name.as_deref(), Some("SynthLab"));
    assert_eq!(device.model.as_deref(), Some("Synthetic Analyzer 3000"));
    let custodian = header.custodian.unwrap();
    assert_eq!(custodian.name.as_deref(), Some("Synthetic Laboratory"));
    assert_eq!(custodian.identifiers[0].value, "2.999.7.4");
}

#[test]
fn reads_sections() {
    let sections = sections(&parse(REPORT)).unwrap();
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].title.as_deref(), Some("Chemistry"));
    assert_eq!(sections[0].text, "Glucose 5.40 mmol/L Hemoglobin <0.5 g/dL");
    assert_eq!(sections[0].entries[0].statement, "act");
    assert_eq!(sections[0].entries[0].type_code.as_deref(), Some("DRIV"));
    assert_eq!(
        sections[1].code.as_ref().unwrap().primary_code(),
        Some("18725-2")
    );
    assert_eq!(sections[1].entries.len(), 2);
    assert_eq!(sections[1].text, "Culture negative.");
}

#[test]
fn maps_laboratory_results() {
    let ClinicalContent::Results { device, groups } = lab_results(&parse(REPORT)).unwrap() else {
        panic!("expected results");
    };
    assert_eq!(device.unwrap().name.as_deref(), Some("SynthLab"));
    assert_eq!(groups.len(), 2);

    let panel = &groups[0];
    assert_eq!(
        panel.patient.as_ref().unwrap().identifiers[0].value,
        "SYN-PAT-42"
    );
    let specimen = panel.specimen.as_ref().unwrap();
    assert_eq!(specimen.identifiers[0].value, "S123");
    assert_eq!(specimen.kind.as_ref().unwrap().primary_code(), Some("SER"));
    let order = panel.order.as_ref().unwrap();
    assert_eq!(order.tests[0].primary_code(), Some("24323-8"));
    assert_eq!(order.specimen_ids, ["S123"]);
    // The requested (RQO) creatinine is not a result.
    assert_eq!(panel.observations.len(), 2);

    let glucose = &panel.observations[0];
    assert_eq!(glucose.code.codings.len(), 2);
    assert_eq!(
        glucose.code.codings[1].system.as_deref(),
        Some("urn:oid:2.999.7.6")
    );
    let Some(ObservationValue::Quantity(quantity)) = &glucose.value else {
        panic!("expected a quantity");
    };
    assert_eq!(quantity.value.as_str(), "5.40");
    assert_eq!(quantity.unit.as_deref(), Some("mmol/L"));
    assert_eq!(
        quantity.system.as_deref(),
        Some("http://unitsofmeasure.org")
    );
    assert_eq!(glucose.interpretation[0].code, "N");
    let range = glucose.reference_range.as_ref().unwrap();
    assert_eq!(
        (
            range.low.as_ref().unwrap().as_str(),
            range.high.as_ref().unwrap().as_str()
        ),
        ("3.9", "6.1")
    );
    assert_eq!(range.text.as_deref(), Some("3.9-6.1"));
    assert_eq!(glucose.status, ObservationStatus::Final);
    assert_eq!(
        glucose.effective_at.unwrap().to_hl7(),
        "20260929142500+0300"
    );
    assert_eq!(
        glucose.method.as_ref().unwrap().primary_code(),
        Some("HEXOKINASE")
    );
    assert_eq!(glucose.notes, ["Slight hemolysis"]);

    let hemoglobin = &panel.observations[1];
    let Some(ObservationValue::Quantity(limit)) = &hemoglobin.value else {
        panic!("expected a limit");
    };
    assert_eq!(
        (limit.comparator, limit.value.as_str()),
        (Some(Comparator::LessThan), "0.5")
    );
    assert_eq!(hemoglobin.status, ObservationStatus::Preliminary);

    let loose = &groups[1];
    assert!(loose.specimen.is_none());
    let Some(ObservationValue::Coded(culture)) = &loose.observations[0].value else {
        panic!("expected a coded result");
    };
    assert_eq!(
        culture.codings[0].system.as_deref(),
        Some("http://snomed.info/sct")
    );
    assert_eq!(loose.observations[0].specimen_id.as_deref(), Some("S124"));
    assert!(matches!(
        loose.observations[1].value,
        Some(ObservationValue::Ratio { .. })
    ));
    assert_eq!(
        loose.observations[1].code.codings[0].system.as_deref(),
        Some("urn:example:local")
    );
}

#[test]
fn rejects_other_documents() {
    let other = parse(b"<order><test/></order>");
    assert!(matches!(header(&other), Err(CdaError::NotCda(_))));
    let empty = parse(
        br#"<ClinicalDocument xmlns="urn:hl7-org:v3"><code code="34133-9" codeSystem="2.16.840.1.113883.6.1"/></ClinicalDocument>"#,
    );
    assert_eq!(lab_results(&empty), Err(CdaError::NoResults));
}

#[test]
fn writes_a_report_that_reads_back() {
    let content = lab_results(&parse(REPORT)).unwrap();
    let settings = CdaEncoding {
        custodian_name: Some("Synthetic Laboratory".into()),
        custodian_id_root: Some("2.999.7.4".into()),
        utc_offset_minutes: 180,
        ..CdaEncoding::default()
    };
    let id = MessageId::from_parts(1_790_000_000_000, 42);
    let received: Timestamp = "2026-09-29T11:30:00Z".parse().unwrap();
    let report = encode_lab_report(&content, &settings, id, received).unwrap();
    let text = String::from_utf8(report.to_bytes()).unwrap();
    assert!(
        text.starts_with(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ClinicalDocument xmlns=\"urn:hl7-org:v3\""
        ),
        "{text}"
    );
    assert!(
        text.contains("<effectiveTime value=\"20260929143000+0300\"/>"),
        "{text}"
    );
    assert!(
        text.contains("<value xsi:type=\"PQ\" value=\"5.40\" unit=\"mmol/L\"/>"),
        "{text}"
    );
    assert!(
        text.contains("<high value=\"0.5\" unit=\"g/dL\" inclusive=\"false\"/>"),
        "{text}"
    );
    assert!(text.contains("<td>&lt;0.5</td>"), "{text}");

    let reread = parse(text.as_bytes());
    let header = header(&reread).unwrap();
    assert_eq!(header.id.unwrap().value, oxim_cda::message_uuid(id));
    assert_eq!(header.patients[0].identifiers[0].value, "SYN-PAT-42");
    assert_eq!(
        header.custodian.unwrap().name.as_deref(),
        Some("Synthetic Laboratory")
    );
    // The results survive a write and read unchanged.
    assert_eq!(lab_results(&reread).unwrap(), content);

    let not_results = ClinicalContent::Orders { groups: Vec::new() };
    assert!(matches!(
        encode_lab_report(&not_results, &settings, id, received),
        Err(CdaError::Unsupported(_))
    ));
}
