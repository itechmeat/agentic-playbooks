//! Free detection of coding agents (spec 2026-07-12, section 7). "Free" means
//! the detection itself is local - no model calls and no network requests FROM
//! the playbook. It only looks at whether the binary exists in PATH,
//! `--version`, and local sources of the agent's models/providers/
//! authentication. This is not a guarantee that the launched agent is
//! offline: what a third-party CLI does during a playbook run is not
//! controlled by the playbook.
//!
//! Probes are sanitized: spawned by an absolute canonical path, argv without
//! a shell, `env_clear()` plus a minimal PATH/HOME, a timeout, and an output
//! limit. Probes run in parallel.
//!
//! This module only gathers EXTERNAL facts (installed, version, the
//! `opencode models` output, auth/provider hints). The model lists apb owns
//! (claude/codex static lists, the zcode allowlist) are never produced or
//! cached here: `agent_catalog` adds them from the running binary's data on
//! every call, and every consumer goes through it (this module never calls
//! it back).
//!
//! The probe results are memoized in `<config_dir>/state/agents-detect.json`.
//! The memo is only reused when it was written by the same apb build
//! ([`build_id`]: version + executable fingerprint + digest of the embedded
//! models data) AND every input of every probe is unchanged (agent binary
//! fingerprint, the config/auth files the probe or the agent's model listing
//! read, env-auth presence). A 24h TTL bounds what no local input can show
//! (e.g. opencode's own remote model catalog).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Agent category (spec 7.1). Vendor is tied to its own provider (claude,
/// codex), Aggregator works with several (opencode, pi).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCategory {
    Vendor,
    Aggregator,
}

/// How authoritative the model list is (spec 8.4). `Full` - the agent itself
/// returned the exact list; `Partial` - partially extracted from a config;
/// `Display` - a list for display only, does not guarantee runnability;
/// `Static` - a list hardcoded into the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Full,
    Partial,
    Display,
    Static,
}

/// Type of detected authentication (no secrets). Best-effort hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    Oauth,
    ApiKey,
    None,
}

/// Inventory of an agent's models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsInventory {
    pub items: Vec<String>,
    pub authority: Authority,
}

/// Hint about the agent's authentication (type only, no values).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthHint {
    pub kind: AuthKind,
}

/// Result of detecting a single agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent: String,
    pub installed: bool,
    pub canonical_path: Option<PathBuf>,
    pub version: Option<String>,
    pub category: AgentCategory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<ModelsInventory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthHint>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
enum ModelsSource {
    /// The agent returns the models via a command (argv), with the given
    /// authority.
    Command {
        args: Vec<String>,
        authority: Authority,
    },
    /// codex: the static list from the models table (`codex_static_models`,
    /// the models a paid Codex account offers, default first);
    /// `[model_providers.*]` sections of `~/.codex/config.toml` still feed
    /// `providers`.
    CodexStatic,
    /// claude: the static list from the models table (added by
    /// `agent_catalog`, never by the probe).
    ClaudeStatic,
    /// zcode: apb's allowlist ([`crate::zcode::ALLOWED_MODELS`]), the two
    /// models on the paid Individual plan; the plan annotation and the login
    /// note still read the account files under HOME.
    ZcodeStatic,
    /// No free source available.
    None,
}

/// Source of the agent's authentication (a file in HOME).
#[derive(Debug, Clone)]
enum AuthSource {
    /// `~/.claude.json` - sign of oauth, otherwise env api-key.
    Claude,
    /// `~/.codex/auth.json`.
    Codex,
    /// opencode auth.json - provider names only.
    Opencode,
    /// hermes: presence of `<home>/.hermes/.env` -> api-key hint (values
    /// never read).
    Hermes,
    /// zcode: key NAMES of `~/.zcode/v2/credentials.json` - a plan identity
    /// key means the standalone CLI is logged in (values never read).
    Zcode,
    None,
}

/// Probe for a single agent: candidate binary names + how to collect
/// metadata.
#[derive(Debug, Clone)]
pub struct Probe {
    pub id: String,
    pub bins: Vec<String>,
    pub category: AgentCategory,
    pub version_args: Vec<String>,
    /// Known install locations relative to `$HOME`, tried after PATH. For an
    /// agent whose installer does not put it on PATH (zcode).
    pub home_paths: Vec<String>,
    models_source: ModelsSource,
    auth_source: AuthSource,
}

