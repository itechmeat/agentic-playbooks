use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The dashboard's port when neither `--port` nor `port:` in the global
/// config names one. The web dev server proxies to it too (generated into
/// `web/src/lib/api.gen.ts`).
pub const DEFAULT_PORT: u16 = 7321;

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
    pub legacy_executors: Option<yaml_serde::Value>,
    /// Deprecated (schema 1), see `legacy_executors`.
    #[serde(default, rename = "default_executor", skip_serializing)]
    pub legacy_default_executor: Option<yaml_serde::Value>,
    /// Runner registry for script nodes (8d): e.g. `ts: [bun, deno]`. The
    /// first one available on the machine is used.
    pub runners: BTreeMap<String, Vec<String>>,
    /// Auto-registration of workspaces in the project registry (spec 6.2).
    /// `None`/`true` enables it, `false` disables it. Also disabled by env
    /// `APB_NO_REGISTRY=1` and in CI.
    pub registry: Option<bool>,
    /// Days in the `unreachable` state before transitioning to `tombstoned`
    /// (spec 6.4). `None` -> 14. Setting this or `registry_purge_days` also
    /// moves the projects registry's hard auto-drop from 30 days to
    /// max(30, unreachable + purge) days.
    pub registry_unreachable_days: Option<u64>,
    /// Days to keep `tombstoned` before physical cleanup (spec 6.4).
    /// `None` -> 90. Setting this or `registry_unreachable_days` also moves
    /// the projects registry's hard auto-drop from 30 days to
    /// max(30, unreachable + purge) days.
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
    /// The user's rules for which models profiles may use (e.g. "keep routine
    /// work on a smaller model"), enforced by `apb validate`, `apb doctor` and the adoption
    /// report (`crate::model_check`). Empty means no rule.
    pub model_policy: Vec<ModelRule>,
    // --- host execution mode (0.23.0) ---
    /// Host execution mode knobs (`crate::execution`): `fallback_to_host`
    /// turns the host-task fallback of MCP-started runs off. Host mode itself
    /// has no machine switch: a run asks for it.
    pub execution: ExecutionSettings,
    // --- end host execution mode ---
    /// Size limits on what apb keeps of agent output
    /// (`crate::output_limits`). Absent keys keep the defaults; a project
    /// `.apb/config.yaml` may only lower them.
    pub output_limits: crate::output_limits::OutputLimitSettings,
    /// "Notify me" (`crate::notify`): the target notifications go to and
    /// whether every run notifies by default. Absent: no run notifies
    /// unless it names a target itself.
    pub notify: crate::notify::NotifySettings,
    /// History discovery (`crate::discover`): the agent history roots it
    /// may read and how far back. Absent: the agents' own default
    /// directories, 90 days.
    pub discover: DiscoverSettings,
    /// The machine's isolation settings (`crate::isolation`): the bwrap
    /// binary and extra read-only paths every sandbox binds. Absent: bwrap
    /// from `/usr/bin` or PATH, no extra paths.
    pub isolation: crate::isolation::IsolationSettings,
}

/// The agent kinds a history root may hold.
pub const HISTORY_AGENTS: [&str; 2] = ["claude_code", "codex"];

/// One named history root of the `discover:` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRootSetting {
    /// How every surface names the root (`[a-z0-9][a-z0-9_-]*`): a path is
    /// never asked for or shown by a surface, only this name.
    pub name: String,
    /// One of [`HISTORY_AGENTS`].
    pub agent: String,
    /// The directory: absolute, or under the home directory as `~/...`
    /// (a relative path would read a different directory from each
    /// surface's working directory).
    pub path: String,
}

/// The `discover:` section of the global config.
///
/// ```yaml
/// discover:
///   roots: [{ name: claude_code, agent: claude_code, path: ~/.claude/projects }]
///   since_days: 90
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoverSettings {
    /// When given, these roots replace the defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<HistoryRootSetting>>,
    /// How many days back sessions are read, 1 to 365.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_days: Option<u32>,
}

