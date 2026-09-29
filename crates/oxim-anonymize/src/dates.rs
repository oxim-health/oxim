//! Date shifting: every date moves by the same number of days, so the
//! intervals between them (age at collection, turnaround times) survive
//! while the real dates do not.
//!
//! Times of day and time zone offsets are kept. Dates recorded with less
//! precision keep it: a year or a year and month is shifted through its
//! middle and written back at the same precision.

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// A shift of every date by a fixed number of days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DateShift {
    days: i64,
}

/// The precision a date was recorded with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Precision {
    Year,
    Month,
    Day,
}

impl DateShift {
    /// A shift by `days` (negative moves dates into the past).
    pub(crate) fn new(days: i64) -> Self {
        Self { days }
    }

    fn shift(
        &self,
        year: i64,
        month: u32,
        day: u32,
        precision: Precision,
    ) -> Option<(i64, u32, u32)> {
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        let (month, day) = match precision {
            Precision::Year => (7, 1),
            Precision::Month => (month, 15),
            Precision::Day => (month, day),
        };
        let shifted = civil_from_days(days_from_civil(year, month, day) + self.days);
        (0..=9999).contains(&shifted.0).then_some(shifted)
    }

    /// Shifts an HL7 or ASTM date/time (`YYYY[MM[DD[HHMM[SS[.S+]]]]][+/-ZZZZ]`).
    /// Returns `None` for text that is not such a date.
    pub(crate) fn compact(&self, text: &str) -> Option<String> {
        let digits = text.bytes().take_while(u8::is_ascii_digit).count();
        let mut rest = &text[digits..];
        // Fractional seconds, then an optional +HHMM or -HHMM offset.
        if digits == 14
            && let Some(fraction) = rest.strip_prefix('.')
        {
            let length = fraction.bytes().take_while(u8::is_ascii_digit).count();
            if length == 0 {
                return None;
            }
            rest = &fraction[length..];
        }
        let zone_ok = rest.is_empty()
            || (rest.len() == 5
                && rest.starts_with(['+', '-'])
                && rest[1..].bytes().all(|b| b.is_ascii_digit()));
        if !zone_ok || !matches!(digits, 4 | 6 | 8 | 10 | 12 | 14) {
            return None;
        }
        let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<u32>().ok();
        let year = i64::from(number(0..4)?);
        let (month, day, precision) = match digits {
            4 => (1, 1, Precision::Year),
            6 => (number(4..6)?, 1, Precision::Month),
            _ => (number(4..6)?, number(6..8)?, Precision::Day),
        };
        let (y, m, d) = self.shift(year, month, day, precision)?;
        let date = match precision {
            Precision::Year => format!("{y:04}"),
            Precision::Month => format!("{y:04}{m:02}"),
            Precision::Day => format!("{y:04}{m:02}{d:02}"),
        };
        let cut = match precision {
            Precision::Year => 4,
            Precision::Month => 6,
            Precision::Day => 8,
        };
        Some(format!("{date}{}", &text[cut..]))
    }

    /// Shifts an ISO 8601 date/time (`YYYY[-MM[-DD[Thh:mm...]]]`). Returns
    /// `None` for text that is not such a date.
    pub(crate) fn iso(&self, text: &str) -> Option<String> {
        let bytes = text.as_bytes();
        let digits = |range: std::ops::Range<usize>| {
            bytes
                .get(range.clone())?
                .iter()
                .all(u8::is_ascii_digit)
                .then(|| text[range].parse::<u32>().ok())?
        };
        let year = i64::from(digits(0..4)?);
        let (month, day, precision, cut) = match (bytes.get(4), bytes.get(7)) {
            (None, _) => (1, 1, Precision::Year, 4),
            (Some(b'-'), None) => (digits(5..7)?, 1, Precision::Month, 7),
            (Some(b'-'), Some(b'-')) => (digits(5..7)?, digits(8..10)?, Precision::Day, 10),
            _ => return None,
        };
        if precision == Precision::Month && bytes.len() != 7 {
            return None;
        }
        if precision == Precision::Day && !matches!(bytes.get(10), None | Some(b'T' | b't' | b' '))
        {
            return None;
        }
        let (y, m, d) = self.shift(year, month, day, precision)?;
        let date = match precision {
            Precision::Year => format!("{y:04}"),
            Precision::Month => format!("{y:04}-{m:02}"),
            Precision::Day => format!("{y:04}-{m:02}-{d:02}"),
        };
        Some(format!("{date}{}", &text[cut..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversions_round_trip() {
        for days in [-800_000, -1, 0, 1, 11_016, 20_725, 800_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(20_725), (2026, 9, 29));
    }

    #[test]
    fn shifts_compact_dates_keeping_precision_and_time() {
        let shift = DateShift::new(-100);
        assert_eq!(shift.compact("20260929").as_deref(), Some("20260621"));
        assert_eq!(
            shift.compact("20260929120000").as_deref(),
            Some("20260621120000")
        );
        assert_eq!(
            shift.compact("20260929120000.25+0300").as_deref(),
            Some("20260621120000.25+0300")
        );
        assert_eq!(shift.compact("202603").as_deref(), Some("202512"));
        assert_eq!(shift.compact("1980").as_deref(), Some("1980"));
        assert_eq!(
            DateShift::new(-365).compact("1980").as_deref(),
            Some("1979")
        );
        assert_eq!(
            DateShift::new(1).compact("20240228").as_deref(),
            Some("20240229")
        );
        for not_a_date in [
            "",
            "abc",
            "12345",
            "20261332",
            "20260230",
            "2026-09-29",
            "20260929X",
        ] {
            assert_eq!(shift.compact(not_a_date), None, "{not_a_date}");
        }
    }

    #[test]
    fn shifts_iso_dates() {
        let shift = DateShift::new(-100);
        assert_eq!(shift.iso("2026-09-29").as_deref(), Some("2026-06-21"));
        assert_eq!(
            shift.iso("2026-09-29T12:00:00+03:00").as_deref(),
            Some("2026-06-21T12:00:00+03:00")
        );
        assert_eq!(shift.iso("2026-03").as_deref(), Some("2025-12"));
        assert_eq!(shift.iso("2026").as_deref(), Some("2026"));
        for not_a_date in ["", "20260929", "2026-13-01", "2026-09-29x", "2026/09/29"] {
            assert_eq!(shift.iso(not_a_date), None, "{not_a_date}");
        }
    }
}
