//! JSON and XML documents have no fixed schema, so their rules come from
//! the configuration:
//!
//! ```yaml
//! json:
//!   - {path: patient.identifier[*].value, action: pseudonymize}
//!   - {path: patient.name, action: name}
//!   - {path: patient.birthDate, action: shift-date}
//!   - {path: patient.address, action: remove}
//!   - {path: notes[*], action: redact}
//! xml:
//!   - {path: /order/patient/@id, action: pseudonymize}
//!   - {path: /order/patient/name, action: name}
//!   - {path: "/order/sample[*]/@collected", action: shift-date}
//! ```
//!
//! JSON paths are dotted member names with `[n]` indexes, `[*]` for every
//! array element and `*` for every member. XML paths follow `oxim-formats`
//! (`/a/b[2]/@attr`, 1-based positions) with `[*]` for every repeated
//! element. An action on an object or array applies to every text and
//! number inside it. Dates may be ISO 8601 or compact (`YYYYMMDD...`).
//! `remove` deletes a JSON member (array elements become `null`) and empties
//! an XML text or attribute.

use serde::Deserialize;
use serde_json::Value;

use crate::{Action, AnonymizeError, Anonymizer, Report};

/// What to do with the values at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathAction {
    /// Replace by an identifier of the same shape.
    Pseudonymize,
    /// Replace by an invented name.
    Name,
    /// Shift dates.
    ShiftDate,
    /// Remove.
    Remove,
    /// Replace by `REDACTED`.
    Redact,
}

/// A path and its action.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRule {
    /// Where.
    pub path: String,
    /// What.
    pub action: PathAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Key(String),
    AnyKey,
    Index(usize),
    AnyIndex,
}

fn json_steps(path: &str) -> Result<Vec<Step>, AnonymizeError> {
    let invalid = || AnonymizeError::Config(format!("invalid JSON path {path:?}"));
    let mut steps = Vec::new();
    for segment in path.split('.') {
        let (name, mut rest) = match segment.find('[') {
            Some(at) => segment.split_at(at),
            None => (segment, ""),
        };
        match name {
            "" if steps.is_empty() && !rest.is_empty() => {}
            "" => return Err(invalid()),
            "*" => steps.push(Step::AnyKey),
            name => steps.push(Step::Key(name.to_owned())),
        }
        while !rest.is_empty() {
            let close = rest.find(']').ok_or_else(invalid)?;
            let inner = rest.get(1..close).ok_or_else(invalid)?;
            steps.push(if inner == "*" {
                Step::AnyIndex
            } else {
                Step::Index(inner.parse().map_err(|_| invalid())?)
            });
            rest = &rest[close + 1..];
            if !rest.is_empty() && !rest.starts_with('[') {
                return Err(invalid());
            }
        }
    }
    Ok(steps)
}

fn transform_text(
    anonymizer: &Anonymizer,
    action: PathAction,
    text: &str,
) -> Option<(String, Action)> {
    let new = match action {
        PathAction::Pseudonymize => (
            String::from_utf8_lossy(&anonymizer.pseudonym(text.as_bytes())).into_owned(),
            Action::Pseudonymized,
        ),
        PathAction::Name => (
            anonymizer.family_name(text.as_bytes()),
            Action::Pseudonymized,
        ),
        PathAction::ShiftDate => (
            anonymizer
                .shift_iso(text)
                .or_else(|| anonymizer.shift_compact(text))?,
            Action::Shifted,
        ),
        PathAction::Remove => (String::new(), Action::Removed),
        PathAction::Redact => ("REDACTED".to_owned(), Action::Redacted),
    };
    (new.0 != text).then_some(new)
}

fn transform_json(
    anonymizer: &Anonymizer,
    action: PathAction,
    value: &mut Value,
    location: &str,
    report: &mut Report,
) {
    match value {
        Value::String(text) => {
            if let Some((new, done)) = transform_text(anonymizer, action, text) {
                *text = new;
                report.note(location, done);
            }
        }
        Value::Number(number) => {
            let text = number.to_string();
            if let Some((new, done)) = transform_text(anonymizer, action, &text) {
                *value = match (action, serde_json::from_str::<Value>(&new)) {
                    (
                        PathAction::Pseudonymize | PathAction::ShiftDate,
                        Ok(number @ Value::Number(_)),
                    ) => number,
                    _ => Value::String(new),
                };
                report.note(location, done);
            }
        }
        Value::Array(items) => {
            for item in items {
                transform_json(anonymizer, action, item, location, report);
            }
        }
        Value::Object(members) => {
            for member in members.values_mut() {
                transform_json(anonymizer, action, member, location, report);
            }
        }
        Value::Null | Value::Bool(_) => {}
    }
}

