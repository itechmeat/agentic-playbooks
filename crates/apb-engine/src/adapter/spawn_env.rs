//! The environment of every agent process apb spawns: one [`prepare`] for a
//! node attempt's start and resume forms alike, and [`SpawnEnv::apply`] for
//! the background supervisor.
//!
//! In order: the profile's `process_env` plan (an allowlist clears the
//! inherited environment and copies back only the kept names), the connector
//! scrub, the run-context variables, the pager, the eval overlay, the agent's
//! run-scoped environment (codex's config home, zcode's provider config),
//! the attempt's status file and, for a sandboxed attempt, the sandbox
//! overlay (`crate::sandbox::SandboxPlan::overlay`). Each later step wins over
//! an earlier one.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use apb_core::process_env::EnvPlan;

use super::failure::ErrorClass;

/// The variable that carries an attempt's token to its processes.
pub const ATTEMPT_TOKEN_ENV: &str = "APB_ATTEMPT_TOKEN";

/// The variable that names the list item an attempt runs (`list:`).
pub const ITEM_ID_ENV: &str = "APB_ITEM_ID";

/// What apb sets on and removes from a spawned agent's environment.
/// `scrub` is the union of env var names referenced by ANY installed
/// connector config (both scopes), removed from the child so a connector
/// token can never be inherited by an agent (spec 4.3). `run_dir`/`node_id`,
/// when set, become the `APB_RUN_DIR`/`APB_NODE_ID` context env that
/// `apb connector call` (a child of the agent) reads to locate the run
/// manifest and check its grants, plus `APB_RUN_ID` (the run directory's
/// name) for provenance, and `APB_ATTEMPT` (the attempt number) with
/// `APB_ATTEMPT_TOKEN` (the attempt's token) so a connector call binds to
/// the attempt that made it. `process`, when set, is the profile's
/// `process_env: allowlist` plan. The default is empty: no scrub, no context
/// env and the whole inherited environment.
#[derive(Debug, Clone, Default)]
pub struct SpawnEnv {
    pub scrub: Vec<String>,
    pub run_dir: Option<PathBuf>,
    pub node_id: Option<String>,
    /// The attempt number the spawned agent runs as.
    pub attempt: Option<u32>,
    /// The attempt's random token (`APB_ATTEMPT_TOKEN`), journaled on its
    /// `attempt_started`: what binds a connector call to this very attempt.
    pub attempt_token: Option<String>,
    /// The list item the agent runs (`APB_ITEM_ID`), for a list node.
    pub item: Option<String>,
    /// An eval run's env overlay (`RunConfig::spawn_env`), set after the
    /// scrub and the run-context env. Empty for every other run.
    pub overlay: Vec<(String, String)>,
    /// The names an allowlisted agent keeps of apb's own environment;
    /// `None` inherits all of it.
    pub process: Option<Arc<EnvPlan>>,
    /// Where the agent's scoped files go for a spawn that belongs to no run
    /// (the doctor's agent probe): the base under which
    /// `agent-home/<agent>/<node>` is laid down, as `run_dir` is for a run.
    /// Unlike `run_dir` it sets no run-context variable. `run_dir` wins.
    pub scoped_base: Option<PathBuf>,
    /// The isolation sandbox the attempt's process runs in; `None` runs it
    /// as before.
    pub sandbox: Option<Arc<crate::sandbox::SandboxPlan>>,
}

impl SpawnEnv {
    /// The env of an agent a run spawns for `node`: inherited connector
    /// tokens scrubbed, the run context set, the run's env overlay applied.
    /// The attempt is set per attempt with [`Self::with_attempt`].
    pub fn for_node(
        scrub: Vec<String>,
        run_dir: &std::path::Path,
        node: &str,
        overlay: Vec<(String, String)>,
    ) -> Self {
        SpawnEnv {
            scrub,
            run_dir: Some(run_dir.to_path_buf()),
            node_id: Some(node.to_string()),
            overlay,
            ..Default::default()
        }
    }

    /// This env for list item `id`.
    pub fn with_item(&self, id: Option<&str>) -> Self {
        SpawnEnv {
            item: id.map(str::to_string),
            ..self.clone()
        }
    }

    /// This env for attempt `attempt`, with a fresh attempt token.
    pub fn with_attempt(&self, attempt: u32) -> Self {
        SpawnEnv {
            attempt: Some(attempt),
            attempt_token: Some(apb_core::connector::marker::new_nonce()),
            ..self.clone()
        }
    }

    /// This env under the process-env plan `process` (`None` inherits).
    pub fn with_process(&self, process: Option<Arc<EnvPlan>>) -> Self {
        SpawnEnv {
            process,
            ..self.clone()
        }
    }

