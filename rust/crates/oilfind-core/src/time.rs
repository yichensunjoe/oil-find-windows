//! Calendar arithmetic on Unix seconds.
//!
//! Day boundaries use the machine's local time zone: `dm:today` must mean the
//! user's today, not UTC's. The cache timestamp stays RFC 3339 so the file is
//! still readable by the earlier implementation.

use chrono::{Datelike, Local, NaiveDate, TimeZone};
use std::time::{SystemTime, UNIX_EPOCH};

const SECS_PER_DAY: i64 = 86_400;

/// Current wall clock as Unix seconds (0 if the system clock predates 1970).
pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// First second of the local day containing `secs`.
pub(crate) fn local_day_start(secs: i64) -> i64 {
    local_midnight(secs).unwrap_or(secs)
}

/// First second of the local month containing `secs`.
pub(crate) fn local_month_start(secs: i64) -> i64 {
    let fallback = local_day_start(secs);
    let Some(date) = local_date(secs) else {
        return fallback;
    };
    let Some(first) = date.with_day(1) else {
        return fallback;
    };
    local_midnight_of(first).unwrap_or(fallback)
}

/// Parses `YYYY-MM-DD` as the first second of that local day.
pub(crate) fn parse_local_date(text: &str) -> Option<i64> {
    // Require the strict YYYY-MM-DD shape; chrono alone would accept "2026-1-1".
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
    local_midnight_of(date)
}

fn local_date(secs: i64) -> Option<NaiveDate> {
    Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|dt| dt.date_naive())
}

fn local_midnight(secs: i64) -> Option<i64> {
    local_midnight_of(local_date(secs)?)
}

fn local_midnight_of(date: NaiveDate) -> Option<i64> {
    let naive = date.and_hms_opt(0, 0, 0)?;
    naive
        .and_local_timezone(Local)
        .earliest()
        .map(|dt| dt.timestamp())
}

/// Number of whole days since 1970-01-01.
pub(crate) fn day_index(secs: i64) -> i64 {
    secs.div_euclid(SECS_PER_DAY)
}

/// Days since the Unix epoch for a proleptic Gregorian date.
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`].
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Formats Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn format_rfc3339(secs: i64) -> String {
    let (year, month, day) = civil_from_days(day_index(secs));
    let second_of_day = secs.rem_euclid(SECS_PER_DAY);
    let hour = second_of_day / 3600;
    let minute = (second_of_day % 3600) / 60;
    let second = second_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Parses the RFC 3339 timestamps the Go implementation writes, tolerating a
/// fractional part and a numeric UTC offset.
pub(crate) fn parse_rfc3339(text: &str) -> Option<i64> {
    let text = text.trim();
    let (body, offset) = if let Some(body) = text.strip_suffix('Z') {
        (body, 0)
    } else if let Some(index) = text.rfind(['+', '-']).filter(|index| *index > 10) {
        let (body, zone) = text.split_at(index);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let zone = &zone[1..];
        let (hours, minutes) = zone.split_once(':')?;
        let hours: i64 = hours.parse().ok()?;
        let minutes: i64 = minutes.parse().ok()?;
        (body, sign * (hours * 3600 + minutes * 60))
    } else {
        (text, 0)
    };
    let body = body.split_once('.').map_or(body, |(head, _)| head);
    let (date, clock) = body.split_once('T').or_else(|| body.split_once(' '))?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next().unwrap_or("0").parse().ok()?;
    if clock_parts.next().is_some() || !(1..=12).contains(&month) || day == 0 || day > 31 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * SECS_PER_DAY + hour * 3600 + minute * 60 + second - offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for days in [-25_000, -1, 0, 1, 19_000, 20_700] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
    }

    #[test]
    fn rfc3339_round_trip() {
        let secs = 1_000_000_000;
        assert_eq!(format_rfc3339(secs), "2001-09-09T01:46:40Z");
        assert_eq!(parse_rfc3339("2001-09-09T01:46:40Z"), Some(secs));
    }

    #[test]
    fn rfc3339_accepts_offsets_and_fractions() {
        assert_eq!(
            parse_rfc3339("2001-09-09T09:46:40.123456789+08:00"),
            Some(1_000_000_000)
        );
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn dates_reject_impossible_days() {
        assert_eq!(parse_local_date("2026-02-29"), None);
        assert_eq!(parse_local_date("2026-13-01"), None);
        assert!(parse_local_date("2024-02-29").is_some());
        assert_eq!(parse_local_date("2026-1-01"), None);
    }

    #[test]
    fn local_day_boundaries_land_on_local_midnight() {
        // The user's "today" must start at 00:00 local time, not 00:00 UTC.
        for offset in [-2 * 86_400, -1, 0, 1, 86_400] {
            let now = now_secs() + offset;
            let start = local_day_start(now);
            let local = Local
                .timestamp_opt(start, 0)
                .single()
                .expect("valid local time");
            assert_eq!(local.time().format("%H:%M:%S").to_string(), "00:00:00");
            assert!(start <= now && now < local_day_start(start + 86_400));
        }
    }

    #[test]
    fn explicit_dates_are_parsed_as_local_midnight() {
        let stamp = parse_local_date("2026-10-24").expect("valid date");
        let local = Local
            .timestamp_opt(stamp, 0)
            .single()
            .expect("valid local time");
        assert_eq!(local.date_naive().to_string(), "2026-10-24");
        assert_eq!(local.time().format("%H:%M:%S").to_string(), "00:00:00");
    }
}