fn walk_json(
    anonymizer: &Anonymizer,
    value: &mut Value,
    steps: &[Step],
    action: PathAction,
    location: &str,
    report: &mut Report,
) {
    let Some((step, rest)) = steps.split_first() else {
        if action == PathAction::Remove {
            if !value.is_null() {
                *value = Value::Null;
                report.note(location, Action::Removed);
            }
        } else {
            transform_json(anonymizer, action, value, location, report);
        }
        return;
    };
    match (step, value) {
        (Step::Key(key), Value::Object(members)) => {
            if rest.is_empty() && action == PathAction::Remove {
                if members.remove(key).is_some() {
                    report.note(location, Action::Removed);
                }
            } else if let Some(member) = members.get_mut(key) {
                walk_json(anonymizer, member, rest, action, location, report);
            }
        }
        (Step::AnyKey, Value::Object(members)) => {
            if rest.is_empty() && action == PathAction::Remove {
                if !members.is_empty() {
                    report.note(location, Action::Removed);
                }
                members.clear();
            } else {
                for member in members.values_mut() {
                    walk_json(anonymizer, member, rest, action, location, report);
                }
            }
        }
        (Step::Index(index), Value::Array(items)) => {
            if let Some(item) = items.get_mut(*index) {
                walk_json(anonymizer, item, rest, action, location, report);
            }
        }
        (Step::AnyIndex, Value::Array(items)) => {
            for item in items {
                walk_json(anonymizer, item, rest, action, location, report);
            }
        }
        _ => {}
    }
}

/// Anonymizes a JSON document by the configured rules.
pub fn anonymize_json(
    anonymizer: &Anonymizer,
    input: &[u8],
    report: &mut Report,
) -> Result<Vec<u8>, AnonymizeError> {
    let rules = &anonymizer.options().json;
    report.messages += 1;
    if rules.is_empty() {
        report.warn("no JSON rules are configured: JSON input was copied unchanged");
        return Ok(input.to_vec());
    }
    let mut document =
        oxim_formats::JsonDocument::parse(input, &oxim_formats::JsonOptions::default())
            .map_err(|e| AnonymizeError::Parse(format!("JSON: {e}")))?;
    let before = report.total();
    for rule in rules {
        let steps = json_steps(&rule.path)?;
        let location = format!("JSON {}", rule.path);
        walk_json(
            anonymizer,
            document.value_mut(),
            &steps,
            rule.action,
            &location,
            report,
        );
    }
    Ok(if report.total() == before {
        input.to_vec()
    } else {
        document.to_bytes()
    })
}

/// Expands every `[*]` of an XML path into the positions that exist.
fn expand_xml(
    document: &oxim_formats::XmlDocument,
    path: &str,
) -> Result<Vec<String>, AnonymizeError> {
    let Some(at) = path.find("[*]") else {
        return Ok(vec![path.to_owned()]);
    };
    let prefix = &path[..at];
    let rest = &path[at + 3..];
    let count = document
        .count(prefix)
        .map_err(|e| AnonymizeError::Config(format!("XML path {path:?}: {e}")))?;
    let mut paths = Vec::new();
    for position in 1..=count {
        paths.extend(expand_xml(
            document,
            &format!("{prefix}[{position}]{rest}"),
        )?);
    }
    Ok(paths)
}

