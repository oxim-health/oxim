//! Parsing never panics, and whatever normalized content is encoded
//! produces JSON that parses back.

#![allow(clippy::unwrap_used, clippy::panic)]

use oxim_fhir::{
    BundleType, FhirEncoding, Resource, encode_bundle, normalize, summarize, validate,
};
use oxim_model::{
    AdministrativeSex, ClinicalContent, ClinicalDateTime, CodeableConcept, Coding, Comparator,
    Decimal, Device, HumanName, Identifier, MessageId, Observation, ObservationStatus,
    ObservationValue, Order, OrderControl, OrderGroup, Patient, Priority, QcResult, Quantity,
    ReferenceRange, ResultGroup, Specimen, Timestamp,
};
use proptest::prelude::*;
use serde_json::{Map, Value};

// ---------------------------------------------------------------------------
// Arbitrary JSON shaped like FHIR.

const TYPES: &[&str] = &[
    "Bundle",
    "Observation",
    "DiagnosticReport",
    "ServiceRequest",
    "Patient",
    "Specimen",
    "Device",
    "OperationOutcome",
    "Basic",
];

const KEYS: &[&str] = &[
    "resourceType",
    "id",
    "status",
    "intent",
    "type",
    "code",
    "coding",
    "system",
    "display",
    "text",
    "value",
    "unit",
    "comparator",
    "valueQuantity",
    "valueString",
    "valueBoolean",
    "valueCodeableConcept",
    "valueRange",
    "valueRatio",
    "valueDateTime",
    "low",
    "high",
    "numerator",
    "denominator",
    "referenceRange",
    "interpretation",
    "category",
    "subject",
    "specimen",
    "basedOn",
    "result",
    "reference",
    "identifier",
    "entry",
    "fullUrl",
    "resource",
    "request",
    "response",
    "method",
    "url",
    "outcome",
    "issue",
    "severity",
    "presentedForm",
    "data",
    "note",
    "effectiveDateTime",
    "issued",
    "gender",
    "birthDate",
    "name",
    "given",
    "device",
    "deviceName",
    "version",
    "extension",
    "collection",
    "collectedDateTime",
];

fn json_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        "-?(0|[1-9][0-9]{0,3})(\\.[0-9]{1,3})?(e[+-]?[0-9])?"
            .prop_map(|n| serde_json::from_str::<Value>(&n).unwrap()),
        prop::sample::select(vec![
            "final",
            "transaction",
            "transaction-response",
            "collection",
            "201 Created",
            "422",
            "POST",
            "order",
            "active",
            "Patient/p1",
            "urn:uuid:a",
            "2026-09-29T14:30:00+03:00",
            "1990",
            "male",
            "error",
            "http://terminology.hl7.org/CodeSystem/v3-ObservationInterpretation",
            "urn:oxim:observation-category",
            "quality-control",
            "Quality control material: lot=1; level=x",
            "The result is an attachment in the DiagnosticReport's presentedForm.",
        ])
        .prop_map(|s| Value::String(s.to_owned())),
        "[ -~]{0,6}".prop_map(Value::String),
    ]
}

fn json_tree() -> impl Strategy<Value = Value> {
    json_leaf().prop_recursive(5, 64, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::vec((prop::sample::select(KEYS), inner), 0..6)
                .prop_map(|pairs| Value::Object(object(pairs))),
        ]
    })
}

/// An object whose `resourceType`, when present, names a resource type.
fn object(pairs: Vec<(&str, Value)>) -> Map<String, Value> {
    pairs
        .into_iter()
        .map(|(key, value)| {
            let value = if key == "resourceType" {
                let index = value.to_string().len() % TYPES.len();
                Value::String(TYPES[index].to_owned())
            } else {
                value
            };
            (key.to_owned(), value)
        })
        .collect()
}

fn check_parsed(resource: &Resource) {
    let _ = validate(resource);
    let _ = summarize(resource);
    if let Ok(content) = normalize(resource) {
        let _ = encode_bundle(
            &content,
            &FhirEncoding::default(),
            MessageId::from_parts(1, 1),
            Timestamp::from_unix_nanos(0),
        );
    }
    let written = resource.to_json().unwrap();
    assert_eq!(&Resource::from_json(&written).unwrap(), resource);
}

// ---------------------------------------------------------------------------
// Arbitrary normalized content.

fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[A-Za-z0-9]{1,6}",
        1 => "[ -~]{0,6}",
        1 => "\\PC{0,4}",
    ]
}

