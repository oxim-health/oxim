use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const SECONDS_PER_DAY: i64 = 86_400;

/// Returned when text is not a valid date or time.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid date/time {0:?}")]
pub struct InvalidDateTime(pub String);

/// An instant in UTC with nanosecond precision (years 1678 to 2261).
///
/// Used for system events such as the time a message was received. Clinical
/// times with partial precision use [`ClinicalDateTime`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// A timestamp from nanoseconds since the Unix epoch.
    pub const fn from_unix_nanos(nanos: i64) -> Self {
        Self(nanos)
    }

    /// A timestamp from milliseconds since the Unix epoch, if in range.
    pub fn from_unix_millis(millis: i64) -> Option<Self> {
        millis.checked_mul(1_000_000).map(Self)
    }

    /// Converts a [`SystemTime`] supplied by the caller, if in range.
    pub fn from_system_time(time: SystemTime) -> Option<Self> {
        match time.duration_since(UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_nanos()).ok().map(Self),
            Err(before) => i64::try_from(before.duration().as_nanos())
                .ok()
                .map(|n| Self(-n)),
        }
    }

    /// Nanoseconds since the Unix epoch.
    pub const fn unix_nanos(self) -> i64 {
        self.0
    }

    /// Milliseconds since the Unix epoch, rounded down.
    pub const fn unix_millis(self) -> i64 {
        self.0.div_euclid(1_000_000)
    }

    fn from_civil(
        (year, month, day): (i64, u32, u32),
        (hour, minute, second): (u32, u32, u32),
        nanos: u32,
        offset_minutes: i64,
    ) -> Option<Self> {
        let days = days_from_civil(year, month, day);
        let seconds = days
            .checked_mul(SECONDS_PER_DAY)?
            .checked_add(i64::from(hour * 3600 + minute * 60 + second))?
            .checked_sub(offset_minutes * 60)?;
        seconds
            .checked_mul(NANOS_PER_SECOND)?
            .checked_add(i64::from(nanos))
            .map(Self)
    }

    fn to_civil(self) -> ((i64, u32, u32), (u32, u32, u32), u32) {
        let seconds = self.0.div_euclid(NANOS_PER_SECOND);
        let nanos = self.0.rem_euclid(NANOS_PER_SECOND) as u32;
        let days = seconds.div_euclid(SECONDS_PER_DAY);
        let of_day = seconds.rem_euclid(SECONDS_PER_DAY) as u32;
        (
            civil_from_days(days),
            (of_day / 3600, of_day / 60 % 60, of_day % 60),
            nanos,
        )
    }
}

impl fmt::Display for Timestamp {
    /// RFC 3339 in UTC, for example `2026-09-29T09:00:00.25Z`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ((year, month, day), (hour, minute, second), nanos) = self.to_civil();
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
        )?;
        write_fraction(f, nanos, 9)?;
        f.write_str("Z")
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({self})")
    }
}

impl FromStr for Timestamp {
    type Err = InvalidDateTime;

    /// Parses RFC 3339 (`2026-09-29T12:00:00.5+03:00`).
    fn from_str(s: &str) -> Result<Self, InvalidDateTime> {
        let parsed = ClinicalDateTime::parse_iso(s)?;
        if parsed.second.is_none() || parsed.offset_minutes.is_none() {
            return Err(InvalidDateTime(s.to_owned()));
        }
        parsed
            .to_timestamp(0)
            .ok_or_else(|| InvalidDateTime(s.to_owned()))
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// How precisely a [`ClinicalDateTime`] is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Precision {
    /// Only the year.
    Year,
    /// Year and month.
    Month,
    /// A calendar date.
    Day,
    /// Date and hour.
    Hour,
    /// Date, hour and minute.
    Minute,
    /// Date and time to the second.
    Second,
    /// Date and time with a decimal fraction of a second.
    Fraction,
}

/// A clinical date or date-time with the precision it was recorded with.
///
/// Clinical systems routinely exchange partial values such as a birth year
/// or a date without a time, with or without a UTC offset. This type keeps
/// exactly what was provided so that no precision is invented or lost. It
/// reads and writes the HL7 v2 `DTM` form (`20260929143005.25+0300`, also
/// used by ASTM) and ISO 8601 / FHIR `dateTime` (`2026-09-29T14:30:05.25+03:00`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClinicalDateTime {
    year: u16,
    month: Option<u8>,
    day: Option<u8>,
    hour: Option<u8>,
    minute: Option<u8>,
    second: Option<u8>,
    nanos: u32,
    fraction_digits: u8,
    offset_minutes: Option<i16>,
}

