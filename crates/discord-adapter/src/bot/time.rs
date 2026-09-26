//! Pure ISO 8601 timestamp parsing for the bot gateway/REST JSON payloads.
//!
//! Discord (API v10) always stamps timestamps as
//! `YYYY-MM-DDTHH:MM:SS[.ffffff]+00:00` (or `Z`), e.g.
//! `2026-09-24T15:00:00.123000+00:00`. This module parses exactly that
//! profile without pulling in a date/time crate, using a days-from-civil
//! calendar algorithm (Howard Hinnant's `days_from_civil`) so the whole thing
//! stays pure integer arithmetic.

use litecord_types::Timestamp;

/// Parses an ISO 8601 timestamp of the form
/// `YYYY-MM-DDTHH:MM:SS[.fraction][Z|±HH:MM]` into a [`Timestamp`].
///
/// The fractional-seconds component (0-9 digits) is truncated to
/// milliseconds. A timezone designator (`Z` or `±HH:MM`) is required — the
/// value returned is always the equivalent UTC instant. Returns `None` for
/// anything that does not match, including calendar-invalid dates (e.g.
/// February 30th, or February 29th in a non-leap year).
pub fn parse_iso8601(s: &str) -> Option<Timestamp> {
    if s.len() < 20 {
        return None;
    }

    let year = parse_digits(s.get(0..4)?)?;
    if s.as_bytes().get(4)? != &b'-' {
        return None;
    }
    let month = u32::try_from(parse_digits(s.get(5..7)?)?).ok()?;
    if s.as_bytes().get(7)? != &b'-' {
        return None;
    }
    let day = u32::try_from(parse_digits(s.get(8..10)?)?).ok()?;
    if !matches!(s.as_bytes().get(10)?, b'T' | b't') {
        return None;
    }
    let hour = parse_digits(s.get(11..13)?)?;
    if s.as_bytes().get(13)? != &b':' {
        return None;
    }
    let minute = parse_digits(s.get(14..16)?)?;
    if s.as_bytes().get(16)? != &b':' {
        return None;
    }
    let second = parse_digits(s.get(17..19)?)?;

    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return None;
    }

    let mut rest = s.get(19..)?;

    let mut millis: i64 = 0;
    if let Some(after_dot) = rest.strip_prefix('.') {
        let frac_len = after_dot.bytes().take_while(u8::is_ascii_digit).count();
        if frac_len == 0 {
            return None;
        }
        let frac = after_dot.get(..frac_len)?;
        let truncated = &frac[..frac_len.min(3)];
        let mut ms: i64 = parse_digits(truncated)?;
        for _ in truncated.len()..3 {
            ms *= 10;
        }
        millis = ms;
        rest = after_dot.get(frac_len..)?;
    }

    let offset_minutes = if rest.eq_ignore_ascii_case("z") {
        0
    } else if !rest.is_empty() {
        parse_offset(rest)?
    } else {
        // No timezone designator at all: reject rather than guess UTC.
        return None;
    };

    let days = days_from_civil(year, month, day);
    let seconds_of_day = hour * 3600 + minute * 60 + second;
    let total_seconds = days
        .checked_mul(86_400)?
        .checked_add(seconds_of_day)?
        .checked_sub(offset_minutes * 60)?;
    let total_millis = total_seconds.checked_mul(1_000)?.checked_add(millis)?;
    Some(Timestamp::from_millis(total_millis))
}

