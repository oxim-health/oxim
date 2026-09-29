use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Returned when text is not a decimal number.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid decimal {0:?}")]
pub struct InvalidDecimal(pub String);

/// A decimal number kept exactly as written.
///
/// Clinical values must not pass through binary floating point: `5.40` and
/// `5.4` carry different precision, and a value such as `0.1` cannot be
/// represented exactly as `f64`. The text is validated but never
/// reformatted.
///
/// Accepted form: optional sign, digits with an optional fraction (at least
/// one digit overall) and an optional exponent, at most 64 characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Decimal(String);

impl Decimal {
    /// Validates and wraps decimal text.
    pub fn new(text: impl Into<String>) -> Result<Self, InvalidDecimal> {
        let text = text.into();
        if is_decimal(text.as_bytes()) {
            Ok(Self(text))
        } else {
            Err(InvalidDecimal(text))
        }
    }

    /// The decimal exactly as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The value as `f64`, for comparisons and statistics only. Precision may
    /// be lost; never write the result back into a clinical message.
    pub fn to_f64(&self) -> f64 {
        self.0.parse().unwrap_or(f64::NAN)
    }

    /// The number of digits after the decimal point, ignoring any exponent.
    pub fn scale(&self) -> usize {
        let mantissa = self.0.split(['e', 'E']).next().unwrap_or_default();
        mantissa
            .split_once('.')
            .map_or(0, |(_, fraction)| fraction.len())
    }
}

fn is_decimal(text: &[u8]) -> bool {
    if text.is_empty() || text.len() > 64 {
        return false;
    }
    let mut rest = text;
    if let [b'+' | b'-', tail @ ..] = rest {
        rest = tail;
    }
    let integer = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    rest = &rest[integer..];
    let mut fraction = 0;
    if let [b'.', tail @ ..] = rest {
        fraction = tail.iter().take_while(|b| b.is_ascii_digit()).count();
        rest = &tail[fraction..];
    }
    if integer + fraction == 0 {
        return false;
    }
    if let [b'e' | b'E', tail @ ..] = rest {
        let tail = match tail {
            [b'+' | b'-', tail @ ..] => tail,
            tail => tail,
        };
        let exponent = tail.iter().take_while(|b| b.is_ascii_digit()).count();
        if exponent == 0 || exponent > 4 {
            return false;
        }
        rest = &tail[exponent..];
    }
    rest.is_empty()
}

impl TryFrom<String> for Decimal {
    type Error = InvalidDecimal;

    fn try_from(value: String) -> Result<Self, InvalidDecimal> {
        Self::new(value)
    }
}

impl From<Decimal> for String {
    fn from(value: Decimal) -> String {
        value.0
    }
}

impl FromStr for Decimal {
    type Err = InvalidDecimal;

    fn from_str(s: &str) -> Result<Self, InvalidDecimal> {
        Self::new(s)
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_clinical_numbers() {
        for text in [
            "5.4", "5.40", "-0.5", "+12", ".5", "5.", "007", "1.2E3", "4e-2", "0",
        ] {
            assert_eq!(Decimal::new(text).unwrap().as_str(), text);
        }
        assert_eq!(Decimal::new("5.40").unwrap().scale(), 2);
        assert_eq!(Decimal::new("1.2E3").unwrap().scale(), 1);
        assert_eq!(Decimal::new("1.2E3").unwrap().to_f64(), 1200.0);
    }

    #[test]
    fn rejects_non_numbers() {
        for text in [
            "", ".", "-", "+.", "5.4.1", "5,4", " 5", "5 ", "<5", "1e", "1e+", "1e12345", "NaN",
            "inf", "0x10",
        ] {
            assert!(Decimal::new(text).is_err(), "{text:?}");
        }
        assert!(Decimal::new("1".repeat(65)).is_err());
    }
}