impl ClinicalDateTime {
    /// A calendar date.
    pub fn date(year: u16, month: u8, day: u8) -> Result<Self, InvalidDateTime> {
        let value = Self {
            year,
            month: Some(month),
            day: Some(day),
            ..Self::year_only(year)
        };
        value.validate()
    }

    /// A date and time to the second, with an optional UTC offset in minutes.
    pub fn date_time(
        (year, month, day): (u16, u8, u8),
        (hour, minute, second): (u8, u8, u8),
        offset_minutes: Option<i16>,
    ) -> Result<Self, InvalidDateTime> {
        Self {
            year,
            month: Some(month),
            day: Some(day),
            hour: Some(hour),
            minute: Some(minute),
            second: Some(second),
            nanos: 0,
            fraction_digits: 0,
            offset_minutes,
        }
        .validate()
    }

    /// The civil date and time of `timestamp` at the given UTC offset, to
    /// the nanosecond.
    pub fn from_timestamp(timestamp: Timestamp, offset_minutes: i16) -> Option<Self> {
        let shifted = timestamp
            .0
            .checked_add(i64::from(offset_minutes) * 60 * NANOS_PER_SECOND)?;
        let ((year, month, day), (hour, minute, second), nanos) = Timestamp(shifted).to_civil();
        Self {
            year: u16::try_from(year).ok()?,
            month: Some(month as u8),
            day: Some(day as u8),
            hour: Some(hour as u8),
            minute: Some(minute as u8),
            second: Some(second as u8),
            nanos,
            fraction_digits: if nanos == 0 { 0 } else { 9 },
            offset_minutes: Some(offset_minutes),
        }
        .validate()
        .ok()
    }

    fn year_only(year: u16) -> Self {
        Self {
            year,
            month: None,
            day: None,
            hour: None,
            minute: None,
            second: None,
            nanos: 0,
            fraction_digits: 0,
            offset_minutes: None,
        }
    }

    /// The year.
    pub fn year(&self) -> u16 {
        self.year
    }

    /// The month (1–12), if known.
    pub fn month(&self) -> Option<u8> {
        self.month
    }

    /// The day of the month, if known.
    pub fn day(&self) -> Option<u8> {
        self.day
    }

    /// The hour (0–23), if known.
    pub fn hour(&self) -> Option<u8> {
        self.hour
    }

    /// The minute, if known.
    pub fn minute(&self) -> Option<u8> {
        self.minute
    }

    /// The second, if known.
    pub fn second(&self) -> Option<u8> {
        self.second
    }

    /// The fraction of a second in nanoseconds (0 when not recorded).
    pub fn nanos(&self) -> u32 {
        self.nanos
    }

    /// The UTC offset in minutes, if recorded.
    pub fn offset_minutes(&self) -> Option<i16> {
        self.offset_minutes
    }

    /// How precisely the value is known.
    pub fn precision(&self) -> Precision {
        match (
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
            self.fraction_digits,
        ) {
            (None, ..) => Precision::Year,
            (_, None, ..) => Precision::Month,
            (_, _, None, ..) => Precision::Day,
            (_, _, _, None, ..) => Precision::Hour,
            (_, _, _, _, None, _) => Precision::Minute,
            (.., 0) => Precision::Second,
            _ => Precision::Fraction,
        }
    }

    /// Converts to an instant. Missing time components count as zero and a
    /// missing offset is replaced by `assumed_offset_minutes`. Returns `None`
    /// when the date itself is incomplete or out of range.
    pub fn to_timestamp(&self, assumed_offset_minutes: i16) -> Option<Timestamp> {
        let (month, day) = (self.month?, self.day?);
        Timestamp::from_civil(
            (i64::from(self.year), u32::from(month), u32::from(day)),
            (
                u32::from(self.hour.unwrap_or(0)),
                u32::from(self.minute.unwrap_or(0)),
                u32::from(self.second.unwrap_or(0)),
            ),
            self.nanos,
            i64::from(self.offset_minutes.unwrap_or(assumed_offset_minutes)),
        )
    }