/// Anonymizes an XML document by the configured rules.
pub fn anonymize_xml(
    anonymizer: &Anonymizer,
    input: &[u8],
    report: &mut Report,
) -> Result<Vec<u8>, AnonymizeError> {
    let rules = &anonymizer.options().xml;
    report.messages += 1;
    if rules.is_empty() {
        report.warn("no XML rules are configured: XML input was copied unchanged");
        return Ok(input.to_vec());
    }
    let mut document =
        oxim_formats::XmlDocument::parse(input, &oxim_formats::XmlOptions::default())
            .map_err(|e| AnonymizeError::Parse(format!("XML: {e}")))?;
    for rule in rules {
        let location = format!("XML {}", rule.path);
        for path in expand_xml(&document, &rule.path)? {
            let current = document
                .get(&path)
                .map_err(|e| AnonymizeError::Config(format!("XML path {path:?}: {e}")))?;
            let Some(text) = current else { continue };
            if let Some((new, done)) = transform_text(anonymizer, rule.action, &text) {
                document
                    .set(&path, &new)
                    .map_err(|e| AnonymizeError::Parse(format!("XML {path}: {e}")))?;
                report.note(&location, done);
            }
        }
    }
    Ok(document.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;

    fn anonymizer(options: Options) -> Anonymizer {
        Anonymizer::new(options, Some(b"0123456789abcdef"), Some(-100)).unwrap()
    }

    #[test]
    fn parses_json_paths() {
        assert_eq!(
            json_steps("a.b[2][*].*").unwrap(),
            [
                Step::Key("a".into()),
                Step::Key("b".into()),
                Step::Index(2),
                Step::AnyIndex,
                Step::AnyKey
            ]
        );
        assert_eq!(
            json_steps("[0].x").unwrap(),
            [Step::Index(0), Step::Key("x".into())]
        );
        for invalid in ["a..b", "a[", "a[x]", "a[1]b", ""] {
            assert!(json_steps(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn applies_json_rules() {
        let options = Options::from_yaml(
            "json:
  - {path: 'patient.ids[*].value', action: pseudonymize}
  - {path: patient.name, action: name}
  - {path: patient.birthDate, action: shift-date}
  - {path: patient.address, action: remove}
  - {path: 'notes[*]', action: redact}
  - {path: patient.mrn, action: pseudonymize}
",
        )
        .unwrap();
        let a = anonymizer(options);
        let mut report = a.report();
        let input = br#"{"patient":{"ids":[{"value":"PAT001"},{"value":"X9"}],"name":{"family":"Doe","given":["Jane"]},"birthDate":"1980-01-01","address":{"line":"1 Main St"},"mrn":1234567},"notes":["Jane called"],"result":{"value":5.40}}"#;
        let out: Value =
            serde_json::from_slice(&anonymize_json(&a, input, &mut report).unwrap()).unwrap();
        let text = out.to_string();
        for secret in [
            "PAT001",
            "\"X9\"",
            "Doe",
            "Jane",
            "1980-01-01",
            "Main St",
            "1234567",
        ] {
            assert!(!text.contains(secret), "{secret} survived: {text}");
        }
        assert_eq!(out["patient"]["birthDate"], "1979-09-23");
        assert!(out["patient"].get("address").is_none());
        assert!(out["patient"]["mrn"].is_number());
        assert_eq!(out["notes"][0], "REDACTED");
        assert_eq!(out["result"]["value"].to_string(), "5.40");

        let none = anonymizer(Options::default());
        let mut report = none.report();
        assert_eq!(anonymize_json(&none, input, &mut report).unwrap(), input);
        assert_eq!(report.warnings.len(), 1);
    }

    #[test]
    fn applies_xml_rules() {
        let options = Options::from_yaml(
            "xml:
  - {path: /order/patient/@id, action: pseudonymize}
  - {path: /order/patient/name, action: name}
  - {path: '/order/sample[*]/@collected', action: shift-date}
  - {path: /order/patient/phone, action: remove}
",
        )
        .unwrap();
        let a = anonymizer(options);
        let mut report = a.report();
        let input = br#"<order><patient id="PAT001"><name>Doe</name><phone>555-0100</phone></patient><sample collected="20260929120000"/><sample collected="2026-09-30"/></order>"#;
        let out = String::from_utf8(anonymize_xml(&a, input, &mut report).unwrap()).unwrap();
        for secret in ["PAT001", "Doe", "555-0100", "20260929", "2026-09-30"] {
            assert!(!out.contains(secret), "{secret} survived: {out}");
        }
        assert!(out.contains("collected=\"20260621120000\""), "{out}");
        assert!(out.contains("collected=\"2026-06-22\""), "{out}");
        assert!(
            out.contains("<phone></phone>") || out.contains("<phone/>"),
            "{out}"
        );
        assert_eq!(report.changes["XML /order/sample[*]/@collected"].shifted, 2);
    }
}
