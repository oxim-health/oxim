//! HL7 v2 rules.
//!
//! | Segment | Pseudonymized | Removed | Dates shifted |
//! |---|---|---|---|
//! | MSH, EVN | | | MSH-7, EVN-2, EVN-3, EVN-6 |
//! | PID | 2, 3, 4, 18, 21 (identifiers); 5 (name) | 6, 9, 11, 12, 13, 14, 19, 20, 23 | 7, 29 |
//! | PD1 | 4 (clinician) | | |
//! | NK1 | 2, 30 (names); 33 (identifiers) | 4, 5, 6, 31, 32, 37 | 8, 16 |
//! | PV1 | 7, 8, 9, 17, 52 (clinicians); 19, 50 (visit numbers) | | 44, 45 |
//! | PV2 | | | 8, 9 |
//! | IN1 | 16 (name); 36, 49 (identifiers) | 19 | 18 |
//! | GT1 | 2 (identifier); 3 (name) | 5, 6, 7, 12 | 8 |
//! | MRG | 1, 2, 3 (identifiers); 7 (name) | | |
//! | ORC | 2, 3 (order numbers); 10, 12 (clinicians) | | 9, 15 |
//! | OBR | 2, 3 (order numbers); 10, 16, 32 (clinicians) | | 6, 7, 8, 14, 22 |
//! | OBX | 16 (clinician) | | 14, 19 |
//! | SPM | 2, 3 (specimen identifiers) | | 17, 18 |
//! | SAC | 3 (container identifier) | | |
//! | TQ1 | | | 7, 8 |
//! | QPD, QRD | QPD-3, QRD-8 (query subject) | | QRD-1 |
//!
//! Order, specimen and container numbers are pseudonymized unless
//! [`Options::specimens`](crate::Options::specimens) is off. With
//! [`Options::free_text`](crate::Options::free_text), NTE-3 and OBX-5 of
//! text observations (`TX`, `FT`, `ST`) become `REDACTED`. Other segments,
//! including Z-segments, are kept: review them before sharing.

use std::collections::HashMap;

use crate::{AnonymizeError, Anonymizer, Kind, Report, Separators};

use Kind::{
    Clinician as C, Date as D, Identifier as I, Name as N, Remove as R, Specimen as S, Text as T,
};

const RULES: &[(&str, &[(usize, Kind)])] = &[
    ("MSH", &[(7, D)]),
    ("EVN", &[(2, D), (3, D), (6, D)]),
    (
        "PID",
        &[
            (2, I),
            (3, I),
            (4, I),
            (5, N),
            (6, R),
            (7, D),
            (9, R),
            (11, R),
            (12, R),
            (13, R),
            (14, R),
            (18, I),
            (19, R),
            (20, R),
            (21, I),
            (23, R),
            (29, D),
        ],
    ),
    ("PD1", &[(4, C)]),
    (
        "NK1",
        &[
            (2, N),
            (4, R),
            (5, R),
            (6, R),
            (8, D),
            (16, D),
            (30, N),
            (31, R),
            (32, R),
            (33, I),
            (37, R),
        ],
    ),
    (
        "PV1",
        &[
            (7, C),
            (8, C),
            (9, C),
            (17, C),
            (19, I),
            (44, D),
            (45, D),
            (50, I),
            (52, C),
        ],
    ),
    ("PV2", &[(8, D), (9, D)]),
    ("IN1", &[(16, N), (18, D), (19, R), (36, I), (49, I)]),
    (
        "GT1",
        &[(2, I), (3, N), (5, R), (6, R), (7, R), (8, D), (12, R)],
    ),
    ("MRG", &[(1, I), (2, I), (3, I), (7, N)]),
    ("ORC", &[(2, S), (3, S), (9, D), (10, C), (12, C), (15, D)]),
    (
        "OBR",
        &[
            (2, S),
            (3, S),
            (6, D),
            (7, D),
            (8, D),
            (10, C),
            (14, D),
            (16, C),
            (22, D),
            (32, C),
        ],
    ),
    ("OBX", &[(5, T), (14, D), (16, C), (19, D)]),
    ("NTE", &[(3, T)]),
    ("SPM", &[(2, S), (3, S), (17, D), (18, D)]),
    ("SAC", &[(3, S)]),
    ("TQ1", &[(7, D), (8, D)]),
    ("QPD", &[(3, S)]),
    ("QRD", &[(1, D), (8, I)]),
];

