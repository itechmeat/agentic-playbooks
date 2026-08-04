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
}