fn opt_text() -> impl Strategy<Value = Option<String>> {
    prop::option::of(text())
}

fn decimal() -> BoxedStrategy<Decimal> {
    "[+-]?([0-9]{1,4}(\\.[0-9]{0,3})?|\\.[0-9]{1,3})([eE][+-]?[0-9]{1,2})?"
        .prop_filter_map("a decimal", |t| Decimal::new(t).ok())
        .boxed()
}

fn datetime() -> BoxedStrategy<ClinicalDateTime> {
    (
        1900u16..2100,
        1u8..=12,
        1u8..=28,
        0u8..24,
        0u8..60,
        0u8..60,
        prop::option::of(-720i16..=840),
        prop::sample::select(vec![4usize, 6, 8, 10, 12, 14]),
    )
        .prop_map(|(year, month, day, hour, minute, second, offset, digits)| {
            let full = format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}");
            let mut text = full[..digits].to_owned();
            if let Some(offset) = offset.filter(|_| digits > 8) {
                let sign = if offset < 0 { '-' } else { '+' };
                let offset = offset.unsigned_abs();
                text.push_str(&format!("{sign}{:02}{:02}", offset / 60, offset % 60));
            }
            ClinicalDateTime::parse_hl7(&text).unwrap()
        })
        .boxed()
}

fn coding() -> BoxedStrategy<Coding> {
    (opt_text(), text(), opt_text())
        .prop_map(|(system, code, display)| Coding {
            system,
            code,
            display,
        })
        .boxed()
}

fn concept() -> BoxedStrategy<CodeableConcept> {
    (prop::collection::vec(coding(), 0..3), opt_text())
        .prop_map(|(codings, text)| CodeableConcept { codings, text })
        .boxed()
}

fn quantity() -> BoxedStrategy<Quantity> {
    (
        decimal(),
        prop::option::of(prop::sample::select(vec![
            Comparator::LessThan,
            Comparator::LessOrEqual,
            Comparator::GreaterOrEqual,
            Comparator::GreaterThan,
        ])),
        opt_text(),
        opt_text(),
        opt_text(),
    )
        .prop_map(|(value, comparator, unit, system, code)| Quantity {
            value,
            comparator,
            unit,
            system,
            code,
        })
        .boxed()
}

fn value() -> BoxedStrategy<ObservationValue> {
    prop_oneof![
        quantity().prop_map(ObservationValue::Quantity),
        text().prop_map(ObservationValue::Text),
        concept().prop_map(ObservationValue::Coded),
        (prop::option::of(quantity()), prop::option::of(quantity()))
            .prop_map(|(low, high)| ObservationValue::Range { low, high }),
        (quantity(), quantity()).prop_map(|(numerator, denominator)| ObservationValue::Ratio {
            numerator,
            denominator
        }),
        any::<bool>().prop_map(ObservationValue::Boolean),
        datetime().prop_map(ObservationValue::DateTime),
        (opt_text(), text(), opt_text()).prop_map(|(content_type, data, title)| {
            ObservationValue::Attachment {
                content_type,
                data,
                title,
            }
        }),
    ]
    .boxed()
}

fn status() -> impl Strategy<Value = ObservationStatus> {
    prop::sample::select(vec![
        ObservationStatus::Registered,
        ObservationStatus::Preliminary,
        ObservationStatus::Final,
        ObservationStatus::Amended,
        ObservationStatus::Corrected,
        ObservationStatus::Cancelled,
        ObservationStatus::EnteredInError,
        ObservationStatus::Unknown,
    ])
}

fn observation() -> BoxedStrategy<Observation> {
    (
        (
            concept(),
            prop::option::of(value()),
            prop::option::of((
                prop::option::of(decimal()),
                prop::option::of(decimal()),
                opt_text(),
            )),
            prop::collection::vec(coding(), 0..2),
            status(),
        ),
        (
            prop::option::of(datetime()),
            prop::option::of(datetime()),
            prop::option::of(concept()),
            opt_text(),
            opt_text(),
            opt_text(),
            prop::collection::vec(text(), 0..2),
        ),
    )
        .prop_map(
            |(
                (code, value, range, interpretation, status),
                (effective_at, issued_at, method, device_id, operator, specimen_id, notes),
            )| Observation {
                sequence: None,
                code,
                value,
                reference_range: range.map(|(low, high, text)| ReferenceRange { low, high, text }),
                interpretation,
                status,
                effective_at,
                issued_at,
                method,
                device_id,
                operator,
                specimen_id,
                notes,
            },
        )
        .boxed()
}