impl DiscoverSettings {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(d) = self.since_days
            && !(1..=365).contains(&d)
        {
            return Err("discover.since_days must be between 1 and 365".into());
        }
        let mut seen = BTreeSet::new();
        for r in self.roots.iter().flatten() {
            if !is_root_name(&r.name) {
                return Err(format!(
                    "discover.roots: `{}` is not a root name ([a-z0-9][a-z0-9_-]*, at most 64 characters)",
                    r.name
                ));
            }
            if !HISTORY_AGENTS.contains(&r.agent.as_str()) {
                return Err(format!(
                    "discover.roots.{}: unknown agent `{}` (known: {})",
                    r.name,
                    r.agent,
                    HISTORY_AGENTS.join(", ")
                ));
            }
            let p = r.path.as_str();
            if !(p.starts_with("~/") && p.len() > 2 || std::path::Path::new(p).is_absolute()) {
                return Err(format!(
                    "discover.roots.{}: path must be absolute or start with `~/`",
                    r.name
                ));
            }
            if !seen.insert(r.name.as_str()) {
                return Err(format!("discover.roots: `{}` is named twice", r.name));
            }
        }
        Ok(())
    }
}

/// A history root's name: `[a-z0-9][a-z0-9_-]*`, at most 64 characters.
pub fn is_root_name(name: &str) -> bool {
    name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

// --- host execution mode (0.23.0) ---
/// The `execution:` section of the global `config.yaml` (see
/// `crate::execution`).
///
/// ```yaml
/// execution:
///   fallback_to_host: false   # default true
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_to_host: Option<bool>,
}
// --- end host execution mode ---

