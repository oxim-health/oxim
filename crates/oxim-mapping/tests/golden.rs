//! Golden tests with synthetic messages (no real patient data).

#![allow(clippy::unwrap_used, clippy::panic)]

use oxim_mapping::codes::{ASTM_LOCAL, ASTM_TEST_ID};
use oxim_mapping::hl7::{Hl7Encoding, SpecimenPlacement};
use oxim_mapping::{astm, hl7, poct1a};
use oxim_model::{
    AdministrativeSex, ClinicalContent, CodeableConcept, Coding, Comparator, HumanName, Identifier,
    MessageId, ObservationStatus, ObservationValue, Order, OrderControl, OrderGroup, Patient,
    Priority, Specimen, Timestamp,
};

/// Shows CR as a visible line break so failures are readable.
fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\r', "\n")
}

const ASTM_RESULTS: &[u8] = b"H|\\^&|||ChemAnalyzer^2.1^SN1234|||||LIS||P|LIS2-A2|20260929143000\r\
P|1|PID001|LAB42||Doe^Jane^M||19800101|F\r\
O|1|S123^01^02||^^^GLU\\^^^HGB|R|20260929120000|20260929115500||||N||||SER\r\
R|1|^^^GLU|5.40|mmol/L|3.9-6.1|N||F||tech1|20260929142000|20260929142500|ChemAnalyzer\r\
C|1|I|Hemolysis low|G\r\
R|2|^^^HGB|<0.5|g/dL|12-16|L||F||tech1||20260929142600|ChemAnalyzer\r\
L|1|N\r";

fn id() -> MessageId {
    MessageId::from_parts(1_790_000_000_000, 42)
}

fn received() -> Timestamp {
    // 2026-09-29T11:30:00Z
    "2026-09-29T11:30:00Z".parse().unwrap()
}

fn settings() -> Hl7Encoding {
    let mut settings = Hl7Encoding::default();
    settings.receiving_application = Some("LIS".into());
    settings.utc_offset_minutes = 180;
    settings
}

#[test]
fn normalizes_astm_results() {
    let message = oxim_astm::Message::parse(ASTM_RESULTS).unwrap();
    let ClinicalContent::Results { device, groups } = astm::normalize(&message).unwrap() else {
        panic!("expected results");
    };
    let device = device.unwrap();
    assert_eq!(device.name.as_deref(), Some("ChemAnalyzer"));
    assert_eq!(device.software_version.as_deref(), Some("2.1"));
    assert_eq!(device.serial_number.as_deref(), Some("SN1234"));
    assert_eq!(groups.len(), 1);
    let group = &groups[0];
    let patient = group.patient.as_ref().unwrap();
    assert_eq!(
        patient.identifiers,
        [
            Identifier {
                value: "PID001".into(),
                kind: Some("PI".into()),
                ..Identifier::default()
            },
            Identifier {
                value: "LAB42".into(),
                kind: Some("LR".into()),
                ..Identifier::default()
            },
        ]
    );
    assert_eq!(
        patient.name,
        Some(HumanName {
            family: Some("Doe".into()),
            given: vec!["Jane".into(), "M".into()],
            ..HumanName::default()
        })
    );
    assert_eq!(patient.birth_date.unwrap().to_hl7(), "19800101");
    assert_eq!(patient.sex, Some(AdministrativeSex::Female));

    let specimen = group.specimen.as_ref().unwrap();
    assert_eq!(specimen.identifiers[0].value, "S123");
    assert_eq!(specimen.container.as_deref(), Some("01^02"));
    assert_eq!(specimen.collected_at.unwrap().to_hl7(), "20260929115500");
    assert_eq!(specimen.kind.as_ref().unwrap().primary_code(), Some("SER"));

    let order = group.order.as_ref().unwrap();
    assert_eq!(order.tests.len(), 2);
    assert_eq!(order.tests[1].code_in(ASTM_LOCAL), Some("HGB"));
    assert_eq!(order.priority, Some(Priority::Routine));
    assert_eq!(order.control, Some(OrderControl::New));
    assert_eq!(order.specimen_ids, ["S123"]);

    let glucose = &group.observations[0];
    assert_eq!(glucose.sequence, Some(1));
    assert_eq!(glucose.code.code_in(ASTM_LOCAL), Some("GLU"));
    assert_eq!(glucose.code.code_in(ASTM_TEST_ID), Some("^^^GLU"));
    let Some(ObservationValue::Quantity(quantity)) = &glucose.value else {
        panic!("expected a quantity");
    };
    assert_eq!(quantity.value.as_str(), "5.40");
    assert_eq!(quantity.unit.as_deref(), Some("mmol/L"));
    assert_eq!(
        glucose.reference_range.as_ref().unwrap().text.as_deref(),
        Some("3.9-6.1")
    );
    assert_eq!(glucose.interpretation, [Coding::new("N")]);
    assert_eq!(glucose.status, ObservationStatus::Final);
    assert_eq!(glucose.operator.as_deref(), Some("tech1"));
    assert_eq!(glucose.effective_at.unwrap().to_hl7(), "20260929142500");
    assert_eq!(glucose.device_id.as_deref(), Some("ChemAnalyzer"));
    assert_eq!(glucose.specimen_id.as_deref(), Some("S123"));
    assert_eq!(glucose.notes, ["Hemolysis low"]);

    let Some(ObservationValue::Quantity(limit)) = &group.observations[1].value else {
        panic!("expected a quantity");
    };
    assert_eq!(
        (limit.comparator, limit.value.as_str()),
        (Some(Comparator::LessThan), "0.5")
    );
}

