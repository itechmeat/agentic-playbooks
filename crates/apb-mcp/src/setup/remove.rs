//! `apb setup remove`: takes back what the setup record says apply did.
//! Default playbooks go to the global trash, skills and role profiles go
//! only while nobody changed them, a host registration goes only while it
//! is exactly the entry apb wrote (the end-of-turn hook alike), and the
//! standing block only where its bytes are unchanged. A default a replace took over is restored instead:
//! the person's changed skill comes back from its backup folder, and the
//! playbook's `current` goes back to the person's version. Everything else
//! is kept and listed, and stays in the record so a later remove can try
//! again.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::hosts;
use super::plan::ActionKind;
use super::record::{self, Entry};
use super::standing;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Removed {
    pub kind: String,
    pub target: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Kept {
    pub kind: String,
    pub target: String,
    pub reason: String,
}

/// What a remove did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RemoveOutcome {
    pub removed: Vec<Removed>,
    /// Defaults a replace took over, given back to the person's copy.
    pub restored: Vec<Removed>,
    pub kept: Vec<Kept>,
}

/// How one entry was taken back.
enum Undone {
    Removed(String),
    Restored(String),
}

fn is_replace(kind: &str) -> bool {
    kind == ActionKind::ReplaceModifiedPlaybook.as_str()
        || kind == ActionKind::ReplaceModifiedSkill.as_str()
}

/// Gives a default a replace took over back to the person's copy, from the
/// replace's entry `e`.
fn restore(e: &Entry) -> Result<String, String> {
    if e.kind == ActionKind::ReplaceModifiedSkill.as_str() {
        let backup = e
            .data
            .get("backup")
            .and_then(Value::as_str)
            .ok_or("the record does not name the backup of the changed copy")?;
        apb_core::bundled::restore_replaced_skill(&e.target, Path::new(backup))
            .map_err(|err| err.to_string())?;
        return Ok(format!("the changed copy is back from {backup}"));
    }
    let previous = e
        .data
        .get("previous")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or("the record does not name the version to return to")?;
    let from = apb_core::bundled::restore_replaced_playbook(&e.target, previous)
        .map_err(|err| err.to_string())?;
    Ok(format!(
        "current is back at {previous}; the shipped {from} stays in the history"
    ))
}

/// Undoes one recorded entry: `Ok(detail)` or `Err(reason kept)`.
fn undo(root: &Path, e: &Entry, agents: &[apb_core::detect::AgentInfo]) -> Result<Undone, String> {
    if is_replace(&e.kind) {
        return restore(e).map(Undone::Restored);
    }
    undo_one(root, e, agents).map(Undone::Removed)
}

fn undo_one(
    root: &Path,
    e: &Entry,
    agents: &[apb_core::detect::AgentInfo],
) -> Result<String, String> {
    let kind = e.kind.as_str();
    if kind == ActionKind::InstallPlaybook.as_str() || kind == ActionKind::UpgradePlaybook.as_str()
    {
        return match apb_core::bundled::remove_playbook(&e.target, apb_core::clock::now_ms()) {
            Ok(Some(to)) => Ok(format!("moved to the trash: {}", to.display())),
            Ok(None) => Ok("already gone".to_string()),
            Err(err) => Err(err.to_string()),
        };
    }
    if kind == ActionKind::InstallSkill.as_str() || kind == ActionKind::UpgradeSkill.as_str() {
        return match apb_core::bundled::remove_skill(&e.target) {
            Ok(true) => Ok("removed".to_string()),
            Ok(false) => Ok("already gone".to_string()),
            Err(err) => Err(err.to_string()),
        };
    }
    if kind == ActionKind::WriteRoleProfile.as_str() {
        return undo_profile(root, e);
    }
    if kind == ActionKind::RegisterMcp.as_str() {
        return hosts::unregister(&e.target, &e.data, agents).map(|()| "unregistered".to_string());
    }
    if kind == ActionKind::RegisterHook.as_str() {
        return super::hook::unregister(&e.target, &e.data).map(|()| "unregistered".to_string());
    }
    if kind == ActionKind::WriteStandingBlock.as_str() {
        let kept = standing::remove(&e.data);
        return if kept.is_empty() {
            Ok("removed".to_string())
        } else {
            Err(kept
                .into_iter()
                .map(|(path, why)| format!("{path}: {why}"))
                .collect::<Vec<_>>()
                .join("; "))
        };
    }
    Err(format!("unknown setup action `{kind}`"))
}

/// Removes a role profile only while its digest is the one setup wrote.
fn undo_profile(root: &Path, e: &Entry) -> Result<String, String> {
    let current = match crate::profile_tools::profile_get(root, &e.target, "global") {
        Ok(v) => v,
        Err(crate::tools::ToolError::NotFound(_)) => return Ok("already gone".to_string()),
        Err(err) => return Err(err.to_string()),
    };
    let written = e.data.get("profile_digest").and_then(Value::as_str);
    if current["profile_digest"].as_str() != written || written.is_none() {
        return Err("changed since setup wrote it".to_string());
    }
    crate::profile_tools::profile_delete(root, &e.target, "global", false)
        .map(|_| "removed".to_string())
        .map_err(|err| err.to_string())
}

/// Takes back what the setup record holds, newest first, one entry per
/// action target.
pub fn remove(root: &Path) -> Result<RemoveOutcome, String> {
    apb_core::process_mode::guard().map_err(|e| e.to_string())?;
    if let Some(why) = super::step_refusal() {
        return Err(why);
    }
    let dir = record::setup_dir().ok_or("no config directory")?;
    let _lock = record::lock(&dir)?;
    let mut rec = record::load(&dir)?;
    let mut out = RemoveOutcome::default();
    if rec.entries.is_empty() {
        return Ok(out);
    }
    let needs_agents = rec
        .entries
        .iter()
        .any(|e| e.kind == ActionKind::RegisterMcp.as_str());
    let agents = if needs_agents {
        apb_core::agent_catalog::agents(false)
    } else {
        Vec::new()
    };
    // Playbooks and skills share a target per kind family; one undo each.
    // A replace in the family wins: the person's copy comes back rather
    // than the default going.
    let family = |kind: &str| match kind {
        "upgrade_playbook" | "replace_modified_playbook" => "install_playbook".to_string(),
        "upgrade_skill" | "replace_modified_skill" => "install_skill".to_string(),
        k => k.to_string(),
    };
    let mut handled = BTreeSet::new();
    let mut keep: Vec<Entry> = Vec::new();
    for newest in rec.entries.iter().rev() {
        let key = (family(&newest.kind), newest.target.clone());
        if !handled.insert(key.clone()) {
            continue;
        }
        let e = rec
            .entries
            .iter()
            .rev()
            .find(|e| is_replace(&e.kind) && (family(&e.kind), e.target.clone()) == key)
            .unwrap_or(newest);
        match undo(root, e, &agents) {
            Ok(Undone::Removed(detail)) => out.removed.push(Removed {
                kind: e.kind.clone(),
                target: e.target.clone(),
                detail,
            }),
            Ok(Undone::Restored(detail)) => out.restored.push(Removed {
                kind: e.kind.clone(),
                target: e.target.clone(),
                detail,
            }),
            Err(reason) => {
                out.kept.push(Kept {
                    kind: e.kind.clone(),
                    target: e.target.clone(),
                    reason,
                });
                keep.push(e.clone());
            }
        }
    }
    keep.reverse();
    rec.entries = keep;
    if rec.entries.is_empty() {
        rec.applied.clear();
    }
    record::save(&dir, &rec)?;
    Ok(out)
}
