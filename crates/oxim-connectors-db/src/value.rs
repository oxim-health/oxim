//! Database values in a neutral form, and their JSON and text renderings.
//!
//! Every database driver converts its column values to [`Cell`]s. Rows
//! become JSON objects (column name to value, in column order); exact
//! numerics stay text so no digit is lost (ADR 0011), dates and times are
//! ISO 8601, and binary values are Base64.

use base64::Engine as _;
use serde_json::{Map, Value};

/// One column value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Cell {
    /// SQL `NULL`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A signed integer.
    Int(i64),
    /// An unsigned integer.
    UInt(u64),
    /// A binary floating-point number.
    Float(f64),
    /// An exact numeric, as its decimal text.
    Decimal(String),
    /// Text.
    Text(String),
    /// Binary data.
    Bytes(Vec<u8>),
    /// A date, `YYYY-MM-DD`.
    Date(String),
    /// A time of day, `HH:MM:SS[.fraction]`.
    Time(String),
    /// A date and time, `YYYY-MM-DDTHH:MM:SS[.fraction][offset]`.
    DateTime(String),
    /// A JSON document stored in the database.
    Json(Value),
}

impl Cell {
    /// The JSON rendering.
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(*value),
            Self::Int(value) => Value::from(*value),
            Self::UInt(value) => Value::from(*value),
            Self::Float(value) => serde_json::Number::from_f64(*value)
                .map_or_else(|| Value::String(value.to_string()), Value::Number),
            Self::Decimal(text)
            | Self::Text(text)
            | Self::Date(text)
            | Self::Time(text)
            | Self::DateTime(text) => Value::String(text.clone()),
            Self::Bytes(bytes) => Value::String(base64(bytes)),
            Self::Json(value) => value.clone(),
        }
    }

    /// The text rendering, used to bind the value again (for example in a
    /// `post_query`); `None` for `NULL`.
    pub(crate) fn to_text(&self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Bool(value) => Some(value.to_string()),
            Self::Int(value) => Some(value.to_string()),
            Self::UInt(value) => Some(value.to_string()),
            Self::Float(value) => Some(value.to_string()),
            Self::Decimal(text)
            | Self::Text(text)
            | Self::Date(text)
            | Self::Time(text)
            | Self::DateTime(text) => Some(text.clone()),
            Self::Bytes(bytes) => Some(base64(bytes)),
            Self::Json(value) => Some(value.to_string()),
        }
    }

    /// The raw content, used as a message payload: binary values as they
    /// are, everything else as text.
    pub(crate) fn to_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::Bytes(bytes) => Some(bytes.clone()),
            other => other.to_text().map(String::into_bytes),
        }
    }
}

/// One result row.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Row {
    /// Column names and values, in column order.
    pub(crate) columns: Vec<(String, Cell)>,
}

impl Row {
    /// The value of a column; the exact name first, then ignoring case.
    pub(crate) fn get(&self, name: &str) -> Option<&Cell> {
        self.columns
            .iter()
            .find(|(column, _)| column == name)
            .or_else(|| {
                self.columns
                    .iter()
                    .find(|(column, _)| column.eq_ignore_ascii_case(name))
            })
            .map(|(_, cell)| cell)
    }

    /// The row as a JSON object.
    pub(crate) fn to_json(&self) -> Value {
        let mut object = Map::new();
        for (name, cell) in &self.columns {
            object.insert(name.clone(), cell.to_json());
        }
        Value::Object(object)
    }
}

/// A statement parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Param {
    /// SQL `NULL`.
    Null,
    /// Text, converted by the database to the parameter's type.
    Text(String),
    /// Binary data.
    Bytes(Vec<u8>),
}

impl Param {
    /// A text parameter, or `NULL` for no value.
    pub(crate) fn text(value: Option<String>) -> Self {
        value.map_or(Self::Null, Self::Text)
    }
}

