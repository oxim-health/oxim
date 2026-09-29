//! Golden tests: full oxim-core pipelines built from YAML with the steps of
//! this crate.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use oxim_core::{
    ChannelConfig, CompiledPipeline, Document, Encoded, Encoder, MessageContext, Normalizer,
    Registry, StepError,
};
use oxim_model::{
    ChannelId, ClinicalContent, CodeableConcept, Coding, ConnectorId, DataType, Decimal, Envelope,
    MessageId, MessageStatus, Observation, ObservationValue, Quantity, ResultGroup, Timestamp,
};
use oxim_store::{Processed, Stage};
use oxim_transform::{TransformEnvironment, register};

/// Maps OBX segments to observations; enough to exercise map-observations.
#[derive(Debug)]
struct ObxNormalizer;

impl Normalizer for ObxNormalizer {
    fn normalize(&self, document: &Document) -> Result<ClinicalContent, StepError> {
        let mut observations = Vec::new();
        for n in 1.. {
            let Some(code) = document.get(&format!("OBX[{n}]-3.1"))? else {
                break;
            };
            let value = document.get(&format!("OBX[{n}]-5"))?.unwrap_or_default();
            observations.push(Observation {
                code: CodeableConcept::from_coding(Coding::new(code)),
                value: Some(ObservationValue::Quantity(Quantity::new(
                    Decimal::new(value).map_err(|e| StepError::new("normalize", e.to_string()))?,
                    document.get(&format!("OBX[{n}]-6"))?,
                ))),
                ..Observation::default()
            });
        }
        Ok(ClinicalContent::Results {
            device: None,
            groups: vec![ResultGroup {
                observations,
                ..ResultGroup::default()
            }],
        })
    }
}

/// Writes the normalized content as JSON.
#[derive(Debug)]
struct ClinicalJson;

impl Encoder for ClinicalJson {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        Ok(Encoded {
            data_type: DataType::Json,
            data: serde_json::to_vec(&context.clinical)
                .map_err(|e| StepError::new("encode", e.to_string()))?,
        })
    }
}

fn pipeline(yaml: &str, dir: &std::path::Path) -> CompiledPipeline {
    let mut registry = Registry::new();
    register(&mut registry, TransformEnvironment::new(dir));
    registry.add_normalizer(DataType::Hl7V2, Arc::new(ObxNormalizer));
    registry.add_encoder("clinical-json", |_| {
        Ok(Arc::new(ClinicalJson) as Arc<dyn Encoder>)
    });
    registry
        .compile(&ChannelConfig::from_yaml(yaml).unwrap())
        .unwrap()
}

fn envelope(data_type: DataType, raw: &[u8]) -> Envelope {
    Envelope::new(
        MessageId::from_parts(1_790_000_000_000, 1),
        ChannelId::new("lab").unwrap(),
        ConnectorId::new("source").unwrap(),
        Timestamp::from_unix_nanos(1_790_000_000_000_000_000),
        data_type,
        raw.to_vec(),
    )
}

fn encoded(processed: &Processed, destination: &str) -> String {
    let content = processed
        .contents
        .iter()
        .find(|c| {
            c.stage == Stage::Encoded
                && c.destination.as_ref().map(ConnectorId::as_str) == Some(destination)
        })
        .unwrap_or_else(|| panic!("no encoded content for {destination}: {processed:?}"));
    String::from_utf8(content.data.clone())
        .unwrap()
        .replace('\r', "\n")
}

fn tables() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("tables")).unwrap();
    std::fs::write(
        dir.path().join("tables").join("tests.csv"),
        "from,to,display,system\nGLU,1520,Glucose,urn:lis\nHGB,2010,Hemoglobin,urn:lis\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("tables").join("loinc.csv"),
        "from,to,display,system\nGLU,2345-7,Glucose,http://loinc.org\n",
    )
    .unwrap();
    dir
}

