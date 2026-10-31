//! The environment of one chain step of an agent_task node: the minimal
//! agent environment (claude's apb-owned settings) and the profile's
//! `process_env` plan for the step's own agent, so a fallback onto another
//! agent gets that agent's set from the same profile policy.

use std::path::Path;
use std::sync::Arc;

use apb_core::process_env::EnvPlan;

use crate::adapter::HermeticEnv;
use crate::error::EngineError;
use crate::manifest::ManifestProfile;
use crate::scheduler::journal::Journal;

/// The chain step a [`StepEnv`] is for.
pub(super) struct StepTarget<'a> {
    pub agent: &'a str,
    pub model: &'a str,
    /// A host task: nothing is spawned, so nothing applies.
    pub host: bool,
}

/// What every attempt of one chain step runs with.
pub(super) struct StepEnv {
    /// The minimal agent environment, `None` for `environment: full`, an
    /// agent without the mechanism, and a host step.
    pub hermetic: Option<HermeticEnv>,
    /// The `process_env: allowlist` plan, `None` for `inherit` and a host
    /// step.
    pub process: Option<Arc<EnvPlan>>,
    /// What forced the allowlist on a profile that inherits (`isolation`).
    pub forced_by: Option<&'static str>,
}

/// The [`StepEnv`] of `step` for node `node_id` bound to `entry`.
///
/// The minimal agent environment (issue #136 item 4, the profile default):
/// claude/claude-code get an apb-owned settings file, only the project and
/// local setting sources and only apb's own MCP servers. The profile's
/// declared skills are then delivered through `--add-dir` (the user source
/// that used to provide them is not loaded), except on an `isolated` node,
/// whose working directory already holds them. Other agents have no such
/// mechanism and run as configured. The file content is fixed, so writing
/// it once per step is enough.
pub(super) fn for_step(
    run_dir: &Path,
    node_id: &str,
    entry: &ManifestProfile,
    step: &StepTarget,
    isolated: bool,
    scrub: &[String],
) -> Result<StepEnv, EngineError> {
    if step.host {
        return Ok(StepEnv {
            hermetic: None,
            process: None,
            forced_by: None,
        });
    }
    let hermetic = if entry.hermetic && crate::adapter::agent_supports_hermetic(step.agent) {
        let skills_dir = if !isolated && !entry.skills.is_empty() {
            // One copy per profile bundle per run, at a path every node of
            // the profile shares, so the agent's system portion stays
            // byte-identical across them and a provider prompt cache hits
            // (issue #67 item 7). Checked against the snapshot digests
            // before each step and laid down again when it drifted.
            Some(crate::scheduler::skills_copy::shared_skills_dir(
                run_dir, entry,
            )?)
        } else {
            None
        };
        Some(HermeticEnv {
            settings: crate::adapter::write_hermetic_settings(run_dir, node_id)?,
            skills_dir,
        })
    } else {
        None
    };
    Ok(StepEnv {
        hermetic,
        ..process_plan(entry, step, isolated, scrub)
    })
}

/// The [`StepEnv`] of an internal step that never gets the minimal agent
/// environment (the finish answer): the `process_env` plan alone.
pub(super) fn process_only(
    entry: &ManifestProfile,
    step: &StepTarget,
    scrub: &[String],
) -> StepEnv {
    process_plan(entry, step, false, scrub)
}

/// The step's `process_env` plan. An isolated node forces the allowlist
/// (`apb_core::process_env::plan_isolated`): a profile that inherits gets
/// one with no extras, and the event says isolation forced it.
fn process_plan(
    entry: &ManifestProfile,
    step: &StepTarget,
    isolated: bool,
    scrub: &[String],
) -> StepEnv {
    let (process, forced_by) = if step.host {
        (None, None)
    } else if isolated {
        let own = entry.process_env.as_ref();
        let policy = apb_core::process_env::plan_isolated(own);
        let plan = crate::agent_env::plan_for(step.agent, step.model, &policy, scrub);
        let forced = !own.is_some_and(|p| p.is_allowlist());
        (Some(Arc::new(plan)), forced.then_some("isolation"))
    } else {
        let plan = crate::agent_env::plan_for_entry(
            entry.process_env.as_ref(),
            step.agent,
            step.model,
            scrub,
        );
        (plan, None)
    };
    StepEnv {
        hermetic: None,
        process,
        forced_by,
    }
}

impl StepEnv {
    /// Journals `agent_env_applied` for attempt `attempt` when the step runs
    /// under an allowlist; nothing otherwise.
    pub(super) fn journal(
        &self,
        journal: &Journal,
        node_id: &str,
        attempt: u32,
        agent: &str,
    ) -> Result<(), EngineError> {
        match &self.process {
            Some(plan) => journal.append(crate::agent_env::applied_event_by(
                plan,
                node_id,
                attempt,
                agent,
                self.forced_by,
            )),
            None => Ok(()),
        }
    }
}