/// Standard Base64.
pub(crate) fn base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The civil date of a day count relative to 1970-01-01.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// `YYYY-MM-DD` for a day count relative to 1970-01-01.
pub(crate) fn date_text(days_since_unix_epoch: i64) -> String {
    let (y, m, d) = civil_from_days(days_since_unix_epoch);
    if (0..=9999).contains(&y) {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!("{y:+05}-{m:02}-{d:02}")
    }
}

/// `HH:MM:SS[.fraction]` for a time of day in units of `10^-scale`
/// seconds; the fraction keeps its significant digits only.
pub(crate) fn time_text(ticks: u64, scale: u32) -> String {
    let per_second = 10u64.pow(scale);
    let seconds = ticks / per_second;
    let fraction = ticks % per_second;
    let (h, m, s) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    let mut text = format!("{h:02}:{m:02}:{s:02}");
    if fraction > 0 {
        let digits = format!("{fraction:0width$}", width = scale as usize);
        text.push('.');
        text.push_str(digits.trim_end_matches('0'));
    }
    text
}

/// `+HH:MM` or `-HH:MM` for an offset in minutes; `Z` for zero.
pub(crate) fn offset_text(minutes: i32) -> String {
    if minutes == 0 {
        return "Z".into();
    }
    let sign = if minutes < 0 { '-' } else { '+' };
    let minutes = minutes.unsigned_abs();
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The decimal text of `value / 10^scale`.
pub(crate) fn scaled_decimal(value: i128, scale: u32) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let scale = scale as usize;
    let text = if scale == 0 {
        digits
    } else if digits.len() > scale {
        let (int, frac) = digits.split_at(digits.len() - scale);
        format!("{int}.{frac}")
    } else {
        format!("0.{}{digits}", "0".repeat(scale - digits.len()))
    };
    if negative { format!("-{text}") } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_rows_as_json() {
        let row = Row {
            columns: vec![
                ("id".into(), Cell::Int(7)),
                ("glucose".into(), Cell::Decimal("5.40".into())),
                ("flag".into(), Cell::Bool(true)),
                ("note".into(), Cell::Null),
                ("raw".into(), Cell::Bytes(vec![1, 2, 255])),
                (
                    "taken".into(),
                    Cell::DateTime("2026-09-29T12:00:00Z".into()),
                ),
                ("ratio".into(), Cell::Float(0.5)),
                ("nan".into(), Cell::Float(f64::NAN)),
                ("doc".into(), Cell::Json(serde_json::json!({"a": 1}))),
            ],
        };
        let json = row.to_json();
        assert_eq!(
            serde_json::to_string(&json).unwrap(),
            r#"{"id":7,"glucose":"5.40","flag":true,"note":null,"raw":"AQL/","taken":"2026-09-29T12:00:00Z","ratio":0.5,"nan":"NaN","doc":{"a":1}}"#
        );
        assert_eq!(row.get("ID"), Some(&Cell::Int(7)));
        assert_eq!(row.get("missing"), None);
        assert_eq!(
            Cell::Decimal("5.40".into()).to_text().as_deref(),
            Some("5.40")
        );
        assert_eq!(Cell::Null.to_text(), None);
        assert_eq!(Cell::Bytes(vec![0, 1]).to_bytes(), Some(vec![0, 1]));
    }

    #[test]
    fn formats_dates_times_and_decimals() {
        assert_eq!(date_text(0), "1970-01-01");
        assert_eq!(date_text(20_725), "2026-09-29");
        assert_eq!(date_text(-719_162), "0001-01-01");
        assert_eq!(time_text(45_296_500_000, 6), "12:34:56.5");
        assert_eq!(time_text(45_296, 0), "12:34:56");
        assert_eq!(time_text(452_961_234_567, 7), "12:34:56.1234567");
        assert_eq!(offset_text(180), "+03:00");
        assert_eq!(offset_text(-330), "-05:30");
        assert_eq!(offset_text(0), "Z");
        assert_eq!(scaled_decimal(540, 2), "5.40");
        assert_eq!(scaled_decimal(-5, 3), "-0.005");
        assert_eq!(scaled_decimal(12, 0), "12");
        assert_eq!(scaled_decimal(0, 2), "0.00");
    }
}