const ORU: &[u8] = b"MSH|^~\\&|CHEM|LAB|||20260929143005||ORU^R01|42|P|2.5.1\rPID|1||12345||Doe^Jane||19800115|F\rOBX|1|NM|GLU||5.40|mmol/L|||N\rOBX|2|NM|HGB||13.2|g/dL|||N\r";
const ADT: &[u8] =
    b"MSH|^~\\&|HIS|HOSP|||20260929143005||ADT^A01|43|P|2.5.1\rPID|1||12345||Doe^Jane\r";

#[test]
fn filters_maps_and_encodes_hl7() {
    let dir = tables();
    let pipeline = pipeline(
        r#"
id: lab
source: {type: mllp, data_type: hl7v2}
filters:
  - type: condition
    all:
      - {path: MSH-9.1, in: [ORU, OUL]}
      - not: {path: MSH-3, equals: TEST}
transformers:
  - type: map
    operations:
      # Read the display text before the code itself is replaced.
      - lookup: {table: tables/tests.csv, from: "OBX[*]-3.1", to: "OBX[*]-3.2", column: display}
      - lookup: {table: tables/tests.csv, from: "OBX[*]-3.1", on_missing: error}
      - set: {path: MSH-5, value: LIS}
      - set: {path: MSH-6, value: "{MSH-4}-{message.id}"}
destinations:
  - id: lis
    type: mllp
  - id: legacy
    type: file
    transformers:
      - type: map
        operations:
          - date: {path: PID-7, from: hl7, to: "%d.%m.%Y"}
          - when: {path: "OBX[*]-5", greater_than: "10"}
            set: {path: "OBX[*]-13", value: HIGH-VALUE-COPY}
"#,
        dir.path(),
    );

    let processed = pipeline.process(envelope(DataType::Hl7V2, ORU));
    assert_eq!(
        processed.status,
        MessageStatus::Transformed,
        "{processed:?}"
    );
    let lis = encoded(&processed, "lis");
    assert!(lis.starts_with("MSH|^~\\&|CHEM|LAB|LIS|LAB-"), "{lis}");
    assert!(
        lis.contains("\nOBX|1|NM|1520^Glucose||5.40|mmol/L|||N\n"),
        "{lis}"
    );
    assert!(
        lis.contains("\nOBX|2|NM|2010^Hemoglobin||13.2|g/dL|||N\n"),
        "{lis}"
    );
    assert!(lis.contains("PID|1||12345||Doe^Jane||19800115|F"), "{lis}");
    let legacy = encoded(&processed, "legacy");
    assert!(
        legacy.contains("PID|1||12345||Doe^Jane||15.01.1980|F"),
        "{legacy}"
    );
    assert!(
        legacy.contains("OBX|2|NM|2010^Hemoglobin||13.2|g/dL|||N||||HIGH-VALUE-COPY"),
        "{legacy}"
    );
    assert!(
        !legacy.contains("OBX|1|NM|1520^Glucose||5.40|mmol/L|||N||||"),
        "{legacy}"
    );

    let filtered = pipeline.process(envelope(DataType::Hl7V2, ADT));
    assert_eq!(filtered.status, MessageStatus::Filtered);

    let unknown = pipeline.process(envelope(
        DataType::Hl7V2,
        b"MSH|^~\\&|CHEM|LAB|||||ORU^R01|44|P|2.5.1\rOBX|1|NM|XYZ||1\r",
    ));
    assert_eq!(unknown.status, MessageStatus::Error);
    assert!(unknown.error.unwrap().contains("XYZ"));
}

