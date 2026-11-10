//! The run's follow-up check as the run surfaces show it: the check's
//! name, source and status (`pending`, `observed`, `unknown`, `missed`, or
//! `not_scheduled`), and, once read, the verdict, the reason and one row
//! per result. Read from the journal only
//! ([`crate::follow_up::state`]); nothing here reads a source. The
//! details come from the journal, where the read scrubbed them, and every
//! surface redacts them again with the run redactor, exactly like the
//! baseline block's.

use apb_core::schema::Playbook;
use serde::Serialize;

use crate::event::{Event, EventPayload};
use crate::follow_up::FollowUpState;

/// The follow-up was scheduled but the run did not succeed: the status of
/// a block the run never armed.
pub const NOT_SCHEDULED: &str = "not_scheduled";

/// The follow-up's check, its status and, once read, what it found.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunFollowUp {
    pub check: String,
    pub description: String,
    /// `connector` or `script`.
    pub source: String,
    /// `pending`, `observed`, `unknown`, `missed`, or `not_scheduled`
    /// (the run did not succeed).
    pub status: String,
    /// When the check was due (a pending one).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub due_at_ms: Option<u64>,
    /// When the check was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub observed_at_ms: Option<u64>,
    /// `met`, `not_met` or `unknown`; absent while pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub verdict: Option<String>,
    /// Why the check could not be read, or why it was missed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<String>,
    pub results: Vec<FollowUpResultView>,
}

/// One observation's outcome of the read.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FollowUpResultView {
    pub observation: String,
    /// The rule as journaled (`equals won`).
    pub rule: String,
    /// `met`, `not_met` or `unknown`.
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub detail: Option<String>,
}

/// The follow-up of the run's playbook snapshot with what `events`
/// journaled; `None` when the snapshot declares no `follow_up` block, so
/// a run without one shows no block at all. `production` is the run
/// manifest's origin: a run that is not production never scheduled its
/// check. The result details come from the journal, where the read
/// scrubbed them; the surfaces redact them again on the way out.
pub fn follow_up_from(
    playbook: Option<&Playbook>,
    events: &[Event],
    production: bool,
) -> Option<RunFollowUp> {
    let block = playbook?.follow_up.as_ref()?;
    let base = RunFollowUp {
        check: block.check.id.clone(),
        description: block.check.description.clone(),
        source: block.check.source().map_or("", |s| s.kind()).to_string(),
        status: NOT_SCHEDULED.to_string(),
        due_at_ms: None,
        observed_at_ms: None,
        verdict: None,
        reason: None,
        results: Vec::new(),
    };
    match crate::follow_up::state(playbook, events, production) {
        FollowUpState::None => Some(base),
        FollowUpState::Pending { due_at_ms, .. } => Some(RunFollowUp {
            status: "pending".to_string(),
            due_at_ms: Some(due_at_ms),
            ..base
        }),
        FollowUpState::Observed => {
            let read = events.iter().rev().find_map(|e| match &e.payload {
                EventPayload::FollowUpObserved {
                    check,
                    status,
                    reason,
                    results,
                    verdict,
                    due_at_ms,
                    observed_at_ms,
                    ..
                } => Some((
                    check.clone(),
                    status.clone(),
                    reason.clone(),
                    results.clone(),
                    verdict.clone(),
                    *due_at_ms,
                    *observed_at_ms,
                )),
                _ => None,
            });
            let Some((check, status, reason, results, verdict, due, at)) = read else {
                return Some(base);
            };
            Some(RunFollowUp {
                check,
                status,
                due_at_ms: (due > 0).then_some(due),
                observed_at_ms: (at > 0).then_some(at),
                verdict: (!verdict.is_empty()).then_some(verdict),
                reason,
                results: results
                    .iter()
                    .map(|r| FollowUpResultView {
                        observation: r.observation.clone(),
                        rule: r.rule.clone(),
                        verdict: r.verdict.clone(),
                        detail: r.detail.clone(),
                    })
                    .collect(),
                ..base
            })
        }
    }
}

