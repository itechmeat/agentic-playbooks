//! The follow-up check of an ended run: when it is due
//! ([`state`] and [`arm_plan`]), and who reads it ([`read`] and
//! [`waker`]).
//!
//! The schema is [`apb_core::follow_up`]. The fold reads the run's
//! journal: a follow-up exists for a production run whose latest end is a
//! success, until one `follow_up_observed` exists.

pub mod read;
pub mod waker;

use apb_core::follow_up::FollowUp;

use std::path::Path;

use crate::error::EngineError;
use crate::event::{Event, EventPayload};

/// What a run's follow-up is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowUpState {
    /// No `follow_up` in the run's playbook snapshot, or the run's latest
    /// end is not a success, or the run is not production.
    None,
    /// Due at `due_at_ms`, for the `run_finished` at `finished_seq`.
    Pending {
        finished_seq: u64,
        due_at_ms: u64,
        grace_ms: u64,
    },
    /// One `follow_up_observed` exists.
    Observed,
}

/// The test-only `after` override (a release build ignores it), exactly
/// like the notify retry gap.
fn after_override_ms() -> Option<u64> {
    const ENV: &str = "APB_FOLLOW_UP_AFTER_MS";
    crate::env_duration::test_override(std::env::var(ENV).ok().as_deref())
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(0))
}

/// The follow-up state of the run whose journal is `events`, whose
/// playbook snapshot is `playbook` (`None`: no snapshot, no follow-up),
/// and whose manifest origin is production when `production`.
pub fn state(
    playbook: Option<&apb_core::schema::Playbook>,
    events: &[Event],
    production: bool,
) -> FollowUpState {
    let Some(follow_up) = playbook.and_then(|p| p.follow_up.as_ref()) else {
        return FollowUpState::None;
    };
    if !production {
        return FollowUpState::None;
    }
    if events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::FollowUpObserved { .. }))
    {
        return FollowUpState::Observed;
    }
    // The latest end: the last `run_finished`, and it must be a success
    // with no `run_resumed` after it (a resumed-and-unfinished run has no
    // follow-up).
    let Some(finished) = events.iter().rev().find(|e| {
        matches!(
            e.payload,
            EventPayload::RunFinished { .. } | EventPayload::RunResumed { .. }
        )
    }) else {
        return FollowUpState::None;
    };
    let EventPayload::RunFinished { outcome } = &finished.payload else {
        return FollowUpState::None;
    };
    if outcome != "succeeded" {
        return FollowUpState::None;
    }
    let Some(after_ms) = after_seconds_ms(follow_up) else {
        return FollowUpState::None;
    };
    let grace_ms = u64::try_from(
        (follow_up
            .grace_seconds()
            .unwrap_or(apb_core::follow_up::DEFAULT_GRACE_SECONDS) as u128)
            * 1000,
    )
    .unwrap_or(u64::MAX);
    FollowUpState::Pending {
        finished_seq: finished.seq,
        due_at_ms: u64::try_from(finished.ts)
            .unwrap_or(0)
            .saturating_add(after_ms),
        grace_ms,
    }
}

/// The `after` of `follow_up` in milliseconds: the test override when this
/// build honours it, else the parsed duration.
fn after_seconds_ms(follow_up: &FollowUp) -> Option<u64> {
    if let Some(ms) = after_override_ms() {
        return Some(ms);
    }
    let seconds = follow_up.after_seconds()?;
    u64::try_from(seconds as u128 * 1000).ok()
}

/// Whether a detached waker should be armed for the run: `Some` for a
/// pending follow-up, `None` otherwise.
pub fn arm_plan(
    playbook: Option<&apb_core::schema::Playbook>,
    events: &[Event],
    production: bool,
) -> Option<crate::waker::WakerFor> {
    match state(playbook, events, production) {
        FollowUpState::Pending { finished_seq, .. } => {
            Some(crate::waker::WakerFor::FollowUp(finished_seq))
        }
        _ => None,
    }
}

