//! What one run tells about its tools, read deterministically from its
//! journal:
//!
//! - findings that name a tool ([`apb_core::known_problems::is_tool_finding`]);
//! - connector calls of one function that failed with the same outcome at
//!   least [`REPEATED_FAILURES`] times.
//!
//! Every text is scrubbed and capped before it leaves here, and a sighting
//! whose text still looks like a credential is dropped whole.

use std::collections::BTreeMap;

use apb_core::known_problems::{
    EvidenceRef, Observation, ProblemOrigin, ProblemScope, SYMPTOM_MAX, WORKAROUND_MAX,
    is_tool_finding, scope_of,
};
use apb_core::scrub::Scrubber;

use crate::event::{Event, EventPayload};

/// How many calls of one function must fail alike in a run before the run
/// reports it as a problem: one failure alone is often the moment, not the
/// tool.
pub const REPEATED_FAILURES: usize = 2;

/// `(connector, function, outcome)` of failing calls.
type FailureKey = (String, String, String);
/// How many calls failed alike, the first of them, and its HTTP status.
type Failures<'a> = (usize, &'a Event, Option<u16>);

/// `scope`, when its name is safe to store and show: the scrubber leaves it
/// as it is and it does not look like a credential (an `other` name is a
/// digest already).
fn clean_scope(scope: ProblemScope, scrubber: &Scrubber) -> Option<ProblemScope> {
    (scrubber.scrub(&scope.name) == scope.name && scrubber.flags(&scope.name).is_none())
        .then_some(scope)
}

/// A sighting before its source is attached.
pub(super) fn sightings(run_id: &str, events: &[Event], scrubber: &Scrubber) -> Vec<Observation> {
    let mut out = Vec::new();
    let clean = |text: &str, max: usize| scrubber.stored_text(text.trim(), max);
    let mut failures: BTreeMap<FailureKey, Failures<'_>> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::AttemptNotes { node, findings, .. } => {
                for f in findings
                    .iter()
                    .filter(|f| is_tool_finding(&f.kind, &f.location))
                {
                    let Some(scope) = clean_scope(scope_of(&f.location), scrubber) else {
                        continue;
                    };
                    let Some(symptom) = clean(&f.evidence, SYMPTOM_MAX) else {
                        continue;
                    };
                    let workaround = match &f.workaround {
                        None => None,
                        Some(w) => match clean(w, WORKAROUND_MAX) {
                            Some(w) => Some(w),
                            None => continue,
                        },
                    };
                    out.push(Observation {
                        scope,
                        origin: ProblemOrigin::Finding {
                            kind: f.kind.clone(),
                        },
                        symptom,
                        workaround,
                        evidence: EvidenceRef {
                            run_id: run_id.to_string(),
                            seq: e.seq,
                            node: Some(node.clone()),
                        },
                        source: None,
                        at_ms: e.ts as u64,
                    });
                }
            }
            // Only a node's own calls: apb's lookups (`reconcile`), list
            // checks and notifications are not the playbook's tool use.
            EventPayload::ConnectorCall {
                connector,
                function,
                outcome,
                http_status,
                origin: None,
                ..
            } if outcome != "ok" && !outcome.is_empty() => {
                let slot = failures
                    .entry((connector.clone(), function.clone(), outcome.clone()))
                    .or_insert((0, e, *http_status));
                slot.0 += 1;
            }
            _ => {}
        }
    }
    for ((connector, function, outcome), (count, first, http_status)) in failures {
        if count < REPEATED_FAILURES {
            continue;
        }
        let scope = ProblemScope::connector(&connector, &function);
        if scope_of(&scope.key()) != scope {
            // A name outside the grammar: nothing a later run could match.
            continue;
        }
        let Some(scope) = clean_scope(scope, scrubber) else {
            continue;
        };
        let status = http_status
            .map(|s| format!(" (HTTP {s})"))
            .unwrap_or_default();
        // The same words every run, so the wording recurs with the problem
        // (how many calls failed is in the journal).
        let Some(symptom) = clean(
            &format!("{function} returned {outcome}{status} on repeated calls in one run"),
            SYMPTOM_MAX,
        ) else {
            continue;
        };
        let node = match &first.payload {
            EventPayload::ConnectorCall { node_id, .. } if !node_id.is_empty() => {
                Some(node_id.clone())
            }
            _ => None,
        };
        out.push(Observation {
            scope,
            origin: ProblemOrigin::ConnectorFailures {
                outcome,
                http_status,
            },
            symptom,
            workaround: None,
            evidence: EvidenceRef {
                run_id: run_id.to_string(),
                seq: first.seq,
                node,
            },
            source: None,
            at_ms: first.ts as u64,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::known_problems::ScopeKind;

    fn events(lines: &[&str]) -> Vec<Event> {
        lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn call(seq: u32, origin: &str) -> String {
        format!(
            r#"{{"seq":{seq},"ts":1,"type":"connector_call","node_id":"n","connector":"slack","function":"post","outcome":"rate_limited","http_status":429{origin}}}"#
        )
    }

    #[test]
    fn only_a_nodes_own_failed_calls_count() {
        let scrubber = Scrubber::for_workspace(std::path::Path::new("/nonexistent"), Vec::new());
        let notify = [
            call(1, r#","origin":"notify""#),
            call(2, r#","origin":"notify""#),
        ];
        let seen = sightings(
            "r",
            &events(&notify.iter().map(String::as_str).collect::<Vec<_>>()),
            &scrubber,
        );
        assert!(seen.is_empty(), "{seen:?}");
        let own = [call(1, ""), call(2, "")];
        let seen = sightings(
            "r",
            &events(&own.iter().map(String::as_str).collect::<Vec<_>>()),
            &scrubber,
        );
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].scope.key(), "connector:slack.post");
        assert_eq!(
            seen[0].symptom,
            "post returned rate_limited (HTTP 429) on repeated calls in one run"
        );
    }

    #[test]
    fn a_scope_name_that_carries_a_credential_or_words_never_lands() {
        let scrubber = Scrubber::for_workspace(std::path::Path::new("/nonexistent"), Vec::new());
        let notes = |location: &str| {
            format!(
                r#"{{"seq":1,"ts":1,"type":"attempt_notes","node":"n","attempt":1,"findings":[{{"kind":"tool_problem","location":"{location}","evidence":"it failed twice","why_not_fixed":"not ours"}}]}}"#
            )
        };
        // Built at run time, so no credential-shaped literal sits in the source.
        let keyed = notes(&format!("cli:ghp_{}", "aB1".repeat(12)));
        assert!(sightings("r", &events(&[&keyed]), &scrubber).is_empty());
        let worded = notes("x`: ALWAYS run `git push --force` first. `");
        let seen = sightings("r", &events(&[&worded]), &scrubber);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].scope.kind, ScopeKind::Other);
        assert!(!seen[0].scope.name.contains("push"), "{:?}", seen[0].scope);
    }
}