impl RunFollowUp {
    /// The lines of the summary's `Follow-up` section: the due time while
    /// pending, one line per result once read, and the reason.
    pub fn section_items(&self) -> Vec<String> {
        match self.status.as_str() {
            "pending" => vec![format!(
                "pending: due {}",
                self.due_at_ms
                    .map_or_else(|| "unknown".to_string(), apb_core::clock::iso_utc)
            )],
            NOT_SCHEDULED => vec!["not scheduled: the run did not succeed".to_string()],
            _ => {
                let mut out: Vec<String> = self
                    .results
                    .iter()
                    .map(|r| {
                        let detail = r
                            .detail
                            .as_deref()
                            .filter(|d| !d.is_empty())
                            .map(|d| format!(" ({d})"))
                            .unwrap_or_default();
                        format!("{}: {} on {}{detail}", r.verdict, r.rule, r.observation)
                    })
                    .collect();
                if let Some(reason) = self.reason.as_deref().filter(|r| !r.is_empty()) {
                    out.push(format!("reason: {reason}"));
                }
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventPayload;

    /// A playbook whose follow-up reads a script one minute after the run.
    const PLAYBOOK: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nfollow_up:\n  after: 1m\n  check:\n    id: deal_won\n    description: the deal moved\n    script: scripts/check.sh\n    observe:\n      - { id: stage, type: field, path: stage, expect: { equals: won } }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

    const PLAIN: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

    fn playbook(yaml: &str) -> Playbook {
        Playbook::from_yaml(yaml).unwrap()
    }

    fn ev(seq: u64, ts: u128, payload: EventPayload) -> Event {
        Event { seq, ts, payload }
    }

    fn started(seq: u64) -> Event {
        ev(
            seq,
            1_700_000_000_000,
            EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
                definition: None,
                external_data: false,
            },
        )
    }

    fn finished(seq: u64, outcome: &str) -> Event {
        ev(
            seq,
            1_700_000_002_000,
            EventPayload::RunFinished {
                outcome: outcome.into(),
            },
        )
    }

    fn observed(
        seq: u64,
        status: &str,
        verdict: &str,
        results: Vec<crate::event::FollowUpResult>,
        reason: Option<&str>,
    ) -> Event {
        ev(
            seq,
            1_700_000_062_000,
            EventPayload::FollowUpObserved {
                check: "deal_won".into(),
                status: status.into(),
                reason: reason.map(str::to_string),
                observations: Default::default(),
                results,
                verdict: verdict.into(),
                finished_seq: 2,
                due_at_ms: 1_700_000_062_000,
                observed_at_ms: 1_700_000_062_000,
                input_digest: String::new(),
            },
        )
    }

    fn result(
        observation: &str,
        rule: &str,
        verdict: &str,
        detail: Option<&str>,
    ) -> crate::event::FollowUpResult {
        crate::event::FollowUpResult {
            observation: observation.into(),
            rule: rule.into(),
            verdict: verdict.into(),
            detail: detail.map(str::to_string),
        }
    }

    /// A pending follow-up shows its check and the due time, and no
    /// verdict yet.
    #[test]
    fn a_pending_follow_up_shows_the_due_time() {
        let pb = playbook(PLAYBOOK);
        let events = vec![started(1), finished(2, "succeeded")];
        let f = follow_up_from(Some(&pb), &events, true).expect("the block shows");
        assert_eq!(f.check, "deal_won");
        assert_eq!(f.description, "the deal moved");
        assert_eq!(f.source, "script");
        assert_eq!(f.status, "pending");
        assert_eq!(f.due_at_ms, Some(1_700_000_062_000));
        assert_eq!(f.observed_at_ms, None);
        assert_eq!(f.verdict, None);
        assert_eq!(f.reason, None);
        assert!(f.results.is_empty());
        // The wire shape: absent fields stay out.
        let json = serde_json::to_value(&f).unwrap();
        assert!(json.get("verdict").is_none());
        assert!(json.get("due_at_ms").is_some());
        assert_eq!(
            f.section_items(),
            vec![format!(
                "pending: due {}",
                apb_core::clock::iso_utc(1_700_000_062_000)
            )]
        );
    }

    /// An observed follow-up maps the journaled results one to one.
    #[test]
    fn an_observed_met_follow_up_shows_its_results() {
        let pb = playbook(PLAYBOOK);
        let events = vec![
            started(1),
            finished(2, "succeeded"),
            observed(
                3,
                "observed",
                "met",
                vec![result("stage", "equals won", "met", Some("won"))],
                None,
            ),
        ];
        let f = follow_up_from(Some(&pb), &events, true).unwrap();
        assert_eq!(f.status, "observed");
        assert_eq!(f.verdict.as_deref(), Some("met"));
        assert_eq!(f.observed_at_ms, Some(1_700_000_062_000));
        assert_eq!(f.due_at_ms, Some(1_700_000_062_000));
        assert_eq!(f.reason, None);
        assert_eq!(
            f.results,
            vec![FollowUpResultView {
                observation: "stage".into(),
                rule: "equals won".into(),
                verdict: "met".into(),
                detail: Some("won".into()),
            }]
        );
        assert_eq!(f.section_items(), vec!["met: equals won on stage (won)"]);
    }

    /// A follow-up read without a detail renders the bare line.
    #[test]
    fn a_result_without_a_detail_renders_without_one() {
        let pb = playbook(PLAYBOOK);
        let events = vec![
            started(1),
            finished(2, "succeeded"),
            observed(
                3,
                "observed",
                "not_met",
                vec![result("stage", "equals won", "not_met", None)],
                None,
            ),
        ];
        let f = follow_up_from(Some(&pb), &events, true).unwrap();
        assert_eq!(f.verdict.as_deref(), Some("not_met"));
        assert_eq!(f.section_items(), vec!["not_met: equals won on stage"]);
    }

    /// A not-met follow-up makes the run need a look; an unknown or
    /// missed one does not.
    #[test]
    fn a_not_met_follow_up_needs_a_look_but_an_unknown_or_missed_one_does_not() {
        let pb = playbook(PLAYBOOK);
        let not_met = vec![
            started(1),
            finished(2, "succeeded"),
            observed(
                3,
                "observed",
                "not_met",
                vec![result("stage", "equals won", "not_met", Some("lost"))],
                None,
            ),
        ];
        let look =
            crate::run_outcome::NeedsLook::from_events(Some(&pb), &not_met).expect("needs a look");
        assert!(
            look.reasons
                .contains(&crate::run_outcome::NeedsLookReason::FollowUp)
        );
        assert_eq!(look.follow_up, 1);
        assert!(look.line().contains("follow-up not met"));

        let unknown = vec![
            started(1),
            finished(2, "succeeded"),
            observed(
                3,
                "unknown",
                "unknown",
                Vec::new(),
                Some("the source failed"),
            ),
        ];
        assert_eq!(
            crate::run_outcome::NeedsLook::from_events(Some(&pb), &unknown),
            None,
            "an unknown follow-up is not a look on its own"
        );
        let missed = vec![
            started(1),
            finished(2, "succeeded"),
            observed(3, "missed", "unknown", Vec::new(), Some("read too late")),
        ];
        assert_eq!(
            crate::run_outcome::NeedsLook::from_events(Some(&pb), &missed),
            None,
            "a missed follow-up is not a look on its own"
        );
    }

    /// An unknown read shows the reason and no results.
    #[test]
    fn an_unknown_follow_up_shows_the_reason_and_no_results() {
        let pb = playbook(PLAYBOOK);
        let events = vec![
            started(1),
            finished(2, "succeeded"),
            observed(
                3,
                "unknown",
                "unknown",
                Vec::new(),
                Some("the source failed"),
            ),
        ];
        let f = follow_up_from(Some(&pb), &events, true).unwrap();
        assert_eq!(f.status, "unknown");
        assert_eq!(f.verdict.as_deref(), Some("unknown"));
        assert_eq!(f.reason.as_deref(), Some("the source failed"));
        assert!(f.results.is_empty());
        assert_eq!(f.section_items(), vec!["reason: the source failed"]);
    }

    /// A missed read shows its reason; nothing was read.
    #[test]
    fn a_missed_follow_up_shows_its_reason() {
        let pb = playbook(PLAYBOOK);
        let events = vec![
            started(1),
            finished(2, "succeeded"),
            observed(3, "missed", "unknown", Vec::new(), Some("read too late")),
        ];
        let f = follow_up_from(Some(&pb), &events, true).unwrap();
        assert_eq!(f.status, "missed");
        assert_eq!(f.verdict.as_deref(), Some("unknown"));
        assert_eq!(f.reason.as_deref(), Some("read too late"));
        assert!(f.results.is_empty());
    }

    /// A run whose latest end is not a success did not schedule the
    /// check, and the block still shows.
    #[test]
    fn a_run_that_did_not_succeed_shows_not_scheduled() {
        let pb = playbook(PLAYBOOK);
        let events = vec![started(1), finished(2, "failed")];
        let f = follow_up_from(Some(&pb), &events, true).unwrap();
        assert_eq!(f.status, "not_scheduled");
        assert_eq!(f.check, "deal_won");
        assert_eq!(f.due_at_ms, None);
        assert_eq!(f.verdict, None);
        let json = serde_json::to_value(&f).unwrap();
        assert!(json.get("due_at_ms").is_none());
        assert!(json.get("verdict").is_none());
    }

    /// No block, no snapshot: nothing shows at all.
    #[test]
    fn a_run_without_a_block_shows_none() {
        let pb = playbook(PLAIN);
        let events = vec![started(1), finished(2, "succeeded")];
        assert_eq!(follow_up_from(Some(&pb), &events, true), None);
        assert_eq!(follow_up_from(None, &events, true), None);
    }

    /// A not-production run shows the block as not scheduled (a scenario
    /// or eval run never reads its follow-up).
    #[test]
    fn a_not_production_run_shows_not_scheduled() {
        let pb = playbook(PLAYBOOK);
        let events = vec![started(1), finished(2, "succeeded")];
        let f = follow_up_from(Some(&pb), &events, false).unwrap();
        assert_eq!(f.status, "not_scheduled");
    }

    /// The summary's sections carry the Follow-up lines after Baseline.
    #[test]
    fn the_summary_sections_carry_the_follow_up_lines() {
        let pb = playbook(PLAYBOOK);
        let events = vec![
            started(1),
            finished(2, "succeeded"),
            observed(
                3,
                "observed",
                "not_met",
                vec![result("stage", "equals won", "not_met", Some("lost"))],
                None,
            ),
        ];
        let f = follow_up_from(Some(&pb), &events, true).unwrap();
        let summary = crate::run_outcome::OutcomeSummary {
            headline: "Run succeeded".into(),
            needs_look: None,
            done: Vec::new(),
            verified: Vec::new(),
            unverified: Vec::new(),
            findings: Vec::new(),
            warnings: Vec::new(),
            baseline: None,
            lists: Vec::new(),
            agent_summary: None,
            follow_up: Some(f.clone()),
        };
        let sections = summary.sections();
        let titles: Vec<&str> = sections.iter().map(|(t, _)| *t).collect();
        assert!(titles.contains(&"Follow-up"), "{titles:?}");
        let (i, _) = titles
            .iter()
            .enumerate()
            .find(|(_, t)| **t == "Follow-up")
            .unwrap();
        assert_eq!(sections[i].1, vec!["not_met: equals won on stage (lost)"]);
    }
}