/// The built-in agents and the binary each one is found and launched as when
/// the config names no `agents.<id>.program`: the one copy of these names.
/// Detection probes them ([`builtin_probes`]) and the engine launches
/// [`default_program`].
const BUILTIN_BINS: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("codex", "codex"),
    ("agy", "agy"),
    ("opencode", "opencode"),
    ("pi", "pi"),
    ("hermes", "hermes"),
    ("grok", "grok"),
    // cursor is installed as `cursor-agent`; the bare `cursor` binary is the
    // GUI editor CLI, not the headless agent.
    ("cursor", "cursor-agent"),
    ("qoder", "qoder"),
    (crate::zcode::AGENT_ID, crate::zcode::PATH_BIN),
];

/// The id a built-in agent alias stands for (`claude-code` is `claude`).
pub fn canonical_agent_id(agent_id: &str) -> &str {
    match agent_id {
        "claude-code" => "claude",
        other => other,
    }
}

/// The binary a built-in agent is found as on PATH, `None` for any other id.
fn builtin_bin(agent_id: &str) -> Option<&'static str> {
    let id = canonical_agent_id(agent_id);
    BUILTIN_BINS.iter().find(|(a, _)| *a == id).map(|(_, b)| *b)
}

/// The program apb launches for `agent_id` when the config names none: the
/// built-in agent's binary, zcode's home-deployed CLI when it is off PATH,
/// and the id itself for any other agent.
pub fn default_program(agent_id: &str) -> String {
    if canonical_agent_id(agent_id) == crate::zcode::AGENT_ID {
        return crate::zcode::default_program();
    }
    builtin_bin(agent_id).unwrap_or(agent_id).to_string()
}

/// Built-in probes for the ten agents (claude, codex, agy, opencode, pi,
/// hermes, grok, cursor, qoder, zcode), with the default binaries of
/// [`BUILTIN_BINS`]. [`probe`] replaces those with `agents.<id>.program`
/// where the config names one.
pub fn builtin_probes() -> Vec<Probe> {
    let v = |s: &str| vec![s.to_string()];
    let bin = |id: &str| v(builtin_bin(id).unwrap_or(id));
    vec![
        Probe {
            id: "claude".into(),
            bins: bin("claude"),
            category: AgentCategory::Vendor,
            version_args: v("--version"),
            home_paths: Vec::new(),
            models_source: ModelsSource::ClaudeStatic,
            auth_source: AuthSource::Claude,
        },
        Probe {
            id: "codex".into(),
            bins: bin("codex"),
            category: AgentCategory::Vendor,
            version_args: v("--version"),
            home_paths: Vec::new(),
            models_source: ModelsSource::CodexStatic,
            auth_source: AuthSource::Codex,
        },
        Probe {
            id: "agy".into(),
            bins: bin("agy"),
            category: AgentCategory::Aggregator,
            version_args: v("--version"),
            home_paths: Vec::new(),
            models_source: ModelsSource::Command {
                args: v("models"),
                authority: Authority::Display,
            },
            auth_source: AuthSource::None,
        },
        Probe {
            id: "opencode".into(),
            bins: bin("opencode"),
            category: AgentCategory::Aggregator,
            version_args: v("--version"),
            home_paths: Vec::new(),
            models_source: ModelsSource::Command {
                args: v("models"),
                authority: Authority::Full,
            },
            auth_source: AuthSource::Opencode,
        },
        Probe {
            id: "pi".into(),
            bins: bin("pi"),
            category: AgentCategory::Aggregator,
            version_args: v("--version"),
            home_paths: Vec::new(),
            models_source: ModelsSource::None,
            auth_source: AuthSource::None,
        },
        Probe {
            id: "hermes".into(),
            bins: bin("hermes"),
            category: AgentCategory::Aggregator,
            version_args: v("--version"),
            home_paths: Vec::new(),
            models_source: ModelsSource::None,
            auth_source: AuthSource::Hermes,
        },
        // Grok Build (xAI). The CLI installs BOTH `grok` and a generic `agent`
        // alias pointing at the same binary; Cursor installs a generic `agent`
        // alias too, so `agent` cannot identify either agent and only the
        // unambiguous `grok` is probed. Model enumeration via the `models`
        // subcommand is deferred: it requires authentication, so an
        // unauthenticated machine would cache an empty list.
        Probe {
            id: "grok".into(),
            bins: bin("grok"),
            category: AgentCategory::Vendor,
            version_args: v("--version"),
            home_paths: Vec::new(),
            models_source: ModelsSource::None,
            auth_source: AuthSource::None,
        },
        // Cursor CLI. An aggregator: it runs gpt, claude, gemini and other
        // vendor models, so it contributes no curated rows of its own.
        Probe {
            id: "cursor".into(),
            bins: bin("cursor"),
            category: AgentCategory::Aggregator,
            version_args: v("--version"),
            home_paths: Vec::new(),
            models_source: ModelsSource::None,
            auth_source: AuthSource::None,
        },
        // Qoder CLI. An aggregator: one subscription serving Qwen, DeepSeek,
        // GLM, Kimi and MiniMax models, so it contributes no curated rows of
        // its own. npm installs both `qoder` and a `qodercli` alias for the
        // same binary; the shorter `qoder` is unambiguous and is the one
        // probed. Model enumeration via `--list-models` requires
        // authentication, so it is deferred like grok's.
        Probe {
            id: "qoder".into(),
            bins: bin("qoder"),
            category: AgentCategory::Aggregator,
            version_args: v("--version"),
            home_paths: Vec::new(),
            models_source: ModelsSource::None,
            auth_source: AuthSource::None,
        },
        // ZCode (Z.ai's desktop agent). Vendor-tied: every plan serves Zhipu's
        // GLM models. The desktop deploys the headless CLI into the home
        // directory and never puts it on PATH, so a PATH `zcode` (a user-made
        // symlink) wins and the known deploy location is the fallback.
        Probe {
            id: crate::zcode::AGENT_ID.into(),
            bins: bin(crate::zcode::AGENT_ID),
            category: AgentCategory::Vendor,
            version_args: v("--version"),
            home_paths: v(crate::zcode::HOME_REL_BIN),
            models_source: ModelsSource::ZcodeStatic,
            auth_source: AuthSource::Zcode,
        },
    ]
}

