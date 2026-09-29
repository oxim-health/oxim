//! Conversions of individual values shared by all mappings.

use oxim_model::{
    ClinicalDateTime, Comparator, Decimal, ObservationStatus, ObservationValue, Priority, Quantity,
    ReferenceRange,
};

/// Trimmed text, or `None` when empty.
pub(crate) fn nonempty(text: impl AsRef<str>) -> Option<String> {
    let text = text.as_ref().trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// A result value from device text: a number (optionally preceded by a
/// comparator such as `<`) becomes a [`Quantity`] with `unit`, anything
/// else stays [`ObservationValue::Text`] exactly as sent. Empty text gives
/// `None`.
pub fn value_from_text(text: &str, unit: Option<&str>) -> Option<ObservationValue> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (comparator, number) = Comparator::split(trimmed);
    match Decimal::new(number.trim()) {
        Ok(value) => Some(ObservationValue::Quantity(Quantity {
            value,
            comparator,
            unit: unit.and_then(nonempty),
            system: None,
            code: None,
        })),
        Err(_) => Some(ObservationValue::Text(text.to_owned())),
    }
}

/// A reference range from text such as `3.9-6.1`, `3.9 to 6.1`, `<5` or
/// `>1`. The text is always kept; limits are filled when they parse.
pub fn reference_range(text: &str) -> Option<ReferenceRange> {
    let text = nonempty(text)?;
    let mut range = ReferenceRange {
        text: Some(text.clone()),
        ..ReferenceRange::default()
    };
    let decimal = |s: &str| Decimal::new(s.trim()).ok();
    if let Some((low, high)) = text.split_once(" to ") {
        range.low = decimal(low);
        range.high = decimal(high);
    } else if let (Some(comparator), rest) = Comparator::split(&text) {
        match comparator {
            Comparator::LessThan | Comparator::LessOrEqual => range.high = decimal(rest),
            Comparator::GreaterThan | Comparator::GreaterOrEqual => range.low = decimal(rest),
        }
    } else {
        // Split at a hyphen that is not a sign: not first, and not after an
        // exponent marker or another hyphen.
        let bytes = text.as_bytes();
        let split = (1..bytes.len())
            .find(|&i| bytes[i] == b'-' && !matches!(bytes[i - 1], b'e' | b'E' | b'-' | b'+'));
        if let Some(at) = split {
            range.low = decimal(&text[..at]);
            range.high = decimal(&text[at + 1..]);
        }
    }
    Some(range)
}

/// The text form of a reference range: its original text, or `low-high`.
pub fn range_text(range: &ReferenceRange) -> String {
    if let Some(text) = &range.text {
        return text.clone();
    }
    match (&range.low, &range.high) {
        (Some(low), Some(high)) => format!("{low}-{high}"),
        (Some(low), None) => format!(">{low}"),
        (None, Some(high)) => format!("<{high}"),
        (None, None) => String::new(),
    }
}

/// A date/time in HL7 `DTM` or ASTM form (`YYYYMMDDHHMMSS`), falling back
/// to ISO 8601. Returns `None` when the text is empty or unreadable.
pub fn datetime(text: &str) -> Option<ClinicalDateTime> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    ClinicalDateTime::parse_hl7(text)
        .or_else(|_| ClinicalDateTime::parse_iso(text))
        .ok()
}

/// HL7 v2 `DTM` text, keeping the recorded precision but at most the four
/// fractional digits HL7 allows.
pub fn hl7_datetime(value: &ClinicalDateTime) -> String {
    let text = value.to_hl7();
    let Some(dot) = text.find('.') else {
        return text;
    };
    let fraction_end = text[dot + 1..]
        .find(['+', '-'])
        .map_or(text.len(), |i| dot + 1 + i);
    let digits = &text[dot + 1..fraction_end];
    if digits.len() <= 4 {
        return text;
    }
    format!("{}.{}{}", &text[..dot], &digits[..4], &text[fraction_end..])
}

/// ASTM date/time text: the digits of the HL7 form up to the second,
/// without fraction or offset, as ASTM E1394 expects.
pub fn astm_datetime(value: &ClinicalDateTime) -> String {
    value
        .to_hl7()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect()
}

/// Result status from an HL7 v2 table 0085 code (OBX-11).
///
/// | Code | Status |
/// |---|---|
/// | `F`, `U` | final |
/// | `C` | corrected |
/// | `A` | amended |
/// | `P`, `R`, `S` | preliminary |
/// | `X` | cancelled |
/// | `I`, `O` | registered |
/// | `D`, `W` | entered-in-error |
/// | other or empty | unknown |
pub fn status_from_hl7(code: &str) -> ObservationStatus {
    match code.trim() {
        "F" | "U" => ObservationStatus::Final,
        "C" => ObservationStatus::Corrected,
        "A" => ObservationStatus::Amended,
        "P" | "R" | "S" => ObservationStatus::Preliminary,
        "X" => ObservationStatus::Cancelled,
        "I" | "O" => ObservationStatus::Registered,
        "D" | "W" => ObservationStatus::EnteredInError,
        _ => ObservationStatus::Unknown,
    }
}

