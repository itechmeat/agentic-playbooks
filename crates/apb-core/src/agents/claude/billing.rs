//! What a cost claude prints is. claude prints `total_cost_usd` on every
//! login, priced from its own table: on an api key that is money billed, on
//! a subscription login it is what the work would cost at list price, and
//! nobody is billed it.
//!
//! A cost reads as a subscription equivalent only when the facts show that
//! a subscription login is what claude uses; any doubt reads as billed or
//! unknown, never as a subscription, so neither a project nor a missing
//! file can lower what a run is counted to spend. The module always decides
//! (it never leaves the basis to a declared subscription, which says what
//! the person pays for, not how this process logs in).
//!
//! What it reads, by name and by the presence of keys, never a credential
//! value:
//!
//! 1. the login variables that reach the spawned process, and the same
//!    names set in the `env` block of every settings file claude loads (the
//!    managed files, the user's own unless the minimal environment leaves
//!    it out, the project's `.claude/settings.json` and
//!    `.claude/settings.local.json` in the working directory, and the
//!    local file at the root of the git repository around it, or of the
//!    main checkout for a worktree), with their `apiKeyHelper` and
//!    `forceLoginMethod`; a git variable that would move the repository
//!    claude finds, or a `.git` entry that cannot be followed, is a doubt;
//! 2. the login claude recorded: a key a Console login created
//!    (`primaryApiKey` in its state file), the plan of a subscription login
//!    (`.credentials.json`, or the account's organization type in the state
//!    file where the credentials live in a keychain).
//!
//! Then: a cloud provider switch is billed; a custom endpoint
//! (`ANTHROPIC_BASE_URL`) charges what it charges, so the basis is unknown;
//! a key, a key helper or a Console login is billed; a settings file that
//! cannot be read honestly (not a regular file, too large, not JSON, a
//! project file reached through a link) leaves it unknown; a subscription
//! token or a subscription login of a known plan is a subscription
//! equivalent; anything else is unknown.
//!
//! claude reloads its settings files while it runs, so the basis is read
//! twice for an attempt, before the spawn and after the exit, and holds
//! only when both readings agree (`CostReading::across`). A reading's
//! evidence is a digest of everything it read: each settings file's
//! content with its identity and change time, and the `.claude`
//! directories of the project, so a file added and removed again while
//! the agent worked still changes it. The login files claude rewrites
//! itself (its state file, a refreshed token) take part only through the
//! facts drawn from them.

use std::path::{Path, PathBuf};

use serde_json::Value;

use sha2::Digest;

use crate::agents::CostBasis;
use crate::agents::cost_basis::{CostFacts, CostReading};
use crate::fsutil::SETTINGS_FILE_MAX_BYTES;

/// The cloud provider switches; any of them on bills a cloud account.
const CLOUD_SWITCHES: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

/// The variables that carry a key claude sends as is.
const KEY_VARS: &[&str] = &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"];

/// The variable that points claude at another endpoint.
const ENDPOINT: &str = "ANTHROPIC_BASE_URL";

/// The variable that carries a subscription login token.
const SUBSCRIPTION_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// The variable that moves claude's config dir; set from a settings file it
/// would move the login apb reads, so it is a doubt.
const CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";

/// The git variables that move the repository claude finds, and with it
/// the local settings file it loads.
const GIT_VARS: &[&str] = &["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"];

/// The largest `.git` file or `commondir` file read.
const GIT_LINK_MAX_BYTES: u64 = 4096;

/// The plans of a subscription login (`claudeAiOauth.subscriptionType` in
/// `.credentials.json`), checked against claude 2.1.316.
const PLANS: &[&str] = &["pro", "max", "team", "enterprise"];

/// The organization types of a subscription account
/// (`oauthAccount.organizationType` in the state file).
const ACCOUNT_KINDS: &[&str] = &[
    "claude_pro",
    "claude_max",
    "claude_team",
    "claude_enterprise",
];

/// The managed settings directory, under the system root.
#[cfg(target_os = "macos")]
const MANAGED_DIR: &str = "Library/Application Support/ClaudeCode";
#[cfg(not(target_os = "macos"))]
const MANAGED_DIR: &str = "etc/claude-code";

/// The largest state file read: `.claude.json` keeps per-project history
/// and grows well past a settings file.
const STATE_FILE_MAX_BYTES: u64 = 16 << 20;