const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const CACHE_TTL_MS: u128 = 24 * 60 * 60 * 1000;

/// Probe timeout: 10s by default, but tests lower it via
/// `APB_PROBE_TIMEOUT_MS` so they don't wait real seconds on a "hung" agent.
fn probe_timeout() -> Duration {
    std::env::var("APB_PROBE_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(10))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DetectCache {
    /// The apb build that wrote the memo ([`build_id`]). A memo from any other
    /// build is ignored.
    #[serde(default)]
    build_id: String,
    /// When the cache was written (unix ms).
    stamped_ms: u128,
    /// agent id -> binary fingerprint at write time ("path:size:mtime_ms").
    fingerprints: BTreeMap<String, String>,
    agents: Vec<AgentInfo>,
}

fn state_dir() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("state"))
}

fn cache_path() -> Option<PathBuf> {
    state_dir().map(|d| d.join("agents-detect.json"))
}

/// File fingerprint for the cache key: "path:size:mtime_ms". Empty if the
/// file does not exist.
fn fingerprint(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            format!("{}:{}:{}", path.display(), m.len(), mtime)
        }
        Err(_) => String::new(),
    }
}

/// Finds the first executable candidate binary in PATH, canonicalizes the
/// path. A candidate given as a path (a configured `program`) is taken as
/// is. PATH entries inside the current working directory are ignored
/// (protection against a project-local binary swap of the agent).
fn find_in_path(bins: &[String]) -> Option<PathBuf> {
    // A configured program given as a path is used as is.
    if let Some(found) = bins
        .iter()
        .filter(|b| b.contains('/') || b.contains('\\'))
        .find(|b| crate::config::program_in_path(b))
    {
        return std::fs::canonicalize(found).ok();
    }
    let path = std::env::var("PATH").ok()?;
    // Canonicalize CWD - comparing raw paths breaks on symlink prefixes
    // (macOS /var -> /private/var).
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|c| std::fs::canonicalize(&c).ok());
    for dir in std::env::split_paths(&path) {
        for bin in bins
            .iter()
            .filter(|b| !b.contains('/') && !b.contains('\\'))
        {
            let cand = dir.join(bin);
            if crate::config::program_in_path(&cand.to_string_lossy())
                && let Ok(canon) = std::fs::canonicalize(&cand)
            {
                // A binary inside the project's working directory is
                // ignored - a project-local swap must not affect detection.
                if let Some(cwd) = &cwd
                    && canon.starts_with(cwd)
                {
                    continue;
                }
                return Some(canon);
            }
        }
    }
    None
}

/// Finds the agent binary: PATH first ([`find_in_path`]), then each known
/// `$HOME`-relative install location that is an executable file.
fn find_binary(p: &Probe, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(found) = find_in_path(&p.bins) {
        return Some(found);
    }
    let home = home?;
    p.home_paths.iter().find_map(|rel| {
        let cand = home.join(rel);
        (crate::config::program_in_path(&cand.to_string_lossy()))
            .then(|| std::fs::canonicalize(&cand).unwrap_or(cand))
    })
}

