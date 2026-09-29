//! Execution mode (host execution mode, informally "mono-agent" mode): who
//! executes a run's agent steps.
//!
//! In `cli` mode (always the default) the engine spawns the agent CLI a
//! node's profile names. In `host` mode it spawns nothing: every agent step
//! becomes a host task that the agent which started the run (the MCP host)
//! executes with its own subagents and submits back.
//!
//! Host mode happens in exactly two ways, and nothing here detects or
//! special-cases a host by name:
//!
//! - **explicit request:** the host agent passes `execution: host` on
//!   `playbook_run` (CLI: `apb run --execution host`) because the person
//!   asked for it. There is no machine-level switch that turns it on.
//! - **fallback:** a `cli` run started by an MCP host session turns a single
//!   agent step into a host task when no CLI of the step's chain can start at
//!   all (the binary is missing, or it is not logged in). Global
//!   `execution.fallback_to_host: false` and a project's
//!   `execution.fallback_to_host: false` disable it.
//!
//! `APB_EXECUTION=cli` in the process environment is the kill switch: it
//! forces `cli` and disables the fallback. The execution is resolved once per
//! run, at start, and written into the run manifest, so a resume keeps it. A
//! sub-playbook inherits its parent's execution.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The environment variable of the kill switch. Only the value `cli` has an
/// effect.
pub const EXECUTION_ENV: &str = "APB_EXECUTION";

/// Who executes a run's agent steps.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    /// The engine spawns the agent CLI the profile names.
    #[default]
    Cli,
    /// The agent that started the run executes every agent step through its
    /// own subagents (host tasks).
    Host,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionMode::Cli => "cli",
            ExecutionMode::Host => "host",
        }
    }

    /// Parses `cli` or `host` (case-insensitive, trimmed).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cli" => Some(ExecutionMode::Cli),
            "host" => Some(ExecutionMode::Host),
            _ => None,
        }
    }
}

impl std::fmt::Display for ExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `execution:` section of the global `config.yaml` (defined next to the
/// config it belongs to, so this module depends on `config` and not back).
pub use crate::config::ExecutionSettings;

/// Where a run's execution mode came from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSource {
    /// `APB_EXECUTION=cli`.
    KillSwitch,
    /// The run argument (`execution` on `playbook_run`, `--execution`).
    Argument,
    /// The parent run (a sub-playbook).
    Inherited,
    /// Nothing asked for anything: `cli`.
    Default,
}

impl ExecutionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionSource::KillSwitch => "kill_switch",
            ExecutionSource::Argument => "argument",
            ExecutionSource::Inherited => "inherited",
            ExecutionSource::Default => "default",
        }
    }
}

/// What a launch surface asks for. `mode` is the run argument;
/// `host_session` says the run is started by an MCP host session that can
/// serve host tasks (the only case with a fallback); `client` is that
/// session's MCP client name, recorded for attribution only; `inherited`
/// marks a sub-playbook child whose fields are its parent's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionRequest {
    pub mode: Option<ExecutionMode>,
    pub host_session: bool,
    pub client: Option<String>,
    pub inherited: bool,
}

/// The resolved execution of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedExecution {
    pub mode: ExecutionMode,
    pub source: ExecutionSource,
    /// Whether a `cli` step whose CLI cannot start becomes a host task. Only
    /// ever true for a `cli` run started by an MCP host session.
    pub fallback_to_host: bool,
    /// The MCP client name the request carried, kept for attribution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Something the resolution ignored and the caller should see (a project
    /// that tried to turn host mode on, an unknown `APB_EXECUTION` value).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The project's `execution:` section. Only `fallback_to_host: false` has an
/// effect; the other values are read so that a project trying to set them is
/// reported instead of silently ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ProjectExecution {
    pub fallback_to_host: Option<bool>,
    pub mode: Option<String>,
}

