//! Follow-up rules (V183-V186): the `follow_up` block's timing, and the
//! limits of a check read once with no before value. The check itself goes
//! through the baseline check's rules of one check (V62-V65, in
//! `baseline.rs`), named `follow-up check` in their messages.

use super::baseline::{check_observations, check_source, check_structure, check_templates};
use super::*;
use crate::baseline::{BaselineCheck, Rule};
use crate::follow_up::{FollowUp, MAX_AFTER_SECONDS, MAX_GRACE_SECONDS, MIN_AFTER_SECONDS};

/// What every message of the follow-up's rules calls its check.
const LABEL: &str = "follow-up check";
/// The shortest `grace` (V184): a minute, as for `after`.
const MIN_GRACE_SECONDS: u64 = MIN_AFTER_SECONDS;
/// The duration grammar ([`crate::duration::parse_duration_str`]), as a
/// message names it.
const GRAMMAR: &str = "use seconds like `90`, a single unit like `30m`, `2h`, `3d`, or a descending compound like `1d12h`";

/// Runs every follow-up rule.
pub(crate) fn check_follow_up(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    let Some(f) = &playbook.follow_up else {
        return;
    };
    let c = &f.check;
    check_structure(LABEL, c, r); // V62
    check_source(LABEL, c, ctx, r); // V63
    check_observations(LABEL, c, r); // V64
    check_templates(LABEL, playbook, c, r); // V65
    check_timing(f, r); // V183, V184
    check_reads(c, r); // V185, V186
}

/// V183: `after` is a duration from a minute to 30 days. V184: `grace`,
/// when set, is a duration from a minute to 7 days.
fn check_timing(f: &FollowUp, r: &mut ValidationReport) {
    if let Some(why) = out_of_bounds(f.after_seconds(), MIN_AFTER_SECONDS, MAX_AFTER_SECONDS) {
        r.error("V183", None, format!("follow_up.after `{}` {why}", f.after));
    }
    if let Some(grace) = &f.grace
        && let Some(why) = out_of_bounds(f.grace_seconds(), MIN_GRACE_SECONDS, MAX_GRACE_SECONDS)
    {
        r.error("V184", None, format!("follow_up.grace `{grace}` {why}"));
    }
}

/// Why a duration does not hold, `None` when it does: it is not a duration
/// (`None` seconds), or it lies outside `min` to `max` seconds.
fn out_of_bounds(seconds: Option<u64>, min: u64, max: u64) -> Option<String> {
    match seconds {
        None => Some(format!("is not a duration; {GRAMMAR}")),
        Some(s) if !(min..=max).contains(&s) => {
            Some(format!("must be {} to {}", span(min), span(max)))
        }
        Some(_) => None,
    }
}

/// Whole seconds in their largest whole unit, as people write them: `1m`,
/// `7d`.
fn span(seconds: u64) -> String {
    for (unit, size) in [("d", 86_400), ("h", 3_600), ("m", 60)] {
        if seconds >= size && seconds.is_multiple_of(size) {
            return format!("{}{unit}", seconds / size);
        }
    }
    format!("{seconds}s")
}