fn identifier() -> BoxedStrategy<Identifier> {
    (opt_text(), text(), opt_text(), opt_text())
        .prop_map(|(system, value, kind, assigner)| Identifier {
            system,
            value,
            kind,
            assigner,
        })
        .boxed()
}

fn patient() -> BoxedStrategy<Patient> {
    (
        prop::collection::vec(identifier(), 0..2),
        prop::option::of((opt_text(), prop::collection::vec(text(), 0..2), opt_text())),
        prop::option::of(datetime()),
        prop::option::of(prop::sample::select(vec![
            AdministrativeSex::Male,
            AdministrativeSex::Female,
            AdministrativeSex::Other,
            AdministrativeSex::Unknown,
        ])),
        prop::option::of(concept()),
        prop::option::of(concept()),
    )
        .prop_map(
            |(identifiers, name, birth_date, sex, species, breed)| Patient {
                identifiers,
                name: name.map(|(family, given, prefix)| HumanName {
                    family,
                    given,
                    prefix,
                    suffix: None,
                }),
                birth_date,
                sex,
                species,
                breed,
            },
        )
        .boxed()
}

fn specimen() -> BoxedStrategy<Specimen> {
    (
        prop::collection::vec(identifier(), 0..2),
        prop::option::of(concept()),
        prop::option::of(datetime()),
        prop::option::of(datetime()),
        opt_text(),
        prop::collection::vec(text(), 0..2),
    )
        .prop_map(
            |(identifiers, kind, collected_at, received_at, container, notes)| Specimen {
                identifiers,
                kind,
                collected_at,
                received_at,
                container,
                notes,
            },
        )
        .boxed()
}

fn order() -> BoxedStrategy<Order> {
    (
        opt_text(),
        opt_text(),
        prop::collection::vec(concept(), 0..3),
        prop::option::of(prop::sample::select(vec![
            Priority::Routine,
            Priority::Urgent,
            Priority::Asap,
            Priority::Stat,
        ])),
        prop::option::of(datetime()),
        prop::collection::vec(text(), 0..2),
        prop::option::of(prop::sample::select(vec![
            OrderControl::New,
            OrderControl::Add,
            OrderControl::Cancel,
            OrderControl::Replace,
        ])),
        prop::collection::vec(text(), 0..2),
    )
        .prop_map(
            |(
                placer_id,
                filler_id,
                tests,
                priority,
                requested_at,
                specimen_ids,
                control,
                notes,
            )| {
                Order {
                    placer_id,
                    filler_id,
                    tests,
                    priority,
                    requested_at,
                    specimen_ids,
                    control,
                    notes,
                }
            },
        )
        .boxed()
}

fn device() -> BoxedStrategy<Device> {
    (
        prop::collection::vec(identifier(), 0..2),
        opt_text(),
        opt_text(),
        opt_text(),
        opt_text(),
        opt_text(),
    )
        .prop_map(
            |(identifiers, manufacturer, model, serial_number, software_version, name)| Device {
                identifiers,
                manufacturer,
                model,
                serial_number,
                software_version,
                name,
            },
        )
        .boxed()
}

fn content() -> BoxedStrategy<ClinicalContent> {
    prop_oneof![
        (
            prop::option::of(device()),
            prop::collection::vec(
                (
                    prop::option::of(patient()),
                    prop::option::of(specimen()),
                    prop::option::of(order()),
                    prop::collection::vec(observation(), 0..4),
                )
                    .prop_map(|(patient, specimen, order, observations)| {
                        ResultGroup {
                            patient,
                            specimen,
                            order,
                            observations,
                        }
                    }),
                0..3,
            ),
        )
            .prop_map(|(device, groups)| ClinicalContent::Results { device, groups }),
        prop::collection::vec(
            (
                prop::option::of(patient()),
                prop::option::of(specimen()),
                order()
            )
                .prop_map(|(patient, specimen, order)| OrderGroup {
                    patient,
                    specimen,
                    order,
                }),
            0..3,
        )
        .prop_map(|groups| ClinicalContent::Orders { groups }),
        (
            prop::option::of(device()),
            prop::collection::vec(
                (
                    opt_text(),
                    opt_text(),
                    opt_text(),
                    prop::option::of(datetime()),
                    observation(),
                )
                    .prop_map(|(material, lot, level, expires_at, observation)| {
                        QcResult {
                            material,
                            lot,
                            level,
                            expires_at,
                            observation,
                        }
                    }),
                0..3,
            ),
        )
            .prop_map(|(device, results)| ClinicalContent::QualityControl { device, results }),
    ]
    .boxed()
}

