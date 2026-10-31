//! The drive's side of the baseline check (see [`crate::baseline`]): the
//! before snapshot ahead of the first node that can act (or, when no node
//! acted, at the finish right before the after snapshot), the after
//! snapshot and the comparisons at the finish, the patch rule, and the
//! before snapshot a `continued_from` successor inherits. Both snapshots
//! are interrupted by an abort or a `--now` pause like any node work, and
//! an interrupted snapshot journals nothing.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use apb_core::baseline::{
    BaselineCheck, BaselineSource, Comparison, Observation, Verdict, compare,
};
use apb_core::schema::Playbook;

use crate::baseline::read::{ReadCtx, read};
use crate::baseline::{BaselineState, ComparisonRecord, Snapshot, phase, project, status};
use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload, read_all};
use crate::run_config::RunConfig;
use crate::stop::{CancelFanout, FanoutGuard};

/// What a snapshot of this run needs.
pub(crate) struct BaselineRun<'a> {
    pub playbook: &'a Playbook,
    pub run_dir: &'a Path,
    pub root: &'a Path,
    pub workdir: &'a Path,
    pub cfg: &'a RunConfig,
    /// Every env var name an installed connector references.
    pub scrub: &'a [String],
}

/// What the finish's baseline step came to.
pub(crate) enum AfterOutcome {
    /// An abort or a `--now` pause interrupted a snapshot: nothing of it
    /// was journaled, and the finish must not complete.
    Interrupted,
    /// Every comparison is journaled; `Some` is why an enforced baseline
    /// fails the run.
    Compared(Option<String>),
}

/// A kill flag for one snapshot: an abort or a `--now` pause latches it.
pub(crate) fn snapshot_flag(fanout: &Arc<CancelFanout>) -> (Arc<AtomicBool>, FanoutGuard) {
    let flag = Arc::new(AtomicBool::new(false));
    let guard = fanout.register_pausable(&flag);
    (flag, guard)
}

impl BaselineRun<'_> {
    fn ctx<'b>(
        &'b self,
        phase: &'b str,
        cancel: &'b AtomicBool,
        events: &'b [Event],
        spawn_env: &'b [(String, String)],
    ) -> ReadCtx<'b> {
        ReadCtx {
            run_dir: self.run_dir,
            root: self.root,
            workdir: self.workdir,
            params: &self.cfg.params,
            spawn_env,
            scrub: self.scrub,
            cancel,
            phase,
            events,
            guard_tree: true,
            scripts_label: "baseline scripts",
        }
    }
}

/// Takes the before snapshot of every check that has none, before the first
/// node that can act runs. A check whose before is journaled is never read
/// again; one with none after an acting node started is journaled `missed`;
/// a `continued_from` successor inherits its predecessor's. A snapshot the
/// cancel flag interrupts journals nothing, so the next drive takes it, and
/// that is still honest because no acting node has run. Returns `false`
/// when the cancel flag interrupted it: the node must not start.
pub(crate) fn ensure_before(
    run: &BaselineRun<'_>,
    cancel: &AtomicBool,
    log: &mut EventLog,
) -> Result<bool, EngineError> {
    let checks = run.playbook.baseline_checks();
    if checks.is_empty() {
        return Ok(true);
    }
    let events = read_all(run.run_dir)?;
    let state = BaselineState::fold(run.playbook, &events);
    let spawn_env = run.cfg.spawn_env();
    for check in checks {
        if state.check(&check.id).is_some_and(|c| c.before.is_some()) {
            continue;
        }
        let snap = if state.acting_started {
            Snapshot::missed()
        } else if let Some(from) = run.cfg.continued_from.as_deref() {
            inherited(run.run_dir, from, &check.id)
        } else {
            let snap = read(
                check,
                &run.ctx(phase::BEFORE, cancel, &events, &spawn_env),
                None,
            );
            if cancel.load(Ordering::SeqCst) {
                return Ok(false);
            }
            snap
        };
        log.append(snap.event(&check.id, phase::BEFORE))?;
    }
    Ok(true)
}

