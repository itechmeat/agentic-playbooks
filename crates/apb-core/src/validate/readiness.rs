//! The top-level `unattended:` block: the probation bound it asks for is
//! one a run can meet (V249), and a playbook that means to run alone is
//! warned about its acting steps without a deterministic check (V250).

use crate::readiness::{MAX_PROBATION, unchecked_actors};
use crate::schema::Playbook;

use super::*;

pub(super) fn check_readiness(playbook: &Playbook, r: &mut ValidationReport) {
    if let Some(unattended) = &playbook.unattended {
        let n = unattended.after_attended_successes;
        if n == 0 || n > MAX_PROBATION {
            r.error(
                "V249",
                None,
                format!("unattended.after_attended_successes is {n}; use 1 to {MAX_PROBATION}"),
            );
        }
        for id in unchecked_actors(playbook) {
            r.warn(
                "V250",
                Some(id),
                format!(
                    "acting agent_task `{id}` has no deterministic check, and this playbook means to run alone; add a script success_check, a script list item check with on_needs_look: fail, or a script goal criterion with goal.enforce: true"
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issues(extra: &str, node: &str) -> Vec<(String, Severity)> {
        let yaml = format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\n{extra}nodes:\n  - {{ id: start, type: start }}\n{node}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: done }}\n"
        );
        let p = Playbook::from_yaml(&yaml).unwrap();
        validate(&p, &ValidationContext::default())
            .issues
            .into_iter()
            .filter(|i| matches!(i.code, "V249" | "V250"))
            .map(|i| (i.code.to_string(), i.severity))
            .collect()
    }

    const PLAIN: &str = "  - { id: w, type: script, script: s.sh, runner: sh }\n";

    #[test]
    fn v249_probation_bounds() {
        for n in [0, 101] {
            let got = issues(
                &format!("unattended:\n  after_attended_successes: {n}\n"),
                PLAIN,
            );
            assert!(
                got.iter()
                    .any(|(c, s)| *c == "V249" && *s == Severity::Error),
                "{n}: {got:?}"
            );
        }
        let ok = issues("unattended:\n  after_attended_successes: 3\n", PLAIN);
        assert!(ok.iter().all(|(c, ..)| *c != "V249"), "{ok:?}");
    }

    /// V250 fires only for a playbook that declares the top-level
    /// `unattended:` block (it says it means to run alone); the readiness
    /// report lists the gap for every playbook.
    #[test]
    fn v250_only_with_an_unattended_block() {
        let unchecked = "  - { id: w, type: agent_task, prompt: P, profile: x }\n";
        let with = issues("unattended:\n  after_attended_successes: 3\n", unchecked);
        assert!(
            with.iter()
                .any(|(c, s)| *c == "V250" && *s == Severity::Warning),
            "{with:?}"
        );
        let without = issues("", unchecked);
        assert!(without.iter().all(|(c, ..)| *c != "V250"), "{without:?}");
        // A checked actor is never warned.
        let checked = "  - { id: w, type: script, script: s.sh, runner: sh, success_check: { script: c.sh } }\n";
        let ok = issues("unattended:\n  after_attended_successes: 3\n", checked);
        assert!(ok.iter().all(|(c, ..)| *c != "V250"), "{ok:?}");
    }
}
