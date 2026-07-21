use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Global CLI config (`~/.config/playbook/config.yaml`, spec 4.2 / 7.1).
/// Describes coding agents (id -> launch command), the runner registry (8d),
/// and the web server port. Executors are bound through profiles (schema 2),
/// not executors.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalConfig {
    /// Default web server port (overridden by the CLI flag).
    pub port: Option<u16>,
    /// Agent descriptions: id -> command/transport.
    pub agents: BTreeMap<String, AgentDef>,
    /// Deprecated (schema 1). Executors were removed from the schema (Task 9),
    /// but an old config.yaml may still carry them. We accept and IGNORE them
    /// so loading doesn't break (otherwise deny_unknown_fields would block all
    /// runs until migration); `apb migrate` strips them from the file. Not
    /// serialized back out.
    #[serde(default, rename = "executors", skip_serializing)]
    pub legacy_executors: Option<serde_yaml_ng::Value>,
    /// Deprecated (schema 1), see `legacy_executors`.
    #[serde(default, rename = "default_executor", skip_serializing)]
    pub legacy_default_executor: Option<serde_yaml_ng::Value>,
    /// Runner registry for script nodes (8d): e.g. `ts: [bun, deno]`. The
    /// first one available on the machine is used.
    pub runners: BTreeMap<String, Vec<String>>,
    /// Auto-registration of workspaces in the project registry (spec 6.2).
    /// `None`/`true` enables it, `false` disables it. Also disabled by env
    /// `APB_NO_REGISTRY=1` and in CI.
    pub registry: Option<bool>,
    /// Days in the `unreachable` state before transitioning to `tombstoned`
    /// (spec 6.4). `None` -> 14.
    pub registry_unreachable_days: Option<u64>,
    /// Days to keep `tombstoned` before physical cleanup (spec 6.4).
    /// `None` -> 90.
    pub registry_purge_days: Option<u64>,
}

/// Transport used to communicate with the agent (spec 7.2).
/// - `headless`: one-shot buffered run (`claude -p ...`), the result is
///   collected on completion. Default, the simplest path.
/// - `acp`: streaming transport (currently based on Claude Code's
///   stream-json): agent events are streamed line by line (for the web UI
///   and logs), the final structured result is extracted from the terminal
///   event. A full Agent Client Protocol (JSON-RPC sessions, permissions,
///   multi-agent) is a follow-up built on top of this same value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Headless,
    Acp,
}

/// How the role system prompt (SOUL) is delivered by a given agent
/// (spec 6.3). This is a capability of the invocation, not a profile
/// preference.
/// - `prefix`: SOUL is prepended to the node prompt (priority equal to the
///   task, weaker protection against override);
/// - `native`: the agent's native system channel (e.g.
///   `--append-system-prompt`), requires `soul_flag` to be set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SoulDelivery {
    #[default]
    Prefix,
    Native,
}

/// Where the node prompt is passed: as an argv element (placeholder
/// `{prompt}`) or via stdin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptVia {
    #[default]
    Argv,
    Stdin,
}

/// Ask-transport ceiling for an interactive node's agent (spec 2026-07-20).
/// The engine treats it as a ceiling, not a promise: it downgrades at runtime
/// when the preferred transport cannot initialize (`Live` falls to `Resume`,
/// `Resume` falls to `Reprompt`).
/// - `live`: the agent asks through a blocking in-process MCP tool (Task 11).
/// - `resume`: the agent prints a question marker and stops; the answer round
///   re-enters the agent's own prior session via its resume form.
/// - `reprompt`: the agent prints a question marker and stops; the answer round
///   re-invokes the node from scratch with the Q&A transcript appended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Interaction {
    Live,
    Resume,
    #[default]
    Reprompt,
}

/// Declarative form of an agent invocation (spec 6.2). The `{prompt}` and
/// `{model}` placeholders are only valid as whole argv elements; no shell
/// interpolation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationDef {
    /// Argument template (without the program name). Placeholders:
    /// `{prompt}`, `{model}`.
    pub argv: Vec<String>,
    #[serde(default)]
    pub prompt_via: PromptVia,
    #[serde(default)]
    pub soul: SoulDelivery,
    /// System prompt flag; required when `soul: native`.
    #[serde(default)]
    pub soul_flag: Option<String>,
    #[serde(default)]
    pub transport: Transport,
    /// Extra argv appended when the engine grants the spawned agent autonomy
    /// for an authorized effectful run (spec 8.5). These are the agent's own
    /// "act without interactive approval" flags (e.g. claude's
    /// `--permission-mode bypassPermissions`), so a headless one-shot run can
    /// perform the file-writes and network access its effects already declared
    /// and the user already consented to at the run-authorization gate. Empty
    /// means the agent has no such mechanism (or it is not wired yet), and the
    /// run stays in the default permission mode.
    #[serde(default)]
    pub autonomous_args: Vec<String>,
    /// Ask-transport ceiling for interactive nodes (spec 2026-07-20). The
    /// engine treats it as a ceiling, not a promise: it downgrades at runtime
    /// when the preferred transport cannot initialize (missing session id,
    /// missing resume form). Defaults to `Reprompt`; the built-in agents set
    /// their own defaults in `apb_engine::invocation::builtin`, and a config
    /// agent may override it under `agents.<id>.invocation.interaction`.
    #[serde(default)]
    pub interaction: Interaction,
}