/// The before snapshot of `check` in predecessor run `from`, labelled as
/// inherited from it. When the predecessor's journal cannot be read, or it
/// has no before snapshot of this check, the before is unknown: a fresh
/// read now would describe the tree halfway through the work.
fn inherited(run_dir: &Path, from: &str, check: &str) -> Snapshot {
    let unknown = |why: String| Snapshot {
        status: status::UNKNOWN.into(),
        reason: Some(why),
        inherited_from: Some(from.to_string()),
        ..Snapshot::default()
    };
    if !apb_core::registry::is_safe_segment(from) {
        return unknown(format!("the predecessor run id `{from}` is not a run id"));
    }
    let Some(dir) = run_dir.parent().map(|p| p.join(from)) else {
        return unknown("the predecessor run cannot be found".into());
    };
    let events = match crate::event::read_journal(&dir) {
        Ok(j) => j.events,
        Err(e) => {
            return unknown(format!("the predecessor run's journal cannot be read: {e}"));
        }
    };
    events
        .iter()
        .find_map(|e| match Snapshot::from_event(&e.payload) {
            Some((c, phase::BEFORE, mut snap)) if c == check => {
                snap.inherited_from = Some(from.to_string());
                Some(snap)
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            unknown("the predecessor run has no before snapshot of this check".into())
        })
}

/// The finish's baseline step: the before snapshot of every check that has
/// none yet (a run whose nodes never acted reaches the finish without one;
/// taken now, right before the after one, it still shows the state the
/// run left alone), then the after snapshot of every check (unless a valid
/// one is journaled) and every comparison the journal does not hold yet.
/// Returns why the run must fail when `baseline_check.enforce` is set and a
/// comparison is not met or unknown.
pub(crate) fn after_and_compare(
    run: &BaselineRun<'_>,
    cancel: &AtomicBool,
    log: &mut EventLog,
) -> Result<AfterOutcome, EngineError> {
    let Some(block) = &run.playbook.baseline_check else {
        return Ok(AfterOutcome::Compared(None));
    };
    if !ensure_before(run, cancel, log)? {
        return Ok(AfterOutcome::Interrupted);
    }
    let events = read_all(run.run_dir)?;
    let state = BaselineState::fold(run.playbook, &events);
    let spawn_env = run.cfg.spawn_env();
    let mut failure: Option<String> = None;
    for check in &block.checks {
        let known = state.check(&check.id);
        let before = known.and_then(|c| c.before.as_ref());
        let (after, journaled) = match known.and_then(|c| c.after.as_ref()) {
            Some(after) => (
                after.clone(),
                known.map(|c| c.comparisons.as_slice()).unwrap_or(&[]),
            ),
            None => {
                let ctx = run.ctx(phase::AFTER, cancel, &events, &spawn_env);
                let snap = read(check, &ctx, before);
                if cancel.load(Ordering::SeqCst) {
                    return Ok(AfterOutcome::Interrupted);
                }
                log.append(snap.event(&check.id, phase::AFTER))?;
                (snap, &[][..])
            }
        };
        let verdicts = journal_comparisons(check, before, &after, journaled, block.enforce, log)?;
        if block.enforce && failure.is_none() {
            failure = enforced_failure(&check.id, verdicts);
        }
    }
    Ok(AfterOutcome::Compared(failure))
}

/// One observation's verdict, journaled now or earlier.
struct Judged {
    observation: String,
    verdict: String,
    detail: String,
}

/// Every comparison of `check`: the ones `journaled` holds for this after
/// snapshot, and the rest computed and journaled now, so a crash between
/// two `baseline_compared` events leaves no observation unjudged.
fn journal_comparisons(
    check: &BaselineCheck,
    before: Option<&Snapshot>,
    after: &Snapshot,
    journaled: &[ComparisonRecord],
    enforced: bool,
    log: &mut EventLog,
) -> Result<Vec<Judged>, EngineError> {
    let mut out = Vec::new();
    for (observation, rule, b, a, c) in comparisons(check, before, after) {
        if let Some(j) = journaled.iter().find(|j| j.observation == observation) {
            out.push(Judged {
                observation,
                verdict: j.verdict.clone(),
                detail: j.detail.clone().unwrap_or_default(),
            });
            continue;
        }
        out.push(Judged {
            observation: observation.clone(),
            verdict: c.verdict.as_str().to_string(),
            detail: c.detail.clone().unwrap_or_default(),
        });
        log.append(EventPayload::BaselineCompared {
            check: check.id.clone(),
            observation,
            rule,
            before: b,
            after: Some(a),
            verdict: c.verdict.as_str().into(),
            regressed: c.regressed,
            detail: c.detail,
            enforced,
        })?;
    }
    Ok(out)
}

/// The `run_error` reason for the first verdict that is not met.
fn enforced_failure(check: &str, verdicts: Vec<Judged>) -> Option<String> {
    let j = verdicts
        .into_iter()
        .find(|j| j.verdict != Verdict::Met.as_str())?;
    let detail = if j.detail.is_empty() {
        String::new()
    } else {
        format!(": {}", j.detail)
    };
    Some(format!(
        "baseline check `{check}` observation `{}` {}{detail}",
        j.observation, j.verdict
    ))
}

/// One comparison row: observation, rule, before, after, result.
type Row = (String, String, Option<Observation>, Observation, Comparison);

/// Every comparison of `check`. A missing before compares as unknown; so
/// does every observation when the check's input changed between the two
/// snapshots.
fn comparisons(check: &BaselineCheck, before: Option<&Snapshot>, after: &Snapshot) -> Vec<Row> {
    let input_changed = before.is_some_and(|b| {
        b.status == status::OBSERVED
            && after.status == status::OBSERVED
            && b.input_digest != after.input_digest
    });
    let mut rows: Vec<Row> = match check.source() {
        Some(BaselineSource::ProjectChecks(_)) => project_rows(before, after),
        _ => check
            .observe
            .iter()
            .map(|o| {
                let b = before.map(|b| b.observation(&o.id));
                let a = after.observation(&o.id);
                let (rule, cmp) = match o.expect.rule() {
                    Ok(rule) => (rule.describe(), compare(b.as_ref(), &a, &rule)),
                    Err(why) => (String::new(), unknown(why)),
                };
                (o.id.clone(), rule, b, a, cmp)
            })
            .collect(),
    };
    if input_changed {
        for row in &mut rows {
            row.4 = unknown("the check's input changed between the before and after snapshots");
        }
    }
    rows
}

/// The rows of a project check: one per command, or one unknown row when
/// no command was recorded on either side.
fn project_rows(before: Option<&Snapshot>, after: &Snapshot) -> Vec<Row> {
    let commands = if after.commands.is_empty() {
        before.map(|b| b.commands.as_slice()).unwrap_or(&[])
    } else {
        after.commands.as_slice()
    };
    let empty = Snapshot::not_taken();
    let b_snap = before.unwrap_or(&empty);
    if commands.is_empty() {
        let why = |s: &Snapshot| s.reason.clone().unwrap_or_else(|| "no command ran".into());
        let a = Observation::unknown(why(after));
        return vec![(
            "commands".into(),
            project::RULE.into(),
            before.map(|b| Observation::unknown(why(b))),
            a,
            unknown(format!("after: {}", why(after))),
        )];
    }
    commands
        .iter()
        .map(|cmd| {
            let (b, a, c) = project::compare(cmd, b_snap, after);
            (cmd.name.clone(), project::RULE.into(), Some(b), a, c)
        })
        .collect()
}

fn unknown(detail: impl Into<String>) -> Comparison {
    Comparison {
        verdict: Verdict::Unknown,
        regressed: None,
        detail: Some(detail.into()),
    }
}

/// Why a supervisor patch may not apply: it changes `baseline_check` after
/// a node that can act has started, so the journaled before snapshot would
/// no longer describe what the patched block reads.
pub(crate) fn patch_refusal(
    running: &Playbook,
    patched: &Playbook,
    events: &[Event],
) -> Option<String> {
    if running.baseline_check == patched.baseline_check {
        return None;
    }
    BaselineState::fold(running, events).acting_started.then(|| {
        "the patched version changes baseline_check after a node that can act started; its before snapshot is already taken, so start a new run to change it".to_string()
    })
}