/// Parses a run of ASCII digits (no sign, no whitespace) into an `i64`.
fn parse_digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Parses a `±HH:MM` UTC offset into a signed minute count.
fn parse_offset(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() != 6 || bytes[3] != b':' {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours = parse_digits(s.get(1..3)?)?;
    let minutes = parse_digits(s.get(4..6)?)?;
    if !(0..24).contains(&hours) || !(0..60).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 60 + minutes))
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic-Gregorian civil date. Howard
/// Hinnant's `days_from_civil` algorithm (public domain), adapted to `i64`.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (i64::from(month) + 9) % 12; // [0, 11], March-based
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_with_microsecond_fraction_and_offset() {
        let ts = parse_iso8601("2026-09-24T15:00:00.123000+00:00").unwrap();
        assert_eq!(ts.as_millis(), ts_millis(2026, 9, 24, 15, 0, 0, 123));
    }

    #[test]
    fn parses_with_z_and_no_fraction() {
        let ts = parse_iso8601("2026-09-24T15:00:00Z").unwrap();
        assert_eq!(ts.as_millis(), ts_millis(2026, 9, 24, 15, 0, 0, 0));
    }

    #[test]
    fn lowercase_z_and_t_are_accepted() {
        let ts = parse_iso8601("2026-09-24t15:00:00z").unwrap();
        assert_eq!(ts.as_millis(), ts_millis(2026, 9, 24, 15, 0, 0, 0));
    }

    #[test]
    fn nonzero_offset_shifts_to_utc() {
        // 15:00 at +02:00 is 13:00 UTC.
        let ts = parse_iso8601("2026-09-24T15:00:00+02:00").unwrap();
        assert_eq!(ts.as_millis(), ts_millis(2026, 9, 24, 13, 0, 0, 0));
    }

    #[test]
    fn negative_offset_shifts_to_utc() {
        // 15:00 at -05:00 is 20:00 UTC.
        let ts = parse_iso8601("2026-09-24T15:00:00-05:00").unwrap();
        assert_eq!(ts.as_millis(), ts_millis(2026, 9, 24, 20, 0, 0, 0));
    }

    #[test]
    fn fraction_shorter_than_three_digits_is_scaled_up() {
        let ts = parse_iso8601("2026-09-24T15:00:00.5Z").unwrap();
        assert_eq!(ts.as_millis(), ts_millis(2026, 9, 24, 15, 0, 0, 500));
    }

    #[test]
    fn fraction_longer_than_three_digits_is_truncated() {
        let ts = parse_iso8601("2026-09-24T15:00:00.123999Z").unwrap();
        assert_eq!(ts.as_millis(), ts_millis(2026, 9, 24, 15, 0, 0, 123));
    }

    #[test]
    fn leap_year_feb_29_is_valid() {
        assert!(parse_iso8601("2024-02-29T00:00:00Z").is_some());
        assert!(parse_iso8601("2000-02-29T00:00:00Z").is_some());
    }

    #[test]
    fn non_leap_year_feb_29_is_rejected() {
        assert!(parse_iso8601("2023-02-29T00:00:00Z").is_none());
        assert!(parse_iso8601("1900-02-29T00:00:00Z").is_none());
    }

    #[test]
    fn invalid_input_is_none() {
        assert!(parse_iso8601("").is_none());
        assert!(parse_iso8601("not-a-timestamp").is_none());
        assert!(parse_iso8601("2026-13-24T15:00:00Z").is_none()); // bad month
        assert!(parse_iso8601("2026-09-24T25:00:00Z").is_none()); // bad hour
        assert!(parse_iso8601("2026-09-24 15:00:00Z").is_none()); // missing T
        assert!(parse_iso8601("2026-09-24T15:00:00").is_none()); // no timezone
        assert!(parse_iso8601("2026-09-24T15:00:00.Z").is_none()); // empty fraction
        assert!(parse_iso8601("2026-09-24T15:00:00+25:00").is_none()); // bad offset
    }

    #[test]
    fn epoch_roundtrips() {
        assert_eq!(
            parse_iso8601("1970-01-01T00:00:00Z").unwrap().as_millis(),
            0
        );
    }

    /// Independent millisecond computation for assertions (not the code under
    /// test): plain civil-calendar day counting.
    fn ts_millis(y: i64, m: i64, d: i64, hh: i64, mm: i64, ss: i64, ms: i64) -> i64 {
        // Days from 1970-01-01 to y-m-d using the same well-known algorithm,
        // recomputed independently here to cross-check `days_from_civil`.
        let yy = if m <= 2 { y - 1 } else { y };
        let era = if yy >= 0 { yy } else { yy - 399 } / 400;
        let yoe = yy - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        ((days * 86_400 + hh * 3600 + mm * 60 + ss) * 1000) + ms
    }
}
