//! `apb setup apply --plan <digest>`: recomputes the plan under the setup
//! lock and runs it only when its digest is the one the caller read. Every
//! action that succeeds is recorded at once, so `remove` can take it back
//! even after a later action failed.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use super::hosts;
use super::plan::{Action, ActionKind, Plan, Refusal, ReplaceModified, plan};
use super::record::{self, Applied, Entry};
use super::standing;

/// How an apply ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    /// Every action ran.
    Applied,
    /// Nothing to do: the plan is empty, or it was applied already.
    NoOp,
    /// The plan is not the one the caller read: nothing was written.
    PlanChanged,
    /// The plan carries a refusal: nothing was written.
    Refused,
    /// At least one action failed; the others ran and are recorded.
    Failed,
}

impl ApplyStatus {
    /// The exit code every surface uses: 0 done, 1 failure, 2 plan
    /// changed, 3 refused.
    pub fn exit_code(self) -> u8 {
        match self {
            ApplyStatus::Applied | ApplyStatus::NoOp => 0,
            ApplyStatus::Failed => 1,
            ApplyStatus::PlanChanged => 2,
            ApplyStatus::Refused => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Done {
    pub kind: ActionKind,
    pub target: String,
    pub detail: String,
    /// What a replace kept of the changed default: the backup folder of a
    /// skill, or the history version of a playbook with the command that
    /// returns to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Failed {
    pub kind: ActionKind,
    pub target: String,
    pub error: String,
}

/// What an apply did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApplyOutcome {
    pub status: ApplyStatus,
    /// The digest the caller passed.
    pub requested: String,
    /// The digest of the plan as recomputed.
    pub digest: String,
    pub done: Vec<Done>,
    pub failed: Vec<Failed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
    /// Why the apply could not run or finish, apart from one action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<PathBuf>,
    /// Role profiles whose approval could not follow the skills this apply
    /// updated (judged once, after every skill action).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl ApplyOutcome {
    fn empty(status: ApplyStatus, requested: &str, digest: &str) -> Self {
        Self {
            status,
            requested: requested.to_string(),
            digest: digest.to_string(),
            done: Vec::new(),
            failed: Vec::new(),
            refusal: None,
            error: None,
            record: None,
            notes: Vec::new(),
        }
    }
}

/// Runs one action; what the record keeps of it. The skill actions go
/// through `skills`, which carries the role profiles' approval once the
/// whole apply is over.
fn run_one(
    root: &Path,
    plan: &Plan,
    a: &Action,
    backups: &Path,
    skills: &mut apb_core::bundled::SkillUpdates,
) -> Result<Value, String> {
    let now = apb_core::clock::now_ms();
    match a.kind {
        ActionKind::InstallPlaybook | ActionKind::UpgradePlaybook => {
            let done =
                apb_core::bundled::install_playbook(&a.target, false).map_err(|e| e.to_string())?;
            Ok(json!({ "version": done.version, "digest": done.digest }))
        }
        ActionKind::InstallSkill | ActionKind::UpgradeSkill => {
            let done = skills
                .install(&a.target, false)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "version": done.version, "digest": done.digest }))
        }
        ActionKind::ReplaceModifiedPlaybook => {
            let done = apb_core::bundled::replace_modified_playbook(&a.target)
                .map_err(|e| e.to_string())?;
            let previous = a.params["observed"]["installed"].as_str().unwrap_or("");
            Ok(json!({
                "version": done.version,
                "digest": done.digest,
                "previous": previous,
                "backup": format!(
                    "{previous} stays in the history: apb version use {} {previous} --scope global",
                    a.target
                ),
            }))
        }
        ActionKind::ReplaceModifiedSkill => {
            let done = skills.install(&a.target, true).map_err(|e| e.to_string())?;
            Ok(json!({
                "version": done.version,
                "digest": done.digest,
                "backup": done.backup,
            }))
        }
        ActionKind::WriteRoleProfile => {
            let roles = plan.roles.as_ref().ok_or("no role plan")?;
            let profile = roles
                .profiles
                .iter()
                .find(|p| p.name == a.target)
                .ok_or("the role is not in the plan")?;
            let one = apb_core::bundled::RolePlan {
                profiles: vec![profile.clone()],
                independence: roles.independence,
                skipped: Vec::new(),
            };
            let written =
                crate::profile_tools::write_role_profiles(root, &one).map_err(|e| e.to_string())?;
            let digest = written
                .first()
                .and_then(|w| w.get("profile_digest").cloned())
                .unwrap_or(Value::Null);
            Ok(json!({ "profile_digest": digest, "agent": profile.agent }))
        }
        ActionKind::RegisterMcp => {
            let state = plan
                .observed
                .hosts
                .iter()
                .find(|h| h.host == a.target)
                .ok_or("the host is not in the plan")?;
            hosts::register(state, backups, now)
        }
        ActionKind::RegisterHook => {
            let state = plan
                .observed
                .hooks
                .iter()
                .find(|h| h.host == a.target)
                .ok_or("the host is not in the plan")?;
            super::hook::register(state, backups, now)
        }
        ActionKind::WriteStandingBlock => {
            standing::write(Path::new(&a.target)).map_err(|e| e.to_string())
        }
    }
}