impl InvocationDef {
    /// Form invariants (spec 6.2), checked on load/resolve.
    pub fn validate(&self) -> Result<(), String> {
        let prompt_slots = self
            .argv
            .iter()
            .filter(|a| a.as_str() == "{prompt}")
            .count();
        let model_slots = self.argv.iter().filter(|a| a.as_str() == "{model}").count();
        for a in &self.argv {
            if a != "{prompt}" && a.contains("{prompt}") {
                return Err(format!(
                    "placeholder {{prompt}} must be a whole argv element, got `{a}`"
                ));
            }
            if a != "{model}" && a.contains("{model}") {
                return Err(format!(
                    "placeholder {{model}} must be a whole argv element, got `{a}`"
                ));
            }
        }
        match self.prompt_via {
            PromptVia::Argv => {
                if prompt_slots != 1 {
                    return Err(format!(
                        "expected exactly one {{prompt}} argv slot, found {prompt_slots}"
                    ));
                }
            }
            PromptVia::Stdin => {
                if prompt_slots != 0 {
                    return Err("prompt_via: stdin forbids a {prompt} argv slot".into());
                }
            }
        }
        if model_slots > 1 {
            return Err(format!(
                "{{model}} may appear at most once, found {model_slots}"
            ));
        }
        if self.soul == SoulDelivery::Native && self.soul_flag.is_none() {
            return Err("soul: native requires soul_flag".into());
        }
        Ok(())
    }
}

/// Description of a coding agent. Binary program, transport, and
/// (optionally) invocation form; when `invocation` is absent for the
/// built-in eight, a default is used (see `apb_engine::invocation::builtin`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDef {
    /// Command used to launch the agent (e.g. "claude"). None -> "claude".
    #[serde(default)]
    pub program: Option<String>,
    /// Communication transport. Defaults to headless.
    #[serde(default)]
    pub transport: Transport,
    /// Explicit invocation form; overrides the built-in default.
    #[serde(default)]
    pub invocation: Option<InvocationDef>,
    /// Enable presence detection for this custom agent (spec 7). The
    /// built-in eight are always probed; a custom agent only with
    /// `probe: true`.
    #[serde(default)]
    pub probe: Option<bool>,
    /// Candidate binary names for detection. Empty -> `program` or the id
    /// itself.
    #[serde(default)]
    pub bins: Vec<String>,
}

/// Whether an executable program exists on PATH (or at a direct path
/// containing a separator). A manual PATH scan without external crates,
/// the way `which` does it. The executability check is platform-specific:
/// on Unix, a regular file with the exec bit set; on Windows, the file
/// itself or with an extension from PATHEXT.
pub fn program_in_path(program: &str) -> bool {
    if program.contains('/') || program.contains('\\') {
        return is_executable(std::path::Path::new(program));
    }
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(program)))
}

/// Whether the path is a runnable program (see `program_in_path`).
#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable(p: &std::path::Path) -> bool {
    if p.is_file() {
        return true;
    }
    // Without an extension, try variants from PATHEXT (.EXE/.CMD/.BAT/...).
    if p.extension().is_some() {
        return false;
    }
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    exts.split(';').filter(|e| !e.is_empty()).any(|ext| {
        let mut cand = p.as_os_str().to_os_string();
        cand.push(ext);
        std::path::Path::new(&cand).is_file()
    })
}

#[cfg(not(any(unix, windows)))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}

/// Global config directory: `APB_CONFIG_DIR` (override for tests/local),
/// then `XDG_CONFIG_HOME/playbook`, then `~/.config/playbook`. None - if
/// no environment variable is set (config-less path).
pub fn config_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("APB_CONFIG_DIR")
        && !d.is_empty()
    {
        return Some(PathBuf::from(d));
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("apb"));
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".config/apb"))
}

impl GlobalConfig {
    /// Loads config from `<config_dir>/config.yaml`. A missing directory or
    /// file - empty default (the config-less path remains functional). A
    /// parse error is returned as Err so a broken config isn't silently
    /// swallowed.
    pub fn load() -> Result<Self, String> {
        let Some(dir) = config_dir() else {
            return Ok(Self::default());
        };
        let path = dir.join("config.yaml");
        if !path.is_file() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        serde_yaml_ng::from_str(&raw)
            .map_err(|e| format!("invalid global config `{}`: {e}", path.display()))
    }

    /// Launch command for agent `id`, if it's described in the config.
    pub fn agent_program(&self, id: &str) -> Option<String> {
        self.agents.get(id).and_then(|a| a.program.clone())
    }

    /// Transport for agent `id`; defaults to headless.
    pub fn agent_transport(&self, id: &str) -> Transport {
        self.agents.get(id).map(|a| a.transport).unwrap_or_default()
    }

    /// Candidate runtimes for a runner key: config (`runners`) first, then
    /// the built-in default. None - the key is unknown and not set in the
    /// config. Single source for launching script nodes and for `apb
    /// doctor`.
    pub fn runner_candidates(&self, runner: &str) -> Option<Vec<String>> {
        if let Some(list) = self.runners.get(runner)
            && !list.is_empty()
        {
            return Some(list.clone());
        }
        match runner {
            "ts" => Some(vec!["bun".to_string(), "deno".to_string()]),
            "py" => Some(vec!["python3".to_string()]),
            "sh" | "bash" => Some(vec!["sh".to_string()]),
            _ => None,
        }
    }
}