/// Result of a successful probe: stdout (truncated to the limit) + a
/// truncation flag.
struct ProbeOut {
    stdout: String,
    truncated: bool,
}

const MAX_STDERR_BYTES: usize = 4 * 1024;

/// SIGKILLs every process in the group led by `pid`, which is the probe child
/// (it is spawned with `process_group(0)`, so its pgid equals its pid).
///
/// This is `libc::kill(-pid, SIGKILL)` rather than a `kill -KILL -<pgid>`
/// subprocess on purpose. BSD kill (macOS) accepts a negative pid as a
/// positional argument, but procps-ng kill (Linux, and so CI) feeds it to
/// getopt first and rejects it as an unknown option - the subprocess exits
/// non-zero, the signal is never delivered, and the daemonized descendants
/// this call exists to reap survive holding the probe's pipes. The status of
/// the spawned `kill` was discarded, so the failure was invisible. The syscall
/// has no argument-parsing layer and behaves identically on both platforms.
/// `apb_engine::proc::run_capture` moved off the subprocess form for the same
/// reason.
///
/// Safe to call after the leader has been reaped: a pgid is not recycled while
/// any process remains in the group, and the call is a harmless ESRCH once the
/// group is empty.
fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    kill_group_with(pid, &|target| {
        // SAFETY: `kill` is async-signal-safe and takes no pointers; `target`
        // came from `group_target`, so it is a validated negative group id,
        // and an unknown group is reported as ESRCH rather than undefined.
        unsafe {
            libc::kill(target, libc::SIGKILL);
        }
    });
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

/// The decision half of a group kill, with delivery injected, so a test can
/// assert exactly what would reach `kill(2)` without sending a signal. See
/// `apb_engine::proc::kill_group_with`: testing `group_target` alone would
/// keep passing if the guard were dropped from the call site, so what has to
/// be pinned is that it is wired in.
fn kill_group_with(pid: u32, send: &dyn Fn(i32)) {
    if let Some(target) = group_target(pid) {
        send(target);
    }
}

/// The `kill(2)` argument addressing the group led by `pid`, or `None` when
/// `pid` cannot lead an addressable one.
///
/// Refuses the three inputs that are wildcards rather than errors, because the
/// group form negates its argument: `0` negates to 0 ("my own group"), `1`
/// negates to -1 ("every process I may signal", the catastrophic one), and
/// anything above `i32::MAX` narrows negative first so negating it lands on a
/// small unrelated pid. The probe child here is always a `Child` we spawned,
/// so none of these is reachable today; the check is what keeps it that way if
/// a caller ever passes a pid read from a file.
///
/// `apb_engine::proc::group_target` is the same rule for the engine's spawns.
/// The duplication is deliberate: apb-core must not depend on apb-engine, and
/// a shared crate for six lines would be worse than two audited copies.
fn group_target(pid: u32) -> Option<i32> {
    match i32::try_from(pid) {
        Ok(p) if p > 1 => Some(-p),
        _ => None,
    }
}

