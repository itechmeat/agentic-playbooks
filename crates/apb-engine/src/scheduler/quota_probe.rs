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
}

/// Runs the probe for node `node` of the run at `run_dir`, on the chain step
/// whose agent and model are `agent` and `model`.
pub(crate) fn run_quota_probe(run_dir: &Path, node: &str, agent: &str, model: &str) -> ProbeRun {
    let ri = match probe_invocation(run_dir, node, agent, model) {
        Ok(Some(ri)) => ri,
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
    let connector_policy = crate::adapter::ConnectorEnvPolicy {
        // The probe calls no connector, so it carries none of their tokens.
        scrub: root
            .map(apb_core::connector::resolve::all_referenced_env_names)
            .unwrap_or_default(),
        run_dir: None,
        node_id: None,
        overlay: crate::run_config::read_run_config(run_dir)
            .map(|c| c.spawn_env())
            .unwrap_or_default(),
    };
    let task = AgentTask {
        prompt: PROBE_PROMPT,
        model: &ri.model,
        workdir: &workdir,
        timeout: Some(PROBE_TIMEOUT),
        stream_log: None,
        soul: None,
        grant_autonomy: false,
        connector_policy: &connector_policy,
        interactive: false,
        report_contract: false,
        node: "__quota_probe",
        agent: &ri.agent_id,
        extract: None,
        status_file: None,
        hermetic_settings: None,
        transcript_dir: None,
    };
    let adapter = crate::adapter::ClaudeAdapter {
        program: ri.canonical_executable.to_string_lossy().into_owned(),
        spec: ri.spec.clone(),
    };
    match adapter.run(&task) {
        Ok(_) => ProbeRun::Answered,
        Err(f) => ProbeRun::Failed(f.message),
    }
}

/// The chain step of node `node` that runs `agent`/`model`, from the run's
/// effective binding (manifest plus rebind overlay). `None` in a host-mode
/// run: a host task is no provider call, so there is nothing to probe.
fn probe_invocation(
    run_dir: &Path,
    node: &str,
    agent: &str,
    model: &str,
) -> Result<Option<crate::invocation::ResolvedInvocation>, EngineError> {
    let Some(manifest) = crate::manifest::read(run_dir)? else {
        return Ok(None);
    };
    if manifest.is_host_mode()? {
        return Ok(None);
    }
    Ok(
        effective_for_node(run_dir, &manifest, node)?.and_then(|entry| {
            entry
                .chain
                .into_iter()
                .find(|ri| ri.agent_id == agent && ri.model == model)
        }),
    )
}
