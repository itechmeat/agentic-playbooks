//! The follow-up check of a version's runs, for `apb stats`: how many of
//! its runs' follow-ups were read and held, did not hold, could not be
//! read, were missed, and are still ahead of their time.
//!
//! The met rate is over observed runs only, and `unknown` is part of that
//! denominator: a run whose source could not be read was observed as
//! unknown, never as met. A missed read (past its grace) and a pending one
//! stay outside the rate altogether.

use apb_core::schema::Playbook;
use serde::Serialize;

use crate::event::{Event, EventPayload};

#[cfg(test)]
use crate::run_stats::StatsRun;

/// One follow-up check over the runs of a version.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FollowUpStats {
    pub check: String,
    /// Runs whose check was read and checked: `met`, `not_met` and
    /// `unknown` together, the denominator of the met rate.
    pub observed: u32,
    pub met: u32,
    pub not_met: u32,
    /// Runs whose verdict is unknown, or whose source could not be read.
    pub unknown: u32,
    /// Runs read past their grace; never in the met rate.
    pub missed: u32,
    /// Runs still ahead of their due time.
    pub pending: u32,
}

/// The follow-up met rates of a `--compare` pair.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct MetRate {
    pub base: f64,
    pub against: f64,
}

/// `met / observed` of one version, rounded to four decimals; `None` when
/// no run of the version was observed.
pub(super) fn met_rate_of(stats: &FollowUpStats) -> Option<f64> {
    (stats.observed > 0).then(|| (stats.met as f64 / stats.observed as f64 * 1e4).round() / 1e4)
}

/// The met rates of both versions; `None` when either side has no
/// observed run.
pub(super) fn met_rate_pair(
    base: Option<&FollowUpStats>,
    against: Option<&FollowUpStats>,
) -> Option<MetRate> {
    Some(MetRate {
        base: met_rate_of(base?)?,
        against: met_rate_of(against?)?,
    })
}

/// Accumulates the runs of one version.
#[derive(Default)]
pub(super) struct Acc {
    check: String,
    seen: bool,
    met: u32,
    not_met: u32,
    unknown: u32,
    missed: u32,
    pending: u32,
}

impl Acc {
    /// Adds one run: its playbook snapshot, its journal, and whether the
    /// run is production (only a production run schedules a follow-up).
    pub(super) fn add(&mut self, production: bool, playbook: Option<&Playbook>, events: &[Event]) {
        let Some(block) = playbook.and_then(|p| p.follow_up.as_ref()) else {
            return;
        };
        self.seen = true;
        self.check = block.check.id.clone();
        match crate::follow_up::state(playbook, events, production) {
            crate::follow_up::FollowUpState::None => {}
            crate::follow_up::FollowUpState::Pending { .. } => self.pending += 1,
            crate::follow_up::FollowUpState::Observed => {
                // The read that settled it: at most one event per run, and
                // the latest one is the run's.
                let Some(e) = events
                    .iter()
                    .rev()
                    .find(|e| matches!(e.payload, EventPayload::FollowUpObserved { .. }))
                else {
                    return;
                };
                let EventPayload::FollowUpObserved {
                    status, verdict, ..
                } = &e.payload
                else {
                    return;
                };
                match status.as_str() {
                    "observed" => {
                        self.met += u32::from(verdict == apb_core::baseline::Verdict::Met.as_str());
                        self.not_met +=
                            u32::from(verdict == apb_core::baseline::Verdict::NotMet.as_str());
                        self.unknown += u32::from(
                            verdict != apb_core::baseline::Verdict::Met.as_str()
                                && verdict != apb_core::baseline::Verdict::NotMet.as_str(),
                        );
                    }
                    "unknown" => self.unknown += 1,
                    "missed" => self.missed += 1,
                    _ => {}
                }
            }
        }
    }

    /// The version's follow-up row; `None` when no run's snapshot
    /// declares a `follow_up` block.
    pub(super) fn finish(self) -> Option<FollowUpStats> {
        self.seen.then(|| FollowUpStats {
            check: self.check,
            observed: self.met + self.not_met + self.unknown,
            met: self.met,
            not_met: self.not_met,
            unknown: self.unknown,
            missed: self.missed,
            pending: self.pending,
        })
    }
}