/// Runs `program args...` in a sanitized environment. Child PATH - trusted
/// system directories plus `extra_path` (the canonical parent of the found
/// binary), so a CLI with a `#!/usr/bin/env node` shebang finds its
/// interpreter; project-local PATH entries never appear this way (extra_path
/// is already filtered by find_in_path). Returns Err on spawn error/timeout/
/// non-zero exit code; the error text includes the captured stderr (for a
/// note).
fn run_probe(
    program: &Path,
    args: &[String],
    extra_path: Option<&Path>,
) -> Result<ProbeOut, String> {
    // PATH is built via join_paths (platform separator, correct handling of
    // paths with special characters and non-UTF8), rather than manual
    // concatenation via ':'. The binary's parent (extra_path) comes FIRST -
    // an interpreter next to the agent takes priority; project-local entries
    // never end up here (extra_path is already filtered by find_in_path).
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(dir) = extra_path {
        dirs.push(dir.to_path_buf());
    }
    dirs.extend(
        ["/usr/bin", "/bin", "/usr/local/bin"]
            .iter()
            .map(std::path::PathBuf::from),
    );
    let path = std::env::join_paths(&dirs).map_err(|e| format!("bad PATH component: {e}"))?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_clear()
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    // The probe runs its own process group: on timeout we kill the WHOLE
    // group (not just the direct child), otherwise a daemonized descendant
    // that inherited the pipe would hold the channel and reader threads open
    // forever.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    // stdout/stderr are drained in SEPARATE threads, the result arrives via
    // channels. We NEVER do an unbounded join: if the probe forked a daemon
    // that inherited the pipe, the reader thread would never see EOF - but we
    // still never hang (we wait on channels only with a deadline). We keep
    // the first MAX_OUTPUT_BYTES of stdout and note truncation; stderr keeps
    // the first MAX_STDERR_BYTES.
    let (tx, rx) = std::sync::mpsc::channel::<(Vec<u8>, bool)>();
    if let Some(mut out) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let _ = std::io::copy(&mut out.by_ref().take(MAX_OUTPUT_BYTES as u64), &mut kept);
            let mut sink = std::io::sink();
            let drained = std::io::copy(&mut out, &mut sink).unwrap_or(0);
            let _ = tx.send((kept, drained > 0));
        });
    }
    let (etx, erx) = std::sync::mpsc::channel::<Vec<u8>>();
    if let Some(mut err) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let _ = std::io::copy(&mut err.by_ref().take(MAX_STDERR_BYTES as u64), &mut kept);
            let _ = std::io::copy(&mut err, &mut std::io::sink());
            let _ = etx.send(kept);
        });
    }
    let stderr_snip = || {
        let raw = erx
            .recv_timeout(Duration::from_millis(500))
            .unwrap_or_default();
        let s = String::from_utf8_lossy(&raw).trim().to_string();
        if s.is_empty() {
            String::new()
        } else {
            format!(": {}", first_line(&s))
        }
    };
    let started = Instant::now();
    let timeout = probe_timeout();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // The probe exited, but may have left daemonized grandchildren
                // that inherited its pipes, and EOF is theirs to give, not the
                // probe's. Reap the whole group BEFORE reading anything: it
                // stops those processes and reader threads accumulating across
                // repeated `detect --refresh` runs, and - because it is what
                // makes EOF actually arrive - it is also the difference
                // between collecting the probe's output and timing out on it.
                // Reaping after the read (as this did originally) meant any
                // agent that daemonizes a helper reported no version at all.
                kill_process_group(child.id());
                if !status.success() {
                    return Err(format!("exited with {:?}{}", status.code(), stderr_snip()));
                }
                // Still deadlined: a descendant that left the process group
                // holds the pipe beyond the reach of the kill above.
                let (kept, truncated) = rx
                    .recv_timeout(Duration::from_millis(500))
                    .unwrap_or_default();
                return Ok(ProbeOut {
                    stdout: String::from_utf8_lossy(&kept).into_owned(),
                    truncated,
                });
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    // Kill the whole process group (negative pid), to take
                    // down daemonized descendants too - they then close the
                    // pipe, reader threads see EOF and finish (they don't
                    // pile up on repeated `detect --refresh` runs).
                    kill_process_group(child.id());
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("probe timed out".into());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

fn nonempty_lines(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Parses `~/.codex/config.toml` best-effort: `[model_providers.*]` section
/// names as providers. No TOML crate - a plain string scan.
fn codex_providers(home: &Path) -> Vec<String> {
    let path = home.join(".codex/config.toml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut providers = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("[model_providers.")
            && let Some(name) = rest.strip_suffix(']')
        {
            providers.push(name.trim_matches('"').to_string());
        }
    }
    providers
}

/// Where opencode keeps its credentials: the XDG data dir (current opencode)
/// first, then the legacy config-dir location.
fn opencode_auth_paths(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/share/opencode/auth.json"),
        home.join(".config/opencode/auth.json"),
    ]
}

/// Best-effort authentication hint based on files in HOME. Secret values are
/// never read: only the fact of the file's presence and a rough type.
fn auth_hint(src: &AuthSource, home: &Path) -> (Option<AuthHint>, Option<Vec<String>>) {
    match src {
        AuthSource::Claude => {
            if home.join(".claude.json").is_file() {
                (
                    Some(AuthHint {
                        kind: AuthKind::Oauth,
                    }),
                    None,
                )
            } else if std::env::var("ANTHROPIC_API_KEY").is_ok() {
                (
                    Some(AuthHint {
                        kind: AuthKind::ApiKey,
                    }),
                    None,
                )
            } else {
                (
                    Some(AuthHint {
                        kind: AuthKind::None,
                    }),
                    None,
                )
            }
        }
        AuthSource::Codex => {
            // Classified by the KEY NAMES in auth.json (not values):
            // `tokens`/`account_id` -> account/oauth; `OPENAI_API_KEY` -> api-key.
            let path = home.join(".codex/auth.json");
            let kind = match std::fs::read_to_string(&path) {
                Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
                    Ok(v) => {
                        let has = |k: &str| v.get(k).is_some();
                        if has("tokens") || has("account_id") {
                            AuthKind::Oauth
                        } else if has("OPENAI_API_KEY") {
                            AuthKind::ApiKey
                        } else {
                            // The file exists but its shape is unfamiliar -
                            // conservatively treat as api-key.
                            AuthKind::ApiKey
                        }
                    }
                    Err(_) => AuthKind::ApiKey,
                },
                Err(_) => AuthKind::None,
            };
            (Some(AuthHint { kind }), None)
        }
        AuthSource::Opencode => {
            // Only provider names (top-level keys), no values. The first
            // existing file wins (opencode keeps it under XDG data).
            let providers = opencode_auth_paths(home)
                .into_iter()
                .find_map(|path| std::fs::read_to_string(&path).ok())
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));
            (None, providers)
        }
        AuthSource::Hermes => {
            // Presence of the .env file only - values are never read.
            if home.join(".hermes/.env").is_file() {
                (
                    Some(AuthHint {
                        kind: AuthKind::ApiKey,
                    }),
                    None,
                )
            } else {
                (None, None)
            }
        }
        AuthSource::Zcode => {
            // Key names only: a plan identity key means `zcode-agent login`
            // ran; the api key alone (synced by the desktop) is not enough for
            // the standalone CLI.
            let logged_in = crate::zcode::logged_in_providers(home);
            if logged_in.is_empty() {
                (
                    Some(AuthHint {
                        kind: AuthKind::None,
                    }),
                    None,
                )
            } else {
                (
                    Some(AuthHint {
                        kind: AuthKind::Oauth,
                    }),
                    None,
                )
            }
        }
        AuthSource::None => (None, None),
    }
}

