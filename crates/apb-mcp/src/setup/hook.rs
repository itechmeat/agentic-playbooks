//! Registering apb's end-of-turn save check (`apb hook stop`) in the agent
//! CLIs that run a command when a turn ends. What a host's settings look
//! like lives in its agent module ([`apb_core::agents::StopHookHost`]);
//! here every host is read, edited, backed up and taken back the same way
//! as an MCP config file ([`super::hosts`]): one inserted entry, a write
//! only while the file still holds what was read, a link kept a link, and
//! a removal of exactly those bytes. Hosts without a hook (Codex, OpenCode)
//! get the reminder only through the standing block.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use apb_core::agents::{HookRegistration, StopHookHost};
use apb_core::detect::AgentInfo;

use super::hosts::{backup, publish, read_text};

/// The command the hook runs.
pub const HOOK_COMMAND: &str = "apb hook stop";

/// Where one host's hook stands.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HookState {
    /// The host's agent id: the `target` of its `register_hook` action.
    pub host: String,
    /// The host's CLI was detected, or its settings file is there.
    pub detected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<PathBuf>,
    pub registration: HookRegistration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The sha256 of the settings file as read, `None` when absent.
    #[serde(skip)]
    pub sha256: Option<String>,
}

impl HookState {
    /// The precondition a `register_hook` action carries.
    pub fn precondition(&self) -> Value {
        json!({ "path": self.settings, "sha256": self.sha256, "command": HOOK_COMMAND })
    }
}

fn host_of(id: &str) -> Option<&'static dyn StopHookHost> {
    apb_core::agents::lookup(id).and_then(|a| a.stop_hook())
}

fn observe_one(id: &str, host: &dyn StopHookHost, agents: &[AgentInfo]) -> HookState {
    let settings = host.settings_path();
    let installed = agents.iter().any(|a| a.agent == id && a.installed);
    let mut state = HookState {
        host: id.to_string(),
        detected: installed || settings.as_ref().is_some_and(|p| p.exists()),
        settings: settings.clone(),
        registration: HookRegistration::Absent,
        note: None,
        sha256: None,
    };
    let Some(path) = settings else {
        return state;
    };
    match read_text(&path) {
        Ok(None) => {}
        Ok(Some((text, sha))) => {
            state.registration = host.registration(&text, HOOK_COMMAND);
            if state.registration == HookRegistration::Unreadable {
                state.note = Some(format!(
                    "{} does not parse or has a hooks shape apb does not edit",
                    path.display()
                ));
            } else if state.registration == HookRegistration::Absent
                && host.insert(Some(&text), HOOK_COMMAND).is_none()
            {
                state.registration = HookRegistration::Unreadable;
                state.note = Some(format!(
                    "{} has a shape apb does not edit safely",
                    path.display()
                ));
            }
            state.sha256 = Some(sha);
        }
        Err(e) => {
            state.registration = HookRegistration::Unreadable;
            state.note = Some(format!("{}: {e}", path.display()));
        }
    }
    state
}

/// Where every hook host stands, read only.
pub fn observe(agents: &[AgentInfo]) -> Vec<HookState> {
    apb_core::agents::all()
        .iter()
        .filter_map(|a| {
            a.stop_hook()
                .map(|h| observe_one(a.descriptor().id, h, agents))
        })
        .collect()
}

/// Registers the hook in `state.host`, backing the settings up into
/// `backups` first; what the setup record keeps.
pub fn register(state: &HookState, backups: &Path, now_ms: u128) -> Result<Value, String> {
    let host = host_of(&state.host).ok_or("not a hook host")?;
    let path = state.settings.as_ref().ok_or("no settings path")?;
    let existing = read_text(path).map_err(|e| e.to_string())?;
    let (out, inserted) = host
        .insert(existing.as_ref().map(|(t, _)| t.as_str()), HOOK_COMMAND)
        .ok_or("the hook cannot be inserted without changing anything else; nothing written")?;
    let read = existing.as_ref();
    let backup = backup(
        path,
        read.map(|(t, _)| t.as_str()),
        backups,
        &format!("{}-hook", state.host),
        now_ms,
    )?;
    if read.is_none()
        && let Some(dir) = path.parent()
    {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    publish(path, read.map(|(_, sha)| sha.as_str()), &out)?;
    Ok(json!({
        "path": path,
        "inserted": inserted,
        "created_file": existing.is_none(),
        "backup": backup,
    }))
}

/// Takes back exactly what [`register`] wrote; the reason when it is left
/// as is.
pub fn unregister(host_id: &str, written: &Value) -> Result<(), String> {
    let host = host_of(host_id).ok_or("not a hook host")?;
    let path = written
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| host.settings_path())
        .ok_or("no settings path")?;
    let Some((text, read_sha)) = read_text(&path).map_err(|e| e.to_string())? else {
        return Ok(());
    };
    match host.registration(&text, HOOK_COMMAND) {
        HookRegistration::Absent => return Ok(()),
        HookRegistration::Same => {}
        HookRegistration::Different => {
            return Err("the hook was changed after setup wrote it".to_string());
        }
        HookRegistration::Unreadable => return Err("the settings could not be read".to_string()),
    }
    let inserted = written
        .get("inserted")
        .and_then(Value::as_str)
        .unwrap_or("");
    let created = written.get("created_file").and_then(Value::as_bool) == Some(true);
    let out = host
        .remove(&text, inserted, created, HOOK_COMMAND)
        .ok_or("the hook setup wrote was changed; left as is")?;
    if created && out.trim().is_empty() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())
    } else {
        publish(&path, Some(&read_sha), &out)
    }
}