    /// Parses the HL7 v2 `DTM` form `YYYY[MM[DD[HH[MM[SS[.S...]]]]]][+/-ZZZZ]`,
    /// which ASTM uses as well. Up to nine fractional digits are accepted.
    pub fn parse_hl7(text: &str) -> Result<Self, InvalidDateTime> {
        let invalid = || InvalidDateTime(text.to_owned());
        let (body, offset) = match text.find(['+', '-']) {
            Some(at) => (&text[..at], Some(&text[at..])),
            None => (text, None),
        };
        let (digits, fraction) = match body.split_once('.') {
            Some((digits, fraction)) => (digits, Some(fraction)),
            None => (body, None),
        };
        if !digits.bytes().all(|b| b.is_ascii_digit())
            || ![4, 6, 8, 10, 12, 14].contains(&digits.len())
        {
            return Err(invalid());
        }
        let number =
            |range: std::ops::Range<usize>| -> Option<u8> { digits.get(range)?.parse().ok() };
        let mut value = Self::year_only(digits[..4].parse().map_err(|_| invalid())?);
        value.month = number(4..6);
        value.day = number(6..8);
        value.hour = number(8..10);
        value.minute = number(10..12);
        value.second = number(12..14);
        if let Some(fraction) = fraction {
            if value.second.is_none() {
                return Err(invalid());
            }
            (value.nanos, value.fraction_digits) = parse_fraction(fraction).ok_or_else(invalid)?;
        }
        if let Some(offset) = offset {
            value.offset_minutes = Some(parse_offset(offset, false).ok_or_else(invalid)?);
        }
        value.validate().map_err(|_| invalid())
    }

    /// Formats as HL7 v2 `DTM`, keeping the recorded precision.
    pub fn to_hl7(&self) -> String {
        let mut out = format!("{:04}", self.year);
        for part in [self.month, self.day, self.hour, self.minute, self.second]
            .into_iter()
            .map_while(|part| part)
        {
            out.push_str(&format!("{part:02}"));
        }
        if self.fraction_digits > 0 {
            out.push('.');
            out.push_str(&fraction_text(self.nanos, self.fraction_digits));
        }
        if let Some(offset) = self.offset_minutes {
            out.push_str(&offset_text(offset, false));
        }
        out
    }

    /// Parses ISO 8601 / FHIR: `YYYY`, `YYYY-MM`, `YYYY-MM-DD`,
    /// `YYYY-MM-DDThh:mm[:ss[.f...]]` with an optional `Z` or `+hh:mm` offset.
    pub fn parse_iso(text: &str) -> Result<Self, InvalidDateTime> {
        let invalid = || InvalidDateTime(text.to_owned());
        let (date, time) = match text.split_once(['T', 't']) {
            Some((date, time)) => (date, Some(time)),
            None => (text, None),
        };
        let mut parts = date.split('-');
        let year = parts.next().filter(|y| y.len() == 4).ok_or_else(invalid)?;
        let mut value = Self::year_only(parse_digits(year).ok_or_else(invalid)?);
        value.month = parts
            .next()
            .map(|m| two_digits(m).ok_or_else(invalid))
            .transpose()?;
        value.day = parts
            .next()
            .map(|d| two_digits(d).ok_or_else(invalid))
            .transpose()?;
        if parts.next().is_some() {
            return Err(invalid());
        }
        if let Some(time) = time {
            if value.day.is_none() {
                return Err(invalid());
            }
            let (clock, offset) = match time.find(['Z', 'z', '+', '-']) {
                Some(at) => (&time[..at], Some(&time[at..])),
                None => (time, None),
            };
            let (clock, fraction) = match clock.split_once('.') {
                Some((clock, fraction)) => (clock, Some(fraction)),
                None => (clock, None),
            };
            let mut fields = clock.split(':');
            value.hour = Some(fields.next().and_then(two_digits).ok_or_else(invalid)?);
            value.minute = Some(fields.next().and_then(two_digits).ok_or_else(invalid)?);
            value.second = fields
                .next()
                .map(|s| two_digits(s).ok_or_else(invalid))
                .transpose()?;
            if fields.next().is_some() {
                return Err(invalid());
            }
            if let Some(fraction) = fraction {
                if value.second.is_none() {
                    return Err(invalid());
                }
                (value.nanos, value.fraction_digits) =
                    parse_fraction(fraction).ok_or_else(invalid)?;
            }
            if let Some(offset) = offset {
                value.offset_minutes = Some(parse_offset(offset, true).ok_or_else(invalid)?);
            }
        }
        value.validate().map_err(|_| invalid())
    }