/// Probes a single agent (spawns the binary, collects metadata). Always
/// returns an AgentInfo (installed=false if the binary is not found).
///
/// File-based sources (`CodexConfig`, auth hints under HOME) are consulted
/// even when the binary is absent: a machine can have `~/.codex/config.toml`
/// (or auth) without `codex` on PATH, and the profile editor's model options
/// still need that annotation. Binary-dependent probes (version, Command
/// models) only run when the binary is found. Static model lists are not
/// set here: `agent_catalog::assemble` adds them.
fn probe_one(p: &Probe) -> AgentInfo {
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let mut info = AgentInfo {
        agent: p.id.to_string(),
        installed: false,
        canonical_path: None,
        version: None,
        category: p.category,
        models: None,
        providers: None,
        auth: None,
        notes: Vec::new(),
    };
    let found = find_binary(p, home.as_deref());
    if let Some(path) = &found {
        info.installed = true;
        info.canonical_path = Some(path.clone());
        let parent = path.parent();

        match run_probe(path, &p.version_args, parent) {
            Ok(out) => {
                if out.truncated {
                    info.notes.push(format!(
                        "version output truncated at {MAX_OUTPUT_BYTES} bytes"
                    ));
                }
                info.version = Some(first_line(&out.stdout));
            }
            Err(e) => info.notes.push(format!("version probe failed: {e}")),
        }
    }

    match &p.models_source {
        // Command models require spawning the binary.
        ModelsSource::Command { args, authority } => {
            if let Some(path) = &found {
                let parent = path.parent();
                match run_probe(path, args, parent) {
                    Ok(out) => {
                        if out.truncated {
                            info.notes.push(format!(
                                "models output truncated at {MAX_OUTPUT_BYTES} bytes"
                            ));
                        }
                        info.models = Some(ModelsInventory {
                            items: nonempty_lines(&out.stdout),
                            authority: *authority,
                        })
                    }
                    Err(e) => info.notes.push(format!("models probe failed: {e}")),
                }
            }
        }
        // The provider annotation is file-based: readable without the binary
        // on PATH. The static model list comes from `agent_catalog`.
        ModelsSource::CodexStatic => {
            if let Some(home) = &home {
                let providers = codex_providers(home);
                if !providers.is_empty() {
                    info.providers = Some(providers);
                }
            }
        }
        ModelsSource::ClaudeStatic => {}
        ModelsSource::ZcodeStatic => {
            // The plan annotation and the login note are external facts; the
            // allowlist itself comes from `agent_catalog`.
            if found.is_some() {
                let family = home
                    .as_deref()
                    .map(crate::zcode::account_family)
                    .unwrap_or_else(|| crate::zcode::DEFAULT_FAMILY.to_string());
                info.providers = Some(vec![format!("{family}-individual")]);
                if let Some(home) = &home
                    && crate::zcode::logged_in_providers(home).is_empty()
                {
                    info.notes.push(
                        "standalone CLI not logged in: run `zcode-agent login` (the desktop's \
                         login alone does not cover headless runs)"
                            .to_string(),
                    );
                }
            }
        }
        ModelsSource::None => {}
    }

    if let Some(home) = &home {
        let (auth, providers) = auth_hint(&p.auth_source, home);
        if auth.is_some() {
            info.auth = auth;
        }
        if let Some(pv) = providers {
            info.providers = Some(pv);
        }
    }
    info
}