#[test]
fn encodes_astm_results_as_oru_r01() {
    let message = oxim_astm::Message::parse(ASTM_RESULTS).unwrap();
    let content = astm::normalize(&message).unwrap();
    let oru = hl7::encode_results(&content, &settings(), id(), received()).unwrap();
    let expected = "MSH|^~\\&|OXIM||LIS||20260929143000+0300||ORU^R01^ORU_R01|01M3250V00000000000000001A|P|2.5.1
PID|1||PID001^^^^PI~LAB42^^^^LR||Doe^Jane^M||19800101|F
ORC|RE||S123
OBR|1||S123|GLU^^L|||20260929115500||||||||||||||||||F||^^^^^R
SPM|1|S123||SER|||||||||||||20260929115500
OBX|1|NM|GLU^^L||5.40|mmol/L|3.9-6.1|N|||F|||20260929142500||tech1||ChemAnalyzer
NTE|1||Hemolysis low
OBX|2|SN|HGB^^L||<^0.5|g/dL|12-16|L|||F|||20260929142600||tech1||ChemAnalyzer
";
    assert_eq!(show(&oru.to_bytes()), expected);
    // The output is valid HL7 and maps back to the same values.
    let reparsed = oxim_hl7::Message::parse(&oru.to_bytes()).unwrap();
    let ClinicalContent::Results { groups, .. } = hl7::normalize(&reparsed).unwrap() else {
        panic!("expected results");
    };
    let ClinicalContent::Results {
        groups: original, ..
    } = content
    else {
        panic!("expected results");
    };
    assert_eq!(
        groups[0].observations[0].value,
        original[0].observations[0].value
    );
    assert_eq!(
        groups[0].observations[1].value,
        original[0].observations[1].value
    );
    assert_eq!(groups[0].patient, original[0].patient);
}

const HL7_ORU: &[u8] = b"MSH|^~\\&|MIDDLEWARE|LAB|LIS|HOSP|20260929143000+0300||ORU^R01^ORU_R01|MSG1|P|2.5.1\r\
PID|1||42^^^HOSP^MR||Yilmaz^Ayse||19750305|F\r\
ORC|RE|ORD7|S900\r\
OBR|1|ORD7|S900|2345-7^Glucose^LN|||20260929120000\r\
SPM|1|S900^S900||SER^Serum^HL70487|||||||||||||20260929120000\r\
OBX|1|NM|2345-7^Glucose^LN^GLU^Glucose^99LAB||5.40|mmol/L^mmol/L^UCUM|3.9-6.1|N|||F|||20260929142500\r\
OBX|2|SN|1558-6^Fasting glucose^LN||<^0.5|mmol/L|||||F\r\
OBX|3|ST|8251-1^Comment^LN||Slightly lipemic||||||F\r\
NTE|1||Repeat recommended\r\
OBX|4|CWE|882-1^ABO+Rh group^LN||A^A positive^99BLD||||||F\r";

