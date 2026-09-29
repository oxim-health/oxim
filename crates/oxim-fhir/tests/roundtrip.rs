//! Round trips between FHIR JSON, the typed resources and the normalized
//! model, with synthetic data only.

#![allow(clippy::unwrap_used, clippy::panic)]

use oxim_fhir::{
    FhirEncoding, Resource, encode_bundle, normalize, to_fhir::V3_INTERPRETATION, validate,
};
use oxim_model::{
    AdministrativeSex, ClinicalContent, ClinicalDateTime, CodeableConcept, Coding, Comparator,
    Decimal, Device, HumanName, Identifier, MessageId, Observation, ObservationStatus,
    ObservationValue, Order, OrderControl, OrderGroup, Patient, Priority, QcResult, Quantity,
    ReferenceRange, ResultGroup, Specimen, Timestamp,
};
use serde_json::Value;

const UCUM: &str = "http://unitsofmeasure.org";
const LOINC: &str = "http://loinc.org";

fn id() -> MessageId {
    MessageId::from_parts(1_790_000_000_000, 42)
}

fn received() -> Timestamp {
    "2026-09-29T11:30:00Z".parse().unwrap()
}

fn at(text: &str) -> ClinicalDateTime {
    ClinicalDateTime::parse_iso(text).unwrap()
}

fn decimal(text: &str) -> Decimal {
    Decimal::new(text).unwrap()
}

fn ucum(value: &str, unit: &str) -> Quantity {
    Quantity {
        system: Some(UCUM.into()),
        code: Some(unit.into()),
        ..Quantity::new(decimal(value), Some(unit.into()))
    }
}

fn loinc(code: &str, display: &str) -> CodeableConcept {
    CodeableConcept::from_coding(Coding::new(code).with_system(LOINC).with_display(display))
}

fn device() -> Device {
    Device {
        manufacturer: Some("Example Diagnostics".into()),
        model: Some("ChemAnalyzer".into()),
        serial_number: Some("SN1234".into()),
        software_version: Some("2.1".into()),
        name: Some("CHEM-1".into()),
        ..Device::default()
    }
}

fn patient() -> Patient {
    Patient {
        identifiers: vec![Identifier {
            system: Some("urn:oid:2.16.840.1.113883.19.5".into()),
            value: "PID001".into(),
            kind: Some("MR".into()),
            assigner: Some("HOSP".into()),
        }],
        name: Some(HumanName {
            family: Some("Doe".into()),
            given: vec!["Jane".into(), "M".into()],
            prefix: Some("Dr".into()),
            suffix: None,
        }),
        birth_date: Some(ClinicalDateTime::date(1980, 1, 1).unwrap()),
        sex: Some(AdministrativeSex::Female),
        species: None,
        breed: None,
    }
}

fn specimen() -> Specimen {
    Specimen {
        identifiers: vec![Identifier {
            system: Some("urn:oxim:test:specimen".into()),
            ..Identifier::new("S123")
        }],
        kind: Some(CodeableConcept::from_coding(
            Coding::new("SER")
                .with_system("http://terminology.hl7.org/CodeSystem/v2-0487")
                .with_display("Serum"),
        )),
        collected_at: Some(at("2026-09-29T11:55:00+03:00")),
        received_at: Some(at("2026-09-29T12:10:30+03:00")),
        container: Some("01^02".into()),
        notes: vec!["Hemolysis low".into()],
    }
}

fn observation(code: CodeableConcept, value: ObservationValue) -> Observation {
    Observation {
        code,
        value: Some(value),
        status: ObservationStatus::Final,
        effective_at: Some(at("2026-09-29T14:25:00+03:00")),
        issued_at: Some(at("2026-09-29T14:26:00Z")),
        device_id: Some("CHEM-1".into()),
        operator: Some("tech1".into()),
        specimen_id: Some("S123".into()),
        ..Observation::default()
    }
}