    /// Formats as ISO 8601 / FHIR, keeping the recorded precision. A zero
    /// offset is written as `Z`.
    pub fn to_iso(&self) -> String {
        let mut out = format!("{:04}", self.year);
        if let Some(month) = self.month {
            out.push_str(&format!("-{month:02}"));
        }
        if let Some(day) = self.day {
            out.push_str(&format!("-{day:02}"));
        }
        if let Some(hour) = self.hour {
            out.push_str(&format!("T{hour:02}:{:02}", self.minute.unwrap_or(0)));
            if let Some(second) = self.second {
                out.push_str(&format!(":{second:02}"));
            }
            if self.fraction_digits > 0 {
                out.push('.');
                out.push_str(&fraction_text(self.nanos, self.fraction_digits));
            }
            match self.offset_minutes {
                Some(0) => out.push('Z'),
                Some(offset) => out.push_str(&offset_text(offset, true)),
                None => {}
            }
        }
        out
    }

    fn validate(self) -> Result<Self, InvalidDateTime> {
        let invalid = || InvalidDateTime(format!("{self:?}"));
        let ordered = !(self.month.is_none() && self.day.is_some()
            || self.day.is_none() && self.hour.is_some()
            || self.hour.is_none() && self.minute.is_some()
            || self.minute.is_none() && self.second.is_some()
            || self.second.is_none() && self.fraction_digits > 0);
        let month_ok = self.month.is_none_or(|m| (1..=12).contains(&m));
        let day_ok = match (self.month, self.day) {
            (Some(m), Some(d)) => d >= 1 && d <= days_in_month(self.year, m),
            _ => true,
        };
        let time_ok = self.hour.is_none_or(|h| h < 24)
            && self.minute.is_none_or(|m| m < 60)
            && self.second.is_none_or(|s| s < 60)
            && self.nanos < 1_000_000_000
            && self.fraction_digits <= 9;
        let offset_ok = self
            .offset_minutes
            .is_none_or(|o| (-12 * 60..=14 * 60).contains(&o));
        if ordered && month_ok && day_ok && time_ok && offset_ok {
            Ok(self)
        } else {
            Err(invalid())
        }
    }
}

impl fmt::Display for ClinicalDateTime {
    /// Writes the ISO 8601 form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_iso())
    }
}

impl FromStr for ClinicalDateTime {
    type Err = InvalidDateTime;

    /// Parses the ISO 8601 form.
    fn from_str(s: &str) -> Result<Self, InvalidDateTime> {
        Self::parse_iso(s)
    }
}

impl Serialize for ClinicalDateTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ClinicalDateTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

fn parse_digits<T: FromStr>(text: &str) -> Option<T> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn two_digits(text: &str) -> Option<u8> {
    (text.len() == 2).then(|| parse_digits(text)).flatten()
}

/// Parses 1–9 fractional digits into nanoseconds and the digit count.
fn parse_fraction(text: &str) -> Option<(u32, u8)> {
    if !(1..=9).contains(&text.len()) {
        return None;
    }
    let value: u32 = parse_digits(text)?;
    let digits = text.len() as u32;
    Some((value * 10u32.pow(9 - digits), digits as u8))
}

/// Parses `+HHMM` (HL7) or `+HH:MM` / `Z` (ISO) into minutes.
fn parse_offset(text: &str, iso: bool) -> Option<i16> {
    if iso && (text == "Z" || text == "z") {
        return Some(0);
    }
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let (hours, minutes) = if iso {
        rest.split_once(':')?
    } else if rest.len() == 4 {
        rest.split_at(2)
    } else {
        return None;
    };
    let hours = i16::from(two_digits(hours)?);
    let minutes = i16::from(two_digits(minutes)?);
    if minutes >= 60 {
        return None;
    }
    Some(sign * (hours * 60 + minutes))
}

fn offset_text(offset: i16, iso: bool) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let (hours, minutes) = (offset.unsigned_abs() / 60, offset.unsigned_abs() % 60);
    if iso {
        format!("{sign}{hours:02}:{minutes:02}")
    } else {
        format!("{sign}{hours:02}{minutes:02}")
    }
}

