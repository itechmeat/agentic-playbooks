//! The `health:` block rule (V212): at least one rule, each field within
//! its range, and the durations at least 10 minutes and at most 90 days.

use super::*;

/// Runs V212.
pub(super) fn check_health(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(block) = &playbook.health else {
        return;
    };
    let mut issues: Vec<String> = Vec::new();
    if block.consecutive_failures.is_none()
        && block.failure_rate.is_none()
        && block.overdue.is_none()
    {
        issues.push(
            "health declares no rule; set consecutive_failures, failure_rate or overdue".into(),
        );
    }
    if let Some(n) = block.consecutive_failures
        && !(1..=100).contains(&n)
    {
        issues.push(format!("health.consecutive_failures is {n}; use 1 to 100"));
    }
    if let Some(fr) = &block.failure_rate {
        if !fr.at_least.is_finite() || fr.at_least <= 0.0 || fr.at_least > 1.0 {
            issues.push(format!(
                "health.failure_rate.at_least is {}; use a share in (0, 1]",
                fr.at_least
            ));
        }
        if !(2..=200).contains(&fr.window) {
            issues.push(format!(
                "health.failure_rate.window is {}; use 2 to 200",
                fr.window
            ));
        }
        if fr.min_runs < 1 || fr.min_runs > fr.window {
            issues.push(format!(
                "health.failure_rate.min_runs is {}; use 1 to the window's {}",
                fr.min_runs, fr.window
            ));
        }
    }
    let duration_in_range = |word: &str, raw: &str| -> Option<String> {
        match crate::duration::parse_duration_str(raw) {
            None => Some(format!(
                "health.{word} is `{raw}`; use a duration such as `26h`"
            )),
            Some(secs) if secs < 600 => {
                Some(format!("health.{word} is `{raw}`; at least 10 minutes"))
            }
            Some(secs) if secs > 90 * 24 * 3600 => {
                Some(format!("health.{word} is `{raw}`; at most 90 days"))
            }
            Some(_) => None,
        }
    };
    if let Some(raw) = &block.overdue
        && let Some(issue) = duration_in_range("overdue", raw)
    {
        issues.push(issue);
    }
    if let Some(raw) = &block.cooldown
        && let Some(issue) = duration_in_range("cooldown", raw)
    {
        issues.push(issue);
    }
    for issue in issues {
        r.error("V212", None, issue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issues(block: &str) -> Vec<String> {
        let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n".to_string()
        .replace("nodes:", &format!("{block}nodes:"));
        let pb = Playbook::from_yaml(&yaml).unwrap();
        crate::validate::validate(&pb, &ValidationContext::default())
            .issues
            .iter()
            .filter(|i| i.code == "V212")
            .map(|i| i.message.clone())
            .collect()
    }

    #[test]
    fn v212_no_rule() {
        assert!(!issues("health: {}\n").is_empty());
    }

    #[test]
    fn v212_consecutive_failures_out_of_range() {
        assert!(
            issues("health: { consecutive_failures: 0 }\n")
                .iter()
                .any(|m| m.contains("consecutive_failures"))
        );
        assert!(
            issues("health: { consecutive_failures: 101 }\n")
                .iter()
                .any(|m| m.contains("consecutive_failures"))
        );
    }

    #[test]
    fn v212_at_least_out_of_range() {
        assert!(
            issues("health: { failure_rate: { at_least: 0, window: 10, min_runs: 5 } }\n")
                .iter()
                .any(|m| m.contains("at_least"))
        );
        assert!(
            issues("health: { failure_rate: { at_least: 1.5, window: 10, min_runs: 5 } }\n")
                .iter()
                .any(|m| m.contains("at_least"))
        );
    }

    #[test]
    fn v212_at_least_not_a_number() {
        for bad in [".nan", ".inf", "-.inf"] {
            assert!(
                issues(&format!(
                    "health: {{ failure_rate: {{ at_least: {bad}, window: 10, min_runs: 5 }} }}\n"
                ))
                .iter()
                .any(|m| m.contains("at_least")),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn v212_window_out_of_range() {
        assert!(
            issues("health: { failure_rate: { at_least: 0.5, window: 1, min_runs: 1 } }\n")
                .iter()
                .any(|m| m.contains("window"))
        );
    }

    #[test]
    fn v212_min_runs_over_window() {
        assert!(
            issues("health: { failure_rate: { at_least: 0.5, window: 10, min_runs: 11 } }\n")
                .iter()
                .any(|m| m.contains("min_runs"))
        );
    }

    #[test]
    fn v212_overdue_not_a_duration() {
        assert!(
            issues("health: { overdue: soon }\n")
                .iter()
                .any(|m| m.contains("overdue"))
        );
    }

    #[test]
    fn v212_overdue_under_ten_minutes() {
        assert!(
            issues("health: { overdue: 5m }\n")
                .iter()
                .any(|m| m.contains("overdue"))
        );
    }

    #[test]
    fn v212_cooldown_over_ninety_days() {
        assert!(
            issues("health: { consecutive_failures: 3, cooldown: 91d }\n")
                .iter()
                .any(|m| m.contains("cooldown"))
        );
    }

    #[test]
    fn a_full_block_is_valid() {
        assert!(
            issues(
                "health: { consecutive_failures: 3, overdue: 26h, on_trip: hold, cooldown: 12h }\n"
            )
            .is_empty()
        );
    }
}