fn results() -> ClinicalContent {
    let mut glucose = observation(
        loinc("2345-7", "Glucose"),
        ObservationValue::Quantity(ucum("5.40", "mmol/L")),
    );
    glucose.reference_range = Some(ReferenceRange {
        low: Some(decimal("3.9")),
        high: Some(decimal("6.10")),
        text: Some("3.9-6.10".into()),
    });
    glucose.interpretation = vec![Coding::new("N").with_display("Normal")];
    glucose.method = Some(CodeableConcept::from_text("Hexokinase"));
    glucose.notes = vec!["Checked twice".into()];

    let mut hemoglobin = observation(
        loinc("718-7", "Hemoglobin"),
        ObservationValue::Quantity(Quantity {
            comparator: Some(Comparator::LessThan),
            ..ucum("0.50", "g/dL")
        }),
    );
    hemoglobin.interpretation = vec![Coding::new("L")];
    hemoglobin.status = ObservationStatus::Corrected;

    let observations = vec![
        glucose,
        hemoglobin,
        observation(
            loinc("8251-1", "Comment"),
            ObservationValue::Text("Slightly lipemic".into()),
        ),
        observation(
            CodeableConcept::from_text("Blood group"),
            ObservationValue::Coded(CodeableConcept::from_coding(
                Coding::new("A").with_system("urn:oxim:test:abo"),
            )),
        ),
        observation(
            CodeableConcept::from_text("Range result"),
            ObservationValue::Range {
                low: Some(ucum("1.0", "mg/L")),
                high: None,
            },
        ),
        observation(
            CodeableConcept::from_text("Titer"),
            ObservationValue::Ratio {
                numerator: Quantity::new(decimal("1"), None),
                denominator: Quantity::new(decimal("64"), None),
            },
        ),
        observation(
            CodeableConcept::from_text("Screen positive"),
            ObservationValue::Boolean(true),
        ),
        observation(
            CodeableConcept::from_text("Analyzed at"),
            ObservationValue::DateTime(at("2026-09-29T14:00:00-05:00")),
        ),
        observation(
            CodeableConcept::from_text("Histogram"),
            ObservationValue::Attachment {
                content_type: Some("image/png".into()),
                data: "iVBORw0KGgo=".into(),
                title: Some("histogram.png".into()),
            },
        ),
    ];
    ClinicalContent::Results {
        device: Some(device()),
        groups: vec![
            ResultGroup {
                patient: Some(patient()),
                specimen: Some(specimen()),
                order: Some(Order {
                    placer_id: Some("ORD7".into()),
                    filler_id: Some("F7".into()),
                    tests: vec![loinc("2345-7", "Glucose"), loinc("718-7", "Hemoglobin")],
                    priority: Some(Priority::Stat),
                    requested_at: Some(at("2026-09-29T11:00:00+03:00")),
                    specimen_ids: vec!["S123".into()],
                    control: None,
                    notes: vec!["Fasting".into()],
                }),
                observations,
            },
            // A second group without patient, specimen or order.
            ResultGroup {
                observations: vec![Observation {
                    code: CodeableConcept::from_text("Ambient temperature"),
                    value: Some(ObservationValue::Quantity(ucum("21.50", "Cel"))),
                    status: ObservationStatus::Preliminary,
                    ..Observation::default()
                }],
                ..ResultGroup::default()
            },
        ],
    }
}

fn encode(content: &ClinicalContent) -> Resource {
    Resource::from(encode_bundle(content, &FhirEncoding::default(), id(), received()).unwrap())
}

#[test]
fn normalized_results_survive_fhir() {
    let content = results();
    let bundle = encode(&content);
    assert_eq!(validate(&bundle), []);
    // Through JSON text, as a server would see it.
    let parsed = Resource::from_json(&bundle.to_json().unwrap()).unwrap();
    assert_eq!(parsed, bundle);
    assert_eq!(normalize(&parsed).unwrap(), content);
}

#[test]
fn fhir_results_survive_the_normalized_model() {
    let bundle = encode(&results());
    let again = encode(&normalize(&bundle).unwrap());
    assert_eq!(again, bundle);
}