#[test]
fn maps_astm_records() {
    let dir = tables();
    let pipeline = pipeline(
        r#"
id: analyzer
source: {type: astm-tcp, data_type: astm}
filters:
  - type: condition
    any:
      - {path: "R[*]-3.4", equals: GLU}
      - {path: "R[*]-3.4", equals: HGB}
transformers:
  - type: map
    operations:
      - store: {path: O-3, as: specimen}
      - lookup: {table: tables/tests.csv, from: "R[*]-3.4", on_missing: keep}
      - set: {path: "R[*]-13", value: "{$specimen}"}
destinations:
  - id: lis
    type: mllp
"#,
        dir.path(),
    );
    let raw = b"H|\\^&|||ANALYZER^1\rP|1||PAT1\rO|1|SMP-7||^^^GLU\rR|1|^^^GLU|5.4|mmol/L\rR|2|^^^NA|140|mmol/L\rL|1|N\r";
    let processed = pipeline.process(envelope(DataType::Astm, raw));
    assert_eq!(
        processed.status,
        MessageStatus::Transformed,
        "{processed:?}"
    );
    let lis = encoded(&processed, "lis");
    assert!(
        lis.contains("R|1|^^^1520|5.4|mmol/L||||||||SMP-7\n"),
        "{lis}"
    );
    assert!(lis.contains("R|2|^^^NA|140|mmol/L||||||||SMP-7\n"), "{lis}");

    let other = pipeline.process(envelope(
        DataType::Astm,
        b"H|\\^&\rP|1\rO|1|S\rR|1|^^^K|4.1\rL|1|N\r",
    ));
    assert_eq!(other.status, MessageStatus::Filtered);
}

#[test]
fn translates_normalized_observations() {
    let dir = tables();
    let pipeline = pipeline(
        r#"
id: lab
source: {type: mllp, data_type: hl7v2, normalize: true}
destinations:
  - id: fhir
    type: http
    transformers:
      - type: map-observations
        table: tables/loinc.csv
        on_missing: drop
        device_id: "{MSH-3}"
    encoder: {type: clinical-json}
"#,
        dir.path(),
    );
    let processed = pipeline.process(envelope(DataType::Hl7V2, ORU));
    assert_eq!(
        processed.status,
        MessageStatus::Transformed,
        "{processed:?}"
    );
    let json: serde_json::Value = serde_json::from_str(&encoded(&processed, "fhir")).unwrap();
    let observations = &json["groups"][0]["observations"];
    assert_eq!(observations.as_array().unwrap().len(), 1);
    assert_eq!(observations[0]["code"]["codings"][0]["code"], "2345-7");
    assert_eq!(observations[0]["code"]["codings"][1]["code"], "GLU");
    assert_eq!(observations[0]["value"]["value"]["value"], "5.40");
    assert_eq!(observations[0]["device_id"], "CHEM");

    // The normalized stage keeps the untranslated content.
    let normalized = processed
        .contents
        .iter()
        .find(|c| c.stage == Stage::Normalized)
        .unwrap();
    let stored: ClinicalContent = serde_json::from_slice(&normalized.data).unwrap();
    match stored {
        ClinicalContent::Results { groups, .. } => assert_eq!(groups[0].observations.len(), 2),
        other => panic!("unexpected content {other:?}"),
    }
}

#[test]
fn reports_configuration_errors_at_compile_time() {
    let dir = tables();
    let mut registry = Registry::new();
    register(&mut registry, TransformEnvironment::new(dir.path()));
    for (yaml, reason) in [
        (
            "id: a\nsource: {type: x, data_type: hl7v2}\nfilters:\n  - {type: condition, path: MSH-9.1}\n",
            "condition without a test",
        ),
        (
            "id: a\nsource: {type: x, data_type: hl7v2}\ntransformers:\n  - {type: map, operations: [{lookup: {table: tables/none.csv, from: OBX-3}}]}\n",
            "missing table file",
        ),
        (
            "id: a\nsource: {type: x, data_type: hl7v2}\ntransformers:\n  - {type: map, operations: [{lookup: {table: ../tests.csv, from: OBX-3}}]}\n",
            "table outside the configuration directory",
        ),
        (
            "id: a\nsource: {type: x, data_type: hl7v2}\ntransformers:\n  - {type: map-observations}\n",
            "map-observations without table",
        ),
    ] {
        let config = ChannelConfig::from_yaml(yaml).unwrap();
        assert!(registry.compile(&config).is_err(), "{reason}");
    }
}
