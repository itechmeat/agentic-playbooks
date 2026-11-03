//! Registering the MCP server in the agent CLIs that are MCP hosts. What a
//! host's config looks like lives in its agent module
//! ([`apb_core::agents::McpHost`]); here every host is read, edited, backed
//! up and taken back the same way: a host CLI registers through its own
//! command, a config file gets exactly one inserted entry, and the removal
//! takes out only those bytes.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde_json::{Value, json};

use apb_core::agents::{Agent, McpHost, McpMethod, McpRegistration, McpServer};
use apb_core::detect::AgentInfo;

/// The name the MCP server is registered under in every host.
pub const SERVER_NAME: &str = "agentic-playbooks";
/// The command line that starts it.
pub const SERVER_COMMAND: &[&str] = &["apb", "mcp"];

/// The server apb registers.
pub fn server() -> McpServer<'static> {
    McpServer {
        name: SERVER_NAME,
        command: SERVER_COMMAND,
    }
}

/// Where one host stands.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostState {
    /// The host's agent id: the `target` of its `register_mcp` action.
    pub host: String,
    /// The host's CLI was detected, or its config is there.
    pub detected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    /// `cli`: the host's CLI registers; `config_file`: apb edits the file.
    pub method: &'static str,
    pub registration: McpRegistration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The entry is absent, but apb cannot add it without changing
    /// anything else (an inline `mcp_servers` table): the exact text a
    /// person adds by hand. Setup then writes nothing to this host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual: Option<String>,
    /// The sha256 of the config file as read, `None` when absent.
    #[serde(skip)]
    pub config_sha256: Option<String>,
}

impl HostState {
    /// The precondition a `register_mcp` action carries, checked again by
    /// the digest at apply time.
    pub fn precondition(&self) -> Value {
        if self.method == "cli" {
            // The host rewrites its own file all the time: only the
            // registration counts, and the program apply starts.
            json!({ "registration": self.registration, "cli": self.cli })
        } else {
            json!({ "path": self.config, "sha256": self.config_sha256 })
        }
    }
}

fn host_of(id: &str) -> Option<&'static dyn McpHost> {
    apb_core::agents::lookup(id).and_then(|a| a.mcp_host())
}

/// The CLI path of `agent` when detection reports it installed.
fn installed_cli(agents: &[AgentInfo], id: &str) -> Option<PathBuf> {
    agents
        .iter()
        .find(|a| a.agent == id && a.installed)
        .map(|a| {
            a.canonical_path
                .clone()
                .unwrap_or_else(|| PathBuf::from(id))
        })
}