fn fraction_text(nanos: u32, digits: u8) -> String {
    let full = format!("{nanos:09}");
    full[..usize::from(digits.min(9))].to_owned()
}

/// Writes a fraction of a second without trailing zeros, if non-zero.
fn write_fraction(f: &mut fmt::Formatter<'_>, nanos: u32, digits: u8) -> fmt::Result {
    if nanos == 0 {
        return Ok(());
    }
    let text = fraction_text(nanos, digits);
    write!(f, ".{}", text.trim_end_matches('0'))
}

fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_index = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard
/// Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversion_round_trips() {
        for days in (-800_000..800_000).step_by(997) {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
    }

    #[test]
    fn timestamps_format_as_rfc3339() {
        let ts = Timestamp::from_unix_millis(1_790_067_600_250).unwrap();
        assert_eq!(ts.to_string(), "2026-09-22T09:00:00.25Z");
        assert_eq!(
            "2026-09-22T12:00:00.25+03:00".parse::<Timestamp>().unwrap(),
            ts
        );
        assert_eq!(
            Timestamp::from_unix_nanos(0).to_string(),
            "1970-01-01T00:00:00Z"
        );
        assert_eq!(
            Timestamp::from_unix_nanos(-1).to_string(),
            "1969-12-31T23:59:59.999999999Z"
        );
        assert!("2026-09-22T12:00".parse::<Timestamp>().is_err());
        assert!("2026-09-22T12:00:00".parse::<Timestamp>().is_err());
    }

    #[test]
    fn parses_and_formats_hl7_datetimes() {
        for text in [
            "2026",
            "202609",
            "20260929",
            "2026092914",
            "202609291430",
            "20260929143005",
            "20260929143005.25+0300",
            "20260929143005-0500",
            "19800101",
        ] {
            let value = ClinicalDateTime::parse_hl7(text).unwrap();
            assert_eq!(value.to_hl7(), text);
        }
        let value = ClinicalDateTime::parse_hl7("20260929143005.2500+0300").unwrap();
        assert_eq!(value.precision(), Precision::Fraction);
        assert_eq!(value.nanos(), 250_000_000);
        assert_eq!(value.to_iso(), "2026-09-29T14:30:05.2500+03:00");
        for bad in [
            "",
            "26",
            "2026131",
            "20261301",
            "20260230",
            "20260929250000",
            "2026.5",
            "20260929+03",
            "20260929143005.1234567890",
            "2026abcd",
        ] {
            assert!(ClinicalDateTime::parse_hl7(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_and_formats_iso_datetimes() {
        for text in [
            "2026",
            "2026-09",
            "2026-09-29",
            "2026-09-29T14:30",
            "2026-09-29T14:30:05",
            "2026-09-29T14:30:05.25Z",
            "2026-09-29T14:30:05+03:00",
            "2024-02-29",
        ] {
            assert_eq!(ClinicalDateTime::parse_iso(text).unwrap().to_iso(), text);
        }
        for bad in [
            "2026-9",
            "2023-02-29",
            "2026-09-29T",
            "2026-09-29T14",
            "2026T14:30",
            "2026-09-29T14:30:05+0300",
            "2026-09-29T14:30:05+15:00",
        ] {
            assert!(ClinicalDateTime::parse_iso(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn converts_to_timestamps() {
        let value = ClinicalDateTime::parse_hl7("20260929143005+0300").unwrap();
        let ts = value.to_timestamp(0).unwrap();
        assert_eq!(ts.to_string(), "2026-09-29T11:30:05Z");
        assert_eq!(
            ClinicalDateTime::from_timestamp(ts, 180).unwrap().to_hl7(),
            "20260929143005+0300"
        );
        let date = ClinicalDateTime::date(2026, 9, 29).unwrap();
        assert_eq!(
            date.to_timestamp(180).unwrap().to_string(),
            "2026-09-28T21:00:00Z"
        );
        assert!(
            ClinicalDateTime::parse_hl7("2026")
                .unwrap()
                .to_timestamp(0)
                .is_none()
        );
    }

    #[test]
    fn serializes_as_iso_text() {
        let value = ClinicalDateTime::parse_hl7("202609291430").unwrap();
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, "\"2026-09-29T14:30\"");
        assert_eq!(
            serde_json::from_str::<ClinicalDateTime>(&json).unwrap(),
            value
        );
    }
}
