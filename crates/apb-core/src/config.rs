use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// Timing knobs for the suggestion-decision store (spec
    /// 2026-07-29-suggestion-decisions-design). Absent keys fall back to the
    /// named constants in `crate::dismiss`; a project `.apb/config.yaml` may
    /// override either key for its own project.
    pub suggestions: SuggestionSettings,
    /// Server-deployment knobs (spec 2026-08-16-server-mode-design). Absent
    /// section means the historical behavior: loopback bind, no proxy trust.
    pub server: ServerConfig,
    /// Inbound webhook listener (spec 2026-08-16-webhook-ingest-design).
    /// Absent section means disabled, which is the historical behavior: apb
    /// opens no inbound port unless an operator asks for one.
    pub ingest: IngestConfig,
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
/// built-in nine, a default is used (see `apb_engine::invocation::builtin`).
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
    /// built-in nine are always probed; a custom agent only with
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

/// Optional `suggestions:` section, in the global config and in the project
/// `.apb/config.yaml`. Every key is optional so a user who never touches the
/// section keeps the defaults; a present key overrides only itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SuggestionSettings {
    /// Escalating snooze for repeated soft declines, in days.
    pub soft_backoff_days: Option<Vec<u64>>,
    /// Silence window for a hard dismissal, in days.
    pub hard_ttl_days: Option<u64>,
}

impl SuggestionSettings {
    /// Semantic checks the serde layer cannot express: values are days, so
    /// positive integers, and an empty schedule is a configuration error
    /// rather than "never snooze".
    pub fn validate(&self) -> Result<(), String> {
        if let Some(days) = &self.soft_backoff_days {
            if days.is_empty() {
                return Err("suggestions.soft_backoff_days must not be empty".into());
            }
            if days.contains(&0) {
                return Err(
                    "suggestions.soft_backoff_days values are days and must be positive".into(),
                );
            }
        }
        if self.hard_ttl_days == Some(0) {
            return Err(
                "suggestions.hard_ttl_days is a number of days and must be positive".into(),
            );
        }
        Ok(())
    }
}

/// Partial view of the project `.apb/config.yaml`: only the `suggestions:`
/// section. Deliberately tolerant of the other keys that file carries
/// (`skills_dir`, `port`), the same way `crate::skills` reads it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ProjectSuggestionsFile {
    suggestions: SuggestionSettings,
}