pub(super) fn cost_basis(f: &CostFacts) -> Option<CostReading> {
    let mut ev = Evidence::default();
    let basis = decide(f, &mut ev);
    Some(CostReading::new(basis, ev.finish(basis)))
}

/// A digest of every fact a reading was decided from.
#[derive(Default)]
struct Evidence(sha2::Sha256);

impl Evidence {
    fn note(&mut self, part: &str) {
        self.0.update(part.as_bytes());
        self.0.update([0]);
    }

    /// The identity, size and change times of `path` (of a link itself
    /// unless `follow`), or that it is absent.
    fn stat(&mut self, path: &Path, follow: bool) {
        let meta = if follow {
            std::fs::metadata(path)
        } else {
            std::fs::symlink_metadata(path)
        };
        let stamp = match meta {
            Ok(m) => stamp(&m),
            Err(e) if is_absent(&e) => "absent".to_string(),
            Err(e) => format!("error {:?}", e.kind()),
        };
        self.note(&format!("{} {stamp}", path.display()));
    }

    /// What one settings file held.
    fn read(&mut self, read: &Read) {
        let part = match read {
            Read::Absent => "absent".to_string(),
            Read::Odd => "odd".to_string(),
            Read::Json(v) => crate::content::sha256_hex(v.to_string().as_bytes()),
        };
        self.note(&part);
    }

    fn finish(mut self, basis: CostBasis) -> String {
        self.note(basis.key());
        crate::content::hex_lower(&self.0.finalize())
    }
}

#[cfg(unix)]
fn stamp(m: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!(
        "{}:{} {} {}.{} {}.{}",
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec()
    )
}

#[cfg(not(unix))]
fn stamp(m: &std::fs::Metadata) -> String {
    format!("{} {:?}", m.len(), m.modified().ok())
}

/// What the process and the settings files claude loads say about its login.
#[derive(Default)]
struct Seen {
    cloud: bool,
    endpoint: bool,
    billed: bool,
    token: bool,
    doubt: bool,
}

impl Seen {
    /// Takes in one settings file as claude would load it.
    fn settings(&mut self, read: Read) {
        let v = match read {
            Read::Absent => return,
            Read::Odd => {
                self.doubt = true;
                return;
            }
            Read::Json(v) => v,
        };
        let Some(obj) = v.as_object() else {
            self.doubt = true;
            return;
        };
        if obj.contains_key("apiKeyHelper") {
            self.billed = true;
        }
        match obj.get("forceLoginMethod") {
            None => {}
            Some(Value::String(m)) if m == "claudeai" => {}
            Some(Value::String(m)) if m == "console" => self.billed = true,
            Some(_) => self.doubt = true,
        }
        match obj.get("env") {
            None => {}
            Some(Value::Object(env)) => {
                for (name, value) in env {
                    self.variable(name, || match value {
                        Value::String(s) => Some(crate::process_env::is_truthy(s)),
                        _ => None,
                    });
                }
            }
            Some(_) => self.doubt = true,
        }
    }

    /// Takes in variable `name`; `switched_on` tells a cloud switch's value
    /// (`None` when it is not a text).
    fn variable(&mut self, name: &str, switched_on: impl FnOnce() -> Option<bool>) {
        if KEY_VARS.contains(&name) {
            self.billed = true;
        } else if name == ENDPOINT {
            self.endpoint = true;
        } else if name == SUBSCRIPTION_TOKEN {
            self.token = true;
        } else if name == CONFIG_DIR {
            self.doubt = true;
        } else if CLOUD_SWITCHES.contains(&name) {
            match switched_on() {
                Some(on) => self.cloud |= on,
                None => self.doubt = true,
            }
        }
    }
}