fn rules(id: &str) -> &'static [(usize, Kind)] {
    RULES
        .iter()
        .find(|(segment, _)| *segment == id)
        .map_or(&[], |(_, rules)| *rules)
}

/// Anonymizes one HL7 v2 message.
pub fn anonymize_message(
    anonymizer: &Anonymizer,
    input: &[u8],
    report: &mut Report,
) -> Result<Vec<u8>, AnonymizeError> {
    let mut message = oxim_hl7::Message::parse(input)
        .map_err(|e| AnonymizeError::Parse(format!("HL7 v2: {e}")))?;
    let delimiters = *message.delimiters();
    let separators = Separators {
        repetition: delimiters.repetition,
        component: delimiters.component,
    };
    let mut occurrences: HashMap<String, usize> = HashMap::new();
    let mut edits = Vec::new();
    for segment in message.segments() {
        let id = String::from_utf8_lossy(segment.id()).into_owned();
        let occurrence = {
            let count = occurrences.entry(id.clone()).or_default();
            *count += 1;
            *count
        };
        for &(field, kind) in rules(&id) {
            let path = format!("{id}[{occurrence}]-{field}");
            let Some(value) = message.get(&path) else {
                continue;
            };
            if id == "OBX" && field == 5 {
                let value_type = message
                    .get(&format!("OBX[{occurrence}]-2"))
                    .map(|v| v.raw().to_vec())
                    .unwrap_or_default();
                if !matches!(value_type.as_slice(), b"TX" | b"FT" | b"ST") {
                    continue;
                }
            }
            if let Some(new) = anonymizer.field(
                kind,
                value.raw(),
                separators,
                &format!("HL7 {id}-{field}"),
                report,
            ) {
                edits.push((path, new));
            }
        }
    }
    for (path, raw) in edits {
        message
            .set_raw(&path, &raw)
            .map_err(|e| AnonymizeError::Parse(format!("HL7 v2 {path}: {e}")))?;
    }
    report.messages += 1;
    Ok(message.to_bytes())
}

/// Start offsets of the messages in a file of one or more HL7 messages.
fn message_starts(input: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    for (index, window) in input.windows(4).enumerate() {
        let at_line_start = index == 0 || matches!(input[index - 1], b'\r' | b'\n');
        if at_line_start && window.starts_with(b"MSH") && !window[3].is_ascii_alphanumeric() {
            starts.push(index);
        }
    }
    starts
}