/// Custom probes from the global config: agents with `probe: true` (the
/// built-in ten are not duplicated). Only checks for the binary's presence -
/// no model/auth sources. Best-effort: a malformed config yields an empty
/// list.
fn custom_probes(cfg: &crate::config::GlobalConfig) -> Vec<Probe> {
    let mut out = Vec::new();
    for (id, def) in &cfg.agents {
        if def.probe != Some(true) || builtin_bin(id).is_some() {
            continue;
        }
        let bins = if !def.bins.is_empty() {
            def.bins.clone()
        } else if let Some(prog) = &def.program {
            vec![prog.clone()]
        } else {
            vec![id.clone()]
        };
        out.push(Probe {
            id: id.clone(),
            bins,
            category: AgentCategory::Aggregator,
            version_args: vec!["--version".to_string()],
            home_paths: Vec::new(),
            models_source: ModelsSource::None,
            auth_source: AuthSource::None,
        });
    }
    out
}

/// The probes to run: the built-in ones, each looking for the program a run
/// would launch - `agents.<id>.program` when the config names one (then only
/// that program, not the default name on PATH or the home fallback) - plus
/// the custom `probe: true` agents. The binaries are part of every memo key
/// ([`agent_cache_key`]), so repointing a program re-probes. A config that
/// does not load yields the built-in defaults only.
fn configured_probes() -> Vec<Probe> {
    let mut probes = builtin_probes();
    let Ok(cfg) = crate::config::GlobalConfig::load() else {
        return probes;
    };
    for p in &mut probes {
        if let Some(program) = cfg.agent_program(&p.id) {
            p.bins = vec![program];
            p.home_paths.clear();
        }
    }
    probes.extend(custom_probes(&cfg));
    probes
}

/// Source files (config/auth) that a probe consults. They are part of the
/// cache key: editing them invalidates the cache before the TTL expires.
fn probe_source_files(p: &Probe, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(home) = home else {
        return out;
    };
    match p.auth_source {
        AuthSource::Claude => out.push(home.join(".claude.json")),
        AuthSource::Codex => out.push(home.join(".codex/auth.json")),
        AuthSource::Opencode => out.extend(opencode_auth_paths(home)),
        AuthSource::Hermes => out.push(home.join(".hermes/.env")),
        AuthSource::Zcode => out.push(home.join(crate::zcode::HOME_REL_CREDENTIALS)),
        AuthSource::None => {}
    }
    if matches!(p.models_source, ModelsSource::CodexStatic) {
        // The provider annotation comes from config.toml, so an edit there
        // must invalidate the cache.
        out.push(home.join(".codex/config.toml"));
    }
    if p.id == "opencode" {
        // `opencode models` reads its config and its cached model catalog:
        // a change to either can change the listed models.
        for rel in [
            ".config/opencode/opencode.json",
            ".config/opencode/opencode.jsonc",
            ".cache/opencode/models.json",
        ] {
            out.push(home.join(rel));
        }
    }
    if matches!(p.models_source, ModelsSource::ZcodeStatic) {
        // The plan annotation takes the account family from the settings
        // (the login note is covered by the credentials file above).
        out.push(home.join(crate::zcode::HOME_REL_SETTINGS));
    }
    out
}

/// Cache key for an agent. ALWAYS present (even when the binary is absent),
/// so that adding/removing a probe definition (e.g. custom `probe: true`
/// without the binary installed) invalidates the cache. Composed of: the
/// probe description (id, bins), the fingerprint of the found binary or an
/// `absent` marker, fingerprints of the consulted sources, and env-auth
/// presence (without secret values).
fn agent_cache_key(p: &Probe, home: Option<&Path>) -> String {
    let mut fp = format!("{}[{}]", p.id, p.bins.join(","));
    match find_binary(p, home) {
        Some(bin) => {
            fp.push_str("|bin:");
            fp.push_str(&fingerprint(&bin));
        }
        None => fp.push_str("|bin:absent"),
    }
    for src in probe_source_files(p, home) {
        fp.push('|');
        fp.push_str(&fingerprint(&src));
    }
    // Env-auth presence (only the fact that the variable exists, not its
    // value): the key appearing/disappearing changes the auth hint and must
    // invalidate the cache.
    if matches!(p.auth_source, AuthSource::Claude) {
        fp.push_str(if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
            "|env:anthropic=1"
        } else {
            "|env:anthropic=0"
        });
    }
    fp
}