fn decide(f: &CostFacts, ev: &mut Evidence) -> CostBasis {
    let mut seen = Seen::default();
    for name in KEY_VARS.iter().chain([&ENDPOINT, &SUBSCRIPTION_TOKEN]) {
        if (f.present)(name) {
            ev.note(name);
            seen.variable(name, || None);
        }
    }
    for name in CLOUD_SWITCHES {
        if let Some(v) = (f.setting)(name) {
            let on = crate::process_env::is_truthy(&v.to_string_lossy());
            ev.note(&format!("{name}={on}"));
            seen.variable(name, || Some(on));
        }
    }
    if GIT_VARS.iter().any(|name| (f.present)(name)) {
        seen.doubt = true;
    }
    let config = config_dir(f);
    for path in managed_files(f.system_root, &mut seen) {
        seen.settings(read_settings(&path, ev));
    }
    if !f.hermetic
        && let Some(config) = &config
    {
        seen.settings(read_settings(&config.join("settings.json"), ev));
    }
    if let Some(w) = f.workdir {
        for (base, name) in project_files(w, &mut seen) {
            let dir = base.join(".claude");
            ev.stat(&dir, false);
            let path = dir.join(name);
            ev.stat(&path, false);
            let read = read_project(&base, &path);
            ev.read(&read);
            seen.settings(read);
        }
    }
    ev.note(&format!("doubt={}", seen.doubt));
    if seen.cloud {
        return CostBasis::Billed;
    }
    if seen.endpoint {
        return CostBasis::Unknown;
    }
    if seen.billed {
        return CostBasis::Billed;
    }
    if seen.doubt {
        return CostBasis::Unknown;
    }
    let Some(config) = config else {
        return CostBasis::Unknown;
    };
    let state = match state_file(f, &config).map(|p| read_trusted(&p, STATE_FILE_MAX_BYTES)) {
        Some(Read::Json(v)) => v,
        Some(Read::Absent) => Value::Null,
        Some(Read::Odd) | None => return CostBasis::Unknown,
    };
    if state.get("primaryApiKey").is_some() {
        return CostBasis::Billed;
    }
    if seen.token {
        return CostBasis::SubscriptionEquivalent;
    }
    match read_trusted(&config.join(".credentials.json"), SETTINGS_FILE_MAX_BYTES) {
        Read::Json(v) => {
            ev.note("credentials");
            return known(&v["claudeAiOauth"]["subscriptionType"], PLANS);
        }
        Read::Odd => return CostBasis::Unknown,
        Read::Absent => {}
    }
    ev.note("state account");
    known(&state["oauthAccount"]["organizationType"], ACCOUNT_KINDS)
}

/// The project settings files claude loads for a session started in
/// `workdir`, each with the directory no link may appear below:
/// `.claude/settings.json` and `.claude/settings.local.json` of the
/// working directory (where claude before 2.1.211 kept the local file, and
/// still reads it), and the local file of the git repository's root and,
/// for a worktree, of the main checkout's root. More files than claude
/// reads can only make the basis billed or unknown, never a subscription.
fn project_files(workdir: &Path, seen: &mut Seen) -> Vec<(PathBuf, &'static str)> {
    let mut files = vec![
        (workdir.to_path_buf(), "settings.json"),
        (workdir.to_path_buf(), "settings.local.json"),
    ];
    for root in repository_roots(workdir, seen) {
        if !files.contains(&(root.clone(), "settings.local.json")) {
            files.push((root, "settings.local.json"));
        }
    }
    files
}

/// The root of the git repository `workdir` is in, and the main checkout's
/// root when that is a linked worktree; none outside a repository. A `.git`
/// entry that is neither a directory nor a regular file naming its git
/// directory, or a worktree of a repository without a main checkout, is a
/// doubt.
fn repository_roots(workdir: &Path, seen: &mut Seen) -> Vec<PathBuf> {
    let Some((top, meta)) = workdir
        .ancestors()
        .find_map(|a| Some((a, std::fs::symlink_metadata(a.join(".git")).ok()?)))
    else {
        return Vec::new();
    };
    let mut roots = vec![top.to_path_buf()];
    if meta.is_dir() {
        return roots;
    }
    match meta.is_file().then(|| main_checkout(top)).flatten() {
        Some(Some(main)) => roots.push(main),
        Some(None) => {}
        None => seen.doubt = true,
    }
    roots
}

/// The main checkout of the worktree at `top`, whose `.git` is a file:
/// `Some(None)` when its git directory has no `commondir` (a submodule),
/// `None` when a file cannot be read or names no checkout.
fn main_checkout(top: &Path) -> Option<Option<PathBuf>> {
    let link = small_text(&top.join(".git"))??;
    let git_dir = top.join(link.trim().strip_prefix("gitdir:")?.trim());
    let common = match small_text(&git_dir.join("commondir"))? {
        None => return Some(None),
        Some(c) => git_dir.join(c.trim()).canonicalize().ok()?,
    };
    if common.file_name()? != ".git" {
        return None;
    }
    Some(Some(common.parent()?.to_path_buf()))
}