/// V185: a follow-up reads a connector or a script, never the project's
/// own checks. V186: its rules are absolute, since a follow-up has no
/// before value to compare a change with.
fn check_reads(c: &BaselineCheck, r: &mut ValidationReport) {
    if c.project_checks.is_some() {
        r.error(
            "V185",
            None,
            format!(
                "{LABEL} `{}`: a follow-up reads a connector or a script, not project_checks; the project's own checks, run days later, measure the commits made since, not this run",
                c.id
            ),
        );
    }
    for o in &c.observe {
        if let Ok(rule @ Rule::Change(_)) = o.expect.rule() {
            r.error(
                "V186",
                None,
                format!(
                    "{LABEL} `{}` observation `{}`: rule `{}` compares with a before value, and a follow-up has none; use equals, at_most, at_least, empty or contains",
                    c.id,
                    o.id,
                    rule.describe()
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid playbook whose follow-up is the example of the design.
    const GOOD: &str = r#"
schema: 2
id: crm
name: CRM
version: 1.0.0
params:
  - { name: deal_id, type: text }
goal:
  statement: The deal is won
  criteria:
    - { description: Deal report, check: { type: marker, marker: DONE } }
follow_up:
  after: 3d
  check:
    id: deal_won
    description: the deal moved to won
    connector: { name: crm, function: get_deal, args: { id: "{{params.deal_id}}" } }
    observe:
      - { id: stage, type: field, path: deal.stage, expect: { equals: won } }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: hi, profile: x }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

    const AFTER: &str = "  after: 3d\n";
    const CONNECTOR: &str =
        "    connector: { name: crm, function: get_deal, args: { id: \"{{params.deal_id}}\" } }\n";
    const OBSERVE: &str =
        "      - { id: stage, type: field, path: deal.stage, expect: { equals: won } }\n";
    /// The codes a follow-up can raise: the four rules of one check, then
    /// its own.
    const CODES: [&str; 8] = ["V62", "V63", "V64", "V65", "V183", "V184", "V185", "V186"];

    fn issues(yaml: &str) -> Vec<Issue> {
        let p = Playbook::from_yaml(yaml).unwrap();
        validate(&p, &ValidationContext::default()).issues
    }

    /// The follow-up codes `yaml` raises, all of them errors.
    fn codes(yaml: &str) -> Vec<&'static str> {
        let got: Vec<Issue> = issues(yaml)
            .into_iter()
            .filter(|i| CODES.contains(&i.code))
            .collect();
        for i in &got {
            assert_eq!(i.severity, Severity::Error, "{i:?}");
        }
        got.iter().map(|i| i.code).collect()
    }

    /// The message of the one `code` issue `yaml` raises.
    fn message(yaml: &str, code: &str) -> String {
        let got: Vec<Issue> = issues(yaml)
            .into_iter()
            .filter(|i| i.code == code)
            .collect();
        assert_eq!(got.len(), 1, "{code}: {got:?}");
        got[0].message.clone()
    }

    /// GOOD with `from` replaced by `to`; `from` must be in it.
    fn with(from: &str, to: &str) -> String {
        assert!(GOOD.contains(from), "{from}");
        GOOD.replace(from, to)
    }

    fn with_grace(grace: &str) -> String {
        with(AFTER, &format!("{AFTER}  grace: {grace}\n"))
    }

    #[test]
    fn a_valid_follow_up_has_no_follow_up_issues() {
        let none: Vec<&str> = Vec::new();
        assert_eq!(codes(GOOD), none);
        // The bounds themselves, and a bare number of seconds, are allowed.
        for after in ["60s", "1m", "30d", "3600"] {
            let yaml = with(AFTER, &format!("  after: {after}\n"));
            assert_eq!(codes(&yaml), none, "{after}");
        }
        for grace in ["1m", "2d", "7d"] {
            assert_eq!(codes(&with_grace(grace)), none, "{grace}");
        }
        // A script source is a follow-up's other source.
        assert_eq!(
            codes(&with(CONNECTOR, "    script: scripts/deal.sh\n")),
            none
        );
        // No block, no rule.
        let start = GOOD.find("follow_up:").unwrap();
        let end = GOOD.find("nodes:").unwrap();
        let plain = format!("{}{}", &GOOD[..start], &GOOD[end..]);
        assert_eq!(codes(&plain), none);
    }

    #[test]
    fn v183_after_is_a_duration_from_a_minute_to_thirty_days() {
        for after in ["10s", "59s", "60d", "31d", "nonsense", "1.5h"] {
            let yaml = with(AFTER, &format!("  after: {after}\n"));
            assert_eq!(codes(&yaml), vec!["V183"], "{after}");
            let msg = message(&yaml, "V183");
            assert!(msg.contains("follow_up.after"), "{msg}");
            assert!(msg.contains(after), "{msg}");
        }
    }

    #[test]
    fn v184_grace_is_a_duration_from_a_minute_to_seven_days() {
        for grace in ["5s", "59s", "8d", "late"] {
            let yaml = with_grace(grace);
            assert_eq!(codes(&yaml), vec!["V184"], "{grace}");
            let msg = message(&yaml, "V184");
            assert!(msg.contains("follow_up.grace"), "{msg}");
            assert!(msg.contains(grace), "{msg}");
        }
    }

    #[test]
    fn v185_a_follow_up_does_not_read_the_project_checks() {
        let yaml = with(CONNECTOR, "    project_checks: { only: [test] }\n")
            .replace(&format!("    observe:\n{OBSERVE}"), "");
        assert_eq!(codes(&yaml), vec!["V185"]);
        assert!(message(&yaml, "V185").contains("follow-up check `deal_won`"));
    }

    #[test]
    fn v186_a_follow_up_has_no_before_value_for_a_change_rule() {
        for observe in [
            "{ id: amount, type: number, path: deal.amount, expect: { change: decrease } }",
            "{ id: notes, type: list, path: \"deal.notes[]\", expect: { change: no_new } }",
            "{ id: stage, type: field, path: deal.stage, expect: { change: unchanged } }",
        ] {
            let yaml = with(OBSERVE, &format!("      - {observe}\n"));
            assert_eq!(codes(&yaml), vec!["V186"], "{observe}");
            let msg = message(&yaml, "V186");
            assert!(
                msg.contains("follow-up check `deal_won` observation"),
                "{msg}"
            );
            assert!(msg.contains("change"), "{msg}");
        }
    }

    /// The baseline check's rules name the follow-up's check as such, and
    /// a baseline check still as a baseline check.
    #[test]
    fn a_v63_problem_names_the_follow_up_check() {
        let bad = with(CONNECTOR, "    script: ../escape.sh\n");
        assert_eq!(codes(&bad), vec!["V63"]);
        let msg = message(&bad, "V63");
        assert!(
            msg.starts_with("follow-up check `deal_won`: script `../escape.sh`"),
            "{msg}"
        );

        let start = GOOD.find("follow_up:").unwrap();
        let end = GOOD.find("nodes:").unwrap();
        let baseline = format!(
            "{}baseline_check:\n  checks:\n    - id: deal_won\n      script: ../escape.sh\n      observe:\n  {OBSERVE}{}",
            &GOOD[..start],
            &GOOD[end..]
        );
        let msg = message(&baseline, "V63");
        assert!(
            msg.starts_with("baseline check `deal_won`: script `../escape.sh`"),
            "{msg}"
        );
    }
}