/// Anonymizes a file holding one HL7 v2 message or several (batch files and
/// concatenated messages). Batch header and trailer segments are kept.
pub fn anonymize_batch(
    anonymizer: &Anonymizer,
    input: &[u8],
    report: &mut Report,
) -> Result<Vec<u8>, AnonymizeError> {
    let starts = message_starts(input);
    let Some(&first) = starts.first() else {
        return Err(AnonymizeError::Parse("HL7 v2: no MSH segment".into()));
    };
    let mut out = input[..first].to_vec();
    for (index, &start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(input.len());
        let mut chunk = &input[start..end];
        // Batch trailers (BTS, FTS) after the last message stay as they are.
        let mut trailer: &[u8] = &[];
        if index + 1 == starts.len() {
            for marker in [&b"\rBTS"[..], b"\nBTS", b"\rFTS", b"\nFTS"] {
                if let Some(position) = chunk.windows(marker.len()).position(|w| w == marker) {
                    trailer = &chunk[position + 1..];
                    chunk = &chunk[..=position];
                    break;
                }
            }
        }
        out.extend_from_slice(&anonymize_message(anonymizer, chunk, report)?);
        out.extend_from_slice(trailer);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;

    const ORU: &str = "MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01|MSG1|P|2.5.1\r\
PID|1||PAT001^^^HOSP^MR~99887766^^^NAT^NI||Doe^Jane^Q||19800101|F|||1 Main St^^Town^^12345||555-0100\r\
PV1|1|O|||||D123^House^Greg||||||||||||V555\r\
ORC|RE|ORD1|FIL1\r\
OBR|1|ORD1|FIL1|GLU^Glucose^L|||20260929110000\r\
OBX|1|NM|GLU^Glucose^L||5.40|mmol/L|3.9-6.1|N|||F|||20260929115900\r\
OBX|2|TX|COMMENT^Comment^L||Patient Jane Doe called||||||F\r\
NTE|1||Hemolyzed, told Dr House\r\
SPM|1|SMP001||SER|||||||||||||20260929105000\r\
ZXT|1|keep me\r";

    fn anonymizer(free_text: bool) -> Anonymizer {
        let options = Options {
            free_text,
            ..Options::default()
        };
        Anonymizer::new(options, Some(b"0123456789abcdef"), Some(-100)).unwrap()
    }

    #[test]
    fn removes_patient_data_and_keeps_results() {
        let a = anonymizer(true);
        let mut report = a.report();
        let out = anonymize_message(&a, ORU.as_bytes(), &mut report).unwrap();
        let text = String::from_utf8(out.clone()).unwrap();
        for secret in [
            "Doe", "Jane", "PAT001", "99887766", "Main St", "555-0100", "House", "Greg", "D123",
            "V555", "ORD1", "FIL1", "SMP001", "19800101", "Dr House",
        ] {
            assert!(!text.contains(secret), "{secret} survived:\n{text}");
        }
        let message = oxim_hl7::Message::parse(&out).unwrap();
        let get = |path: &str| {
            message
                .get(path)
                .map(|v| v.raw().to_vec())
                .map(|v| String::from_utf8(v).unwrap())
                .unwrap_or_default()
        };
        assert_eq!(get("PID-7"), "19790923");
        assert_eq!(get("MSH-7"), "20260621120000");
        assert_eq!(get("OBX-5"), "5.40");
        assert_eq!(get("OBX-6"), "mmol/L");
        assert_eq!(get("OBX[2]-5"), "REDACTED");
        assert_eq!(get("NTE-3"), "REDACTED");
        assert_eq!(get("SPM-17"), "20260621105000");
        assert_eq!(get("ZXT-2"), "keep me");
        assert!(get("PID-3").contains("^^^HOSP^MR~"));
        assert_eq!(get("PID-11"), "");
        assert_eq!(get("MSH-10"), "MSG1");
        assert_eq!(report.messages, 1);
        assert_eq!(report.changes["HL7 PID-5"].pseudonymized, 1);
        let rendered = report.to_string();
        assert!(
            !rendered.contains("Doe") && !rendered.contains("-100"),
            "{rendered}"
        );

        // The same run gives the same pseudonyms; the identifier links messages.
        let again = anonymize_message(&a, ORU.as_bytes(), &mut report).unwrap();
        assert_eq!(again, out);
    }

    #[test]
    fn handles_batches() {
        let a = anonymizer(false);
        let mut report = a.report();
        let batch = format!("BHS|^~\\&|LAB\r{ORU}{ORU}BTS|2\r");
        let out = anonymize_batch(&a, batch.as_bytes(), &mut report).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("BHS|^~\\&|LAB\rMSH|"), "{text}");
        assert!(text.ends_with("BTS|2\r"), "{text}");
        assert_eq!(report.messages, 2);
        assert!(!text.contains("Doe^Jane"));
        // Free text is kept unless requested: it may name people.
        assert!(text.contains("Patient Jane Doe called"));
        assert!(anonymize_batch(&a, b"PID|1", &mut report).is_err());
    }
}
