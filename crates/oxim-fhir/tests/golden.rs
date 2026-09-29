//! Golden tests: synthetic ASTM and HL7 v2 messages (no real patient data)
//! → normalized content → FHIR transaction Bundles, compared with the
//! expected files in `tests/fixtures`.
//!
//! Set `OXIM_BLESS=1` to rewrite the expected files after an intended
//! change, then review the diff.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use oxim_fhir::{
    BundleType, FhirEncoding, Resource, encode_bundle, normalize, summarize_response, validate,
};
use oxim_model::{ClinicalContent, MessageId, ObservationValue, Timestamp};

const ASTM_RESULTS: &[u8] = b"H|\\^&|||ChemAnalyzer^2.1^SN1234|||||LIS||P|LIS2-A2|20260929143000\r\
P|1|PID001|LAB42||Doe^Jane^M||19800101|F\r\
O|1|S123^01^02||^^^GLU\\^^^HGB|R|20260929120000|20260929115500||||N||||SER\r\
R|1|^^^GLU|5.40|mmol/L|3.9-6.1|N||F||tech1|20260929142000|20260929142500|ChemAnalyzer\r\
C|1|I|Hemolysis low|G\r\
R|2|^^^HGB|<0.5|g/dL|12-16|L||F||tech1||20260929142600|ChemAnalyzer\r\
L|1|N\r";

const HL7_ORU: &[u8] = b"MSH|^~\\&|MIDDLEWARE|LAB|LIS|HOSP|20260929143000+0300||ORU^R01^ORU_R01|MSG1|P|2.5.1\r\
PID|1||42^^^HOSP^MR||Doe^John||19750305|M\r\
ORC|RE|ORD7|S900\r\
OBR|1|ORD7|S900|2345-7^Glucose^LN|||20260929120000\r\
SPM|1|S900^S900||SER^Serum^HL70487|||||||||||||20260929120000\r\
OBX|1|NM|2345-7^Glucose^LN^GLU^Glucose^99LAB||5.40|mmol/L^mmol/L^UCUM|3.9-6.1|H|||F|||20260929142500+0300\r\
OBX|2|SN|1558-6^Fasting glucose^LN||<^0.5|mmol/L|||||F\r\
OBX|3|ST|8251-1^Comment^LN||Slightly lipemic||||||F\r\
NTE|1||Repeat recommended\r\
OBX|4|CWE|882-1^ABO+Rh group^LN||A^A positive^99BLD||||||P\r";

fn id() -> MessageId {
    MessageId::from_parts(1_790_000_000_000, 42)
}

fn received() -> Timestamp {
    "2026-09-29T11:30:00Z".parse().unwrap()
}

