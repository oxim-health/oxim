//! Comparing OXIM's messages with Mirth Connect's, field by field.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How messages are compared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareOptions {
    /// Paths whose values are ignored, such as `MSH-7` and `MSH-10`
    /// (HL7 v2) or `H-14` (ASTM).
    pub ignore: Vec<String>,
    /// A path whose value pairs OXIM's and Mirth's messages, such as
    /// `OBR-3`; without it messages are paired in order.
    pub key: Option<String>,
    /// Whether the report shows the differing values. Off by default,
    /// because they may be patient data.
    pub show_values: bool,
}

/// One differing field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDifference {
    /// The field, for example `PID-5` or `OBX[2]-5`.
    pub path: String,
    /// Mirth's value (or its length when values are hidden).
    pub mirth: String,
    /// OXIM's value (or its length when values are hidden).
    pub oxim: String,
}

/// A pair of messages that differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Difference {
    /// The position of Mirth's message among the captured ones.
    pub mirth_index: usize,
    /// The inbound message that produced OXIM's message, when known.
    pub inbound_index: Option<usize>,
    /// The pairing key, when a key path was given.
    pub key: Option<String>,
    /// The differing fields.
    pub fields: Vec<FieldDifference>,
}

/// The comparison of one destination.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestinationReport {
    /// The OXIM destination.
    pub destination: String,
    /// Messages OXIM produced.
    pub oxim_messages: usize,
    /// Messages Mirth sent, as captured.
    pub mirth_messages: usize,
    /// Pairs that are identical after ignoring the ignored paths.
    pub identical: usize,
    /// Pairs that differ.
    pub different: Vec<Difference>,
    /// Mirth messages without an OXIM counterpart.
    pub missing_in_oxim: usize,
    /// OXIM messages without a Mirth counterpart.
    pub extra_in_oxim: usize,
}

/// A message as labelled fields.
fn fields(data: &[u8], ignore: &[String]) -> Vec<(String, Vec<u8>)> {
    if data.starts_with(b"MSH")
        && let Ok(mut message) = oxim_hl7::Message::parse(data)
    {
        for path in ignore {
            if message.get(path).is_some() {
                let _ = message.set_raw(path, b"");
            }
        }
        let mut seen: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
        let mut out = Vec::new();
        for segment in message.segments() {
            let id = segment.id().to_vec();
            let occurrence = seen.entry(id.clone()).or_default();
            *occurrence += 1;
            let name = String::from_utf8_lossy(&id).into_owned();
            let label = if *occurrence == 1 {
                name
            } else {
                format!("{name}[{occurrence}]")
            };
            for n in 1..=segment.field_count() {
                let value = segment
                    .field(n)
                    .map(|v| v.raw().to_vec())
                    .unwrap_or_default();
                out.push((format!("{label}-{n}"), value));
            }
        }
        return out;
    }
    if data.starts_with(b"H")
        && let Ok(mut message) = oxim_astm::Message::parse(data)
    {
        for path in ignore {
            if message.get(path).is_some() {
                let _ = message.set_raw(path, b"");
            }
        }
        let mut seen: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
        let mut out = Vec::new();
        for record in message.records() {
            let kind = record.record_type().to_vec();
            let occurrence = seen.entry(kind.clone()).or_default();
            *occurrence += 1;
            let name = String::from_utf8_lossy(&kind).into_owned();
            let label = if *occurrence == 1 {
                name
            } else {
                format!("{name}[{occurrence}]")
            };
            for n in 2..=record.field_count() {
                let value = record
                    .field(n)
                    .map(|v| v.raw().to_vec())
                    .unwrap_or_default();
                out.push((format!("{label}-{n}"), value));
            }
        }
        return out;
    }
    data.split(|b| *b == b'\r' || *b == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(i, line)| (format!("line {}", i + 1), line.to_vec()))
        .collect()
}

fn key_of(data: &[u8], path: &str) -> Option<String> {
    if data.starts_with(b"MSH") {
        let message = oxim_hl7::Message::parse(data).ok()?;
        return message.get(path).map(|v| v.to_string_lossy().into_owned());
    }
    if data.starts_with(b"H") {
        let message = oxim_astm::Message::parse(data).ok()?;
        return message.get(path).map(|v| v.to_string_lossy().into_owned());
    }
    None
}

fn shown(value: &[u8], show: bool) -> String {
    if show {
        let text = String::from_utf8_lossy(value);
        if text.chars().count() > 80 {
            format!("{}…", text.chars().take(80).collect::<String>())
        } else {
            text.into_owned()
        }
    } else if value.is_empty() {
        "(empty)".to_owned()
    } else {
        format!("({} bytes)", value.len())
    }
}