/// The pure resolution (see the module docs). `global` and `project` are the
/// `execution:` sections of the two config files, `env` the raw
/// `APB_EXECUTION` value.
pub fn resolve(
    request: &ExecutionRequest,
    global: &ExecutionSettings,
    project: &ProjectExecution,
    env: Option<&str>,
) -> ResolvedExecution {
    let mut notes = Vec::new();
    let client = request
        .client
        .as_ref()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    let killed = match env.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) if ExecutionMode::parse(v) == Some(ExecutionMode::Cli) => true,
        Some(v) => {
            notes.push(format!(
                "{EXECUTION_ENV}={v} ignored: the variable can only force cli"
            ));
            false
        }
        None => false,
    };
    if let Some(m) = &project.mode {
        notes.push(format!(
            "the project .apb/config.yaml sets execution.mode: {m}, which is ignored: a project may only set execution.fallback_to_host: false"
        ));
    }
    if project.fallback_to_host == Some(true) {
        notes.push(
            "the project .apb/config.yaml sets execution.fallback_to_host: true, which is ignored: a project may only turn the fallback off".to_string(),
        );
    }
    if killed {
        return ResolvedExecution {
            mode: ExecutionMode::Cli,
            source: ExecutionSource::KillSwitch,
            fallback_to_host: false,
            client,
            notes,
        };
    }
    let (mode, source) = match request.mode {
        Some(m) if request.inherited => (m, ExecutionSource::Inherited),
        Some(m) => (m, ExecutionSource::Argument),
        None => (ExecutionMode::Cli, ExecutionSource::Default),
    };
    let fallback_to_host = mode == ExecutionMode::Cli
        && request.host_session
        && global.fallback_to_host != Some(false)
        && project.fallback_to_host != Some(false);
    ResolvedExecution {
        mode,
        source,
        fallback_to_host,
        client,
        notes,
    }
}

/// Partial view of the project `.apb/config.yaml`: only `execution:`.
/// Tolerant of every other key, like the other partial readers of that file.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ProjectFile {
    execution: ProjectExecution,
}

