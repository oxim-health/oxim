//! ASTM E1394 (CLSI LIS02) rules. Field numbers count the record type as
//! field 1.
//!
//! | Record | Pseudonymized | Removed | Dates shifted |
//! |---|---|---|---|
//! | H | | | H-14 |
//! | P | P-3, P-4, P-5 (identifiers); P-6 (name); P-14 (physician) | P-7 (mother's maiden name), P-11 (address), P-13 (telephone) | P-8 (birth date), P-24 |
//! | O | O-3, O-4 (specimen identifiers); O-17 (ordering physician) | O-18 (physician telephone) | O-7, O-8, O-9, O-23 |
//! | R | | | R-12, R-13 |
//! | Q | Q-3, Q-4 (patient and specimen identifiers) | | Q-7, Q-8 |
//! | C | | | |
//!
//! Specimen identifiers are pseudonymized unless
//! [`Options::specimens`](crate::Options::specimens) is off. With
//! [`Options::free_text`](crate::Options::free_text), comment text (C-4)
//! becomes `REDACTED`. The rules work record by record on the bytes, so
//! they also apply to records inside LIS01 frames of a capture.

use crate::{Anonymizer, Kind, Report, Separators};

use Kind::{
    Clinician as C, Date as D, Identifier as I, Name as N, QueryRange as Q, Remove as R,
    Specimen as S, Text as T,
};

const RULES: &[(u8, &[(usize, Kind)])] = &[
    (b'H', &[(14, D)]),
    (
        b'P',
        &[
            (3, I),
            (4, I),
            (5, I),
            (6, N),
            (7, R),
            (8, D),
            (11, R),
            (13, R),
            (14, C),
            (24, D),
        ],
    ),
    (
        b'O',
        &[
            (3, S),
            (4, S),
            (7, D),
            (8, D),
            (9, D),
            (17, C),
            (18, R),
            (23, D),
        ],
    ),
    (b'R', &[(12, D), (13, D)]),
    (b'Q', &[(3, Q), (4, Q), (7, D), (8, D)]),
    (b'C', &[(4, T)]),
];

/// The delimiters an ASTM message declares in its header record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delimiters {
    /// Field delimiter (`|`).
    pub field: u8,
    /// Repeat delimiter (`\`).
    pub repeat: u8,
    /// Component delimiter (`^`).
    pub component: u8,
    /// Escape character (`&`).
    pub escape: u8,
}

impl Default for Delimiters {
    fn default() -> Self {
        Self {
            field: b'|',
            repeat: b'\\',
            component: b'^',
            escape: b'&',
        }
    }
}

impl Delimiters {
    /// The delimiters of a header record (`H|\^&...`).
    pub fn from_header(record: &[u8]) -> Option<Self> {
        match record {
            [b'H', field, repeat, component, escape, ..] => Some(Self {
                field: *field,
                repeat: *repeat,
                component: *component,
                escape: *escape,
            }),
            _ => None,
        }
    }
}

fn anonymize_record(
    anonymizer: &Anonymizer,
    record: &[u8],
    delimiters: &mut Delimiters,
    report: &mut Report,
) -> Option<Vec<u8>> {
    if let Some(declared) = Delimiters::from_header(record) {
        *delimiters = declared;
    }
    let record_type = *record.first()?;
    let rules = RULES
        .iter()
        .find(|(kind, _)| *kind == record_type.to_ascii_uppercase())
        .map(|(_, rules)| *rules)?;
    let mut fields: Vec<Vec<u8>> = record
        .split(|&b| b == delimiters.field)
        .map(<[u8]>::to_vec)
        .collect();
    // A single letter is the record type; anything else is not a record.
    if fields.first().is_none_or(|t| t.len() != 1) {
        return None;
    }
    let separators = Separators {
        repetition: delimiters.repeat,
        component: delimiters.component,
    };
    let mut changed = false;
    for &(field, kind) in rules {
        let Some(value) = fields.get(field - 1) else {
            continue;
        };
        let location = format!(
            "ASTM {}-{field}",
            char::from(record_type.to_ascii_uppercase())
        );
        if let Some(new) = anonymizer.field(kind, value, separators, &location, report) {
            fields[field - 1] = new;
            changed = true;
        }
    }
    changed.then(|| fields.join(&delimiters.field))
}