    /// This env inside `sandbox` (`None`: no sandbox).
    pub fn with_sandbox(&self, sandbox: Option<Arc<crate::sandbox::SandboxPlan>>) -> Self {
        SpawnEnv {
            sandbox,
            ..self.clone()
        }
    }

    /// Whether the spawned process gets apb's own value of `name`: it is set
    /// here and neither the plan nor the scrub drops it.
    pub fn passes(&self, name: &str) -> bool {
        std::env::var_os(name).is_some()
            && !self.scrub.iter().any(|s| s == name)
            && self.process.as_ref().is_none_or(|p| p.keeps(name))
    }

    /// Applies the plan, the scrub, the run-context env, the pager and the
    /// overlay to `cmd`. Called at every agent spawn site.
    pub fn apply(&self, cmd: &mut Command) {
        if let Some(plan) = &self.process {
            hide_own_environ();
            cmd.env_clear();
            for name in &plan.keep {
                if let Some(value) = std::env::var_os(name) {
                    cmd.env(name, value);
                }
            }
        }
        for name in &self.scrub {
            cmd.env_remove(name);
        }
        if let Some(dir) = &self.run_dir {
            cmd.env("APB_RUN_DIR", dir);
            // The run id (C7), for an `Apb-Run:` commit trailer or a tracker
            // record: the run directory's name is the id.
            if let Some(id) = dir.file_name() {
                cmd.env("APB_RUN_ID", id);
            }
        }
        if let Some(node) = &self.node_id {
            cmd.env("APB_NODE_ID", node);
        }
        match self.attempt {
            Some(n) => cmd.env("APB_ATTEMPT", n.to_string()),
            // Never inherit another attempt's number from the parent.
            None => cmd.env_remove("APB_ATTEMPT"),
        };
        match &self.attempt_token {
            Some(token) => cmd.env(ATTEMPT_TOKEN_ENV, token),
            None => cmd.env_remove(ATTEMPT_TOKEN_ENV),
        };
        match &self.item {
            Some(id) => cmd.env(ITEM_ID_ENV, id),
            // Never inherit another item's id from the parent.
            None => cmd.env_remove(ITEM_ID_ENV),
        };
        // A host task id belongs to the host's subagent, never to an agent
        // apb spawns itself.
        cmd.env_remove("APB_HOST_TASK");
        // Spawned agents are headless; force a pass-through pager so CLI
        // porcelain (notably `gh issue view`) cannot exit 0 with empty stdout
        // when a TTY-like pager path is engaged (see issue #42 finding 12).
        cmd.env("GH_PAGER", "cat");
        cmd.env("PAGER", "cat");
        for (k, v) in &self.overlay {
            cmd.env(k, v);
        }
    }
}

/// Makes this apb process non-dumpable before its first allowlisted agent
/// spawn, once per process. On Linux, `/proc/<pid>/environ` of a dumpable
/// process is readable by every process of the same user, so an agent under
/// `process_env: allowlist` could otherwise read apb's whole environment
/// through `/proc/$PPID/environ`. A non-dumpable process keeps
/// `/proc/<pid>/stat` and `cmdline` readable (liveness and `ps` still work),
/// produces no core dump and cannot be ptraced by the same user. The flag
/// does not pass to children: `execve` resets it, so agents stay ordinary
/// processes. Not isolation from a hostile agent of the same user: the
/// operator's other processes stay readable. No equivalent elsewhere. When
/// the call fails, one warning on stderr says the protection is not active
/// (fixed text and the OS error, never a value); spawning goes on.
fn hide_own_environ() {
    #[cfg(target_os = "linux")]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // SAFETY: prctl(2) with PR_SET_DUMPABLE takes plain integers and
            // touches only this process's dumpable flag.
            if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
                eprintln!(
                    "apb: warning: could not make apb non-dumpable, so an allowlisted agent can read apb's environment through /proc: {}",
                    std::io::Error::last_os_error()
                );
            }
        });
    }
}

/// The process an agent attempt spawns: its env, agent, model, node and
/// status file.
pub(crate) struct SpawnTarget<'a> {
    pub env: &'a SpawnEnv,
    pub agent: &'a str,
    pub model: &'a str,
    pub node: &'a str,
    pub status_file: Option<&'a std::path::Path>,
}