/// The project's `execution:` section. A missing file or section is empty;
/// a malformed file is an error.
pub fn project_execution(root: &Path) -> Result<ProjectExecution, String> {
    let path = root.join(".apb/config.yaml");
    if !path.is_file() {
        return Ok(ProjectExecution::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed: ProjectFile = serde_yaml_ng::from_str(&raw)
        .map_err(|e| format!("invalid project config `{}`: {e}", path.display()))?;
    Ok(parsed.execution)
}

/// Resolves the execution of a run started in `root`, reading the global
/// config, the project config and the environment. A project config that
/// does not parse disables the fallback with a note (fail-safe: an unreadable
/// opt-out must not leave the fallback on).
pub fn resolve_for(root: &Path, request: &ExecutionRequest) -> Result<ResolvedExecution, String> {
    let global = crate::config::GlobalConfig::load()?;
    let env = std::env::var(EXECUTION_ENV).ok();
    match project_execution(root) {
        Ok(project) => Ok(resolve(
            request,
            &global.execution,
            &project,
            env.as_deref(),
        )),
        Err(e) => {
            let off = ProjectExecution {
                fallback_to_host: Some(false),
                mode: None,
            };
            let mut r = resolve(request, &global.execution, &off, env.as_deref());
            r.notes.push(e);
            Ok(r)
        }
    }
}

/// One line for `apb doctor` and the adoption report: the default mode, how
/// host mode is chosen, and whether the fallback is on for runs an MCP host
/// session starts here.
pub fn doctor_line(root: &Path) -> (bool, String) {
    let request = ExecutionRequest {
        host_session: true,
        ..Default::default()
    };
    match resolve_for(root, &request) {
        Ok(r) => {
            let mut line = match r.source {
                ExecutionSource::KillSwitch => {
                    "cli, forced by APB_EXECUTION=cli (host mode and the host fallback are off)"
                        .to_string()
                }
                _ => format!(
                    "cli by default; host mode only when a run asks for it (execution: host), and then profile CLIs are ignored; host fallback for MCP-started runs {}",
                    if r.fallback_to_host { "on" } else { "off" }
                ),
            };
            for n in &r.notes {
                line.push_str("; ");
                line.push_str(n);
            }
            (r.notes.is_empty(), line)
        }
        Err(e) => (false, format!("cannot resolve: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(mode: Option<ExecutionMode>, host_session: bool) -> ExecutionRequest {
        ExecutionRequest {
            mode,
            host_session,
            client: Some("some-host".into()),
            inherited: false,
        }
    }

    fn none() -> ProjectExecution {
        ProjectExecution::default()
    }

    #[test]
    fn nothing_set_is_cli_without_fallback_outside_a_host_session() {
        let r = resolve(
            &req(None, false),
            &ExecutionSettings::default(),
            &none(),
            None,
        );
        assert_eq!(
            (r.mode, r.source, r.fallback_to_host),
            (ExecutionMode::Cli, ExecutionSource::Default, false)
        );
    }

    #[test]
    fn a_host_session_gets_cli_with_the_fallback() {
        let r = resolve(
            &req(None, true),
            &ExecutionSettings::default(),
            &none(),
            None,
        );
        assert_eq!((r.mode, r.fallback_to_host), (ExecutionMode::Cli, true));
        assert_eq!(r.client.as_deref(), Some("some-host"));
    }

    #[test]
    fn the_run_argument_selects_host_mode() {
        let r = resolve(
            &req(Some(ExecutionMode::Host), true),
            &ExecutionSettings::default(),
            &none(),
            None,
        );
        assert_eq!(
            (r.mode, r.source, r.fallback_to_host),
            (ExecutionMode::Host, ExecutionSource::Argument, false)
        );
    }

    #[test]
    fn global_and_project_can_turn_the_fallback_off() {
        let off = ExecutionSettings {
            fallback_to_host: Some(false),
        };
        assert!(!resolve(&req(None, true), &off, &none(), None).fallback_to_host);
        let project_off = ProjectExecution {
            fallback_to_host: Some(false),
            mode: None,
        };
        assert!(
            !resolve(
                &req(None, true),
                &ExecutionSettings::default(),
                &project_off,
                None
            )
            .fallback_to_host
        );
    }

    #[test]
    fn a_project_can_never_turn_host_mode_or_the_fallback_on() {
        let project = ProjectExecution {
            fallback_to_host: Some(true),
            mode: Some("host".into()),
        };
        let off = ExecutionSettings {
            fallback_to_host: Some(false),
        };
        let r = resolve(&req(None, true), &off, &project, None);
        assert_eq!((r.mode, r.fallback_to_host), (ExecutionMode::Cli, false));
        assert_eq!(r.notes.len(), 2, "{:?}", r.notes);
    }

    #[test]
    fn env_kill_switch_forces_cli_and_disables_the_fallback() {
        let r = resolve(
            &req(Some(ExecutionMode::Host), true),
            &ExecutionSettings::default(),
            &none(),
            Some("cli"),
        );
        assert_eq!(
            (r.mode, r.source, r.fallback_to_host),
            (ExecutionMode::Cli, ExecutionSource::KillSwitch, false)
        );
    }

    #[test]
    fn env_cannot_force_host() {
        let r = resolve(
            &req(None, false),
            &ExecutionSettings::default(),
            &none(),
            Some("host"),
        );
        assert_eq!(r.mode, ExecutionMode::Cli);
        assert!(r.notes[0].contains("can only force cli"));
    }

    #[test]
    fn inherited_request_reports_inherited() {
        let r = resolve(
            &ExecutionRequest {
                mode: Some(ExecutionMode::Host),
                host_session: true,
                client: None,
                inherited: true,
            },
            &ExecutionSettings::default(),
            &none(),
            None,
        );
        assert_eq!(
            (r.mode, r.source),
            (ExecutionMode::Host, ExecutionSource::Inherited)
        );
    }

    #[test]
    fn project_execution_reads_only_its_section() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            project_execution(dir.path()).unwrap(),
            ProjectExecution::default()
        );
        std::fs::create_dir_all(dir.path().join(".apb")).unwrap();
        std::fs::write(
            dir.path().join(".apb/config.yaml"),
            "skills_dir: x\nexecution:\n  fallback_to_host: false\n",
        )
        .unwrap();
        assert_eq!(
            project_execution(dir.path()).unwrap().fallback_to_host,
            Some(false)
        );
    }

    #[test]
    fn global_config_accepts_the_execution_section() {
        let g: crate::config::GlobalConfig =
            serde_yaml_ng::from_str("execution:\n  fallback_to_host: false\n").unwrap();
        assert_eq!(g.execution.fallback_to_host, Some(false));
        assert!(serde_yaml_ng::from_str::<ExecutionSettings>("mode: host\n").is_err());
    }
}