#[test]
fn round_trips_hl7_results() {
    let message = oxim_hl7::Message::parse(HL7_ORU).unwrap();
    let content = hl7::normalize(&message).unwrap();
    let ClinicalContent::Results { groups, .. } = &content else {
        panic!("expected results");
    };
    let observations = &groups[0].observations;
    assert_eq!(observations.len(), 4);
    assert_eq!(
        observations[0].code.code_in("http://loinc.org"),
        Some("2345-7")
    );
    assert_eq!(observations[0].code.code_in("99LAB"), Some("GLU"));
    let Some(ObservationValue::Quantity(quantity)) = &observations[0].value else {
        panic!("expected a quantity");
    };
    assert_eq!(quantity.value.as_str(), "5.40");
    assert_eq!(
        quantity.system.as_deref(),
        Some("http://unitsofmeasure.org")
    );
    assert_eq!(observations[2].notes, ["Repeat recommended"]);
    assert_eq!(
        observations[3].value,
        Some(ObservationValue::Coded(CodeableConcept {
            codings: vec![Coding {
                system: Some("99BLD".into()),
                code: "A".into(),
                display: Some("A positive".into())
            }],
            text: Some("A positive".into()),
        }))
    );
    assert_eq!(
        groups[0].order.as_ref().unwrap().placer_id.as_deref(),
        Some("ORD7")
    );
    assert_eq!(
        groups[0].specimen.as_ref().unwrap().identifiers[0].value,
        "S900"
    );

    let encoded = hl7::encode_results(&content, &settings(), id(), received()).unwrap();
    let again = hl7::normalize(&oxim_hl7::Message::parse(&encoded.to_bytes()).unwrap()).unwrap();
    let ClinicalContent::Results { groups: second, .. } = &again else {
        panic!("expected results");
    };
    assert_eq!(
        second[0].observations,
        groups[0].observations,
        "{}",
        show(&encoded.to_bytes())
    );
    assert_eq!(second[0].patient, groups[0].patient);
    assert_eq!(second[0].specimen, groups[0].specimen);
}

#[test]
fn encodes_orders_for_analyzers() {
    let content = ClinicalContent::Orders {
        groups: vec![OrderGroup {
            patient: Some(Patient {
                identifiers: vec![Identifier {
                    value: "PID001".into(),
                    kind: Some("PI".into()),
                    ..Identifier::default()
                }],
                name: Some(HumanName {
                    family: Some("Doe".into()),
                    given: vec!["Jane".into()],
                    ..HumanName::default()
                }),
                sex: Some(AdministrativeSex::Female),
                ..Patient::default()
            }),
            specimen: Some(Specimen {
                identifiers: vec![Identifier::new("S123")],
                ..Specimen::default()
            }),
            order: Order {
                tests: vec![
                    CodeableConcept::from_coding(Coding::new("GLU").with_system(ASTM_LOCAL)),
                    CodeableConcept::from_coding(Coding::new("HGB")),
                ],
                priority: Some(Priority::Stat),
                control: Some(OrderControl::New),
                ..Order::default()
            },
        }],
    };
    let message =
        astm::encode_orders(&content, &astm::AstmEncoding::default(), received()).unwrap();
    assert_eq!(
        show(&message.to_bytes()),
        "H|\\^&|||OXIM|||||||P|LIS2-A2|20260929113000
P|1|PID001|||Doe^Jane|||F
O|1|S123||^^^GLU\\^^^HGB|S||||||N||||||||||||||O
L|1|N
"
    );
    let empty = ClinicalContent::Orders { groups: vec![] };
    let message = astm::encode_orders(&empty, &astm::AstmEncoding::default(), received()).unwrap();
    assert!(show(&message.to_bytes()).ends_with("L|1|I\n"));
    assert!(
        astm::encode_orders(
            &ClinicalContent::Results {
                device: None,
                groups: vec![]
            },
            &astm::AstmEncoding::default(),
            received()
        )
        .is_err()
    );
}