/// A file's text and its sha256: `Ok(None)` when absent.
pub(super) fn read_text(path: &Path) -> std::io::Result<Option<(String, String)>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let sha = apb_core::content::sha256_hex(&bytes);
            let text = String::from_utf8(bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(Some((text, sha)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Writes `text` to `path` only while the file still holds what was read
/// (`read_sha`, `None` for a file that was missing): a config another
/// program changed meanwhile is never overwritten with the older copy. A
/// symlinked config (a dotfiles folder) stays a link and its target is
/// edited, with the old file's permissions. The link is resolved once; the
/// check and the publication are one step on that file: right before the
/// rename the file is read again and the link resolved again, and an
/// editor's save or a link moved to another file meanwhile writes nothing.
pub(super) fn publish(path: &Path, read_sha: Option<&str>, text: &str) -> Result<(), String> {
    let changed = || {
        format!(
            "{} changed while setup was editing it; nothing written (run apb setup plan again)",
            path.display()
        )
    };
    let target = apb_core::fsutil::link_target(path).map_err(|e| e.to_string())?;
    let digest = |p: &Path| read_text(p).map(|o| o.map(|(_, sha)| sha));
    if digest(&target).map_err(|e| e.to_string())?.as_deref() != read_sha {
        return Err(changed());
    }
    apb_core::fsutil::atomic_rewrite_in_place_checked(path, text.as_bytes(), |resolved| {
        #[cfg(test)]
        publish_hook::fire();
        let same_target = resolved == target
            && apb_core::fsutil::link_target(path).is_ok_and(|now| now == target);
        if !same_target || digest(&target)?.as_deref() != read_sha {
            return Err(std::io::Error::other(changed()));
        }
        Ok(())
    })
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// How long one host CLI command may run before setup stops it.
pub(super) const HOST_CLI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn observe_one(agent: &dyn Agent, host: &dyn McpHost, agents: &[AgentInfo]) -> HostState {
    let id = agent.descriptor().id;
    let cli = installed_cli(agents, id);
    let method = match host.method(&server()) {
        McpMethod::Cli { .. } => "cli",
        McpMethod::EditFile => "config_file",
    };
    let config = host.config_path();
    let mut state = HostState {
        host: id.to_string(),
        detected: cli.is_some() || host.marker().is_some_and(|m| m.exists()),
        cli,
        config: config.clone(),
        method,
        registration: McpRegistration::Absent,
        note: None,
        manual: None,
        config_sha256: None,
    };
    if let Some(note) = host.unsupported() {
        state.registration = McpRegistration::Unreadable;
        state.note = Some(note);
        return state;
    }
    let Some(path) = config else {
        return state;
    };
    match read_text(&path) {
        Ok(None) => {}
        Ok(Some((text, sha))) => {
            state.registration = host.registration(&text, &server());
            if state.registration == McpRegistration::Unreadable {
                state.note = Some(format!("{} does not parse", path.display()));
            }
            if state.registration == McpRegistration::Absent
                && method == "config_file"
                && host.insert(Some(&text), &server()).is_none()
            {
                state.manual = host.snippet(&server());
                state.note = Some(format!(
                    "{} has a shape apb does not edit safely; add the entry by hand",
                    path.display()
                ));
            }
            state.config_sha256 = Some(sha);
        }
        Err(e) => {
            state.registration = McpRegistration::Unreadable;
            state.note = Some(format!("{}: {e}", path.display()));
        }
    }
    state
}

/// Where every MCP host stands, read only: no program is started.
pub fn observe(agents: &[AgentInfo]) -> Vec<HostState> {
    apb_core::agents::all()
        .iter()
        .filter_map(|a| a.mcp_host().map(|h| observe_one(*a, h, agents)))
        .collect()
}

/// Runs a host CLI with `args`, stopped after [`HOST_CLI_TIMEOUT`]; its
/// own error output on a failure.
fn run(cli: &Path, args: &[String]) -> Result<(), String> {
    run_bounded(cli, args, HOST_CLI_TIMEOUT)
}

/// [`run`] with the time limit `limit`: a command still running then is
/// killed and reported, so setup never waits on a stalled host. The
/// command runs in its own process group, and the whole group is killed
/// once the command exits or its time is up: a descendant that kept the
/// error pipe open (a launcher that started something in the background)
/// can never hold setup past the limit. The error output is read on a
/// thread setup waits on only until the limit, never longer.
pub(super) fn run_bounded(
    cli: &Path,
    args: &[String],
    limit: std::time::Duration,
) -> Result<(), String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + limit;
    let mut cmd = Command::new(cli);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    // A host CLI just installed can still be busy for a moment.
    let mut child = apb_core::fsutil::spawn_when_not_busy(&mut cmd)
        .map_err(|e| format!("could not start {}: {e}", cli.display()))?;
    let mut stderr = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel();
    // Detached: it ends when the pipe closes, which the group kill below
    // brings about for every process that stayed in the group.
    std::thread::spawn(move || {
        // Keep the first 64 KiB for the message but drain to the end: a
        // reader that stops early makes a chatty CLI die on a closed pipe.
        const KEEP: usize = 64 * 1024;
        let mut kept: Vec<u8> = Vec::new();
        if let Some(e) = stderr.as_mut() {
            let mut buf = [0u8; 8192];
            loop {
                match e.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let room = KEEP.saturating_sub(kept.len());
                        kept.extend_from_slice(&buf[..n.min(room)]);
                    }
                }
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&kept).into_owned());
    });
    let stop_group = |child: &mut std::process::Child| {
        apb_engine::proc::kill_process_group(child.id());
        let _ = child.wait();
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                stop_group(&mut child);
                return Err(format!(
                    "{} {} did not finish within {} seconds and was stopped",
                    cli.display(),
                    args.join(" "),
                    limit.as_secs_f32()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                stop_group(&mut child);
                return Err(format!("{}: {e}", cli.display()));
            }
        }
    };
    // The leader is reaped, but its group id stays reserved while any
    // member lives, so this reaches exactly what it left behind.
    apb_engine::proc::kill_process_group(child.id());
    if status.success() {
        return Ok(());
    }
    // Whatever time is left, and a moment for a pipe the kill just closed.
    let wait = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(200));
    let err = rx.recv_timeout(wait).unwrap_or_default();
    Err(format!(
        "{} {} exited with {status}: {}",
        cli.display(),
        args.join(" "),
        err.trim().chars().take(400).collect::<String>()
    ))
}