/// One `model_policy` rule: for `agent` (and, with `when`, only its models
/// matching that glob), a profile model must match one of the `allow` globs.
/// Globs match case-insensitively; an `@effort` suffix is ignored.
///
/// ```yaml
/// model_policy:
///   - agent: claude
///     allow: ["*sonnet*", "*haiku*"]
///     reason: keep routine work on a smaller model
///   - agent: opencode
///     when: "anthropic/*"
///     allow: ["*haiku*"]
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRule {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
    /// `{prompt}`, `{model}`. A `--` element right before `{prompt}` marks the
    /// prompt as a trailing positional: the engine emits `-- <prompt>` last,
    /// after every flag it appends (SOUL, autonomy, session), so a prompt that
    /// starts with `-` is never parsed as an option. Without that marker the
    /// prompt is taken to be an option's value, and a dash-led prompt is sent
    /// with a leading newline instead.
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
/// built-in ten, a default is used (see `apb_engine::invocation::builtin`).
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
    /// built-in ten are always probed; a custom agent only with
    /// `probe: true`.
    #[serde(default)]
    pub probe: Option<bool>,
    /// Candidate binary names for detection. Empty -> `program` or the id
    /// itself.
    #[serde(default)]
    pub bins: Vec<String>,
    /// Desktop-history sync (zcode only; other agents ignore it). `true`:
    /// after every finished zcode session apb inserts a row into ZCode's
    /// desktop task index so the session shows in the desktop app's history
    /// (see `crate::zcode_tasks_index`). Best effort: a failed sync is a
    /// warning, never a failed step. Absent or `false`: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_sync: Option<bool>,
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
/// then `XDG_CONFIG_HOME/apb`, then `~/.config/apb`. None - if
/// no environment variable is set (config-less path).
pub fn config_dir() -> Option<PathBuf> {
    let dir = resolve_config_dir();
    #[cfg(all(debug_assertions, any(test, feature = "testing")))]
    if let Some(d) = &dir {
        test_isolation::refuse_forbidden(d);
    }
    dir
}

fn resolve_config_dir() -> Option<PathBuf> {
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

/// Keeps every test process off the developer's real config dir. A suite
/// that ran apb code or spawned `apb` with nothing redirecting the config
/// dir used to register its temp projects in the real `projects.json`
/// (thousands of `/tmp/.tmp*` entries) and write launch floors there.
///
/// [`isolate_test_config!`](crate::isolate_test_config) installs
/// [`isolate`](test_isolation::isolate) before `main` in a test binary: it
/// records the dirs a leak would reach in [`FORBIDDEN_ENV`]
/// (test_isolation::FORBIDDEN_ENV) and points `XDG_CONFIG_HOME` at a
/// sandbox, which every process the test spawns inherits. From then on
/// [`config_dir`] panics when it resolves one of the forbidden dirs, in the
/// test process and in a spawned test-build `apb` alike, so a leak fails
/// loudly instead of writing. Test builds only (debug assertions plus
/// `cfg(test)` or the `testing` feature), so no release binary has it.
#[cfg(all(debug_assertions, any(test, feature = "testing")))]
pub mod test_isolation {
    use std::path::{Path, PathBuf};

    /// The config dirs no test process may resolve, as a path list.
    pub const FORBIDDEN_ENV: &str = "APB_TEST_FORBIDDEN_CONFIG_DIRS";

    /// The config dirs no test process may resolve, as a path list: what
    /// [`resolve_config_dir`] reaches right now, the real (password
    /// database) home's `.config/apb`, and the config dir `HOME` names, even
    /// when `HOME` differs from the password database entry, since that is
    /// the home a redirected test would actually read.
    pub fn forbidden_paths() -> Vec<PathBuf> {
        let mut real: Vec<PathBuf> = Vec::new();
        real.extend(super::resolve_config_dir());
        real.extend(super::real_home().map(|h| h.join(".config/apb")));
        if let Ok(h) = std::env::var("HOME")
            && !h.is_empty()
        {
            let home_dir = PathBuf::from(h).join(".config/apb");
            if !real.contains(&home_dir) {
                real.push(home_dir);
            }
        }
        real
    }

    /// Records the real config dirs in [`FORBIDDEN_ENV`] and redirects
    /// `XDG_CONFIG_HOME` to `sandbox`, removing `APB_CONFIG_DIR`.
    /// `APB_TEST_DEFAULT_CONFIG_DIR` is set to the sandbox too, so
    /// [`default_config_dir`](super::default_config_dir) resolves inside it
    /// and never at the developer's real home. A process that inherited the
    /// list (a test binary or `apb` spawned by a test) keeps it: its own
    /// config dir is already a sandbox. Runs before `main`, so no other
    /// thread exists yet.
    pub fn isolate(sandbox: &Path) {
        if std::env::var_os(FORBIDDEN_ENV).is_none()
            && let Ok(list) = std::env::join_paths(forbidden_paths())
        {
            // SAFETY: before `main`, single-threaded.
            unsafe { std::env::set_var(FORBIDDEN_ENV, list) };
        }
        // SAFETY: before `main`, single-threaded.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", sandbox);
            std::env::set_var("APB_TEST_DEFAULT_CONFIG_DIR", sandbox);
            std::env::remove_var("APB_CONFIG_DIR");
        }
    }

    /// Prefix of the per-process sandboxes [`isolate_under`] makes. The name
    /// is `<prefix><pid>-<random>`, so another process cannot predict it.
    pub const SANDBOX_PREFIX: &str = "apb-test-config-";

    /// A fresh sandbox directory under `base`, named with this process's pid
    /// and a random suffix and made with `create_dir`, so a taken name is
    /// retried with another suffix. The caller created it, so an at-exit
    /// removal only ever removes what this process made.
    pub fn sandbox_for(base: &Path) -> PathBuf {
        use std::hash::{BuildHasher, Hasher};
        for _ in 0..16 {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default(),
            );
            let dir = base.join(format!(
                "{SANDBOX_PREFIX}{}-{:016x}",
                std::process::id(),
                h.finish()
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => return dir,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => break,
            }
        }
        base.join(format!("{SANDBOX_PREFIX}{}", std::process::id()))
    }

    /// The owning pid of a sandbox name, if the name carries one.
    fn sandbox_owner(name: &std::ffi::OsStr) -> Option<u32> {
        name.to_str()?
            .strip_prefix(SANDBOX_PREFIX)?
            .split('-')
            .next()?
            .parse()
            .ok()
    }

    /// Whether a process with this pid exists (unix).
    #[cfg(unix)]
    fn pid_alive(pid: u32) -> bool {
        // SAFETY: `kill` with signal 0 only probes the process.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// Removes the sandbox directories of other processes whose owning
    /// process is provably gone (the pid in the name no longer exists, or
    /// the sandbox mtime is a week old, so a pid reused by an unrelated
    /// process cannot keep a leftover forever). A live process's sandbox,
    /// this process's own included, is never swept, and only a real
    /// directory is removed: a sandbox-named link is never followed.
    pub fn sweep_dead_sandboxes(base: &Path) {
        let Ok(entries) = std::fs::read_dir(base) else {
            return;
        };
        let week_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7 * 24 * 3600);
        for entry in entries.flatten() {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with(SANDBOX_PREFIX)
            {
                continue;
            }
            let Some(owner) = sandbox_owner(&entry.file_name()) else {
                continue;
            };
            #[cfg(unix)]
            let dead = !pid_alive(owner);
            #[cfg(not(unix))]
            let dead = false;
            let ancient = entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t < week_ago);
            // Only a real directory (read_dir's file type never follows a
            // link) owned by another, provably dead process is removable.
            // This process's own sandbox is never swept: its top-level
            // mtime only moves when entries are added directly under it, so
            // an ancient mtime proves nothing about the live owner.
            let removable = entry.file_type().is_ok_and(|t| t.is_dir())
                && owner != std::process::id()
                && (dead || ancient);
            if removable {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    /// The sandbox [`isolate_under`] made in this process, removed at exit.
    static OWN_SANDBOX: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

    /// At-exit removal of this process's own sandbox (unix).
    #[cfg(unix)]
    extern "C" fn remove_own_sandbox() {
        if let Some(dir) = OWN_SANDBOX.get() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// [`isolate`] with a fresh sandbox under `base`
    /// ([`sandbox_for`]: pid plus random suffix), which this process removes
    /// again at exit, so a test run leaves none behind. Sandboxes of other
    /// processes are swept only when their owner is provably gone
    /// ([`sweep_dead_sandboxes`]).
    pub fn isolate_under(base: &Path) {
        sweep_dead_sandboxes(base);
        let dir = sandbox_for(base);
        isolate(&dir);
        if OWN_SANDBOX.set(dir.clone()).is_ok() {
            // SAFETY: registers a plain function with no captured state.
            #[cfg(unix)]
            unsafe {
                libc::atexit(remove_own_sandbox);
            }
        }
    }

    /// Whether `dir` is one of the forbidden config dirs or inside one.
    pub fn is_forbidden(dir: &Path) -> bool {
        let Some(list) = std::env::var_os(FORBIDDEN_ENV) else {
            return false;
        };
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let dir_c = canon(dir);
        std::env::split_paths(&list)
            .filter(|f| !f.as_os_str().is_empty())
            .any(|f| dir.starts_with(&f) || dir_c.starts_with(canon(&f)))
    }

    /// Panics when `dir` is forbidden: a test reached the real config dir.
    pub fn refuse_forbidden(dir: &Path) {
        if is_forbidden(dir) {
            panic!(
                "test isolation: this test resolved the real apb config dir `{}`; \
                 spawn `apb` through the suite's helpers and keep \
                 `APB_CONFIG_DIR`/`XDG_CONFIG_HOME` on a sandbox",
                dir.display()
            );
        }
    }
}

/// Installs [`config::test_isolation::isolate`](crate::config::test_isolation::isolate)
/// before `main` in the test binary that expands it, with its sandbox
/// config home under `$dir` (a `Path` expression), see
/// [`isolate_under`](crate::config::test_isolation::isolate_under).
/// One per test binary, at its crate root.
#[cfg(all(debug_assertions, any(test, feature = "testing")))]
#[macro_export]
macro_rules! isolate_test_config {
    ($dir:expr) => {
        #[used]
        #[cfg_attr(
            any(target_os = "linux", target_os = "android", target_os = "freebsd"),
            unsafe(link_section = ".init_array")
        )]
        #[cfg_attr(
            target_vendor = "apple",
            unsafe(link_section = "__DATA,__mod_init_func")
        )]
        static APB_ISOLATE_TEST_CONFIG: extern "C" fn() = {
            extern "C" fn isolate() {
                let base = $dir;
                $crate::config::test_isolation::isolate_under(::std::convert::AsRef::as_ref(&base));
            }
            isolate
        };
    };
}