/// The text lines `apb stats` prints for a version's follow-up. The check
/// name a journal or snapshot carries is shown with any hidden character
/// replaced, so a file written by hand cannot drive the terminal.
pub(super) fn lines(stats: &FollowUpStats) -> Vec<String> {
    use apb_core::text::visible;
    let met = match stats.observed {
        0 => format!("met {}/{}", stats.met, stats.observed),
        _ => format!(
            "met {}/{} ({:.0}%)",
            stats.met,
            stats.observed,
            stats.met as f64 / stats.observed as f64 * 100.0
        ),
    };
    vec![format!(
        "follow-up {}: {met}, unknown {}, missed {}, pending {}",
        visible(&stats.check),
        stats.unknown,
        stats.missed,
        stats.pending
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A playbook whose follow-up reads a script one minute after the run.
    const PLAYBOOK: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nfollow_up:\n  after: 1m\n  check:\n    id: deal_won\n    description: the deal moved\n    script: scripts/check.sh\n    observe:\n      - { id: stage, type: field, path: stage, expect: { equals: won } }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

    const PLAIN: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

    fn ev(seq: u64, ts: u128, v: serde_json::Value) -> Event {
        Event {
            seq,
            ts,
            payload: serde_json::from_value(v).unwrap(),
        }
    }

    /// A production run of `p` that succeeded at ts 2_000, with `body`
    /// after it.
    fn run(
        id: &str,
        version: &str,
        snapshot: &str,
        body: Vec<(u128, serde_json::Value)>,
    ) -> StatsRun {
        let mut events = vec![ev(
            0,
            1_000,
            json!({"type": "run_started", "playbook": "p", "version": version}),
        )];
        for (i, (ts, v)) in body.into_iter().enumerate() {
            events.push(ev(i as u64 + 1, ts, v));
        }
        StatsRun {
            run_id: id.into(),
            playbook: "p".into(),
            version: version.into(),
            events,
            snapshot: Some(apb_core::schema::Playbook::from_yaml(snapshot).unwrap()),
            candidate_trial: false,
            expected_models: Default::default(),
            execution: Some("cli".into()),
            node_profiles: Default::default(),
            grades: Vec::new(),
            definitions: Default::default(),
            diagnostics: Vec::new(),
            origin: crate::manifest::RunOrigin::Production,
            identities: Vec::new(),
            declared: Default::default(),
            ephemeral_nodes: Default::default(),
        }
    }

    fn succeeded() -> (u128, serde_json::Value) {
        (
            2_000,
            json!({"type": "run_finished", "outcome": "succeeded"}),
        )
    }

    fn observed_event(status: &str, verdict: &str) -> (u128, serde_json::Value) {
        (
            3_000,
            json!({
                "type": "follow_up_observed",
                "check": "deal_won",
                "status": status,
                "verdict": verdict,
                "results": [
                    {"observation": "stage", "rule": "equals won", "verdict": verdict}
                ],
                "finished_seq": 2,
                "due_at_ms": 62_000,
                "observed_at_ms": 63_000
            }),
        )
    }

    fn add_all(runs: &[StatsRun]) -> FollowUpStats {
        let mut acc = Acc::default();
        for r in runs {
            acc.add(r.origin.is_production(), r.snapshot.as_ref(), &r.events);
        }
        acc.finish().expect("a run declares the block")
    }

    /// The met rate counts met runs over the observed ones only: an
    /// unknown verdict is in the denominator, never the numerator.
    #[test]
    fn the_met_rate_is_over_observed_only() {
        let runs = [
            run(
                "a",
                "1.0.0",
                PLAYBOOK,
                vec![succeeded(), observed_event("observed", "met")],
            ),
            run(
                "b",
                "1.0.0",
                PLAYBOOK,
                vec![succeeded(), observed_event("observed", "met")],
            ),
            run(
                "c",
                "1.0.0",
                PLAYBOOK,
                vec![succeeded(), observed_event("observed", "not_met")],
            ),
            run(
                "d",
                "1.0.0",
                PLAYBOOK,
                vec![succeeded(), observed_event("observed", "unknown")],
            ),
        ];
        let stats = add_all(&runs);
        assert_eq!(stats.check, "deal_won");
        assert_eq!(stats.observed, 4);
        assert_eq!((stats.met, stats.not_met, stats.unknown), (2, 1, 1));
        assert_eq!((stats.missed, stats.pending), (0, 0));
        assert_eq!(met_rate_of(&stats), Some(0.5));
    }

    /// A source that could not be read and a read past its grace never
    /// count as met; the missed one stays out of the denominator.
    #[test]
    fn unknown_and_missed_are_counted_apart_and_never_as_met() {
        let unknown_run = run(
            "a",
            "1.0.0",
            PLAYBOOK,
            vec![succeeded(), observed_event("unknown", "unknown")],
        );
        let stats = add_all(std::slice::from_ref(&unknown_run));
        assert_eq!(stats.met, 0);
        assert_eq!(stats.unknown, 1);
        assert_eq!(
            stats.observed, 1,
            "an unreadable source was observed as unknown"
        );

        let missed = run(
            "b",
            "1.0.0",
            PLAYBOOK,
            vec![succeeded(), observed_event("missed", "unknown")],
        );
        let stats = add_all(std::slice::from_ref(&missed));
        assert_eq!(stats.met, 0);
        assert_eq!(stats.missed, 1);
        assert_eq!(stats.observed, 0, "a missed read is not an observed one");
        assert_eq!(met_rate_of(&stats), None);
    }

    /// A run still ahead of its time is pending.
    #[test]
    fn pending_is_counted_from_pending_states() {
        let pending = run("a", "1.0.0", PLAYBOOK, vec![succeeded()]);
        let stats = add_all(std::slice::from_ref(&pending));
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.observed, 0);
        assert_eq!(met_rate_of(&stats), None);
    }

    /// A run whose playbook declares no block gives no field at all.
    #[test]
    fn no_block_gives_no_field() {
        let plain = run("a", "1.0.0", PLAIN, vec![succeeded()]);
        let mut acc = Acc::default();
        acc.add(true, plain.snapshot.as_ref(), &plain.events);
        assert_eq!(acc.finish(), None);
        assert_eq!(Acc::default().finish(), None);
    }

    /// The compare rate needs an observed run on both sides.
    #[test]
    fn the_compare_rate_needs_both_sides_observed() {
        let observed = FollowUpStats {
            check: "deal_won".into(),
            observed: 4,
            met: 3,
            ..Default::default()
        };
        let unobserved = FollowUpStats {
            check: "deal_won".into(),
            observed: 0,
            pending: 2,
            ..Default::default()
        };
        let rate = met_rate_pair(Some(&observed), Some(&observed)).unwrap();
        assert_eq!((rate.base, rate.against), (0.75, 0.75));
        assert_eq!(met_rate_pair(Some(&observed), None), None);
        assert_eq!(met_rate_pair(Some(&observed), Some(&unobserved)), None);
    }

    /// The text line names the check and counts every bucket.
    #[test]
    fn the_text_line_names_the_check_and_every_bucket() {
        let stats = FollowUpStats {
            check: "deal_won".into(),
            observed: 4,
            met: 3,
            not_met: 0,
            unknown: 1,
            missed: 0,
            pending: 2,
        };
        assert_eq!(
            lines(&stats),
            vec!["follow-up deal_won: met 3/4 (75%), unknown 1, missed 0, pending 2"]
        );
        let none = FollowUpStats {
            check: "deal_won".into(),
            missed: 1,
            ..Default::default()
        };
        assert_eq!(
            lines(&none),
            vec!["follow-up deal_won: met 0/0, unknown 0, missed 1, pending 0"]
        );
    }

    /// The report wiring: the version carries the field and the compare
    /// pair carries the rate.
    #[test]
    fn the_report_carries_the_field_and_the_compare_rate() {
        let runs = vec![
            run(
                "a",
                "1.0.0",
                PLAYBOOK,
                vec![succeeded(), observed_event("observed", "met")],
            ),
            run(
                "b",
                "1.0.0",
                PLAYBOOK,
                vec![succeeded(), observed_event("observed", "not_met")],
            ),
            run(
                "c",
                "1.1.0",
                PLAYBOOK,
                vec![succeeded(), observed_event("observed", "met")],
            ),
        ];
        let filter = crate::run_stats::StatsFilter {
            playbook: Some("p".into()),
            since_ms: None,
            compare: Some("1.0.0".into()),
        };
        let report = crate::run_stats::build(&runs, &filter);
        let base = report
            .versions
            .iter()
            .find(|v| v.version == "1.0.0")
            .expect("the base version");
        assert_eq!(
            base.follow_up
                .as_ref()
                .map(|f| (f.observed, f.met, f.not_met)),
            Some((2, 1, 1))
        );
        let compare = report.compare.expect("a comparison");
        let rate = compare.follow_up_met_rate.expect("both sides observed");
        assert_eq!((rate.base, rate.against), (0.5, 1.0));

        let plain_runs = vec![run("a", "1.0.0", PLAIN, vec![succeeded()])];
        let report = crate::run_stats::build(&plain_runs, &filter);
        assert!(report.versions[0].follow_up.is_none());
    }
}
