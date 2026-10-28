//! The paid probe of an agent CLI behind `apb doctor --probe-agent`: one
//! short prompt through the agent's form, then a check of the output
//! against what the built-in form promises (`OutputFormat::promises`) and
//! the conformance verdict (`apb_core::agents::conformance`).
//!
//! It costs one model call, so it runs only when a person asks for it: the
//! CLI prints [`COST_NOTICE`] first, and a run step is refused
//! ([`refusal`]). The call goes through the spawn path of a run step: the
//! form and program are resolved under the agent name as given (as a
//! profile's agent is), connector-referenced variables are withheld, the
//! agent's scoped files (codex's isolated home, zcode's provider config) and
//! claude's minimal-environment settings live in the probe's own temporary
//! directory, and the call grants no autonomy (the agent's default
//! permission posture). It runs from an empty directory inside it, has a
//! timeout and
//! stops when the caller's cancel flag is set; the directory is removed
//! afterwards. Nothing of the output is kept; the report holds only the
//! verdict, the checks and the reported usage.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use apb_core::agents::{AgentUsage, Conformance, ExposedTools, Promises};
use apb_core::config::GlobalConfig;
use apb_core::step_env::StepCaller;

use crate::adapter::{AgentAdapter, AgentTask, ClaudeAdapter, HermeticEnv, SpawnEnv};

/// Printed before the probe runs.
pub const COST_NOTICE: &str =
    "the probe sends one short prompt to the agent: one paid model call on the agent's own account";

/// The prompt the probe sends.
pub const PROMPT: &str = "Reply with the single word OK. Do not use any tools.";

/// One promise of the built-in form and whether the output kept it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromiseCheck {
    pub what: &'static str,
    pub kept: bool,
}

/// What the probe found.
#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub agent: String,
    pub model: String,
    /// The agent ran its built-in form (no `agents.<id>.invocation`).
    pub form_is_builtin: bool,
    pub conformance: Conformance,
    /// The promises the output was checked against (built-in form only).
    pub promises: Vec<PromiseCheck>,
    /// The usage and cost the agent reported.
    pub usage: Option<AgentUsage>,
    /// The tools the agent reported, on the stream transport.
    pub exposure: Option<ExposedTools>,
    /// The form's autonomy flags, left out because the probe grants no
    /// autonomy: a run step that is granted autonomy passes them too.
    pub omitted_flags: Vec<String>,
}

impl ProbeReport {
    /// The output format drifted from the built-in form: the machine shape
    /// is gone, or a promise is broken.
    pub fn drifted(&self) -> bool {
        self.conformance == Conformance::Drifted || self.promises.iter().any(|p| !p.kept)
    }
}

/// Why a probe could not run or the agent call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError(pub String);

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The refusal of a probe started from a run step: a step never spends a
/// model call the operator did not ask for. `None` for a person.
pub fn refusal(step: &StepCaller) -> Option<String> {
    step.describe().map(|from| {
        format!(
            "the paid agent probe is refused {from}: it spends a model call, so only a person starts it, outside a run"
        )
    })
}

/// What to probe.
#[derive(Debug, Clone)]
pub struct ProbeRequest<'a> {
    /// A built-in agent id (an alias counts). The config is read under this
    /// very name, the way a run reads a profile's agent.
    pub agent: &'a str,
    /// The model; `None` takes the first model the catalog lists.
    pub model: Option<&'a str>,
    pub timeout: Duration,
    /// The project whose connector configs name the variables the agent
    /// must not inherit.
    pub project: &'a Path,
}

/// The error of a probe whose cancel flag stopped the agent.
pub const INTERRUPTED: &str = "the probe was interrupted";

/// The node name the probe's spawn runs as.
const PROBE_NODE: &str = "__agent_probe";