/// The config dir this user's apb uses when nothing redirects it:
/// `<home>/.config/apb`, the home taken from the password database for the
/// real user id (never `HOME`, `XDG_CONFIG_HOME` or `APB_CONFIG_DIR`, which
/// a caller sets). `None` off unix or when the user has no entry.
///
/// In apb's own test builds `APB_TEST_DEFAULT_CONFIG_DIR` names it, so a
/// test controls the default dir a redirected one is held to and never
/// reads the developer's real one. The override exists only with debug
/// assertions, and the `testing` feature is refused without them
/// (`lib.rs`), so no release or dist binary reads it.
pub fn default_config_dir() -> Option<PathBuf> {
    #[cfg(all(debug_assertions, any(test, feature = "testing")))]
    if let Some(dir) = std::env::var_os("APB_TEST_DEFAULT_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    real_home().map(|h| h.join(".config/apb"))
}

#[cfg(unix)]
fn real_home() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: getpwuid_r writes into `pwd` and `buf`, both owned here and
    // alive for the call; `result` is checked before `pwd` is read.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        );
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        let dir = CStr::from_ptr(pwd.pw_dir).to_bytes();
        (!dir.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(dir)))
    }
}

#[cfg(not(unix))]
fn real_home() -> Option<PathBuf> {
    None
}