/// Applies the plan of `root` (and `project`, with the replaces `replace`
/// asks for: the same options the plan was read with) whose digest is
/// `requested`. Refused inside a run step.
pub fn apply(
    root: &Path,
    project: Option<&Path>,
    replace: &ReplaceModified,
    requested: &str,
) -> ApplyOutcome {
    let fail = |error: String| ApplyOutcome {
        error: Some(error),
        ..ApplyOutcome::empty(ApplyStatus::Failed, requested, "")
    };
    if let Err(e) = apb_core::process_mode::guard() {
        return fail(e.to_string());
    }
    if let Some(message) = super::step_refusal() {
        return ApplyOutcome {
            refusal: Some(Refusal {
                code: super::RUN_STEP.to_string(),
                message,
            }),
            ..ApplyOutcome::empty(ApplyStatus::Refused, requested, "")
        };
    }
    let Some(dir) = record::setup_dir() else {
        return fail("no config directory".to_string());
    };
    let _lock = match record::lock(&dir) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    let plan = match plan(root, project, replace) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let mut record = match record::load(&dir) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    if plan.digest != requested {
        let applied_before = record.applied.iter().any(|a| a.digest == requested);
        let status = if applied_before && plan.actions.is_empty() {
            ApplyStatus::NoOp
        } else {
            ApplyStatus::PlanChanged
        };
        return ApplyOutcome::empty(status, requested, &plan.digest);
    }
    if let Some(refusal) = plan.refusal.clone() {
        return ApplyOutcome {
            refusal: Some(refusal),
            ..ApplyOutcome::empty(ApplyStatus::Refused, requested, &plan.digest)
        };
    }
    let mut out = ApplyOutcome::empty(ApplyStatus::NoOp, requested, &plan.digest);
    if plan.actions.is_empty() {
        return out;
    }
    let backups = record::backups_dir(&dir);
    let mut skills = apb_core::bundled::SkillUpdates::new();
    for a in &plan.actions {
        match run_one(root, &plan, a, &backups, &mut skills) {
            Ok(data) => {
                let backup = data
                    .get("backup")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                record.entries.push(Entry {
                    kind: a.kind.as_str().to_string(),
                    target: a.target.clone(),
                    at_ms: apb_core::clock::now_ms(),
                    data,
                });
                if let Err(e) = record::save(&dir, &record) {
                    out.failed.push(Failed {
                        kind: a.kind,
                        target: a.target.clone(),
                        error: format!("done, but the setup record was not saved: {e}"),
                    });
                    continue;
                }
                out.done.push(Done {
                    kind: a.kind,
                    target: a.target.clone(),
                    detail: a.detail.clone(),
                    backup,
                });
            }
            Err(error) => out.failed.push(Failed {
                kind: a.kind,
                target: a.target.clone(),
                error,
            }),
        }
    }
    out.notes = skills.finish();
    record.applied.push(Applied {
        digest: plan.digest.clone(),
        at_ms: apb_core::clock::now_ms(),
    });
    if let Err(e) = record::save(&dir, &record) {
        out.error = Some(format!("the setup record was not saved: {e}"));
    }
    out.record = Some(record::record_path(&dir));
    out.status = if out.failed.is_empty() && out.error.is_none() {
        ApplyStatus::Applied
    } else {
        ApplyStatus::Failed
    };
    out
}
