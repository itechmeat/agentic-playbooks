//! The wake-up probe of a quota pause (issue #200, design 6.6): one minimal
//! invocation of the executor that ran out of quota, before the node runs
//! again. The same agent and model through the invocation fixed in the run
//! manifest (the path a node attempt takes), a fixed tiny prompt, no
//! autonomy grant (so no tools beyond the agent's default posture), no
//! skills, no SOUL, a short deadline.
//!
//! Split out of `scheduler` for navigability; shares the parent module's
//! imports via `use super::*`.

use super::*;

/// The probe's whole prompt.
const PROBE_PROMPT: &str = "Reply with the single word OK.";

/// The probe's deadline: a cheap call either answers quickly or reports its
/// quota quickly.
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a running probe re-reads the run's journal for a reason to
/// stop.
const PROBE_WATCH: Duration = Duration::from_millis(200);

/// How a probe went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeRun {
    /// The executor answered.
    Answered,
    /// The executor failed; the adapter's failure detail.
    Failed(String),
    /// No probe could be built (the executor is not in the node's chain any
    /// more, no manifest); the reason.
    Unavailable(String),
    /// The probe was stopped while it ran ([`probe_unwanted`]); the reason.
    Cancelled(&'static str),
}

/// Runs the probe for node `node` of the run at `run_dir`, on the chain step
/// whose agent and model are `agent` and `model`, for the pause `hold`.
pub(crate) fn run_quota_probe(
    run_dir: &Path,
    node: &str,
    agent: &str,
    model: &str,
    hold: crate::resume_kind::WakeHold,
) -> ProbeRun {
    let (ri, process_env) = match probe_invocation(run_dir, node, agent, model) {
        Ok(Some(found)) => found,
        Ok(None) => {
            return ProbeRun::Unavailable(format!(
                "node `{node}` has no `{agent}/{model}` step in its executor chain"
            ));
        }
        Err(e) => return ProbeRun::Unavailable(e.to_string()),
    };
    let workdir = run_dir.join("quota-probe");
    if let Err(e) = apb_core::fsutil::create_dir_under(run_dir, &workdir) {
        return ProbeRun::Unavailable(e.to_string());
    }
    let root = run_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent);
    let run_config = crate::run_config::read_run_config(run_dir);
    // The probe calls no connector, so it carries none of their tokens.
    let scrub = root
        .map(apb_core::connector::resolve::all_referenced_env_names)
        .unwrap_or_default();
    // The probe sees the environment the real attempt would: the entry's
    // `process_env` for this step's agent. Not journaled (no attempt).
    let spawn_env = crate::adapter::SpawnEnv {
        process: crate::agent_env::plan_for_entry(
            process_env.as_ref(),
            &ri.agent_id,
            &ri.model,
            &scrub,
        ),
        scrub,
        overlay: run_config
            .as_ref()
            .map(|c| c.spawn_env())
            .unwrap_or_default(),
        // The catalog row the run started with: a cloud model runs on
        // apb's provider route, as the node's attempts do.
        cloud: ri.cloud.clone(),
        ..Default::default()
    };
    let task = AgentTask {
        prompt: PROBE_PROMPT,
        untrusted_section: None,
        model: &ri.model,
        workdir: &workdir,
        timeout: Some(PROBE_TIMEOUT),
        stream_log: None,
        soul: None,
        grant_autonomy: false,
        spawn_env: &spawn_env,
        interactive: false,
        report_contract: false,
        node: "__quota_probe",
        agent: &ri.agent_id,
        extract: None,
        status_file: None,
        hermetic_settings: None,
        transcript_dir: None,
        // The limits the run resolved at its start, as every attempt of it.
        output_limits: run_config
            .as_ref()
            .map(crate::run_config::RunConfig::output_limits)
            .unwrap_or_default(),
    };
    let adapter = crate::adapter::ClaudeAdapter {
        program: ri.canonical_executable.to_string_lossy().into_owned(),
        spec: ri.spec.clone(),
    };
    // A probe is agent work like any attempt: the basis it would be paid on
    // must be covered by the run's billing record (or the settings in force
    // for a run without one) before it is sent. Held work is not probed; the
    // node's own spawn check pauses the run for a person when it wakes.
    let basis = crate::adapter::read_cost_basis(&task).basis;
    if let Some(hold) = crate::pause::billing::spawn_hold(run_dir, &ri.agent_id, basis) {
        return ProbeRun::Unavailable(format!(
            "{}: the probe is not sent",
            hold.reason.unwrap_or_default()
        ));
    }
    // A run with a deadline is probed only while it has time left, checked
    // right before the spawn (the waker's own check came before the
    // preflight above), and the probe is bounded by that time: past it the
    // run ends, and the probe's executor with it.
    let left = read_all(run_dir)
        .ok()
        .and_then(|events| crate::run_deadline::remaining_ms(&events, apb_core::clock::now_ms()));
    let task = match left {
        Some(0) => {
            return ProbeRun::Unavailable(
                "the run passed its deadline: the probe is not sent".into(),
            );
        }
        Some(ms) => AgentTask {
            timeout: Some(PROBE_TIMEOUT.min(Duration::from_millis(ms))),
            ..task
        },
        None => task,
    };
    // The bound above is the time left when the probe starts. The run can
    // change while it runs (a resume that sets another deadline, a stop):
    // a watch over the journal stops the executor then, through the same
    // cancellation an attempt has.
    let cancel = AtomicBool::new(false);
    let done = AtomicBool::new(false);
    let why = std::sync::OnceLock::new();
    let ran = std::thread::scope(|s| {
        s.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                if let Some(reason) = probe_unwanted(run_dir, hold) {
                    let _ = why.set(reason);
                    cancel.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::park_timeout(PROBE_WATCH);
            }
        });
        let ran = adapter.run_cancellable(&task, &cancel, None, None, None, None);
        done.store(true, Ordering::Relaxed);
        ran
    });
    if let Some(reason) = why.get() {
        return ProbeRun::Cancelled(reason);
    }
    match ran {
        Ok(_) => ProbeRun::Answered,
        Err(f) => ProbeRun::Failed(f.message),
    }
}

