//! Node `expected_duration` parsing and per-kind defaults. The 120s
//! agent_task/script default lives here and nowhere else.

/// Default estimated wall time for an agent_task or script node with no
/// explicit `expected_duration`.
pub const DEFAULT_TASK_SECONDS: u64 = 120;

/// Parses an `expected_duration` scalar: a plain integer count of seconds
/// (`90`), or an integer with a single unit suffix (`30s`, `5m`, `2h`, `2d`).
/// Returns None for anything else (empty, float, multi-unit, unknown suffix).
pub fn parse_duration_str(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let (num, mult) = match s.as_bytes()[s.len() - 1] {
        b's' => (&s[..s.len() - 1], 1u64),
        b'm' => (&s[..s.len() - 1], 60u64),
        b'h' => (&s[..s.len() - 1], 3600u64),
        b'd' => (&s[..s.len() - 1], 86_400u64),
        _ => return None,
    };
    let n: u64 = num.trim().parse().ok()?;
    n.checked_mul(mult)
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
    }

    #[test]
    fn rejects_bad_values() {
        assert_eq!(parse_duration_str(""), None);
        assert_eq!(parse_duration_str("5x"), None);
        assert_eq!(parse_duration_str("1.5m"), None);
        assert_eq!(parse_duration_str("5m30s"), None);
        assert_eq!(parse_duration_str("m"), None);
    }
}
