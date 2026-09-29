//! Paths with an optional `[*]` wildcard.

use oxim_core::{Document, EngineError, StepError};

use crate::settings::config_error;

/// The largest number of occurrences a wildcard visits.
pub(crate) const MAX_OCCURRENCES: usize = 10_000;

/// A document path, possibly with one `[*]` wildcard for "every
/// occurrence", as in `OBX[*]-3.1` (HL7), `R[*]-3` (ASTM) or
/// `/order/test[*]/@code` (XML).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathSpec {
    text: String,
    wildcard: Option<(String, String)>,
}

impl PathSpec {
    /// Parses a path. At most one `[*]` is allowed.
    pub(crate) fn parse(at: &str, text: &str) -> Result<Self, EngineError> {
        if text.trim().is_empty() {
            return Err(config_error(at, "a path must not be empty"));
        }
        let wildcard = match text.split_once("[*]") {
            Some((prefix, suffix)) => {
                if suffix.contains("[*]") {
                    return Err(config_error(at, format!("{text:?} has more than one [*]")));
                }
                if prefix.is_empty() {
                    return Err(config_error(at, format!("{text:?} has nothing before [*]")));
                }
                Some((prefix.to_owned(), suffix.to_owned()))
            }
            None => None,
        };
        Ok(Self {
            text: text.to_owned(),
            wildcard,
        })
    }

    /// The part before `[*]`, if the path has a wildcard.
    pub(crate) fn wildcard_prefix(&self) -> Option<&str> {
        self.wildcard.as_ref().map(|(prefix, _)| prefix.as_str())
    }

    /// The concrete path for occurrence `n` of the wildcard. Paths without a
    /// wildcard, or calls without an occurrence, return the path unchanged.
    pub(crate) fn resolve(&self, occurrence: Option<usize>) -> String {
        match (&self.wildcard, occurrence) {
            (Some((prefix, suffix)), Some(n)) => format!("{prefix}[{n}]{suffix}"),
            _ => self.text.clone(),
        }
    }
}

/// Whether `prefix` is an HL7 segment or ASTM record identifier.
fn is_segment_id(prefix: &str) -> bool {
    (1..=3).contains(&prefix.len()) && prefix.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Counts the occurrences a wildcard with `prefix` visits in `document`.
///
/// HL7 segments and ASTM records are counted by probing field 1 of
/// `SEG[n]`; XML elements by probing `prefix[n]`. Other data types do not
/// support wildcards.
pub(crate) fn occurrences(document: &Document, prefix: &str) -> Result<usize, StepError> {
    let probe: Box<dyn Fn(usize) -> String> = match document {
        Document::Hl7(_) | Document::Astm(_) if is_segment_id(prefix) => {
            Box::new(move |n| format!("{prefix}[{n}]-1"))
        }
        Document::Format(oxim_formats::Document::Xml(_)) => {
            Box::new(move |n| format!("{prefix}[{n}]"))
        }
        _ => {
            return Err(StepError::new(
                "wildcard",
                format!(
                    "[*] after {prefix:?} is not supported here; use it after an HL7 segment, an ASTM record or an XML element"
                ),
            ));
        }
    };
    let mut count = 0;
    while count < MAX_OCCURRENCES && document.get(&probe(count + 1))?.is_some() {
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use oxim_core::DocumentParser;
    use oxim_model::DataType;

    use super::*;

    #[test]
    fn parses_and_resolves_wildcards() {
        let path = PathSpec::parse("t", "OBX[*]-3.1").unwrap();
        assert_eq!(path.wildcard_prefix(), Some("OBX"));
        assert_eq!(path.resolve(Some(2)), "OBX[2]-3.1");
        assert_eq!(path.resolve(None), "OBX[*]-3.1");
        let plain = PathSpec::parse("t", "PID-5.1").unwrap();
        assert_eq!(plain.resolve(Some(2)), "PID-5.1");
        for bad in ["", "  ", "[*]-3", "OBX[*]-3[*]"] {
            assert!(PathSpec::parse("t", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn counts_segments_records_and_elements() {
        let hl7 = DocumentParser::new(DataType::Hl7V2, None)
            .unwrap()
            .parse(b"MSH|^~\\&\rOBX|1\rNTE|1\rOBX|2\rOBX|3\r")
            .unwrap();
        assert_eq!(occurrences(&hl7, "OBX").unwrap(), 3);
        assert_eq!(occurrences(&hl7, "ZZZ").unwrap(), 0);
        let astm = DocumentParser::new(DataType::Astm, None)
            .unwrap()
            .parse(b"H|\\^&\rR|1\rR|2\rL|1\r")
            .unwrap();
        assert_eq!(occurrences(&astm, "R").unwrap(), 2);
        let xml = DocumentParser::new(DataType::Xml, None)
            .unwrap()
            .parse(b"<order><test code='A'/><test code='B'/></order>")
            .unwrap();
        assert_eq!(occurrences(&xml, "/order/test").unwrap(), 2);
        let json = DocumentParser::new(DataType::Json, None)
            .unwrap()
            .parse(b"[1,2]")
            .unwrap();
        assert!(occurrences(&json, "items").is_err());
    }
}
