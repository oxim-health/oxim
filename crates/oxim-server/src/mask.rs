//! Masking of patient-identifying values for callers without unmasked
//! access.
//!
//! Masking keeps the structure of the content: HL7 v2 and ASTM messages
//! still parse, JSON stays JSON and XML stays well formed. Masked values are
//! replaced by `***`. Content of a data type that cannot be masked reliably
//! is withheld entirely.

use std::sync::LazyLock;

use oxim_model::DataType;
use regex::bytes::Regex;
use serde_json::Value;

/// The replacement for masked values.
pub const MASK: &str = "***";

/// The result of masking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Masked {
    /// The content with identifying values replaced.
    Content(Vec<u8>),
    /// The content cannot be masked reliably and must not be shown.
    Withheld,
}

/// HL7 v2 fields that identify a patient or a related person, by segment.
const HL7_FIELDS: &[(&str, &[usize])] = &[
    // Patient identification: identifiers, names, birth date, alias,
    // address, phones, account, SSN, driver's license, mother's identifier,
    // birth place.
    (
        "PID",
        &[2, 3, 4, 5, 6, 7, 9, 11, 12, 13, 14, 18, 19, 20, 21, 23],
    ),
    // Next of kin.
    ("NK1", &[2, 4, 5, 6, 30, 31, 32]),
    // Merged patient identifiers.
    ("MRG", &[1, 2, 3, 4, 5, 6, 7]),
    // Guarantor.
    ("GT1", &[2, 3, 4, 5, 6, 7, 8, 12, 19, 20]),
    // Insured person.
    ("IN1", &[16, 18, 19, 36, 49]),
    ("IN2", &[1, 2, 6, 8]),
    // Patient visit: visit number, alternate visit ID.
    ("PV1", &[19, 50]),
];

/// ASTM patient record fields: practice, laboratory and third patient
/// identifiers, name, mother's maiden name, birth date, address and phone.
const ASTM_PATIENT_FIELDS: &[usize] = &[3, 4, 5, 6, 7, 8, 11, 13];

fn mask_hl7(data: &[u8]) -> Masked {
    let Ok(mut message) = oxim_hl7::Message::parse(data) else {
        return Masked::Withheld;
    };
    for (segment, fields) in HL7_FIELDS {
        let count = message.segments_named(segment).count();
        for occurrence in 1..=count {
            for field in *fields {
                let path = format!("{segment}[{occurrence}]-{field}");
                let present = message.get(&path).is_some_and(|value| !value.is_empty());
                if present && message.set_raw(&path, MASK.as_bytes()).is_err() {
                    return Masked::Withheld;
                }
            }
        }
    }
    Masked::Content(message.to_bytes())
}

fn mask_astm(data: &[u8]) -> Masked {
    let Ok(mut message) = oxim_astm::Message::parse(data) else {
        return Masked::Withheld;
    };
    let count = message.records_of_type("P").count();
    for occurrence in 1..=count {
        for field in ASTM_PATIENT_FIELDS {
            let path = format!("P[{occurrence}]-{field}");
            let present = message.get(&path).is_some_and(|value| !value.is_empty());
            if present && message.set_raw(&path, MASK.as_bytes()).is_err() {
                return Masked::Withheld;
            }
        }
    }
    Masked::Content(message.to_bytes())
}

/// Masks `patient` objects of the normalized model (and FHIR-like JSON):
/// identifiers, names, birth dates, addresses and contact details.
fn mask_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "patient" || key == "subject" {
                    mask_patient(child);
                } else {
                    mask_json_value(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(mask_json_value),
        _ => {}
    }
}

