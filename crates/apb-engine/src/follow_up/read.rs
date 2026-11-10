//! The one read of a run's due follow-up: the trust check, the lease, the
//! journal re-read under it, the source read, the scrub before the append,
//! and the notify sweep of a finished run.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::error::EngineError;
use crate::event::{Event, EventPayload};

/// What [`observe_due`] journaled, or `None` when it wrote nothing.
pub fn observe_due(root: &Path, run_id: &str, now_ms: u64) -> Result<Option<Event>, EngineError> {
    let run_dir = root.join(".apb/runs").join(run_id);
    // Trust first: a run directory this machine did not create (one that
    // came with a repository) is refused, journals nothing, reads nothing,
    // and returns `None` (`apb resume --all` prints one stderr line).
    if crate::gate::check_resume(root, run_id, true).is_err() {
        return Ok(None);
    }
    // The `run_finished` the pending follow-up is for names its lease; the
    // state is checked again under it.
    let Some(finished_seq) = pending_seq(&run_dir)? else {
        return Ok(None);
    };
    // One reader at a time: the lease `follow-up-<seq>` the waker also
    // holds while it waits. A held lease means another process has it.
    let Some(_lease) =
        crate::waker::Lease::try_take(&run_dir, crate::waker::WakerFor::FollowUp(finished_seq))
            .map_err(EngineError::Io)?
    else {
        return Ok(None);
    };
    observe_due_leased(root, &run_dir, run_id, finished_seq, now_ms)
}

/// The `run_finished` seq of the run's pending follow-up, read without a
/// lease; `None` when nothing is pending.
fn pending_seq(run_dir: &Path) -> Result<Option<u64>, EngineError> {
    let events = crate::event::read_all(run_dir)?;
    let snapshot = crate::legacy_snapshot::load_run_playbook(run_dir);
    let production =
        crate::manifest::RunOrigin::of_read(&crate::manifest::read(run_dir)).runs_follow_up();
    Ok(
        match crate::follow_up::state(snapshot.as_ref(), &events, production) {
            crate::follow_up::FollowUpState::Pending { finished_seq, .. } => Some(finished_seq),
            _ => None,
        },
    )
}