fn settings() -> FhirEncoding {
    FhirEncoding {
        patient_identifier_system: Some("urn:oid:2.16.840.1.113883.19.5".into()),
        specimen_identifier_system: Some("urn:oxim:test:specimen".into()),
        utc_offset_minutes: 180,
        ..FhirEncoding::default()
    }
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Compares `content` encoded as a Bundle with the expected file.
fn check_golden(content: &ClinicalContent, name: &str) -> Resource {
    let bundle = Resource::from(encode_bundle(content, &settings(), id(), received()).unwrap());
    let mut json = String::from_utf8(bundle.to_json_pretty().unwrap()).unwrap();
    json.push('\n');
    let path = fixture(name);
    if std::env::var_os("OXIM_BLESS").is_some() {
        std::fs::write(&path, &json).unwrap();
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n");
    assert_eq!(json, expected, "{name} differs; set OXIM_BLESS=1 to update");
    // The expected file is itself a valid bundle.
    let parsed = Resource::from_json(expected.as_bytes()).unwrap();
    assert_eq!(parsed, bundle);
    assert_eq!(validate(&parsed), []);
    parsed
}

#[test]
fn astm_results_become_a_transaction_bundle() {
    let message = oxim_astm::Message::parse(ASTM_RESULTS).unwrap();
    let content = oxim_mapping::astm::normalize(&message).unwrap();
    let bundle = check_golden(&content, "astm-results.bundle.json");
    let Resource::Bundle(bundle) = &bundle else {
        panic!("expected a bundle");
    };
    assert_eq!(bundle.kind, "transaction");
    let types: Vec<&str> = bundle
        .entry
        .iter()
        .map(|e| e.resource.as_ref().unwrap().resource_type())
        .collect();
    assert_eq!(
        types,
        [
            "Device",
            "Patient",
            "Specimen",
            "ServiceRequest",
            "ServiceRequest",
            "Observation",
            "Observation",
            "DiagnosticReport"
        ]
    );
}

#[test]
fn hl7_results_become_a_transaction_bundle() {
    let message = oxim_hl7::Message::parse(HL7_ORU).unwrap();
    let content = oxim_mapping::hl7::normalize(&message).unwrap();
    let bundle = check_golden(&content, "hl7-oru.bundle.json");

    // Mapping the bundle back keeps the values.
    let ClinicalContent::Results { groups, .. } = normalize(&bundle).unwrap() else {
        panic!("expected results");
    };
    let ClinicalContent::Results {
        groups: original, ..
    } = &content
    else {
        panic!("expected results");
    };
    assert_eq!(groups.len(), 1);
    for (back, original) in groups[0].observations.iter().zip(&original[0].observations) {
        assert_eq!(back.value, with_ucum(original.value.clone()));
        assert_eq!(back.code, original.code);
        assert_eq!(back.status, original.status);
    }
    // Identifiers without a system get the configured one.
    let mut patient = original[0].patient.clone().unwrap();
    for identifier in &mut patient.identifiers {
        identifier.system = settings().patient_identifier_system;
    }
    assert_eq!(groups[0].patient.as_ref(), Some(&patient));
}

/// The value as mapped: units that look like UCUM codes and have no
/// system get the UCUM system and code.
fn with_ucum(value: Option<ObservationValue>) -> Option<ObservationValue> {
    match value {
        Some(ObservationValue::Quantity(mut q)) if q.system.is_none() && q.code.is_none() => {
            q.system = Some("http://unitsofmeasure.org".into());
            q.code.clone_from(&q.unit);
            Some(ObservationValue::Quantity(q))
        }
        other => other,
    }
}

#[test]
fn collections_have_no_requests() {
    let message = oxim_hl7::Message::parse(HL7_ORU).unwrap();
    let content = oxim_mapping::hl7::normalize(&message).unwrap();
    let settings = FhirEncoding {
        bundle_type: BundleType::Collection,
        ..settings()
    };
    let bundle = encode_bundle(&content, &settings, id(), received()).unwrap();
    assert_eq!(bundle.kind, "collection");
    assert!(bundle.entry.iter().all(|e| e.request.is_none()));
    assert_eq!(validate(&Resource::from(bundle)), []);
}

#[test]
fn encoding_is_deterministic() {
    let message = oxim_hl7::Message::parse(HL7_ORU).unwrap();
    let content = oxim_mapping::hl7::normalize(&message).unwrap();
    let first = encode_bundle(&content, &settings(), id(), received()).unwrap();
    let second = encode_bundle(&content, &settings(), id(), received()).unwrap();
    assert_eq!(first, second);
    let other = encode_bundle(
        &content,
        &settings(),
        MessageId::from_parts(1_790_000_000_001, 7),
        received(),
    )
    .unwrap();
    assert_ne!(first.entry[0].full_url, other.entry[0].full_url);
}

#[test]
fn summarizes_the_expected_server_response() {
    let summary =
        summarize_response(&std::fs::read(fixture("transaction-response.json")).unwrap()).unwrap();
    assert!(!summary.success);
    assert_eq!(summary.entries.len(), 3);
    assert_eq!(
        summary.entries.iter().map(|e| e.code).collect::<Vec<_>>(),
        [Some(201), Some(200), Some(422)]
    );
    assert_eq!(summary.entries[2].issues[0].code, "required");
}