/// The project-level `suggestions:` section. A missing file is an empty
/// section; a malformed one is an error so a typo is not silently ignored.
pub fn project_suggestion_settings(root: &Path) -> Result<SuggestionSettings, String> {
    let path = root.join(".apb/config.yaml");
    if !path.is_file() {
        return Ok(SuggestionSettings::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed: ProjectSuggestionsFile = serde_yaml_ng::from_str(&raw)
        .map_err(|e| format!("invalid project config `{}`: {e}", path.display()))?;
    Ok(parsed.suggestions)
}

/// The address the dashboard binds when no flag is given. Loopback, because a
/// server that anyone on the network can reach must be an explicit decision.
pub const DEFAULT_BIND: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Optional `server:` section of the global config (spec
/// 2026-08-16-server-mode-design). Every key is optional: an operator who
/// never touches the section keeps today's loopback-only behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// IP address to bind, parsed at startup. `0.0.0.0` for a server
    /// deployment. An unparseable value is a startup error, never a silent
    /// fallback to loopback.
    pub bind: Option<String>,
    /// Public origin the dashboard is reached at, e.g.
    /// `https://apb.example.com`. Used to print absolute URLs and to decide
    /// whether the session cookie carries the `Secure` attribute.
    pub public_base_url: Option<String>,
    /// Exact peer IPs whose `X-Forwarded-For` and `X-Forwarded-Proto` headers
    /// are believed. No CIDR ranges in v1: a reverse proxy has one address.
    /// Forwarded headers are never used for an authentication decision, only
    /// for rate-limit keying and logging.
    pub trusted_proxies: Vec<String>,
    /// How long a run start may sit in the workdir queue when another
    /// write-run already holds the shared workdir lock, in seconds. `None`
    /// gives [`DEFAULT_WORKDIR_QUEUE_WAIT_SECONDS`]; `0` turns queueing off
    /// and restores the immediate refusal (HTTP 429), which is the right
    /// setting only where the caller is a human who can retry.
    pub workdir_queue_wait_seconds: Option<u64>,
}

/// How long an admitted run waits for the shared workdir by default.
///
/// Fifteen minutes is chosen against what actually holds the lock: one other
/// write-run, which is bounded by its own node timeouts rather than by
/// anything the queue can see. Long enough that an inbound event arriving
/// mid-run is executed rather than discarded, short enough that a workdir
/// wedged by a stuck holder surfaces as a failed run within the hour instead
/// of a thread parked forever.
pub const DEFAULT_WORKDIR_QUEUE_WAIT_SECONDS: u64 = 900;

impl ServerConfig {
    /// The workdir queue ceiling for run starts, or `None` when the operator
    /// turned queueing off with an explicit `0`.
    pub fn workdir_queue_wait(&self) -> Option<Duration> {
        match self.workdir_queue_wait_seconds {
            Some(0) => None,
            Some(secs) => Some(Duration::from_secs(secs)),
            None => Some(Duration::from_secs(DEFAULT_WORKDIR_QUEUE_WAIT_SECONDS)),
        }
    }

    /// Bind precedence: `--bind` flag, then `server.bind`, then loopback.
    pub fn resolve_bind(&self, flag: Option<&str>) -> Result<IpAddr, String> {
        match flag.or(self.bind.as_deref()) {
            None => Ok(DEFAULT_BIND),
            Some(raw) => raw
                .trim()
                .parse::<IpAddr>()
                .map_err(|e| format!("invalid bind address `{raw}`: {e}")),
        }
    }

    /// `trusted_proxies` as parsed addresses. A CIDR range or any other
    /// unparseable entry is an error rather than a silently ignored line.
    pub fn trusted_proxy_set(&self) -> Result<BTreeSet<IpAddr>, String> {
        let mut out = BTreeSet::new();
        for raw in &self.trusted_proxies {
            let addr = raw.trim().parse::<IpAddr>().map_err(|e| {
                format!("invalid server.trusted_proxies entry `{raw}`: {e} (exact IP addresses only, no CIDR ranges)")
            })?;
            out.insert(addr);
        }
        Ok(out)
    }

    /// Whether the configured public origin is https, which is one of the two
    /// signals that make the session cookie `Secure`.
    pub fn public_scheme_is_https(&self) -> bool {
        self.public_base_url
            .as_deref()
            .map(|u| u.trim().starts_with("https://"))
            .unwrap_or(false)
    }
}

/// The port the ingest listener binds when nothing overrides it. Adjacent to
/// the dashboard's 7321 so the pair is easy to remember and to firewall.
pub const DEFAULT_INGEST_PORT: u16 = 7322;

/// Optional `ingest:` section of the global config (spec
/// 2026-08-16-webhook-ingest-design). The listener it configures is a
/// separate socket with a separate router carrying only the hook routes: a
/// tunnel or proxy pointed at this port is structurally incapable of
/// reaching the dashboard API.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestConfig {
    /// Whether `apb dashboard` co-starts the ingest listener. Off by default:
    /// an inbound port is an explicit decision. It does not gate `apb ingest`,
    /// which runs when it is asked to and prints an advisory noting that the
    /// dashboard will not start the same listener on its own.
    pub enabled: bool,
    /// IP address to bind. Loopback by default, because the supported
    /// topology puts a TLS-terminating reverse proxy on the same host. An
    /// unparseable value is a startup error, never a silent fallback.
    pub bind: Option<String>,
    /// Port to bind. `None` means [`DEFAULT_INGEST_PORT`].
    pub port: Option<u16>,
    /// Public origin the provider reaches the hooks at, e.g.
    /// `https://hooks.example.com`. Used only to print the exact callback URL
    /// an operator pastes into a provider console; apb never fetches it.
    pub public_base_url: Option<String>,
}

impl IngestConfig {
    /// Bind precedence: flag, then `ingest.bind`, then loopback.
    pub fn resolve_bind(&self, flag: Option<&str>) -> Result<IpAddr, String> {
        match flag.or(self.bind.as_deref()) {
            None => Ok(DEFAULT_BIND),
            Some(raw) => raw
                .trim()
                .parse::<IpAddr>()
                .map_err(|e| format!("invalid ingest bind address `{raw}`: {e}")),
        }
    }

    /// Port precedence: flag, then `ingest.port`, then the default.
    pub fn resolve_port(&self, flag: Option<u16>) -> u16 {
        flag.or(self.port).unwrap_or(DEFAULT_INGEST_PORT)
    }

    /// The exact URL to register with a provider for one connector account.
    /// Building it here rather than in each caller keeps the doctor, the
    /// dashboard and the docs from drifting apart on the path.
    ///
    /// The two ways it can fail are separate values rather than one `None`,
    /// because an operator is told what to fix: a doctor that collapsed them
    /// told someone with a perfectly good `public_base_url` and an account
    /// named `Main Account` to go set a config key that was already right.
    pub fn callback_url(&self, connector: &str, account: &str) -> Result<String, CallbackGap> {
        for segment in [connector, account] {
            if crate::profile::validate_profile_name(segment).is_err() {
                return Err(CallbackGap::UnroutableSegment(segment.to_string()));
            }
        }
        let base = self
            .public_base_url
            .as_deref()
            .map(|u| u.trim().trim_end_matches('/'))
            .unwrap_or("");
        if base.is_empty() {
            return Err(CallbackGap::NoPublicBase);
        }
        Ok(format!("{base}/hooks/{connector}/{account}"))
    }
}

/// Why no callback URL could be printed for a connector account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackGap {
    /// `ingest.public_base_url` is unset, or set to nothing usable.
    NoPublicBase,
    /// The named connector or account is not a routable path segment, so no
    /// hook URL could address it whatever the public base is.
    UnroutableSegment(String),
}