fn settings() -> impl Strategy<Value = FhirEncoding> {
    (any::<bool>(), opt_text(), opt_text(), text(), -720i16..=840).prop_map(
        |(collection, patient, specimen, category, offset)| FhirEncoding {
            bundle_type: if collection {
                BundleType::Collection
            } else {
                BundleType::Transaction
            },
            patient_identifier_system: patient,
            specimen_identifier_system: specimen,
            observation_category: category,
            utc_offset_minutes: offset,
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn parsing_arbitrary_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
        if let Ok(resource) = Resource::from_json(&bytes) {
            check_parsed(&resource);
        }
    }

    #[test]
    fn parsing_arbitrary_fhir_like_json_never_panics(
        kind in prop::sample::select(TYPES),
        fields in prop::collection::vec((prop::sample::select(KEYS), json_tree()), 0..8),
    ) {
        let mut root = object(fields);
        root.insert("resourceType".into(), Value::String(kind.to_owned()));
        let bytes = serde_json::to_vec(&Value::Object(root)).unwrap();
        if let Ok(resource) = Resource::from_json(&bytes) {
            check_parsed(&resource);
        }
    }

    #[test]
    fn encoded_content_parses_back(content in content(), settings in settings()) {
        let bundle = encode_bundle(
            &content,
            &settings,
            MessageId::from_parts(1_790_000_000_000, 42),
            "2026-09-29T11:30:00Z".parse().unwrap(),
        )
        .unwrap();
        let resource = Resource::from(bundle);
        let json = resource.to_json().unwrap();
        let parsed = Resource::from_json(&json).unwrap();
        prop_assert_eq!(&parsed, &resource);
        let pretty = resource.to_json_pretty().unwrap();
        prop_assert_eq!(&Resource::from_json(&pretty).unwrap(), &resource);
        let _ = validate(&parsed);
        // Whatever was encoded maps back without errors.
        if matches!(&content, ClinicalContent::Results { groups, .. } if groups.iter().any(|g| !g.observations.is_empty()))
            || matches!(&content, ClinicalContent::QualityControl { results, .. } if !results.is_empty())
        {
            normalize(&parsed).unwrap();
        }
    }

    #[test]
    fn hl7_messages_encode_to_valid_json(
        body in prop::collection::vec(
            prop_oneof![5 => prop::sample::select(b"|^~\\&\rPIDORCOBXNTESPM0123456789.<>-^ ".to_vec()), 1 => any::<u8>()],
            0..300,
        ),
    ) {
        let mut input = b"MSH|^~\\&|A||B||20260101||ORU^R01|1|P|2.5.1\r".to_vec();
        input.extend(body);
        if let Ok(message) = oxim_hl7::Message::parse(&input)
            && let Ok(content) = oxim_mapping::hl7::normalize(&message)
            && let Ok(bundle) = encode_bundle(
                &content,
                &FhirEncoding::default(),
                MessageId::from_parts(1, 1),
                Timestamp::from_unix_nanos(0),
            )
        {
            let resource = Resource::from(bundle);
            let parsed = Resource::from_json(&resource.to_json().unwrap()).unwrap();
            prop_assert_eq!(parsed, resource);
        }
    }
}

/// Nesting is bounded by serde_json's recursion limit: deeply nested
/// bundles are parsed or rejected, and never overflow the 2 MiB stack of
/// an engine worker thread, even in debug builds.
#[test]
fn deeply_nested_bundles_do_not_overflow_the_stack() {
    for (depth, parses) in [(40, true), (200, false)] {
        let mut json =
            String::from(r#"{"resourceType":"Observation","status":"final","code":{"text":"x"}}"#);
        for _ in 0..depth {
            json = format!(
                r#"{{"resourceType":"Bundle","type":"collection","entry":[{{"resource":{json}}}]}}"#
            );
        }
        let parsed = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || match Resource::from_json(json.as_bytes()) {
                Ok(resource) => {
                    check_parsed(&resource);
                    true
                }
                Err(_) => false,
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(parsed, parses, "depth {depth}");
    }
}