fn mask_patient(value: &mut Value) {
    const KEYS: &[&str] = &[
        "identifiers",
        "identifier",
        "name",
        "birth_date",
        "birthDate",
        "address",
        "telecom",
        "reference",
        "display",
    ];
    match value {
        Value::Object(map) => {
            for key in KEYS {
                if let Some(field) = map.get_mut(*key) {
                    replace_leaves(field);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(mask_patient),
        other => replace_leaves(other),
    }
}

fn replace_leaves(value: &mut Value) {
    match value {
        Value::Object(map) => map.values_mut().for_each(replace_leaves),
        Value::Array(items) => items.iter_mut().for_each(replace_leaves),
        Value::Null => {}
        leaf => *leaf = Value::String(MASK.into()),
    }
}

fn mask_json(data: &[u8]) -> Masked {
    let Ok(mut value) = serde_json::from_slice::<Value>(data) else {
        return Masked::Withheld;
    };
    mask_json_value(&mut value);
    match serde_json::to_vec(&value) {
        Ok(bytes) => Masked::Content(bytes),
        Err(_) => Masked::Withheld,
    }
}

static POCT_PATIENT: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"(<PT\.[A-Za-z0-9_.]+\b[^>]*?\sV=")[^"]*(")"#).ok());

fn mask_poct(data: &[u8]) -> Masked {
    let Some(pattern) = POCT_PATIENT.as_ref() else {
        return Masked::Withheld;
    };
    Masked::Content(pattern.replace_all(data, &b"${1}***${2}"[..]).into_owned())
}

/// Masks `data` of `data_type`. Unknown or unmaskable data types are
/// withheld.
pub fn mask(data_type: Option<DataType>, data: &[u8]) -> Masked {
    match data_type {
        Some(DataType::Hl7V2) => mask_hl7(data),
        Some(DataType::Astm) => mask_astm(data),
        Some(DataType::Json | DataType::Fhir) => mask_json(data),
        Some(DataType::Poct1a) => mask_poct(data),
        _ => Masked::Withheld,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(masked: Masked) -> String {
        match masked {
            Masked::Content(bytes) => String::from_utf8(bytes).unwrap(),
            Masked::Withheld => panic!("withheld"),
        }
    }

    #[test]
    fn masks_hl7_patient_fields_and_keeps_structure() {
        let input = b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929||ORU^R01|1|P|2.5.1\r\
PID|1||12345^^^HOSP^MR~99^^^X||Doe^Jane^Q||19800101|F|||1 Main St^^Town||555-1234|||||ACC9|123-45-6789\r\
NK1|1|Doe^John|SPO\r\
OBX|1|NM|GLU^Glucose||5.4|mmol/L\r";
        let masked = text(mask(Some(DataType::Hl7V2), input));
        let message = oxim_hl7::Message::parse(masked.as_bytes()).unwrap();
        for path in [
            "PID-3", "PID-5", "PID-7", "PID-11", "PID-13", "PID-18", "PID-19", "NK1-2",
        ] {
            assert_eq!(message.get(path).unwrap(), MASK, "{path}");
        }
        assert_eq!(message.get("PID-8").unwrap(), "F");
        assert_eq!(message.get("OBX-5").unwrap(), "5.4");
        assert!(!masked.contains("Doe"));
        assert!(!masked.contains("12345"));
        assert!(!masked.contains("19800101"));
    }

    #[test]
    fn masks_astm_patient_records() {
        let input = b"H|\\^&|||ANALYZER\rP|1|PRAC1|LAB22|X3|Doe^Jane||19800101|F||1 Main St||555\rO|1|SMP1||^^^GLU\rR|1|^^^GLU|5.4|mmol/L\rL|1\r";
        let masked = text(mask(Some(DataType::Astm), input));
        let message = oxim_astm::Message::parse(masked.as_bytes()).unwrap();
        for path in ["P-3", "P-4", "P-5", "P-6", "P-8", "P-11", "P-13"] {
            assert_eq!(message.get(path).unwrap(), MASK, "{path}");
        }
        assert_eq!(message.get("P-9").unwrap(), "F");
        assert_eq!(message.get("R-4").unwrap(), "5.4");
        assert!(!masked.contains("Doe"));
    }

    #[test]
    fn masks_normalized_patients() {
        let input = br#"{"kind":"results","groups":[{"patient":{"identifiers":[{"value":"12345","kind":"MR"}],"name":{"family":"Doe","given":["Jane"]},"birth_date":"1980-01-01","sex":"female"},"observations":[{"code":{"codings":[{"code":"GLU"}]},"value":{"type":"quantity","value":{"value":"5.4"}}}]}]}"#;
        let masked = text(mask(Some(DataType::Json), input));
        assert!(!masked.contains("12345"));
        assert!(!masked.contains("Doe"));
        assert!(!masked.contains("1980"));
        assert!(masked.contains("female"));
        assert!(masked.contains("5.4"));
        let value: Value = serde_json::from_str(&masked).unwrap();
        assert_eq!(value["groups"][0]["patient"]["name"]["family"], MASK);
    }

    #[test]
    fn masks_poct_patient_elements() {
        let input = br#"<OBS.R01><SVC><PT><PT.patient_id V="12345"/><PT.name V="Doe, Jane"/><OBS><OBS.value V="7.40"/></OBS></PT></SVC></OBS.R01>"#;
        let masked = text(mask(Some(DataType::Poct1a), input));
        assert!(masked.contains(r#"<PT.patient_id V="***"/>"#));
        assert!(masked.contains(r#"<PT.name V="***"/>"#));
        assert!(masked.contains(r#"<OBS.value V="7.40"/>"#));
    }

    #[test]
    fn withholds_unmaskable_content() {
        assert_eq!(mask(Some(DataType::Raw), b"anything"), Masked::Withheld);
        assert_eq!(mask(None, b"anything"), Masked::Withheld);
        assert_eq!(mask(Some(DataType::Hl7V2), b"not hl7"), Masked::Withheld);
        assert_eq!(mask(Some(DataType::Json), b"{not json"), Masked::Withheld);
    }
}