/// The whole environment of a node agent's process, for its start and
/// resume forms alike: [`SpawnEnv::apply`], then the agent's run-scoped
/// environment, the attempt's status file and, last, the sandbox overlay.
pub(crate) fn prepare(cmd: &mut Command, task: &SpawnTarget) -> Result<(), (ErrorClass, String)> {
    task.env.apply(cmd);
    apply_scoped_env(cmd, task)?;
    // Per-attempt status file (subtask S2): the agent may write its final
    // verdict as JSON here, which the engine reads before the textual report.
    if let Some(sf) = task.status_file {
        cmd.env("APB_STATUS_FILE", sf);
    }
    if let Some(plan) = &task.env.sandbox {
        plan.overlay(cmd);
    }
    Ok(())
}

/// The agent's run-scoped environment (spec 2026-07-21 run-reliability),
/// from its module's `scoped_env`: codex's isolated config home, so a
/// spawned codex cannot inherit the user's interactive MCP configuration,
/// and zcode's provider config carrying the attempt's model selection (zcode
/// has no `--model` flag). The files live in the run's agent home for the
/// node (`agent-home/<agent>/<node>`) when the attempt belongs to a run, or
/// under [`SpawnEnv::scoped_base`] for a spawn outside one; spawn paths that
/// carry neither (internal, connector-less calls such as compaction) pass
/// none. A no-op for an agent that needs nothing.
fn apply_scoped_env(cmd: &mut Command, task: &SpawnTarget) -> Result<(), (ErrorClass, String)> {
    let Some(agent) = apb_core::agents::lookup(task.agent) else {
        return Ok(());
    };
    let scoped = task
        .env
        .run_dir
        .as_deref()
        .or(task.env.scoped_base.as_deref())
        .map(|d| crate::agent_home::scoped_dir(d, agent.descriptor().id, task.node));
    let env = agent
        .setup()
        .scoped_env(task.model, scoped.as_deref())
        .map_err(|e| (ErrorClass::ProcessExit, e.to_string()))?;
    for (name, value) in env {
        cmd.env(name, value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child_env(env: &SpawnEnv, parent: &[(&str, &str)]) -> Vec<String> {
        let mut cmd = Command::new("env");
        for (k, v) in parent {
            cmd.env(k, v);
        }
        env.apply(&mut cmd);
        let out = cmd.output().expect("spawn env");
        assert!(out.status.success(), "env exited {:?}", out.status.code());
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_pass_through_pager_is_forced() {
        // The only defense against gh porcelain swallowing stdout under a
        // TTY-like pager path.
        let lines = child_env(&SpawnEnv::default(), &[]);
        assert!(lines.iter().any(|l| l == "GH_PAGER=cat"));
        assert!(lines.iter().any(|l| l == "PAGER=cat"));
    }

    /// An eval run's env overlay lands after the connector scrub: a
    /// scrubbed name the overlay sets keeps the overlay's value, and the
    /// overlay's own variables arrive.
    #[test]
    fn the_eval_overlay_is_applied_after_the_scrub() {
        let env = SpawnEnv {
            scrub: vec!["PATH_PROBE_TOKEN".into()],
            overlay: vec![
                ("PATH_PROBE_TOKEN".into(), String::new()),
                ("EVAL_PROBE".into(), "1".into()),
            ],
            ..Default::default()
        };
        let lines = child_env(&env, &[("PATH_PROBE_TOKEN", "x")]);
        assert!(lines.iter().any(|l| l == "PATH_PROBE_TOKEN="));
        assert!(lines.iter().any(|l| l == "EVAL_PROBE=1"));
    }

    /// Under a plan, a name the command itself was given before `apply` is
    /// cleared too: only kept names and apb's own variables arrive.
    #[test]
    fn a_plan_clears_everything_it_does_not_keep() {
        let env = SpawnEnv {
            process: Some(Arc::new(EnvPlan {
                keep: vec!["PATH".into()],
                ..Default::default()
            })),
            node_id: Some("n".into()),
            ..Default::default()
        };
        let lines = child_env(&env, &[("SPAWN_ENV_PROBE", "x")]);
        assert!(!lines.iter().any(|l| l.starts_with("SPAWN_ENV_PROBE=")));
        assert!(lines.iter().any(|l| l.starts_with("PATH=")));
        assert!(lines.iter().any(|l| l == "APB_NODE_ID=n"));
    }

    #[test]
    fn passes_follows_the_plan_and_the_scrub() {
        // PATH is set in every test process.
        let inherit = SpawnEnv::default();
        assert!(inherit.passes("PATH"));
        let scrubbed = SpawnEnv {
            scrub: vec!["PATH".into()],
            ..Default::default()
        };
        assert!(!scrubbed.passes("PATH"));
        let planned = inherit.with_process(Some(Arc::new(EnvPlan::default())));
        assert!(!planned.passes("PATH"));
    }
}
