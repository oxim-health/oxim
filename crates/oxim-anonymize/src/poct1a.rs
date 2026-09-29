//! POCT1-A rules. POCT1-A carries values in the `V` attribute of named
//! elements; the attribute is rewritten in place and the rest of the
//! document is kept byte for byte.
//!
//! | Element | Action |
//! |---|---|
//! | `PT.patient_id`, `OPR.operator_id` and other `*.patient_id` | pseudonymized |
//! | `PT.name`, `OPR.name` | invented name |
//! | `*.specimen_id`, `*.order_id` | pseudonymized (unless specimens are kept) |
//! | `PT.birth_date` and every `*_dttm` | date shifted |
//! | `PT.location` | removed |
//! | `NTE.text`, `*.comment` | `REDACTED` with free text redaction |
//!
//! Operators are staff, not patients, but their identifiers are personal
//! data too and link a capture to a site.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::{Action, Anonymizer, Report};

fn pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        #[allow(clippy::expect_used)]
        Regex::new(
            r#"(<([A-Za-z_][A-Za-z0-9_.\-]*)(?:\s[^<>]*?)?\sV\s*=\s*)(?:"([^"]*)"|'([^']*)')"#,
        )
        .expect("the POCT1-A attribute pattern is valid")
    })
}

enum Rule {
    Identifier,
    Specimen,
    Name,
    Date,
    Remove,
    Text,
}

fn rule(element: &str) -> Option<Rule> {
    Some(match element {
        "PT.name" | "OPR.name" => Rule::Name,
        "PT.birth_date" => Rule::Date,
        "PT.location" => Rule::Remove,
        "OPR.operator_id" => Rule::Identifier,
        "NTE.text" => Rule::Text,
        _ if element.ends_with(".patient_id") => Rule::Identifier,
        _ if element.ends_with(".specimen_id") || element.ends_with(".order_id") => Rule::Specimen,
        _ if element.ends_with("_dttm") => Rule::Date,
        _ if element.ends_with(".comment") => Rule::Text,
        _ => return None,
    })
}

/// Anonymizes POCT1-A XML (one document or a stream of documents).
pub fn anonymize_document(anonymizer: &Anonymizer, input: &[u8], report: &mut Report) -> Vec<u8> {
    report.messages += 1;
    anonymize_text(anonymizer, input, report)
}

/// The attribute values to replace in POCT1-A XML: `(start, end,
/// replacement)` byte spans of whole matches, in order.
pub(crate) fn spans(
    anonymizer: &Anonymizer,
    input: &[u8],
    report: &mut Report,
) -> Vec<(usize, usize, Vec<u8>)> {
    let options = anonymizer.options();
    let mut spans = Vec::new();
    for captures in pattern().captures_iter(input) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        let element = String::from_utf8_lossy(captures.get(2).map_or(&b""[..], |m| m.as_bytes()))
            .into_owned();
        let (value, quote) = match (captures.get(3), captures.get(4)) {
            (Some(value), _) => (value.as_bytes(), b'"'),
            (None, Some(value)) => (value.as_bytes(), b'\''),
            (None, None) => continue,
        };
        if value.is_empty() {
            continue;
        }
        let replaced = match rule(&element) {
            Some(Rule::Identifier) => Some((anonymizer.pseudonym(value), Action::Pseudonymized)),
            Some(Rule::Specimen) if options.specimens => {
                Some((anonymizer.pseudonym(value), Action::Pseudonymized))
            }
            Some(Rule::Name) => Some((
                anonymizer.family_name(value).into_bytes(),
                Action::Pseudonymized,
            )),
            Some(Rule::Date) => std::str::from_utf8(value)
                .ok()
                .and_then(|text| anonymizer.shift_iso(text))
                .map(|shifted| (shifted.into_bytes(), Action::Shifted)),
            Some(Rule::Remove) => Some((Vec::new(), Action::Removed)),
            Some(Rule::Text) if options.free_text => Some((b"REDACTED".to_vec(), Action::Redacted)),
            _ => None,
        };
        if let Some((new, action)) = replaced
            && new != value
        {
            report.note(format!("POCT1-A {element}"), action);
            let mut out = captures.get(1).map_or(&b""[..], |m| m.as_bytes()).to_vec();
            out.push(quote);
            out.extend_from_slice(&new);
            out.push(quote);
            spans.push((whole.start(), whole.end(), out));
        }
    }
    spans
}

/// Anonymizes POCT1-A XML without counting a message.
pub(crate) fn anonymize_text(
    anonymizer: &Anonymizer,
    input: &[u8],
    report: &mut Report,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut position = 0;
    for (start, end, replacement) in spans(anonymizer, input, report) {
        out.extend_from_slice(&input[position..start]);
        out.extend_from_slice(&replacement);
        position = end;
    }
    out.extend_from_slice(&input[position..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;

    #[test]
    fn rewrites_attributes_in_place() {
        let a = Anonymizer::new(Options::default(), Some(b"0123456789abcdef"), Some(-100)).unwrap();
        let mut report = a.report();
        let document = "<?xml version=\"1.0\"?><OBS.R01><HDR><HDR.control_id V=\"3\"/>\
            <HDR.creation_dttm V=\"2026-09-29T12:00:00+03:00\"/></HDR><SVC><PT>\
            <PT.patient_id V=\"PAT0042\"/><PT.name V='Doe, Jane'/><PT.birth_date V=\"1980-01-01\"/>\
            <PT.location V=\"Ward 7\"/><OBS><OBS.observation_id V=\"pH\"/><OBS.value V=\"7.41\" U=\"\"/></OBS></PT>\
            <OPR><OPR.operator_id  V=\"nurse.smith\"/></OPR></SVC></OBS.R01>";
        let out =
            String::from_utf8(anonymize_document(&a, document.as_bytes(), &mut report)).unwrap();
        for secret in [
            "PAT0042",
            "Doe",
            "1980-01-01",
            "Ward 7",
            "nurse.smith",
            "2026-09-29",
        ] {
            assert!(!out.contains(secret), "{secret} survived:\n{out}");
        }
        assert!(
            out.contains("<HDR.creation_dttm V=\"2026-06-21T12:00:00+03:00\"/>"),
            "{out}"
        );
        assert!(out.contains("<PT.birth_date V=\"1979-09-23\"/>"), "{out}");
        assert!(out.contains("<PT.location V=\"\"/>"), "{out}");
        assert!(out.contains("<OBS.value V=\"7.41\" U=\"\"/>"), "{out}");
        assert!(out.contains("<OPR.operator_id  V=\""), "{out}");
        assert!(out.contains("<PT.name V='"), "{out}");
        assert_eq!(report.changes["POCT1-A PT.patient_id"].pseudonymized, 1);
    }
}
