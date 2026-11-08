//! The playbook's follow-up check: one fact read once, `after` a run ended
//! successfully, so a run that looked done can be shown to have held up
//! (a deploy still healthy an hour later, a deal moved to won days later).
//!
//! This module is the schema: the YAML shape ([`FollowUp`]) and its
//! durations. The check is one check in the baseline check's shape
//! ([`crate::baseline::BaselineCheck`]), checked without a before value
//! ([`crate::baseline::rule::check`]). Arming the read, reading the source
//! and journaling the result is the engine's job.
//!
//! The check reads a connector, called under the reserved grant site
//! [`FOLLOW_UP_SITE`], or a script. A `project_checks` source (V185) and a
//! `change` rule (V186) are refused, and `after` and `grace` are bounded
//! (V183, V184).

use serde::{Deserialize, Serialize};

/// The grant site a follow-up's connector read is made under: the run
/// manifest records its grant next to the nodes' grants. No node may use
/// this id (validator V68).
pub const FOLLOW_UP_SITE: &str = "@follow_up";
/// The shortest `after`, in seconds (V183).
pub const MIN_AFTER_SECONDS: u64 = 60;
/// The longest `after`, in seconds (V183): 30 days.
pub const MAX_AFTER_SECONDS: u64 = 30 * 24 * 3600;
/// The `grace` of a block that sets none, in seconds: 24 hours.
pub const DEFAULT_GRACE_SECONDS: u64 = 24 * 3600;
/// The longest `grace`, in seconds (V184): 7 days.
pub const MAX_GRACE_SECONDS: u64 = 7 * 24 * 3600;

/// The playbook's `follow_up` block. One per playbook, so one per run.
/// Additive to schema 2: an older apb drops it on load and arms nothing.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FollowUp {
    /// How long after the run's successful end the check is read.
    pub after: String,
    /// How late a read may still be taken; later it is `missed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace: Option<String>,
    /// One check in the baseline check's shape
    /// ([`crate::baseline::BaselineCheck`]).
    pub check: crate::baseline::BaselineCheck,
}

impl FollowUp {
    /// `after` in seconds, `None` when it is not a duration
    /// ([`crate::duration::parse_duration_str`], the grammar of
    /// `expected_duration`).
    pub fn after_seconds(&self) -> Option<u64> {
        crate::duration::parse_duration_str(&self.after)
    }

    /// `grace` in seconds: [`DEFAULT_GRACE_SECONDS`] when the block sets
    /// none, `None` when it is not a duration.
    pub fn grace_seconds(&self) -> Option<u64> {
        match &self.grace {
            None => Some(DEFAULT_GRACE_SECONDS),
            Some(grace) => crate::duration::parse_duration_str(grace),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::{Rule, Scalar};
    use crate::schema::Playbook;

    /// The example of the design, in a playbook of its own.
    const YAML: &str = r#"
schema: 2
id: crm
name: CRM
version: 1.0.0
params:
  - { name: deal_id, type: text }
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
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
"#;

    const AFTER: &str = "  after: 3d\n";

    fn follow_up(yaml: &str) -> FollowUp {
        Playbook::from_yaml(yaml).unwrap().follow_up.unwrap()
    }

    #[test]
    fn the_block_parses_and_round_trips() {
        let p = Playbook::from_yaml(YAML).unwrap();
        let f = p.follow_up.as_ref().unwrap();
        assert_eq!(f.after, "3d");
        assert_eq!(f.grace, None);
        assert_eq!(f.check.id, "deal_won");
        assert_eq!(f.check.description, "the deal moved to won");
        let conn = f.check.connector.as_ref().unwrap();
        assert_eq!(
            (conn.name.as_str(), conn.function.as_str()),
            ("crm", "get_deal")
        );
        assert_eq!(conn.args["id"], "{{params.deal_id}}");
        assert_eq!(f.check.observe.len(), 1);
        assert_eq!(f.check.observe[0].path, "deal.stage");
        assert_eq!(
            f.check.observe[0].expect.rule(),
            Ok(Rule::Equals(Scalar::Text("won".into())))
        );

        let yaml = yaml_serde::to_string(&p).unwrap();
        assert!(
            !yaml.contains("grace"),
            "an absent grace stays absent: {yaml}"
        );
        let again = Playbook::from_yaml(&yaml).unwrap();
        assert_eq!(again.follow_up, p.follow_up);

        // A playbook without the block writes none.
        let plain = YAML.split("follow_up:").next().unwrap().to_string()
            + "nodes:"
            + YAML.split("\nnodes:").nth(1).unwrap();
        let p = Playbook::from_yaml(&plain).unwrap();
        assert!(p.follow_up.is_none());
        assert!(!yaml_serde::to_string(&p).unwrap().contains("follow_up"));
    }

    #[test]
    fn an_unknown_key_is_refused() {
        assert!(YAML.contains(AFTER));
        let bad = YAML.replace(AFTER, "  after: 3d\n  every: 1d\n");
        assert!(Playbook::from_yaml(&bad).is_err());
        // The check keeps the baseline check's strict shape.
        let bad = YAML.replace("    id: deal_won\n", "    id: deal_won\n    phase: after\n");
        assert!(Playbook::from_yaml(&bad).is_err());
    }

    #[test]
    fn after_reads_the_duration_grammar() {
        let f = follow_up(YAML);
        assert_eq!(f.after_seconds(), Some(259_200));
        // A bare number is seconds, as in `expected_duration`.
        let f = follow_up(&YAML.replace(AFTER, "  after: 3600\n"));
        assert_eq!(f.after, "3600");
        assert_eq!(f.after_seconds(), Some(3_600));
        let f = FollowUp {
            after: "1h30m".into(),
            ..f
        };
        assert_eq!(f.after_seconds(), Some(5_400));
        let f = FollowUp {
            after: "soon".into(),
            ..f
        };
        assert_eq!(f.after_seconds(), None);
    }

    #[test]
    fn grace_defaults_to_a_day() {
        let f = follow_up(YAML);
        assert_eq!(f.grace_seconds(), Some(DEFAULT_GRACE_SECONDS));
        assert_eq!(DEFAULT_GRACE_SECONDS, 86_400);
        let f = follow_up(&YAML.replace(AFTER, "  after: 3d\n  grace: 2d\n"));
        assert_eq!(f.grace.as_deref(), Some("2d"));
        assert_eq!(f.grace_seconds(), Some(172_800));
        let f = FollowUp {
            grace: Some("late".into()),
            ..f
        };
        assert_eq!(f.grace_seconds(), None);
    }
}