/// The differing fields of two messages; empty when they are identical
/// apart from the ignored paths.
pub fn differences(mirth: &[u8], oxim: &[u8], options: &CompareOptions) -> Vec<FieldDifference> {
    let left: BTreeMap<String, Vec<u8>> = fields(mirth, &options.ignore).into_iter().collect();
    let right: BTreeMap<String, Vec<u8>> = fields(oxim, &options.ignore).into_iter().collect();
    let mut labels: Vec<&String> = left.keys().chain(right.keys()).collect();
    labels.sort();
    labels.dedup();
    let empty = Vec::new();
    labels
        .into_iter()
        .filter_map(|label| {
            let a = left.get(label).unwrap_or(&empty);
            let b = right.get(label).unwrap_or(&empty);
            (a != b).then(|| FieldDifference {
                path: label.clone(),
                mirth: shown(a, options.show_values),
                oxim: shown(b, options.show_values),
            })
        })
        .collect()
}

/// Compares the messages Mirth sent to a destination with the ones OXIM
/// produced for it.
pub fn compare(
    destination: &str,
    mirth: &[Vec<u8>],
    oxim: &[(usize, Vec<u8>)],
    options: &CompareOptions,
) -> DestinationReport {
    let mut report = DestinationReport {
        destination: destination.to_owned(),
        oxim_messages: oxim.len(),
        mirth_messages: mirth.len(),
        ..DestinationReport::default()
    };
    let mut pairs: Vec<(usize, Option<usize>, Option<String>)> = Vec::new();
    match &options.key {
        Some(path) => {
            let mut unused: Vec<Option<usize>> = (0..oxim.len()).map(Some).collect();
            for (m, message) in mirth.iter().enumerate() {
                let key = key_of(message, path);
                let found = unused.iter_mut().find(|slot| {
                    slot.is_some_and(|o| key.is_some() && key_of(&oxim[o].1, path) == key)
                });
                match found.and_then(Option::take) {
                    Some(o) => pairs.push((m, Some(o), key)),
                    None => report.missing_in_oxim += 1,
                }
            }
            report.extra_in_oxim = unused.iter().flatten().count();
        }
        None => {
            for m in 0..mirth.len() {
                if m < oxim.len() {
                    pairs.push((m, Some(m), None));
                } else {
                    report.missing_in_oxim += 1;
                }
            }
            report.extra_in_oxim = oxim.len().saturating_sub(mirth.len());
        }
    }
    for (m, o, key) in pairs {
        let Some(o) = o else { continue };
        let fields = differences(&mirth[m], &oxim[o].1, options);
        if fields.is_empty() {
            report.identical += 1;
        } else {
            report.different.push(Difference {
                mirth_index: m,
                inbound_index: Some(oxim[o].0).filter(|i| *i != usize::MAX),
                key,
                fields,
            });
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &[u8] = b"MSH|^~\\&|LAB|H|LIS|H|20260929120000||ORU^R01|1|P|2.5.1\rPID|1||42||DOE^JANE\rOBX|1|NM|GLU||5.4\rOBX|2|NM|K||4.1\r";

    #[test]
    fn finds_field_differences_and_ignores_paths() {
        let b = String::from_utf8_lossy(A)
            .replace("20260929120000", "20260929120501")
            .replace("DOE^JANE", "DOE^JOHN")
            .replace("K||4.1", "K||4.2");
        let options = CompareOptions {
            ignore: vec!["MSH-7".into()],
            ..CompareOptions::default()
        };
        let found = differences(A, b.as_bytes(), &options);
        let paths: Vec<&str> = found.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["OBX[2]-5", "PID-5"]);
        assert_eq!(found[1].mirth, "(8 bytes)");
        let shown = differences(
            A,
            b.as_bytes(),
            &CompareOptions {
                show_values: true,
                ..options.clone()
            },
        );
        assert_eq!(shown[1].mirth, "DOE^JANE");
        assert!(differences(A, A, &options).is_empty());
    }

    #[test]
    fn pairs_by_order_or_by_key() {
        let second = String::from_utf8_lossy(A)
            .replace("|42|", "|43|")
            .into_bytes();
        let mirth = vec![A.to_vec(), second.clone()];
        let oxim = vec![(0, second.clone()), (1, A.to_vec())];
        let by_order = compare("lis", &mirth, &oxim, &CompareOptions::default());
        assert_eq!(by_order.identical, 0);
        assert_eq!(by_order.different.len(), 2);
        let by_key = compare(
            "lis",
            &mirth,
            &oxim,
            &CompareOptions {
                key: Some("PID-3".into()),
                ..CompareOptions::default()
            },
        );
        assert_eq!(by_key.identical, 2);
        let short = compare("lis", &mirth, &oxim[..1], &CompareOptions::default());
        assert_eq!(short.missing_in_oxim, 1);
    }
}
