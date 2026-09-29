use std::fmt;

use oxim_model::Decimal;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{FhirError, FhirResult};

/// A FHIR `decimal`, written as a JSON number that keeps its exact text.
///
/// FHIR requires decimal precision to be preserved (`5.40` is not `5.4`).
/// This type stores the number's text and writes it back unchanged, relying
/// on serde_json's `arbitrary_precision` feature, which this crate enables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FhirDecimal(String);

impl FhirDecimal {
    /// A decimal from JSON number text, for example `5.40` or `-1.2e3`.
    pub fn parse(text: &str) -> FhirResult<Self> {
        let number: serde_json::Number = serde_json::from_str(text.trim())
            .map_err(|_| FhirError::Invalid(format!("{text:?} is not a FHIR decimal")))?;
        Ok(Self(number.to_string()))
    }

    /// The JSON form of a normalized decimal. Signs, leading zeros and
    /// trailing points that JSON does not allow are rewritten (`+12` →
    /// `12`, `.5` → `0.5`, `5.` → `5`, `007` → `7`); the digits after the
    /// decimal point, and so the precision, are kept.
    pub fn from_decimal(decimal: &Decimal) -> FhirResult<Self> {
        Self::parse(&json_number_text(decimal.as_str()))
    }

    /// The decimal as a normalized [`Decimal`].
    pub fn to_decimal(&self) -> FhirResult<Decimal> {
        Decimal::new(self.0.clone()).map_err(|e| FhirError::Invalid(e.to_string()))
    }

    /// The exact text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FhirDecimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Rewrites decimal text into the JSON number grammar without changing
/// the digits after the decimal point.
fn json_number_text(text: &str) -> String {
    let (sign, rest) = match text.as_bytes().first() {
        Some(b'-') => ("-", &text[1..]),
        Some(b'+') => ("", &text[1..]),
        _ => ("", text),
    };
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, ""),
    };
    let (integer, fraction) = match mantissa.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (mantissa, None),
    };
    let integer = integer.trim_start_matches('0');
    let integer = if integer.is_empty() { "0" } else { integer };
    let mut out = format!("{sign}{integer}");
    if let Some(fraction) = fraction.filter(|f| !f.is_empty()) {
        out.push('.');
        out.push_str(fraction);
    }
    if !exponent.is_empty() {
        let (marker, digits) = exponent.split_at(1);
        let (exp_sign, digits) = match digits.as_bytes().first() {
            Some(b'+' | b'-') => digits.split_at(1),
            _ => ("", digits),
        };
        let digits = digits.trim_start_matches('0');
        out.push_str(marker);
        out.push_str(exp_sign);
        out.push_str(if digits.is_empty() { "0" } else { digits });
    }
    out
}

impl Serialize for FhirDecimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let number: serde_json::Number =
            serde_json::from_str(&self.0).map_err(serde::ser::Error::custom)?;
        number.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FhirDecimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let number = serde_json::Number::deserialize(deserializer)?;
        Ok(Self(number.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_precision_through_json() {
        for text in ["5.40", "0.0", "-12.300", "100", "0.000710"] {
            let decimal = FhirDecimal::parse(text).unwrap();
            let json = serde_json::to_string(&decimal).unwrap();
            assert_eq!(json, text);
            let back: FhirDecimal = serde_json::from_str(&json).unwrap();
            assert_eq!(back.as_str(), text);
        }
        // serde_json writes exponents in one form; the digits are kept.
        for (text, written) in [("1.20e3", "1.20e+3"), ("7.1E-5", "7.1e-5")] {
            let decimal = FhirDecimal::parse(text).unwrap();
            assert_eq!(decimal.as_str(), written);
            assert_eq!(serde_json::to_string(&decimal).unwrap(), written);
        }
    }

    #[test]
    fn rewrites_non_json_decimals_without_losing_scale() {
        for (model, json) in [
            ("+12", "12"),
            (".5", "0.5"),
            ("5.", "5"),
            ("007", "7"),
            ("-0.50", "-0.50"),
            ("1.20e+03", "1.20e+3"),
        ] {
            let decimal = FhirDecimal::from_decimal(&Decimal::new(model).unwrap()).unwrap();
            assert_eq!(decimal.as_str(), json, "{model}");
        }
    }

    #[test]
    fn rejects_non_numbers() {
        assert!(FhirDecimal::parse("abc").is_err());
        assert!(serde_json::from_str::<FhirDecimal>("\"5.4\"").is_err());
    }
}