/// Identity of the running apb build: its version, the fingerprint of its
/// executable, and the digest of the embedded models data. A detection memo
/// written by any other build is never reused, so a rebuilt binary (new probe
/// code or new model data) always re-probes.
pub fn build_id() -> String {
    let exe = std::env::current_exe()
        .map(|p| fingerprint(&p))
        .unwrap_or_default();
    format!(
        "{}|{}|{}",
        env!("CARGO_PKG_VERSION"),
        exe,
        crate::models_table::builtin_digest()
    )
}

/// The raw probe results: external facts only, no apb-owned model lists.
/// Reuses the memo when it was written by this build ([`build_id`]), within
/// the TTL, and with every probe input unchanged; otherwise probes every
/// agent in parallel and rewrites the memo. `refresh` always probes.
///
/// Consumers want `agent_catalog::load` or `agent_catalog::agents`, which
/// build on this.
pub fn probe(refresh: bool) -> Vec<AgentInfo> {
    let probes = configured_probes();
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    // Current cache keys: probe description + binary/absent + sources +
    // env-auth (to compare against the cache). Every probe has a key, so
    // the set of probes itself affects the result.
    let current_fp: BTreeMap<String, String> = probes
        .iter()
        .map(|p| (p.id.clone(), agent_cache_key(p, home.as_deref())))
        .collect();
    let build = build_id();

    if !refresh
        && let Some(cache) = read_cache()
        && cache.build_id == build
        && crate::clock::now_ms().saturating_sub(cache.stamped_ms) < CACHE_TTL_MS
        && cache.fingerprints == current_fp
    {
        return cache.agents;
    }

    // Each probe spawns up to two short-lived processes and waits on them;
    // running them side by side makes a cold detection as slow as the
    // slowest agent instead of the sum of all of them.
    let agents: Vec<AgentInfo> = std::thread::scope(|scope| {
        let handles: Vec<_> = probes
            .iter()
            .map(|p| scope.spawn(move || probe_one(p)))
            .collect();
        handles
            .into_iter()
            .zip(&probes)
            .map(|(h, p)| {
                h.join().unwrap_or_else(|_| AgentInfo {
                    agent: p.id.clone(),
                    installed: false,
                    canonical_path: None,
                    version: None,
                    category: p.category,
                    models: None,
                    providers: None,
                    auth: None,
                    notes: vec!["probe panicked".to_string()],
                })
            })
            .collect()
    });
    write_cache(&DetectCache {
        build_id: build,
        stamped_ms: crate::clock::now_ms(),
        fingerprints: current_fp,
        agents: agents.clone(),
    });
    agents
}

fn read_cache() -> Option<DetectCache> {
    let path = cache_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_cache(cache: &DetectCache) {
    let Some(path) = cache_path() else {
        return;
    };
    if let Ok(json) = serde_json::to_vec_pretty(cache) {
        let _ = crate::fsutil::atomic_write(&path, &json);
    }
}

#[cfg(test)]
mod signal_target_tests {
    use super::kill_group_with;
    use std::cell::RefCell;

    fn targets_sent_for(pid: u32) -> Vec<i32> {
        let sent = RefCell::new(Vec::new());
        kill_group_with(pid, &|target| sent.borrow_mut().push(target));
        sent.into_inner()
    }

    /// The probe reaper's guard, pinned as wired in rather than merely
    /// correct. Delivery is injected, so a regression is caught without a
    /// single signal being sent - calling the real killer to prove this would
    /// mean a test that ends the developer's session the moment the guard is
    /// removed.
    ///
    /// pid 1 is the one to look at: the group form negates its argument, so it
    /// becomes `kill(-1, SIGKILL)`, "every process I may signal".
    #[test]
    fn a_group_kill_delivers_nothing_for_a_target_it_cannot_address() {
        for pid in [0, 1, u32::MAX, u32::MAX - 1, (i32::MAX as u32) + 1] {
            assert_eq!(
                targets_sent_for(pid),
                Vec::<i32>::new(),
                "pid {pid} cannot address a group, so no signal may be sent"
            );
        }
    }

    #[test]
    fn a_group_kill_of_a_real_pid_addresses_exactly_that_group() {
        assert_eq!(targets_sent_for(2), vec![-2]);
        assert_eq!(targets_sent_for(4321), vec![-4321]);
        assert_eq!(targets_sent_for(i32::MAX as u32), vec![-i32::MAX]);
    }
}