/// The HL7 v2 table 0085 code for a status; `unknown` writes `fallback`.
pub fn status_to_hl7(status: ObservationStatus, fallback: &str) -> String {
    match status {
        ObservationStatus::Final => "F",
        ObservationStatus::Corrected | ObservationStatus::Amended => "C",
        ObservationStatus::Preliminary => "P",
        ObservationStatus::Cancelled => "X",
        ObservationStatus::Registered => "I",
        ObservationStatus::EnteredInError => "W",
        ObservationStatus::Unknown => fallback,
    }
    .to_owned()
}

/// Result status from an ASTM E1394 R-9 code.
///
/// | Code | Status |
/// |---|---|
/// | `F`, `R` (previously transmitted), `V` (operator verified), `M`, `Q` | final |
/// | `C` | corrected |
/// | `P`, `S` (partial), `W` (validity questionable) | preliminary |
/// | `X` (cannot be done) | cancelled |
/// | `I` (pending in instrument) | registered |
/// | other or empty | unknown |
pub fn status_from_astm(code: &str) -> ObservationStatus {
    match code.trim() {
        "F" | "R" | "V" | "M" | "Q" => ObservationStatus::Final,
        "C" => ObservationStatus::Corrected,
        "P" | "S" | "W" => ObservationStatus::Preliminary,
        "X" => ObservationStatus::Cancelled,
        "I" => ObservationStatus::Registered,
        _ => ObservationStatus::Unknown,
    }
}

/// Priority from an ASTM O-6 or HL7 priority code (`S` stat, `A` ASAP,
/// `R` routine, `U` urgent).
pub fn priority_from_code(code: &str) -> Option<Priority> {
    match code.trim() {
        "S" => Some(Priority::Stat),
        "A" => Some(Priority::Asap),
        "R" => Some(Priority::Routine),
        "U" => Some(Priority::Urgent),
        _ => None,
    }
}

/// The ASTM and HL7 code for a priority.
pub fn priority_code(priority: Priority) -> &'static str {
    match priority {
        Priority::Stat => "S",
        Priority::Asap => "A",
        Priority::Routine => "R",
        Priority::Urgent => "U",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_values() {
        let Some(ObservationValue::Quantity(q)) = value_from_text(" 5.40 ", Some("mmol/L")) else {
            panic!("not a quantity");
        };
        assert_eq!(
            (q.value.as_str(), q.unit.as_deref()),
            ("5.40", Some("mmol/L"))
        );
        let Some(ObservationValue::Quantity(q)) = value_from_text("<0.5", None) else {
            panic!("not a quantity");
        };
        assert_eq!(
            (q.comparator, q.value.as_str()),
            (Some(Comparator::LessThan), "0.5")
        );
        assert_eq!(
            value_from_text("Positive (+)", None),
            Some(ObservationValue::Text("Positive (+)".into()))
        );
        assert_eq!(value_from_text("  ", None), None);
    }

    #[test]
    fn parses_reference_ranges() {
        for (text, low, high) in [
            ("3.9-6.1", Some("3.9"), Some("6.1")),
            ("3.9 to 6.1", Some("3.9"), Some("6.1")),
            ("-1-5", Some("-1"), Some("5")),
            ("1e-3-2e-2", Some("1e-3"), Some("2e-2")),
            ("<5", None, Some("5")),
            (">=1", Some("1"), None),
            ("negative", None, None),
        ] {
            let range = reference_range(text).unwrap();
            assert_eq!(range.low.as_ref().map(Decimal::as_str), low, "{text}");
            assert_eq!(range.high.as_ref().map(Decimal::as_str), high, "{text}");
            assert_eq!(range_text(&range), text);
        }
        assert_eq!(reference_range(""), None);
    }

    #[test]
    fn formats_times_for_each_protocol() {
        let value = ClinicalDateTime::parse_iso("2026-09-29T14:30:05.123456+03:00").unwrap();
        assert_eq!(hl7_datetime(&value), "20260929143005.1234+0300");
        assert_eq!(astm_datetime(&value), "20260929143005");
        let date = datetime("19800101").unwrap();
        assert_eq!(hl7_datetime(&date), "19800101");
        assert!(datetime("yesterday").is_none());
        assert_eq!(
            datetime("2026-09-29T14:30").unwrap().to_hl7(),
            "202609291430"
        );
    }

    #[test]
    fn maps_statuses() {
        assert_eq!(status_from_hl7("F"), ObservationStatus::Final);
        assert_eq!(status_to_hl7(ObservationStatus::Unknown, "F"), "F");
        assert_eq!(status_from_astm("X"), ObservationStatus::Cancelled);
        for code in ["F", "C", "P", "X", "I", "W"] {
            assert_eq!(status_to_hl7(status_from_hl7(code), "F"), code);
        }
    }
}