/// The read under the held lease of the `run_finished` at `leased_seq`
/// (the waker drops its lease and calls [`observe_due`], which takes it
/// again and re-checks the state here).
fn observe_due_leased(
    root: &Path,
    run_dir: &Path,
    run_id: &str,
    leased_seq: u64,
    now_ms: u64,
) -> Result<Option<Event>, EngineError> {
    let events = crate::event::read_all(run_dir)?;
    let snapshot = crate::legacy_snapshot::load_run_playbook(run_dir);
    let production =
        crate::manifest::RunOrigin::of_read(&crate::manifest::read(run_dir)).runs_follow_up();
    let crate::follow_up::FollowUpState::Pending {
        finished_seq,
        due_at_ms,
        grace_ms,
    } = crate::follow_up::state(snapshot.as_ref(), &events, production)
    else {
        return Ok(None);
    };
    // The run was resumed and ended again between the two reads: the
    // newer end's follow-up is read under its own lease.
    if finished_seq != leased_seq {
        return Ok(None);
    }
    // A live driver reads the check itself when its run ends; a second
    // reader would journal the observation twice. `None` (no pid file or
    // it cannot be read) counts as not alive, like every surface.
    if crate::liveness::driver_alive(run_dir, run_id) == Some(true) {
        return Ok(None);
    }
    if now_ms < due_at_ms {
        return Ok(None);
    }
    let Some(follow_up) = snapshot.as_ref().and_then(|p| p.follow_up.as_ref()) else {
        return Ok(None);
    };
    let check = follow_up.check.id.clone();
    if now_ms > due_at_ms.saturating_add(grace_ms) {
        // Missed: nothing was read, and nothing is read now.
        let late = now_ms - due_at_ms;
        let reason = format!(
            "read {}s after its time, past its grace of {}s; the machine was off or apb did not run",
            late / 1000,
            grace_ms / 1000
        );
        return append(
            root,
            run_dir,
            run_id,
            &check,
            "missed",
            Some(reason),
            Default::default(),
            Vec::new(),
            "unknown",
            finished_seq,
            due_at_ms,
            now_ms,
        )
        .map(Some);
    }
    // The read: a connector under the recorded `@follow_up` grant, a script
    // in the run's working tree when it still exists.
    let Some(follow_up) = snapshot.as_ref().and_then(|p| p.follow_up.as_ref()) else {
        return Ok(None);
    };
    // The run's working tree: the `worktree_resolved` path when it is
    // still a directory, else the execution root. A tree the fold names
    // that is gone makes every script read unknown.
    let folded = crate::state::RunState::fold(&events);
    let workdir: PathBuf = match folded.worktree.as_deref().map(PathBuf::from) {
        Some(tree) if tree.is_dir() => tree,
        Some(_) => {
            return append(
                root,
                run_dir,
                run_id,
                &check,
                "unknown",
                Some("the run's working tree is gone".into()),
                Default::default(),
                Vec::new(),
                "unknown",
                finished_seq,
                due_at_ms,
                now_ms,
            )
            .map(Some);
        }
        None => PathBuf::from(root),
    };
    let cfg = crate::run_config::read_run_config(run_dir)?;
    let spawn_env = cfg.spawn_env();
    let scrub = apb_core::connector::resolve::all_referenced_env_names(root);
    let cancel = AtomicBool::new(false);
    let ctx = crate::baseline::ReadCtx {
        run_dir,
        root,
        workdir: &workdir,
        params: &cfg.params,
        spawn_env: &spawn_env,
        scrub: &scrub,
        cancel: &cancel,
        phase: "follow_up",
        events: &events,
        guard_tree: true,
        scripts_label: "follow-up scripts",
        site: apb_core::follow_up::FOLLOW_UP_SITE,
        // Read after the run ended: outside its deadline.
        cap_secs: None,
    };
    // Scrub before anything is stored: a scrubber that cannot resolve the
    // run's secrets withholds the whole observation.
    let redactor = crate::run_redact::RunRedactor::for_run(root, run_dir);
    let Ok(scrubber) = redactor.scrubber() else {
        return append(
            root,
            run_dir,
            run_id,
            &check,
            "unknown",
            Some(
                "the run's secret values cannot be read, so the observation was not stored".into(),
            ),
            Default::default(),
            Vec::new(),
            "unknown",
            finished_seq,
            due_at_ms,
            now_ms,
        )
        .map(Some);
    };
    // One read of the check: every observation in one snapshot, then each
    // checked under its rule with no before value. A snapshot that is not
    // honestly observed (the script failed, the tree changed) is unknown,
    // never zero or met.
    let snapshot = crate::baseline::read::read(&follow_up.check, &ctx, None);
    if snapshot.status != crate::baseline::status::OBSERVED {
        return append(
            root,
            run_dir,
            run_id,
            &check,
            "unknown",
            // A source's reason may quote a rendered value (a param in an
            // expected answer), so it is scrubbed like every stored text.
            snapshot.reason.map(|r| scrubber.scrub(&r)),
            Default::default(),
            Vec::new(),
            "unknown",
            finished_seq,
            due_at_ms,
            now_ms,
        )
        .map(Some);
    }
    let mut observations: std::collections::BTreeMap<String, apb_core::baseline::Observation> =
        Default::default();
    let mut results = Vec::new();
    let mut any_not_met = false;
    let mut any_unknown = false;
    for observe in &follow_up.check.observe {
        let observation = snapshot
            .observations
            .get(&observe.id)
            .cloned()
            .unwrap_or_else(|| {
                apb_core::baseline::Observation::unknown("the observation was not read")
            });
        let comparison = match observe.expect.rule() {
            Ok(rule) => apb_core::baseline::rule::check(&observation, &rule),
            Err(reason) => apb_core::baseline::rule::Comparison::unknown(reason),
        };
        match comparison.verdict {
            apb_core::baseline::rule::Verdict::Met => {}
            apb_core::baseline::rule::Verdict::NotMet => any_not_met = true,
            apb_core::baseline::rule::Verdict::Unknown => any_unknown = true,
        }
        let scrubbed = match serde_json::to_value(&observation) {
            Ok(v) => scrubber.redact_value(v),
            Err(_) => serde_json::Value::Null,
        };
        let scrubbed_observation: apb_core::baseline::Observation =
            match serde_json::from_value(scrubbed) {
                Ok(o) => o,
                Err(_) => apb_core::baseline::Observation::unknown("the value was withheld"),
            };
        observations.insert(observe.id.clone(), scrubbed_observation);
        results.push(crate::event::FollowUpResult {
            observation: observe.id.clone(),
            rule: observe
                .expect
                .rule()
                .map(|r| scrubber.scrub(&r.describe()))
                .unwrap_or_default(),
            verdict: comparison.verdict.as_str().into(),
            detail: comparison
                .detail
                .as_deref()
                .map(|d| scrubber.scrub(d))
                .filter(|d| !d.is_empty()),
        });
    }
    let verdict = if any_not_met {
        "not_met"
    } else if any_unknown {
        "unknown"
    } else {
        "met"
    };
    append(
        root,
        run_dir,
        run_id,
        &check,
        "observed",
        None,
        observations,
        results,
        verdict,
        finished_seq,
        due_at_ms,
        now_ms,
    )
    .map(Some)
}

/// Journals one `follow_up_observed` through `EventLog::open`, then runs
/// the notify sweep of a finished run (a due follow-up notice rides it).
#[allow(clippy::too_many_arguments)]
fn append(
    root: &Path,
    run_dir: &Path,
    run_id: &str,
    check: &str,
    status: &str,
    reason: Option<String>,
    observations: std::collections::BTreeMap<String, apb_core::baseline::Observation>,
    results: Vec<crate::event::FollowUpResult>,
    verdict: &str,
    finished_seq: u64,
    due_at_ms: u64,
    observed_at_ms: u64,
) -> Result<Event, EngineError> {
    let mut log = crate::event::EventLog::open(run_dir)?;
    let payload = EventPayload::FollowUpObserved {
        check: check.to_string(),
        status: status.to_string(),
        reason,
        observations,
        results,
        verdict: verdict.to_string(),
        finished_seq,
        due_at_ms,
        observed_at_ms,
        input_digest: String::new(),
    };
    let event = log.append(payload)?;
    drop(log);
    crate::notify::send::Notifier::new(root, run_dir, run_id).sweep_now();
    if crate::notify::retry::retry_ahead(root, run_dir, run_id) {
        crate::waker::spawn_notify_waker(root, run_id);
    }
    Ok(event)
}