/// Copies `text`, what was read from `path`, into `backups` before the
/// file is edited; the copy's path. The copy is of the very text the edit
/// starts from, never a second read.
pub(super) fn backup(
    path: &Path,
    text: Option<&str>,
    backups: &Path,
    host: &str,
    now_ms: u128,
) -> Result<Option<PathBuf>, String> {
    let Some(text) = text else {
        return Ok(None);
    };
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "bak".to_string());
    let dst = backups.join(format!("{host}-{now_ms}.{ext}"));
    apb_core::fsutil::atomic_write_private(&dst, text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(Some(dst))
}

/// Registers the server in `state.host`, backing an edited config up into
/// `backups` first; what the setup record keeps.
pub fn register(state: &HostState, backups: &Path, now_ms: u128) -> Result<Value, String> {
    let host = host_of(&state.host).ok_or("not an MCP host")?;
    match host.method(&server()) {
        McpMethod::Cli { add, .. } => {
            let cli = state.cli.as_ref().ok_or("the host CLI is not installed")?;
            run(cli, &add)?;
            Ok(json!({ "cli": cli }))
        }
        McpMethod::EditFile => {
            let path = state.config.as_ref().ok_or("no config path")?;
            let existing = read_text(path).map_err(|e| e.to_string())?;
            let (out, inserted) = host
                .insert(existing.as_ref().map(|(t, _)| t.as_str()), &server())
                .ok_or(
                    "the entry cannot be inserted without changing anything else; nothing written",
                )?;
            let read = existing.as_ref();
            let backup = backup(
                path,
                read.map(|(t, _)| t.as_str()),
                backups,
                &state.host,
                now_ms,
            )?;
            publish(path, read.map(|(_, sha)| sha.as_str()), &out)?;
            Ok(json!({
                "path": path,
                "inserted": inserted,
                "created_file": existing.is_none(),
                "backup": backup,
            }))
        }
    }
}

/// Takes back exactly what [`register`] wrote; the reason when it is left
/// as is.
pub fn unregister(host_id: &str, written: &Value, agents: &[AgentInfo]) -> Result<(), String> {
    let host = host_of(host_id).ok_or("not an MCP host")?;
    let path = written
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| host.config_path());
    let current = match &path {
        Some(p) => read_text(p).map_err(|e| e.to_string())?,
        None => None,
    };
    let Some((text, read_sha)) = current else {
        return Ok(());
    };
    match host.registration(&text, &server()) {
        McpRegistration::Absent => return Ok(()),
        McpRegistration::Same => {}
        McpRegistration::Different => {
            return Err("the entry was changed after setup wrote it".to_string());
        }
        McpRegistration::Unreadable => return Err("the config could not be read".to_string()),
    }
    match host.method(&server()) {
        McpMethod::Cli { remove, .. } => {
            let cli = installed_cli(agents, host_id)
                .or_else(|| {
                    written
                        .get("cli")
                        .and_then(Value::as_str)
                        .map(PathBuf::from)
                })
                .ok_or("the host CLI is not installed")?;
            run(&cli, &remove)
        }
        McpMethod::EditFile => {
            let path = path.ok_or("no config path")?;
            let inserted = written
                .get("inserted")
                .and_then(Value::as_str)
                .unwrap_or("");
            let created = written.get("created_file").and_then(Value::as_bool) == Some(true);
            let out = host
                .remove(&text, inserted, created, &server())
                .ok_or("the entry setup wrote was changed; left as is")?;
            if created && out.trim().is_empty() {
                std::fs::remove_file(&path).map_err(|e| e.to_string())
            } else {
                publish(&path, Some(&read_sha), &out)
            }
        }
    }
}

/// The test seam of [`publish`]: a closure run on this thread between its
/// read and its rename, to change the file or move the link meanwhile.
#[cfg(test)]
pub(super) mod publish_hook {
    use std::cell::RefCell;

    thread_local! {
        static HOOK: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    /// Runs `f` once, at the next publication on this thread.
    pub(in crate::setup) fn set(f: impl FnOnce() + 'static) {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    pub(super) fn fire() {
        if let Some(f) = HOOK.with(|h| h.borrow_mut().take()) {
            f();
        }
    }
}