#[test]
fn interpretation_codes_use_the_v3_system() {
    let Resource::Bundle(bundle) = encode(&results()) else {
        panic!("expected a bundle");
    };
    let json: Value = serde_json::to_value(&bundle).unwrap();
    let observation = &json["entry"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["resource"]["resourceType"] == "Observation")
        .unwrap()["resource"];
    assert_eq!(
        observation["interpretation"][0]["coding"][0]["system"],
        V3_INTERPRETATION
    );
    // Decimals keep their scale in the JSON text.
    let text = String::from_utf8(Resource::from(*bundle).to_json().unwrap()).unwrap();
    assert!(text.contains(r#""value":5.40"#), "{text}");
    assert!(text.contains(r#""value":6.10"#), "{text}");
    assert!(text.contains(r#""value":0.50,"comparator":"<""#), "{text}");
}

#[test]
fn quality_control_survives_fhir() {
    let content = ClinicalContent::QualityControl {
        device: Some(device()),
        results: vec![QcResult {
            material: Some("Control N".into()),
            lot: Some("L123".into()),
            level: Some("1".into()),
            expires_at: Some(ClinicalDateTime::date(2027, 1, 31).unwrap()),
            observation: observation(
                loinc("2345-7", "Glucose"),
                ObservationValue::Quantity(ucum("5.10", "mmol/L")),
            ),
        }],
    };
    let bundle = encode(&content);
    assert_eq!(validate(&bundle), []);
    assert_eq!(normalize(&bundle).unwrap(), content);
    assert_eq!(encode(&normalize(&bundle).unwrap()), bundle);
}

#[test]
fn orders_survive_fhir() {
    let order = |placer: &str, control| Order {
        placer_id: Some(placer.into()),
        tests: vec![loinc("2345-7", "Glucose"), loinc("718-7", "Hemoglobin")],
        priority: Some(Priority::Routine),
        requested_at: Some(at("2026-09-29T11:00:00+03:00")),
        specimen_ids: vec!["S123".into()],
        control: Some(control),
        ..Order::default()
    };
    let content = ClinicalContent::Orders {
        groups: vec![
            OrderGroup {
                patient: Some(patient()),
                specimen: Some(specimen()),
                order: order("ORD1", OrderControl::New),
            },
            OrderGroup {
                patient: Some(patient()),
                specimen: Some(specimen()),
                order: order("ORD2", OrderControl::Cancel),
            },
        ],
    };
    let bundle = encode(&content);
    assert_eq!(validate(&bundle), []);
    let Resource::Bundle(typed) = &bundle else {
        panic!("expected a bundle");
    };
    // The shared patient is added once; the specimen once per group.
    let patients = typed
        .entry
        .iter()
        .filter(|e| e.resource.as_ref().unwrap().resource_type() == "Patient")
        .count();
    assert_eq!(patients, 1);
    assert_eq!(normalize(&bundle).unwrap(), content);
}

#[test]
fn server_bundles_normalize_with_exact_decimals() {
    // A searchset as a server returns it: server ids, relative references,
    // elements OXIM does not model.
    let bundle = Resource::from_json(
        br#"{
          "resourceType": "Bundle",
          "type": "searchset",
          "total": 2,
          "entry": [
            {"fullUrl": "https://fhir.example.test/r4/Patient/p1",
             "resource": {"resourceType": "Patient", "id": "p1",
               "identifier": [{"system": "urn:example:mrn", "value": "MRN-1"}],
               "gender": "other", "birthDate": "1990"}},
            {"fullUrl": "https://fhir.example.test/r4/DiagnosticReport/r1",
             "resource": {"resourceType": "DiagnosticReport", "id": "r1", "status": "final",
               "code": {"text": "Panel"}, "subject": {"reference": "Patient/p1"},
               "result": [{"reference": "Observation/o1"}]}},
            {"fullUrl": "https://fhir.example.test/r4/Observation/o1",
             "search": {"mode": "match", "score": 0.80},
             "resource": {"resourceType": "Observation", "id": "o1", "status": "amended",
               "meta": {"versionId": "3"},
               "code": {"coding": [{"system": "http://loinc.org", "code": "2345-7"}]},
               "subject": {"reference": "https://fhir.example.test/r4/Patient/p1"},
               "valueQuantity": {"value": 0.050, "unit": "mg/dL"},
               "interpretation": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/v3-ObservationInterpretation", "code": "HH"}]}],
               "referenceRange": [{"low": {"value": 0.010}, "high": {"value": 1.20e1}}],
               "derivedFrom": [{"reference": "Observation/o0"}]}}
          ]
        }"#,
    )
    .unwrap();
    let ClinicalContent::Results { groups, .. } = normalize(&bundle).unwrap() else {
        panic!("expected results");
    };
    assert_eq!(groups.len(), 1);
    let group = &groups[0];
    let patient = group.patient.as_ref().unwrap();
    assert_eq!(patient.identifiers[0].value, "MRN-1");
    assert_eq!(patient.sex, Some(AdministrativeSex::Other));
    assert_eq!(patient.birth_date.unwrap().to_iso(), "1990");
    let observation = &group.observations[0];
    assert_eq!(observation.status, ObservationStatus::Amended);
    assert_eq!(observation.interpretation, [Coding::new("HH")]);
    let Some(ObservationValue::Quantity(quantity)) = &observation.value else {
        panic!("expected a quantity");
    };
    assert_eq!(quantity.value.as_str(), "0.050");
    let range = observation.reference_range.as_ref().unwrap();
    assert_eq!(range.low.as_ref().unwrap().as_str(), "0.010");
    // serde_json writes exponents with a sign; the digits are kept.
    assert_eq!(range.high.as_ref().unwrap().as_str(), "1.20e+1");

    // Mapped back, the decimals keep their scale.
    let text = String::from_utf8(
        encode(&ClinicalContent::Results {
            device: None,
            groups: groups.clone(),
        })
        .to_json()
        .unwrap(),
    )
    .unwrap();
    assert!(text.contains(r#""value":0.050"#), "{text}");
    assert!(text.contains(r#""value":0.010"#), "{text}");
    assert!(text.contains(r#""value":1.20e+1"#), "{text}");
}

#[test]
fn unknown_elements_and_extensions_survive_the_typed_model() {
    let text = br#"{
      "resourceType": "Observation",
      "id": "o1",
      "meta": {"versionId": "2", "profile": ["http://example.test/StructureDefinition/lab"], "security": [{"code": "R"}]},
      "extension": [{"url": "http://example.test/ext/run", "valueInteger": 7}],
      "modifierExtension": [{"url": "http://example.test/ext/flag", "valueBoolean": false}],
      "identifier": [{"system": "urn:example", "value": "1", "period": {"start": "2026-01-01"}}],
      "status": "final",
      "_status": {"extension": [{"url": "http://example.test/ext/why", "valueString": "verified"}]},
      "code": {"coding": [{"system": "http://loinc.org", "code": "2345-7", "userSelected": true,
               "extension": [{"url": "http://example.test/ext/c", "valueDecimal": 1.10}]}]},
      "valueQuantity": {"value": 5.40, "unit": "mmol/L", "_value": {"extension": [{"url": "http://example.test/ext/v", "valueString": "x"}]}},
      "component": [{"code": {"text": "part"}, "valueQuantity": {"value": 1.000}}],
      "hasMember": [{"reference": "Observation/o2"}],
      "futureElement": {"nested": [1, 2.50, "three", null, true]}
    }"#;
    let resource = Resource::from_json(text).unwrap();
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
    let written = resource.to_json().unwrap();
    let original: Value = serde_json::from_slice(text).unwrap();
    let back: Value = serde_json::from_slice(&written).unwrap();
    assert_eq!(back, original);
    let written = String::from_utf8(written).unwrap();
    for exact in [r#""value":1.000"#, r#""valueDecimal":1.10"#, "2.50"] {
        assert!(written.contains(exact), "{exact} in {written}");
    }
}