#[test]
fn normalizes_host_queries() {
    let message =
        oxim_astm::Message::parse(b"H|\\^&|||Analyzer\rQ|1|^S555||^^^ALL||||||||O\rL|1|N\r")
            .unwrap();
    let ClinicalContent::Query { query, .. } = astm::normalize(&message).unwrap() else {
        panic!("expected a query");
    };
    assert_eq!(query.specimen_ids, ["S555"]);
    assert!(query.all_tests);

    let qbp = oxim_hl7::Message::parse(
        b"MSH|^~\\&|ANALYZER||LIS||20260929||QBP^Q11^QBP_Q11|Q1|P|2.5.1\rQPD|WOS^Work Order Step^IHE_LABTF|q1|S777\r",
    )
    .unwrap();
    let ClinicalContent::Query { query, .. } = hl7::normalize(&qbp).unwrap() else {
        panic!("expected a query");
    };
    assert_eq!(query.specimen_ids, ["S777"]);
}

const POCT_OBS: &str = r#"<OBS.R01><HDR><HDR.control_id V="7"/><HDR.version_id V="POCT1"/><HDR.creation_dttm V="2026-09-29T14:30:00+03:00"/></HDR><SVC><SVC.role_cd V="OBS"/><SVC.observation_dttm V="2026-09-29T14:25:00+03:00"/><PT><PT.patient_id V="P77"/><OBS><OBS.observation_id V="2345-7" SN="LN"/><OBS.value V="5.4" U="mmol/L"/><OBS.normal_flag V="N"/></OBS></PT><OPR><OPR.operator_id V="nurse1"/></OPR></SVC><SVC><SVC.role_cd V="LQC"/><CTC><CTC.name V="Level 1"/><CTC.lot_number V="L123"/><OBS><OBS.observation_id V="2345-7" SN="LN"/><OBS.value V="3.1" U="mmol/L"/></OBS></CTC></SVC></OBS.R01>"#;

#[test]
fn normalizes_poct_observations() {
    let message = oxim_poct1a::Message::parse(POCT_OBS.as_bytes()).unwrap();
    let ClinicalContent::Results { groups, .. } = poct1a::normalize(&message).unwrap() else {
        panic!("expected results");
    };
    assert_eq!(groups.len(), 2);
    let patient = &groups[0];
    assert_eq!(
        patient.patient.as_ref().unwrap().identifiers[0].value,
        "P77"
    );
    let glucose = &patient.observations[0];
    assert_eq!(glucose.code.code_in("http://loinc.org"), Some("2345-7"));
    assert_eq!(glucose.operator.as_deref(), Some("nurse1"));
    assert_eq!(
        glucose.effective_at.unwrap().to_iso(),
        "2026-09-29T14:25:00+03:00"
    );
    assert_eq!(glucose.interpretation, [Coding::new("N")]);
    let control = &groups[1];
    assert!(control.patient.is_none());
    assert_eq!(
        control.observations[0].notes,
        ["Control material: Level 1", "Lot: L123"]
    );

    let only_qc = POCT_OBS.replace(r#"<SVC.role_cd V="OBS"/>"#, r#"<SVC.role_cd V="EQC"/>"#);
    let message = oxim_poct1a::Message::parse(only_qc.as_bytes()).unwrap();
    let ClinicalContent::QualityControl { results, .. } = poct1a::normalize(&message).unwrap()
    else {
        panic!("expected quality control");
    };
    assert_eq!(results.len(), 2);
    assert_eq!(results[1].lot.as_deref(), Some("L123"));

    let qc = hl7::encode_results(
        &ClinicalContent::QualityControl {
            device: None,
            results,
        },
        &settings(),
        id(),
        received(),
    )
    .unwrap();
    let text = show(&qc.to_bytes());
    assert!(text.contains("SPM|1||||||||||Q"), "{text}");
    assert!(text.contains("NTE|2||Lot: L123"), "{text}");
}

#[test]
fn obr3_only_placement_skips_spm() {
    let message = oxim_astm::Message::parse(ASTM_RESULTS).unwrap();
    let content = astm::normalize(&message).unwrap();
    let mut settings = settings();
    settings.specimen = SpecimenPlacement::Obr3;
    let oru = hl7::encode_results(&content, &settings, id(), received()).unwrap();
    assert!(!show(&oru.to_bytes()).contains("SPM|"));
    settings.charset = Some("8859/9".into());
    let oru = hl7::encode_results(&content, &settings, id(), received()).unwrap();
    assert!(
        show(&oru.to_bytes())
            .starts_with("MSH|^~\\&|OXIM||LIS||20260929143000+0300||ORU^R01^ORU_R01|")
    );
    assert_eq!(oru.get("MSH-18").unwrap(), "8859/9");
}