/// A small regular file's text: `Some(None)` when absent, `None` when it is
/// not a regular file of text within [`GIT_LINK_MAX_BYTES`].
fn small_text(path: &Path) -> Option<Option<String>> {
    let mut file = match crate::fsutil::open_regular_no_follow(path) {
        Ok(f) => f,
        Err(e) if is_absent(&e) => return Some(None),
        Err(_) => return None,
    };
    match crate::fsutil::read_prefix_capped(&mut file, GIT_LINK_MAX_BYTES) {
        Ok((bytes, false)) => String::from_utf8(bytes).ok().map(Some),
        _ => None,
    }
}

/// A subscription equivalent when `v` is one of `kinds`, else unknown.
fn known(v: &Value, kinds: &[&str]) -> CostBasis {
    match v.as_str() {
        Some(k) if kinds.contains(&k) => CostBasis::SubscriptionEquivalent,
        _ => CostBasis::Unknown,
    }
}

/// The config dir the spawned claude reads: `CLAUDE_CONFIG_DIR`, else
/// `~/.claude`.
fn config_dir(f: &CostFacts) -> Option<PathBuf> {
    match (f.setting)(CONFIG_DIR).filter(|d| !d.is_empty()) {
        Some(d) => Some(PathBuf::from(d)),
        None => f.home.map(|h| h.join(".claude")),
    }
}

/// claude's state file: `.claude.json` in `CLAUDE_CONFIG_DIR`, else in the
/// home directory.
fn state_file(f: &CostFacts, config: &Path) -> Option<PathBuf> {
    match (f.setting)(CONFIG_DIR).filter(|d| !d.is_empty()) {
        Some(_) => Some(config.join(".claude.json")),
        None => f.home.map(|h| h.join(".claude.json")),
    }
}

/// The managed settings files: `managed-settings.json` and the `.json`
/// drop-ins of `managed-settings.d`, in name order. A drop-in directory
/// that cannot be listed is a doubt.
fn managed_files(system_root: &Path, seen: &mut Seen) -> Vec<PathBuf> {
    let dir = system_root.join(MANAGED_DIR);
    let mut files = vec![dir.join("managed-settings.json")];
    match std::fs::read_dir(dir.join("managed-settings.d")) {
        Ok(entries) => {
            let mut drop_ins: Vec<PathBuf> = Vec::new();
            for entry in entries {
                match entry {
                    Ok(e) => drop_ins.push(e.path()),
                    Err(_) => seen.doubt = true,
                }
            }
            drop_ins.retain(|p| p.extension().is_some_and(|x| x == "json"));
            drop_ins.sort();
            files.extend(drop_ins);
        }
        Err(e) if is_absent(&e) => {}
        Err(_) => seen.doubt = true,
    }
    files
}

/// One settings or login file as read.
enum Read {
    Absent,
    /// There, but not a regular file of JSON within the size limit.
    Odd,
    Json(Value),
}

fn is_absent(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

fn parse(text: &[u8]) -> Read {
    serde_json::from_slice(text).map_or(Read::Odd, Read::Json)
}

/// A settings file of the operator's or the machine's, as
/// [`read_trusted`] reads it, taken into the evidence.
fn read_settings(path: &Path, ev: &mut Evidence) -> Read {
    ev.stat(path, true);
    let read = read_trusted(path, SETTINGS_FILE_MAX_BYTES);
    ev.read(&read);
    read
}

/// A file of the operator's or the machine's: a link is followed, what it
/// reaches must be a regular file of at most `cap` bytes.
fn read_trusted(path: &Path, cap: u64) -> Read {
    match crate::fsutil::read_settings_file(path, cap) {
        Ok(None) => Read::Absent,
        Ok(Some(text)) => parse(text.as_bytes()),
        Err(_) => Read::Odd,
    }
}

/// A file of the project at `workdir`: no link anywhere below the workdir,
/// a regular file (never a fifo or a device), at most a settings file's
/// size.
fn read_project(workdir: &Path, path: &Path) -> Read {
    if crate::fsutil::ensure_no_symlink_below(workdir, path).is_err() {
        return Read::Odd;
    }
    let mut file = match crate::fsutil::open_regular_no_follow(path) {
        Ok(f) => f,
        Err(e) if is_absent(&e) => return Read::Absent,
        Err(_) => return Read::Odd,
    };
    match crate::fsutil::read_prefix_capped(&mut file, SETTINGS_FILE_MAX_BYTES) {
        Ok((bytes, false)) => parse(&bytes),
        _ => Read::Odd,
    }
}
