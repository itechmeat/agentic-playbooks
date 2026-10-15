//! Node `expected_duration` parsing and per-kind defaults. The 120s
//! agent_task/script default lives here and nowhere else.

/// Default estimated wall time for an agent_task or script node with no
/// explicit `expected_duration`.
pub const DEFAULT_TASK_SECONDS: u64 = 120;

/// Parses an `expected_duration` scalar: a plain integer count of seconds
/// (`90`), a single integer with a unit suffix (`30s`, `5m`, `2h`, `2d`), or a
/// compound duration whose units are in strictly descending order d > h > m > s
/// (`1h30m`, `2h30m15s`, `1d2h`). Ascending or repeated units (`30m1h`,
/// `1h1h`), unknown units, floats, internal whitespace, and overflow all return
/// None.
pub fn parse_duration_str(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    if !s.is_ascii() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut total: u64 = 0;
    // Rank of the previously consumed unit (d=0, h=1, m=2, s=3); -1 means none.
    // Strictly descending order means the next unit's rank must be strictly
    // greater, which also rejects repeated units.
    let mut last_rank: i8 = -1;
    let mut i: usize = 0;
    let mut any_group = false;
    while i < bytes.len() {
        let digits_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == digits_start {
            return None; // a unit must be preceded by at least one digit
        }
        if i >= bytes.len() {
            return None; // trailing digits with no unit
        }
        let (rank, mult): (u8, u64) = match bytes[i] {
            b'd' => (0, 86_400),
            b'h' => (1, 3_600),
            b'm' => (2, 60),
            b's' => (3, 1),
            _ => return None,
        };
        if (rank as i8) <= last_rank {
            return None; // not strictly descending (also catches repeats)
        }
        let n: u64 = s[digits_start..i].parse().ok()?;
        let scaled = n.checked_mul(mult)?;
        total = total.checked_add(scaled)?;
        last_rank = rank as i8;
        any_group = true;
        i += 1;
    }
    if !any_group {
        return None;
    }
    Some(total)
}

/// Parses an operator pause `until` (issue #200): an RFC 3339 timestamp
/// (`2026-10-16T08:00:00Z`, `2026-10-16T10:00:00+02:00`, fractional seconds
/// allowed) or a duration counted from `now_ms` (`90m`, `2h`, see
/// [`parse_duration_str`]). Returns epoch milliseconds, or a message naming
/// what was wrong. apb has no date dependency, so the civil-date arithmetic
/// lives here.
pub fn parse_until(s: &str, now_ms: u64) -> Result<u64, String> {
    let s = s.trim();
    if let Some(secs) = parse_duration_str(s) {
        return secs
            .checked_mul(1000)
            .and_then(|ms| now_ms.checked_add(ms))
            .ok_or_else(|| format!("duration `{s}` is too large"));
    }
    parse_rfc3339_ms(s).ok_or_else(|| {
        format!(
            "`{s}` is neither an RFC 3339 timestamp (2026-10-16T08:00:00Z) nor a duration (90m, 2h)"
        )
    })
}

/// RFC 3339 to epoch milliseconds; `None` for anything malformed or before
/// the Unix epoch.
fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    if !s.is_ascii() || s.len() < 20 {
        return None;
    }
    let b = s.as_bytes();
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = s.get(from..to)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    if b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mi, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || hh > 23 || mi > 59 || ss > 60 {
        return None;
    }
    let mut i = 19;
    let mut frac_ms = 0i64;
    if b[i] == b'.' {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
        let digits = &s[start..i.min(start + 3)];
        frac_ms = format!("{digits:0<3}").parse().ok()?;
    }
    let offset_secs = match s.get(i..)? {
        "Z" | "z" => 0,
        tz if tz.len() == 6
            && matches!(tz.as_bytes()[0], b'+' | b'-')
            && tz.as_bytes()[3] == b':' =>
        {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let oh: i64 = tz.get(1..3)?.parse().ok()?;
            let om: i64 = tz.get(4..6)?.parse().ok()?;
            if oh > 23 || om > 59 {
                return None;
            }
            sign * (oh * 3600 + om * 60)
        }
        _ => return None,
    };
    let secs =
        days_from_civil(y, mo as u32, d as u32) * 86_400 + hh * 3600 + mi * 60 + ss - offset_secs;
    u64::try_from(secs)
        .ok()?
        .checked_mul(1000)?
        .checked_add(frac_ms as u64)
}

/// (year, month, day) to days since the Unix epoch (Howard Hinnant's
/// `days_from_civil`, public domain), the inverse of the conversion in
/// `dismiss::iso_utc`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_integer_seconds_and_single_unit() {
        assert_eq!(parse_duration_str("90"), Some(90));
        assert_eq!(parse_duration_str("30s"), Some(30));
        assert_eq!(parse_duration_str("5m"), Some(300));
        assert_eq!(parse_duration_str("2h"), Some(7200));
        assert_eq!(parse_duration_str("2d"), Some(172_800));
        assert_eq!(parse_duration_str("  45s "), Some(45));
        // Descending compound duration that used to be rejected.
        assert_eq!(parse_duration_str("5m30s"), Some(330));
    }

    #[test]
    fn parses_descending_compound_durations() {
        assert_eq!(parse_duration_str("1h30m"), Some(5400));
        assert_eq!(parse_duration_str("2h30m15s"), Some(9015));
        assert_eq!(parse_duration_str("1d2h"), Some(93600));
        assert_eq!(parse_duration_str("5m30s"), Some(330));
    }

    #[test]
    fn rejects_bad_values() {
        assert_eq!(parse_duration_str(""), None);
        assert_eq!(parse_duration_str("5x"), None);
        assert_eq!(parse_duration_str("1.5m"), None);
        assert_eq!(parse_duration_str("m"), None);
    }

    #[test]
    fn rejects_bad_compound_durations() {
        assert_eq!(parse_duration_str("30m1h"), None); // ascending
        assert_eq!(parse_duration_str("1h1h"), None); // repeat
        assert_eq!(parse_duration_str("1h30x"), None); // bad unit
        assert_eq!(parse_duration_str("1h 30m"), None); // internal space
        assert_eq!(parse_duration_str("99999999999999999999d"), None); // overflow
    }
    #[test]
    fn until_parses_rfc3339_and_durations() {
        assert_eq!(parse_until("1970-01-01T00:00:10Z", 0), Ok(10_000));
        assert_eq!(parse_until("1970-01-01T02:00:00+02:00", 0), Ok(0));
        assert_eq!(
            parse_until("2026-10-16T08:00:00.5Z", 0),
            Ok(1_792_137_600_500)
        );
        assert_eq!(parse_until("90m", 1000), Ok(1000 + 5_400_000));
        assert_eq!(
            crate::dismiss::iso_utc(parse_until("2026-10-16T08:00:00Z", 0).unwrap()),
            "2026-10-16T08:00:00Z"
        );
        assert!(parse_until("tomorrow", 0).is_err());
        assert!(parse_until("2026-13-16T08:00:00Z", 0).is_err());
        assert!(parse_until("2026-10-16T08:00:00", 0).is_err());
    }
}