/// Whether [`config_dir`] is not [`default_config_dir`]: a variable
/// redirected it. Compared by canonical path where both exist.
pub fn config_dir_redirected() -> bool {
    let canon = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    match (config_dir(), default_config_dir()) {
        (Some(a), Some(b)) => canon(a) != canon(b),
        (None, None) => false,
        _ => true,
    }
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
        let cfg: Self = yaml_serde::from_str(&raw)
            .map_err(|e| format!("invalid global config `{}`: {e}", path.display()))?;
        cfg.output_limits
            .validate()
            .and_then(|()| cfg.notify.validate())
            .and_then(|()| cfg.discover.validate())
            .and_then(|()| cfg.isolation.validate())
            .map_err(|e| format!("invalid global config `{}`: {e}", path.display()))?;
        // A policy glob that does not compile would match nothing and refuse
        // every model its rule covers: report it where it is written instead.
        for (i, rule) in cfg.model_policy.iter().enumerate() {
            for g in rule.when.iter().chain(&rule.allow) {
                if let Err(e) = globset::Glob::new(g) {
                    return Err(format!(
                        "invalid global config `{}`: model_policy[{i}]: bad glob `{g}`: {e}",
                        path.display()
                    ));
                }
            }
        }
        Ok(cfg)
    }

    /// The output limits for runs of the project at `root`: defaults, then
    /// this config's `output_limits`, then the project's (lowering only).
    pub fn output_limits_for(
        &self,
        root: &Path,
    ) -> Result<crate::output_limits::OutputLimits, String> {
        let project = project_output_limits(root)?;
        project.validate()?;
        Ok(
            crate::output_limits::OutputLimits::from_global(&self.output_limits)
                .lowered_by(&project),
        )
    }

    /// Whether the config replaces agent `id`'s built-in form
    /// (`agents.<id>.invocation`). Read under `id` exactly as given, the way
    /// a run reads a profile's agent: an entry under an alias applies only
    /// to that alias.
    pub fn has_custom_form(&self, id: &str) -> bool {
        self.agents.get(id).is_some_and(|a| a.invocation.is_some())
    }

    /// Launch command for agent `id`, if it's described in the config.
    pub fn agent_program(&self, id: &str) -> Option<String> {
        self.agents.get(id).and_then(|a| a.program.clone())
    }

    /// Whether agent `id` opted into the desktop-history sync
    /// (`agents.<id>.ui_sync: true`). Off by default.
    pub fn agent_ui_sync(&self, id: &str) -> bool {
        self.agents.get(id).and_then(|a| a.ui_sync).unwrap_or(false)
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

/// One section a project may set for its own runs (`suggestions:`,
/// `output_limits:`) from the project `.apb/config.yaml`, read on its own: a
/// typo in one section never breaks another, and the other keys that file
/// carries (`skills_dir`, `port`) are left alone, the same way
/// `crate::skills` reads it. A missing or empty file or section is the
/// default; a malformed file or section, or a top level that is not a
/// mapping, is an error so a typo is not silently ignored.
fn project_section<T: serde::de::DeserializeOwned + Default>(
    root: &Path,
    key: &str,
) -> Result<T, String> {
    let path = project_config_path(root);
    let Some(raw) = read_project_config(root)? else {
        return Ok(T::default());
    };
    let invalid =
        |e: yaml_serde::Error| format!("invalid project config `{}`: {e}", path.display());
    let doc: yaml_serde::Value = yaml_serde::from_str(&raw).map_err(invalid)?;
    match doc {
        // An empty file is a project that sets nothing.
        yaml_serde::Value::Null => return Ok(T::default()),
        yaml_serde::Value::Mapping(_) => {}
        _ => {
            return Err(format!(
                "invalid project config `{}`: the top level must be a mapping",
                path.display()
            ));
        }
    }
    match doc.get(key) {
        None | Some(yaml_serde::Value::Null) => Ok(T::default()),
        Some(section) => yaml_serde::from_value(section.clone())
            .map_err(|e| format!("invalid project config `{}`: {key}: {e}", path.display())),
    }
}

/// The project-level `suggestions:` section.
pub fn project_suggestion_settings(root: &Path) -> Result<SuggestionSettings, String> {
    project_section(root, "suggestions")
}

/// The project-level `output_limits:` section (see `crate::output_limits`).
pub fn project_output_limits(
    root: &Path,
) -> Result<crate::output_limits::OutputLimitSettings, String> {
    project_section(root, "output_limits")
}

/// The project config file of the workspace at `root`.
pub(crate) fn project_config_path(root: &Path) -> PathBuf {
    root.join(".apb/config.yaml")
}

/// The text of the project config of the workspace at `root`, `None` when
/// it is absent; read through the one bounded settings reader (a regular
/// file of at most [`crate::fsutil::SETTINGS_FILE_MAX_BYTES`]).
pub(crate) fn read_project_config(root: &Path) -> Result<Option<String>, String> {
    crate::fsutil::read_settings_file(
        &project_config_path(root),
        crate::fsutil::SETTINGS_FILE_MAX_BYTES,
    )
    .map_err(|e| format!("cannot read project config: {e}"))
}

/// The project-level `notify:` section (see `crate::notify`): it may only
/// turn the machine default off or narrow the transitions.
pub fn project_notify(root: &Path) -> Result<crate::notify::ProjectNotify, String> {
    let section: crate::notify::ProjectNotify = project_section(root, "notify")?;
    section.validate().map_err(|e| {
        format!(
            "invalid project config `{}`: {e}",
            root.join(".apb/config.yaml").display()
        )
    })?;
    Ok(section)
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
    /// Extra host names a KEYLESS dashboard answers besides its loopback
    /// names and the host of `public_base_url`: a local proxy or tailnet name
    /// in front of a loopback dashboard. A bare name (`apb.lan`) or
    /// `name:port`. Any other `Host` gets 403 (DNS-rebinding protection). A
    /// dashboard with API keys authenticates every request and does not
    /// consult it.
    pub allowed_hosts: Vec<String>,
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

    /// The configured host names a keyless dashboard answers (lowercase):
    /// the host of `public_base_url` (its authority, and the bare name) plus
    /// every `allowed_hosts` entry.
    pub fn allowed_host_names(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self
            .allowed_hosts
            .iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        if let Some(url) = self.public_base_url.as_deref() {
            let rest = url.trim().split_once("://").map_or(url.trim(), |(_, r)| r);
            let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
            let authority = authority
                .rsplit('@')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if !authority.is_empty() {
                let name = match authority.strip_prefix('[') {
                    Some(v6) => v6.split(']').next().unwrap_or("").to_string(),
                    None => authority.split(':').next().unwrap_or("").to_string(),
                };
                out.insert(authority);
                out.insert(name);
            }
        }
        out
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

    /// The dashboard page of run `run` of workspace `workspace`: under
    /// `public_base_url` when one is set, else on the loopback dashboard at
    /// `port`. Both ids are percent-encoded as path segments.
    pub fn run_url(&self, port: u16, workspace: &str, run: &str) -> String {
        let base = self
            .public_base_url
            .as_deref()
            .map(|u| u.trim().trim_end_matches('/'))
            .filter(|u| !u.is_empty())
            .map_or_else(|| format!("http://127.0.0.1:{port}"), str::to_string);
        format!(
            "{base}/#/run/{}/{}",
            crate::url_component::encode(workspace),
            crate::url_component::encode(run)
        )
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

#[cfg(test)]
mod isolation_tests {
    use std::path::PathBuf;

    use super::test_isolation::{
        FORBIDDEN_ENV, SANDBOX_PREFIX, forbidden_paths, is_forbidden, sandbox_for,
    };

    /// The unit-test binary is isolated before `main`, and resolving a
    /// forbidden config dir panics rather than handing it to a writer.
    #[test]
    fn a_forbidden_config_dir_panics_and_the_sandbox_does_not() {
        let _lock = crate::env_test_lock();
        let list = std::env::var_os(FORBIDDEN_ENV).expect("the isolation hook ran");
        let sandbox = super::config_dir().expect("a sandbox config dir");
        assert!(!is_forbidden(&sandbox), "{}", sandbox.display());
        assert!(
            std::env::split_paths(&list).all(|f| !sandbox.starts_with(&f)),
            "the sandbox is inside a real config dir"
        );

        let real = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("APB_CONFIG_DIR");
        // SAFETY: env mutation is serialized by the lock.
        unsafe {
            std::env::set_var(FORBIDDEN_ENV, real.path());
            std::env::set_var("APB_CONFIG_DIR", real.path().join("apb"));
        }
        let leaked = std::panic::catch_unwind(super::config_dir);
        unsafe {
            std::env::set_var(FORBIDDEN_ENV, &list);
            match prev {
                Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                None => std::env::remove_var("APB_CONFIG_DIR"),
            }
        }
        assert!(leaked.is_err(), "a forbidden config dir was handed out");
    }

    /// Under isolation the default config dir (what a non-redirected call
    /// would read) sits in the sandbox, never in the developer's real home.
    #[test]
    fn default_config_dir_resolves_inside_the_isolation_sandbox() {
        let _lock = crate::env_test_lock();
        let xdg = std::env::var_os("XDG_CONFIG_HOME").expect("the isolation hook ran");
        let overridden = std::env::var_os("APB_TEST_DEFAULT_CONFIG_DIR")
            .expect("isolation set the default config dir override");
        let d = super::default_config_dir().expect("a default config dir under isolation");
        assert_eq!(d, overridden, "default_config_dir honors the override");
        assert_eq!(PathBuf::from(&xdg), d, "the override names the sandbox");
        assert!(
            !forbidden_paths().iter().any(|f| d.starts_with(f)),
            "the default config dir {} is a real config dir",
            d.display()
        );
    }

    /// The forbidden set covers the config dir `HOME` names too, so a
    /// process whose `HOME` differs from the password database is still kept
    /// out of the home it would actually read.
    #[test]
    fn the_forbidden_set_covers_the_home_derived_config_dir() {
        let _lock = crate::env_test_lock();
        let home = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        // SAFETY: env mutation is serialized by the lock.
        unsafe { std::env::set_var("HOME", home.path()) };
        let paths = forbidden_paths();
        // SAFETY: restored before any other test reads it.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        assert!(
            paths.contains(&home.path().join(".config/apb")),
            "the HOME-derived config dir is missing from {paths:?}"
        );
    }

    /// The sandbox name carries this process's pid and a random suffix, and
    /// the sweep removes only sandboxes of processes that are provably gone.
    #[cfg(unix)]
    #[test]
    fn the_sandbox_sweep_only_removes_dead_owners_dirs() {
        use std::process::Command;

        let _lock = crate::env_test_lock();
        let base = tempfile::tempdir().unwrap();

        // A dead owner: a child that already exited. Its pid is not reused
        // within the test.
        let mut child = Command::new("true").spawn().unwrap();
        let dead = child.id();
        assert!(child.wait().unwrap().success());

        let gone = base.path().join(format!("{SANDBOX_PREFIX}{dead}-abc123"));
        std::fs::create_dir(&gone).unwrap();
        let ours = base
            .path()
            .join(format!("{SANDBOX_PREFIX}{}-abc123", std::process::id()));
        std::fs::create_dir(&ours).unwrap();

        super::test_isolation::sweep_dead_sandboxes(base.path());

        assert!(!gone.exists(), "a dead owner's sandbox was kept");
        assert!(ours.exists(), "this process's sandbox was swept");

        // A sandbox this process creates is private: pid and a random
        // suffix, created with create_dir so a collision is retried.
        let a = sandbox_for(base.path());
        let b = sandbox_for(base.path());
        assert_ne!(a, b, "two sandboxes got the same name");
        assert!(a.starts_with(base.path()));
    }

    /// A sandbox's top-level mtime only changes when entries are added
    /// directly under it, so an ancient mtime proves nothing about a live
    /// process: this process's own sandbox must survive even when it reads
    /// as a week old, and a sandbox-named symlink must not be followed into
    /// a removal in a shared base such as /tmp.
    #[cfg(unix)]
    #[test]
    fn the_sandbox_sweep_never_takes_this_processs_own_or_a_symlink() {
        use std::time::{Duration, SystemTime};

        let _lock = crate::env_test_lock();
        let base = tempfile::tempdir().unwrap();

        let ours = base
            .path()
            .join(format!("{SANDBOX_PREFIX}{}-abc123", std::process::id()));
        std::fs::create_dir(&ours).unwrap();
        std::fs::write(ours.join("config.yaml"), "x: 1\n").unwrap();
        // Make it read as a week old: an mtime alone must not cost a live
        // process its sandbox.
        let week_ago = SystemTime::now() - Duration::from_secs(8 * 24 * 3600);
        std::fs::File::open(&ours)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_accessed(week_ago)
                    .set_modified(week_ago),
            )
            .unwrap();

        // A symlink that carries a dead pid in its name, pointing at a
        // directory that must stay.
        let target = base.path().join("real-config-dir");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(
            &target,
            base.path().join(format!("{SANDBOX_PREFIX}2-abc123")),
        )
        .unwrap();

        super::test_isolation::sweep_dead_sandboxes(base.path());

        assert!(
            ours.exists(),
            "this process's own ancient-looking sandbox was swept"
        );
        assert!(
            target.exists(),
            "a sandbox-named symlink was followed into a removal"
        );
    }
}
