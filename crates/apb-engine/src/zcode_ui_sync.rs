//! `agents.zcode.ui_sync`: after a zcode session finishes, record it in the
//! ZCode desktop's task index so the desktop history shows it (see
//! `apb_core::zcode_tasks_index` for the index and why this is needed).
//!
//! A decorator over the adapter that runs the step, so neither the adapter nor
//! the drive loop knows about the desktop: it adds one best-effort write after
//! a successful attempt and never changes the attempt's result. Every failure
//! is a warning on stderr.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use apb_core::config::{GlobalConfig, InvocationDef};
use apb_core::zcode_tasks_index::{self as index, Host, HostFacts, Retry, TaskRecord};

use crate::adapter::{
    AgentAdapter, AgentFailure, AgentReport, AgentTask, ConnectorEnvPolicy, ControlHooks,
    ErrorClass, LiveHooks, StallHooks,
};

/// [`wrap`] for one step of a run: a no-op for every agent but zcode, else
/// the global config is read live (the sync is a side channel, not part of
/// the run's execution contract, so it is not snapshotted). `spec` is the
/// step's snapshotted invocation, `None` for the configured one.
pub fn wrap_step(
    adapter: Box<dyn AgentAdapter>,
    agent: &str,
    spec: Option<&InvocationDef>,
) -> Box<dyn AgentAdapter> {
    if apb_core::detect::canonical_agent_id(agent) != apb_core::zcode::AGENT_ID {
        return adapter;
    }
    let global = GlobalConfig::load().unwrap_or_default();
    match spec {
        Some(spec) => wrap(adapter, agent, spec, &global),
        None => match crate::invocation::spec_for(agent, &global) {
            Ok(spec) => wrap(adapter, agent, &spec, &global),
            Err(_) => adapter,
        },
    }
}

/// Wraps `adapter` with the desktop-history sync when `agent` is zcode and
/// the global config opted in (`agents.zcode.ui_sync: true`); returns it
/// unchanged otherwise. `spec` is the invocation the adapter runs, read for
/// the `--mode` the session gets.
pub fn wrap(
    adapter: Box<dyn AgentAdapter>,
    agent: &str,
    spec: &InvocationDef,
    global: &GlobalConfig,
) -> Box<dyn AgentAdapter> {
    let id = apb_core::zcode::AGENT_ID;
    if apb_core::detect::canonical_agent_id(agent) != id || !global.agent_ui_sync(id) {
        return adapter;
    }
    let Some(home) = apb_core::zcode::home_dir() else {
        return adapter;
    };
    Box::new(ZcodeUiSync::new(
        adapter,
        home,
        index::detect_host(&HostFacts::current()),
        spec,
    ))
}

/// The decorator. See the module docs.
pub struct ZcodeUiSync {
    inner: Box<dyn AgentAdapter>,
    home: PathBuf,
    host: Result<Host, String>,
    /// The `--mode` of the base form (without autonomy).
    base_mode: Option<String>,
    /// The `--mode` of the autonomy grant, when it sets one.
    autonomous_mode: Option<String>,
}

impl ZcodeUiSync {
    /// A sync into `<home>/.zcode/v2/tasks-index.sqlite`, keyed for `host`
    /// (an `Err` is the reason the host could not be told, reported as the
    /// warning of every sync).
    pub fn new(
        inner: Box<dyn AgentAdapter>,
        home: PathBuf,
        host: Result<Host, String>,
        spec: &InvocationDef,
    ) -> Self {
        ZcodeUiSync {
            inner,
            home,
            host,
            base_mode: last_mode(&spec.argv),
            autonomous_mode: last_mode(&spec.autonomous_args),
        }
    }

    /// The row for a finished attempt, or why there is none.
    fn record(&self, task: &AgentTask, report: &AgentReport) -> Result<TaskRecord, String> {
        let host = self.host.clone()?;
        let session = report
            .session
            .clone()
            .ok_or("zcode printed no session id")?;
        let workspace_path = std::path::absolute(task.workdir)
            .unwrap_or_else(|_| task.workdir.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let mode = task
            .grant_autonomy
            .then(|| self.autonomous_mode.clone())
            .flatten()
            .or_else(|| self.base_mode.clone())
            .unwrap_or_else(|| "build".to_string());
        let selection = apb_core::zcode::effective_selection(&self.home, task.model);
        Ok(TaskRecord {
            task_id: session,
            trace_id: apb_core::zcode::trace_id(&report.raw),
            title: title(task),
            workspace_identity: index::workspace_identity(&host, &workspace_path),
            workspace_path,
            mode,
            model: selection
                .as_ref()
                .map(|s| format!("{}/{}", s.provider_id, s.model_id)),
            thought_level: selection.and_then(|s| s.effort),
            searchable_text: format!("{}\n{}", task.prompt, report.output),
            now_ms: i64::try_from(apb_core::clock::now_ms()).unwrap_or(i64::MAX),
        })
    }

    /// Writes the row for a successful attempt; warns on any failure.
    fn sync(&self, task: &AgentTask, report: &AgentReport) {
        let db = self.home.join(index::HOME_REL_TASKS_INDEX);
        let outcome = self
            .record(task, report)
            .and_then(|rec| index::insert_task(&db, &rec, Retry::default()));
        if let Err(e) = outcome {
            eprintln!(
                "apb: warning: zcode ui_sync skipped for node `{}`: {e}",
                task.node
            );
        }
    }
}

/// The desktop title of a session apb ran: the node, and the run when the
/// attempt belongs to one.
fn title(task: &AgentTask) -> String {
    let run = task
        .connector_policy
        .run_dir
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    match run {
        Some(run) => format!("apb: {} ({run})", task.node),
        None => format!("apb: {}", task.node),
    }
}

/// The value of the last `--mode` in `args` (`--mode x` or `--mode=x`); zcode
/// keeps the last one.
fn last_mode(args: &[String]) -> Option<String> {
    let mut found = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--mode" {
            found = it.next().cloned();
        } else if let Some(v) = a.strip_prefix("--mode=") {
            found = Some(v.to_string());
        }
    }
    found
}

impl AgentAdapter for ZcodeUiSync {
    fn run(&self, task: &AgentTask) -> Result<AgentReport, AgentFailure> {
        self.run_cancellable(task, &AtomicBool::new(false), None, None, None, None)
    }

    fn run_cancellable(
        &self,
        task: &AgentTask,
        cancel: &AtomicBool,
        on_spawn: Option<&dyn Fn(u32, u64)>,
        live: Option<&LiveHooks>,
        stall: Option<&StallHooks>,
        control: Option<&ControlHooks>,
    ) -> Result<AgentReport, AgentFailure> {
        let result = self
            .inner
            .run_cancellable(task, cancel, on_spawn, live, stall, control);
        if let Ok(report) = &result {
            self.sync(task, report);
        }
        result
    }

    fn spawn_supervisor(
        &self,
        brief: &str,
        model: &str,
        workdir: &Path,
        soul: Option<&str>,
        policy: &ConnectorEnvPolicy,
    ) -> Result<(), (ErrorClass, String)> {
        self.inner
            .spawn_supervisor(brief, model, workdir, soul, policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_last_mode_wins() {
        assert_eq!(
            last_mode(&v(&["-p", "x", "--mode", "build", "--json"])).as_deref(),
            Some("build")
        );
        assert_eq!(
            last_mode(&v(&["--mode", "build", "--mode=yolo"])).as_deref(),
            Some("yolo")
        );
        assert_eq!(last_mode(&v(&["--json"])), None);
        assert_eq!(last_mode(&v(&["--mode"])), None);
    }
}