/// Runs the probe. `Err` when the agent is not a built-in one, no model is
/// known, the call itself failed, or `cancel` was set while it ran.
pub fn probe(
    req: &ProbeRequest<'_>,
    global: &GlobalConfig,
    cancel: &AtomicBool,
) -> Result<ProbeReport, ProbeError> {
    let module = apb_core::agents::lookup(req.agent).ok_or_else(|| {
        ProbeError(format!(
            "`{}` is not a built-in agent; the probe checks a built-in agent's output format",
            req.agent
        ))
    })?;
    let id = module.descriptor().id;
    let model = match req.model {
        Some(m) => m.to_string(),
        None => first_model(id)
            .ok_or_else(|| ProbeError(format!("no model is known for `{id}`; pass --model")))?,
    };
    // One lookup under the name as given decides the form, the program and
    // whether the form is the built-in one, exactly as for a run step.
    let form_is_builtin = !global.has_custom_form(req.agent);
    let spec =
        crate::invocation::spec_for(req.agent, global).map_err(|e| ProbeError(e.to_string()))?;
    let omitted_flags = spec.autonomous_args.clone();
    let adapter = ClaudeAdapter {
        program: crate::invocation::program_for(req.agent, global),
        spec,
    };
    // The agent runs in an empty `work`; its scoped files sit next to it in
    // `home`, out of its working directory. Both go with the directory.
    let dir = TempDir::new().map_err(|e| ProbeError(format!("temporary directory: {e}")))?;
    let workdir = dir.path().join("work");
    let home = dir.path().join("home");
    std::fs::create_dir(&workdir).map_err(|e| ProbeError(format!("temporary directory: {e}")))?;
    let hermetic = minimal_environment(req.agent, &home)?;
    let spawn_env = SpawnEnv {
        scrub: apb_core::connector::resolve::all_referenced_env_names(req.project),
        scoped_base: Some(home),
        ..SpawnEnv::default()
    };
    let task = AgentTask {
        prompt: PROMPT,
        model: &model,
        workdir: &workdir,
        timeout: Some(req.timeout),
        stream_log: None,
        soul: None,
        grant_autonomy: false,
        spawn_env: &spawn_env,
        interactive: false,
        report_contract: false,
        node: PROBE_NODE,
        agent: req.agent,
        extract: None,
        status_file: None,
        hermetic_settings: hermetic,
        transcript_dir: None,
        output_limits: Default::default(),
    };
    let report = adapter
        .run_cancellable(&task, cancel, None, None, None, None)
        .map_err(|f| {
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                ProbeError(INTERRUPTED.to_string())
            } else {
                ProbeError(format!("the agent call failed: {}", f.message))
            }
        })?;
    let output = module.output();
    let promises = if form_is_builtin {
        check_promises(output.promises(), output, &report.raw)
    } else {
        Vec::new()
    };
    Ok(ProbeReport {
        agent: id.to_string(),
        model,
        form_is_builtin,
        conformance: apb_core::agents::conformance(module, form_is_builtin, &report.raw),
        promises,
        usage: report.usage,
        exposure: report.exposure,
        omitted_flags,
    })
}

/// claude's minimal environment (the profile default), its settings file
/// written under `home`; `None` for an agent without the mechanism.
fn minimal_environment(agent: &str, home: &Path) -> Result<Option<HermeticEnv>, ProbeError> {
    if !crate::adapter::agent_supports_hermetic(agent) {
        return Ok(None);
    }
    std::fs::create_dir_all(home).map_err(|e| ProbeError(format!("temporary directory: {e}")))?;
    let settings = crate::adapter::write_hermetic_settings(home, PROBE_NODE)
        .map_err(|e| ProbeError(format!("minimal environment settings: {e}")))?;
    Ok(Some(HermeticEnv {
        settings,
        skills_dir: None,
    }))
}

/// Each promise of `promises` that one successful call can show, checked
/// against `stdout`. The error flag is left out: a successful call cannot
/// show that a failed one would be marked.
pub fn check_promises(
    promises: Promises,
    output: &dyn apb_core::agents::OutputFormat,
    stdout: &str,
) -> Vec<PromiseCheck> {
    let mut out = Vec::new();
    let mut add = |promised: bool, what: &'static str, kept: bool| {
        if promised {
            out.push(PromiseCheck { what, kept });
        }
    };
    add(promises.reply, "reply", output.reply(stdout).is_some());
    add(
        promises.session,
        "session id",
        output.session_id(stdout).is_some(),
    );
    add(promises.usage, "usage", output.usage(stdout).is_some());
    add(
        promises.final_record,
        "final record",
        output.has_final_record(stdout) == Some(true),
    );
    out
}

/// The first model the catalog lists for `agent`.
fn first_model(agent: &str) -> Option<String> {
    apb_core::agent_catalog::agents(false)
        .into_iter()
        .find(|a| a.agent == agent)
        .and_then(|a| a.models)
        .and_then(|m| m.items.into_iter().next())
}

/// An empty directory under the system temporary directory with a random
/// name, owner-only on unix, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("apb-agent-probe-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        Ok(TempDir(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