/// Arms the run's follow-up, as a succeeded drive ends and as `apb resume
/// --all` passes a finished run: when the state is `Pending`, spawns `apb
/// __pause-waker --follow-up <seq>` (none while a live waker holds its
/// lease). The manifest says whether the run is production, and a run
/// directory this machine did not create is refused first, as
/// `observe_due` refuses it, so no waker is left waiting on it.
pub fn arm(root: &Path, run_dir: &Path, run_id: &str) -> Result<(), EngineError> {
    let events = crate::event::read_journal(run_dir)?.events;
    let snapshot = crate::legacy_snapshot::load_run_playbook(run_dir);
    let production =
        crate::manifest::RunOrigin::of_read(&crate::manifest::read(run_dir)).runs_follow_up();
    let Some(wait) = arm_plan(snapshot.as_ref(), &events, production) else {
        return Ok(());
    };
    if crate::gate::check_resume(root, run_id, true).is_err() {
        return Ok(());
    }
    crate::waker::spawn(root, run_id, wait);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::schema::Playbook;

    /// A playbook whose follow-up reads a connector after 1m.
    const PLAYBOOK: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: main }\nfollow_up:\n  after: 1m\n  check:\n    id: deal_won\n    description: the deal moved\n    connector: { name: crm, account: default, function: get_deal, args: {} }\n    observe:\n      - { id: stage, type: field, path: deal.stage, expect: { equals: won } }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

    fn playbook() -> Playbook {
        Playbook::from_yaml(PLAYBOOK).unwrap()
    }

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: 1_700_000_000_000u128 + u128::from(seq) * 1000,
            payload,
        }
    }

    fn finished(seq: u64, outcome: &str) -> Event {
        ev(
            seq,
            EventPayload::RunFinished {
                outcome: outcome.into(),
            },
        )
    }

    fn started() -> EventPayload {
        EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
            definition: None,
            external_data: false,
        }
    }

    /// A succeeded end gives Pending, due at the `run_finished` ts plus
    /// `after`.
    #[test]
    fn a_succeeded_end_is_pending_due_after_the_finish() {
        let pb = playbook();
        let events = vec![ev(1, started()), finished(2, "succeeded")];
        let s = state(Some(&pb), &events, true);
        let FollowUpState::Pending {
            finished_seq,
            due_at_ms,
            grace_ms,
        } = s
        else {
            panic!("not pending: {s:?}");
        };
        assert_eq!(finished_seq, 2);
        // after 1m over ts 1700000002000.
        assert_eq!(due_at_ms, 1_700_000_062_000);
        assert_eq!(grace_ms, 24 * 3600 * 1000);
        assert_eq!(
            arm_plan(Some(&pb), &events, true),
            Some(crate::waker::WakerFor::FollowUp(2))
        );
    }

    /// A run that processes external data still schedules and shows its
    /// own follow-up (it is part of the run), from the manifest the
    /// readers read; a scenario or eval run does not.
    #[test]
    fn a_flagged_run_schedules_its_follow_up() {
        use crate::manifest::{RunExecutionManifest, RunOrigin};
        let pb = playbook();
        let events = vec![ev(1, started()), finished(2, "succeeded")];
        let dir = tempfile::tempdir().unwrap();
        let m = RunExecutionManifest {
            external_data: true,
            origin: Some(RunOrigin::ExternalData),
            ..Default::default()
        };
        crate::manifest::write(dir.path(), &m).unwrap();
        let follows = RunOrigin::of_read(&crate::manifest::read(dir.path())).runs_follow_up();
        assert!(follows);
        assert_eq!(
            arm_plan(Some(&pb), &events, follows),
            Some(crate::waker::WakerFor::FollowUp(2))
        );
        assert!(!RunOrigin::Eval.runs_follow_up());
    }

    /// A failed end is None, and no waker is armed.
    #[test]
    fn a_failed_end_is_none() {
        let pb = playbook();
        let events = vec![ev(1, started()), finished(2, "failed")];
        assert_eq!(state(Some(&pb), &events, true), FollowUpState::None);
        assert_eq!(arm_plan(Some(&pb), &events, true), None);
    }

    /// No block in the snapshot is None.
    #[test]
    fn no_block_is_none() {
        let events = vec![ev(1, started()), finished(2, "succeeded")];
        assert_eq!(state(None, &events, true), FollowUpState::None);
    }

    /// A `run_resumed` after the success makes it None (the waker ends).
    #[test]
    fn a_resume_after_the_end_is_none() {
        let pb = playbook();
        let events = vec![
            ev(1, started()),
            finished(2, "succeeded"),
            ev(3, EventPayload::resumed("done", None, false)),
        ];
        assert_eq!(state(Some(&pb), &events, true), FollowUpState::None);
        assert_eq!(arm_plan(Some(&pb), &events, true), None);
    }

    /// An observed follow-up is Observed, never re-armed.
    #[test]
    fn an_observed_one_is_observed() {
        let pb = playbook();
        let events = vec![
            ev(1, started()),
            finished(2, "succeeded"),
            ev(
                3,
                EventPayload::FollowUpObserved {
                    check: "deal_won".into(),
                    status: "missed".into(),
                    reason: None,
                    observations: Default::default(),
                    results: Vec::new(),
                    verdict: "unknown".into(),
                    finished_seq: 2,
                    due_at_ms: 0,
                    observed_at_ms: 0,
                    input_digest: String::new(),
                },
            ),
        ];
        assert_eq!(state(Some(&pb), &events, true), FollowUpState::Observed);
        assert_eq!(arm_plan(Some(&pb), &events, true), None);
    }

    /// A scenario or eval origin is not production: None.
    #[test]
    fn not_production_is_none() {
        let pb = playbook();
        let events = vec![ev(1, started()), finished(2, "succeeded")];
        assert_eq!(state(Some(&pb), &events, false), FollowUpState::None);
    }
}
