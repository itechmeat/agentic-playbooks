//! The run `deadline` rule (V200): a duration of at least one minute and at
//! most 30 days.

use super::*;

/// The shortest deadline an author may write, in seconds.
pub const MIN_DEADLINE_SECS: u64 = 60;
/// The longest deadline an author may write, in seconds (30 days).
pub const MAX_DEADLINE_SECS: u64 = 30 * 24 * 3600;

/// The deadline `raw` in milliseconds, or why it is refused. The one
/// reading of an author's `deadline:`; a run argument is parsed by its
/// surface with [`crate::duration::parse_duration_str`] alone.
pub fn parse_deadline(raw: &str) -> Result<u64, String> {
    let Some(secs) = crate::duration::parse_duration_str(raw) else {
        return Err(format!(
            "`{raw}` is not a duration (use a number of seconds or units such as `45m`, `2h`, `1d`)"
        ));
    };
    if secs == 0 {
        return Err("a deadline of zero would end every run at its start".into());
    }
    if secs < MIN_DEADLINE_SECS {
        return Err(format!("`{raw}` is under the 1 minute minimum"));
    }
    if secs > MAX_DEADLINE_SECS {
        return Err(format!("`{raw}` is over the 30 day maximum"));
    }
    Ok(secs * 1000)
}

/// Runs V200.
pub(crate) fn check_deadline(playbook: &Playbook, r: &mut ValidationReport) {
    if let Some(raw) = &playbook.deadline
        && let Err(why) = parse_deadline(raw)
    {
        r.error("V200", None, format!("deadline: {why}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(deadline: &str) -> Vec<String> {
        let pb = Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndeadline: \"{deadline}\"\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
        ))
        .unwrap();
        crate::validate::validate(&pb, &ValidationContext::default())
            .issues
            .iter()
            .filter(|i| i.code.starts_with("V200"))
            .map(|i| i.code.to_string())
            .collect()
    }

    #[test]
    fn a_minute_to_thirty_days_is_accepted() {
        assert!(codes("45m").is_empty());
        assert!(codes("1m").is_empty());
        assert!(codes("30d").is_empty());
        assert_eq!(parse_deadline("45m"), Ok(45 * 60 * 1000));
    }

    #[test]
    fn zero_short_long_and_garbage_are_v200() {
        for bad in ["0", "30s", "31d", "soon"] {
            assert_eq!(codes(bad), vec!["V200".to_string()], "{bad}");
        }
    }
}