/// Anonymizes CR-terminated records, keeping the record terminators as
/// they are (CR, CR LF or LF). `delimiters` is updated from any header
/// record, so it carries over between frames of one conversation.
pub fn anonymize_records(
    anonymizer: &Anonymizer,
    text: &[u8],
    delimiters: &mut Delimiters,
    report: &mut Report,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for piece in text.split_inclusive(|&b| b == b'\r' || b == b'\n') {
        let content_len = piece
            .iter()
            .rposition(|&b| b != b'\r' && b != b'\n')
            .map_or(0, |i| i + 1);
        let (content, terminator) = piece.split_at(content_len);
        match anonymize_record(anonymizer, content, delimiters, report) {
            Some(new) => out.extend_from_slice(&new),
            None => out.extend_from_slice(content),
        }
        out.extend_from_slice(terminator);
    }
    out
}

/// Anonymizes one ASTM message (without LIS01 framing).
pub fn anonymize_message(anonymizer: &Anonymizer, input: &[u8], report: &mut Report) -> Vec<u8> {
    let mut delimiters = Delimiters::default();
    report.messages += 1;
    anonymize_records(anonymizer, input, &mut delimiters, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;

    const RESULTS: &str = "H|\\^&|||Analyzer^2.1|||||||P|LIS2-A2|20260929143000\r\n\
P|1|PRAC01|LAB42||Doe^Jane^M||19800101|F||1 Main St||555-0100|D123^House^Greg\r\n\
O|1|S123^01^02||^^^GLU|R|20260929120000|20260929115500||||N||||SER\r\n\
R|1|^^^GLU|5.40|mmol/L|3.9-6.1|N||F||tech1|20260929142000|20260929142500|Analyzer\r\n\
C|1|I|Jane called about Dr House|G\r\n\
Q|1|LAB42^S123||ALL||20260929||||||O\r\n\
L|1|N\r\n";

    #[test]
    fn anonymizes_records_and_keeps_results() {
        let options = Options {
            free_text: true,
            ..Options::default()
        };
        let a = Anonymizer::new(options, Some(b"0123456789abcdef"), Some(-100)).unwrap();
        let mut report = a.report();
        let out =
            String::from_utf8(anonymize_message(&a, RESULTS.as_bytes(), &mut report)).unwrap();
        for secret in [
            "Doe", "Jane", "PRAC01", "LAB42", "Main St", "555-0100", "House", "D123", "S123",
            "19800101", "20260929",
        ] {
            assert!(!out.contains(secret), "{secret} survived:\n{out}");
        }
        assert!(out.contains("R|1|^^^GLU|5.40|mmol/L|3.9-6.1|N||F||tech1|20260621142000|20260621142500|Analyzer\r\n"), "{out}");
        assert!(out.contains("|19790923|F|"), "{out}");
        assert!(out.contains("C|1|I|REDACTED|G\r\n"), "{out}");
        assert!(out.contains("Q|1|"), "{out}");
        assert!(out.ends_with("L|1|N\r\n"));
        assert_eq!(out.matches("\r\n").count(), 7);
        // The patient's lab identifier maps to the same pseudonym in P and Q.
        let p4 = out
            .split('\r')
            .nth(1)
            .unwrap()
            .split('|')
            .nth(3)
            .unwrap()
            .to_owned();
        assert!(out.contains(&format!("Q|1|{p4}^")), "{out}");
        assert_eq!(report.changes["ASTM P-6"].pseudonymized, 1);
    }

    #[test]
    fn follows_declared_delimiters() {
        let a = Anonymizer::new(Options::default(), Some(b"0123456789abcdef"), Some(-1)).unwrap();
        let mut report = a.report();
        let text = b"H!@#$!!!Analyzer\rP!1!!LAB42!!Doe#Jane\r";
        let out = String::from_utf8(anonymize_message(&a, text, &mut report)).unwrap();
        assert!(!out.contains("Doe") && !out.contains("LAB42"), "{out}");
        assert!(out.contains('#'), "{out}");
    }
}