/// Why a running probe must stop, on the run's journal as it is now: the
/// pause it probes for no longer holds the run (a resume, the only way its
/// deadline changes, a stop, another pause). `None` while it holds, and for
/// a journal that does not read: the bound the probe started with (the
/// time left then) still ends it, as it does at the deadline of a run that
/// stays parked.
fn probe_unwanted(run_dir: &Path, hold: crate::resume_kind::WakeHold) -> Option<&'static str> {
    let events = read_all(run_dir).ok()?;
    (!hold.holds(&events)).then_some("the pause it probed for no longer holds the run")
}

/// A probed chain step and its entry's `process_env`.
type ProbeStep = (
    crate::invocation::ResolvedInvocation,
    Option<apb_core::process_env::ProcessEnvPolicy>,
);

/// The chain step of node `node` that runs `agent`/`model`, with the
/// entry's `process_env`, from the run's effective binding (manifest plus
/// rebind overlay). `None` in a host-mode run: a host task is no provider
/// call, so there is nothing to probe.
fn probe_invocation(
    run_dir: &Path,
    node: &str,
    agent: &str,
    model: &str,
) -> Result<Option<ProbeStep>, EngineError> {
    let Some(manifest) = crate::manifest::read(run_dir)? else {
        return Ok(None);
    };
    if manifest.is_host_mode()? {
        return Ok(None);
    }
    Ok(
        effective_for_node(run_dir, &manifest, node)?.and_then(|entry| {
            let process_env = entry.process_env;
            entry
                .chain
                .into_iter()
                .find(|ri| ri.agent_id == agent && ri.model == model)
                .map(|ri| (ri, process_env))
        }),
    )
}
