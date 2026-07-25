//! The adoption path: a trial run in a throwaway git worktree, the run
//! preparation report, and the approval that promotes a trialled draft.

use std::collections::BTreeMap;
use std::path::Path;

use super::ToolError;
use super::run::build_duration_table_from;
use apb_core::registry::Registry;
use apb_engine::RunOptions;
use apb_engine::event::read_all;
use apb_engine::state::{RunState, RunStatus};
use serde_json::{Value, json};

fn git(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
}

fn is_git_repo(root: &Path) -> bool {
    git(root, &["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Truncates a string to <= `max` bytes, backing off to the nearest UTF-8 character
/// boundary (String::truncate panics on a non-boundary; a git diff can contain
/// multi-byte characters).
fn truncate_on_char_boundary(s: &mut String, max: usize) {
    if s.len() > max {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

/// Waits for a terminal run event (succeeded/failed/aborted) up to the deadline.
fn poll_terminal(run_dir: &Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Ok(events) = read_all(run_dir) {
            let state = RunState::fold(&events);
            if matches!(
                state.run_status,
                RunStatus::Succeeded | RunStatus::Failed | RunStatus::Aborted
            ) {
                return state.run_status.as_str().to_string();
            }
        }
        if std::time::Instant::now() >= deadline {
            return "timeout".to_string();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A trial run of a draft playbook driven by the effects matrix (spec 8.3): filesystem-writing
/// ones run in a git worktree with the diff shown; irreversible ones are forbidden; network-only
/// ones with no writes run unisolated, with a flag.
pub fn playbook_trial(
    root: &Path,
    id: &str,
    version: Option<&str>,
    params: BTreeMap<String, String>,
    instruction: Option<String>,
    scope: &str,
) -> Result<Value, ToolError> {
    use apb_core::schema::Effect;

    // Take the definition from the chosen scope; a global draft must also be runnable
    // (spec 8.3). Execution happens in the current project regardless.
    let (definition_parent, origin_label) = match scope {
        "project" => (root.join(".apb"), "project"),
        "global" => (
            apb_core::store::global_playbooks_parent()
                .ok_or_else(|| ToolError::Engine("no global config dir".into()))?,
            "global",
        ),
        other => return Err(ToolError::Engine(format!("unknown scope `{other}`"))),
    };
    let reg = Registry::open_dir(&definition_parent).map_err(ToolError::from)?;
    let loaded = reg.load(id, version)?;
    let effects = apb_core::effects::effective(&loaded.playbook);
    let digest = apb_core::scope::digest_str(&loaded.yaml);

    if effects.contains(&Effect::Irreversible) {
        return Ok(json!({ "rejected": "trial_forbidden_irreversible", "id": id }));
    }

    let opts = RunOptions {
        instruction,
        params,
        ..Default::default()
    };
    let resolved_version = loaded.version.clone();

    // ResolvedPlaybook: the definition comes from the chosen scope, execution happens in
    // the given execution_root (worktree or the current project).
    let resolved = |exec_root: std::path::PathBuf| apb_core::store::ResolvedPlaybook {
        definition_parent: definition_parent.clone(),
        execution_root: exec_root,
        id: id.to_string(),
        version: resolved_version.clone(),
        digest: digest.clone(),
        origin_label,
    };

    if effects.contains(&Effect::FsWrite) {
        if !is_git_repo(root) {
            return Ok(json!({ "rejected": "trial_needs_git_worktree", "id": id }));
        }
        let scratch =
            std::env::temp_dir().join(format!("apb-trial-{}-{}", id, apb_core::clock::now_ms()));
        let scratch_str = scratch.to_string_lossy().into_owned();
        let add = git(root, &["worktree", "add", "--detach", &scratch_str, "HEAD"]);
        if add.as_ref().map(|o| !o.status.success()).unwrap_or(true) {
            return Err(ToolError::Engine("git worktree add failed".into()));
        }

        // Run in the worktree. We remove the worktree ONLY when the run is definitely
        // terminal (or the spawn failed outright): on timeout the run is still alive in the
        // background thread, and `worktree remove --force` would yank the directory
        // out from under it - so in that case we keep the worktree and report the path.
        match apb_engine::run_background_resolved(&resolved(scratch.clone()), opts) {
            Ok(run_id) => {
                let run_dir = scratch.join(".apb/runs").join(&run_id);
                let status = poll_terminal(&run_dir);
                if status == "timeout" {
                    return Ok(json!({
                        "run_id": run_id,
                        "status": "timeout",
                        "worktree": scratch_str,
                        "notes": ["trial did not finish within the poll window; the run continues and the worktree is preserved at `worktree` - remove it manually once the run ends"],
                    }));
                }
                let mut diff = String::new();
                if let Some(o) = git(
                    &scratch,
                    &["status", "--porcelain", "--", ".", ":(exclude).playbook"],
                ) {
                    diff.push_str(&String::from_utf8_lossy(&o.stdout));
                }
                if let Some(o) = git(&scratch, &["diff", "--", ".", ":(exclude).playbook"]) {
                    diff.push_str(&String::from_utf8_lossy(&o.stdout));
                }
                truncate_on_char_boundary(&mut diff, 64 * 1024);
                let measured = apb_engine::progress::node_durations_seconds(
                    &read_all(&run_dir).unwrap_or_default(),
                );
                let durations = build_duration_table_from(&loaded.playbook, &measured);
                let _ = git(root, &["worktree", "remove", "--force", &scratch_str]);
                let _ = git(root, &["worktree", "prune"]);
                return Ok(json!({
                    "run_id": run_id,
                    "status": status,
                    "diff": diff,
                    "durations": durations,
                    "notes": ["ran in a throwaway git worktree; changes are not applied to your workspace"],
                }));
            }
            Err(e) => {
                // The spawn failed - there is no run, the worktree can be torn down.
                let _ = git(root, &["worktree", "remove", "--force", &scratch_str]);
                let _ = git(root, &["worktree", "prune"]);
                return Err(ToolError::from(e));
            }
        }
    }

    // No filesystem writes: network/external effects run unisolated (the agent
    // was required to confirm with the user before the call, tier 0), we flag this.
    let external = effects.contains(&Effect::Network) || effects.contains(&Effect::External);
    let run_id = apb_engine::run_background_resolved(&resolved(root.to_path_buf()), opts)
        .map_err(ToolError::from)?;
    let run_dir = root.join(".apb/runs").join(&run_id);
    let status = poll_terminal(&run_dir);
    let measured =
        apb_engine::progress::node_durations_seconds(&read_all(&run_dir).unwrap_or_default());
    let durations = build_duration_table_from(&loaded.playbook, &measured);
    Ok(json!({
        "run_id": run_id,
        "status": status,
        "external_effects_executed": external,
        "durations": durations,
        "notes": ["no filesystem writes to isolate"],
    }))
}

/// Phase 1 of the two-phase contract (spec 7): resolves the target in another workspace,
/// runs preflight, and issues a signed plan_token. Read-only: it executes
/// and mutates nothing. An unreachable workspace/refusal is returned
/// structurally.
pub fn playbook_prepare_run(
    id: &str,
    version: Option<&str>,
    workspace: &str,
    params: BTreeMap<String, String>,
) -> Result<Value, ToolError> {
    let root_b = match apb_core::projects::resolve_root(workspace) {
        Ok(p) => p,
        Err(apb_core::projects::ProjectAccessError::Unreachable { workspace_id, path }) => {
            return Ok(
                json!({ "error": "workspace_unreachable", "workspace": workspace_id, "path": path }),
            );
        }
        Err(apb_core::projects::ProjectAccessError::Unknown(w)) => {
            return Ok(json!({ "error": "workspace_unknown", "workspace": w }));
        }
    };
    let pf = match crate::policy::preflight(&root_b, id, version) {
        Ok(p) => p,
        Err(refusal) => return Ok(json!({ "policy_refusal": refusal })),
    };
    let now = apb_core::clock::now_ms() as u64;
    let payload = crate::plan::PlanPayload {
        workspace_id: workspace.to_string(),
        id: id.to_string(),
        version: pf.version.clone(),
        digest: pf.digest.clone(),
        params: params.clone(),
        effects: pf.effects.clone(),
        // Resolve the bundle against the version SELECTED by preflight (pf.version), not the
        // original request: otherwise the token would carry the digest of one version and the bundle
        // of another (for example, with version: None and current != active).
        // A cross-workspace prepare does not spawn an external supervisor agent -> supervised: false.
        profiles: crate::policy::playbook_profile_bundles(&root_b, id, Some(&pf.version), false)
            .into_iter()
            .map(|(key, bundle)| crate::plan::PlanProfile { key, bundle })
            .collect(),
        exp_ms: now + 10 * 60 * 1000,
        nonce: format!("n-{}", uuid::Uuid::new_v4().simple()),
    };
    let store = apb_core::trust::TrustStore::load();
    let trusted = store.is_approved(&pf.digest);
    // Profiles with their bundle and trust status - the user must see exactly what they
    // are confirming (spec 5.2). We show exactly the bundles baked into the plan.
    let profiles: Vec<Value> = payload
        .profiles
        .iter()
        .map(|p| json!({ "ref": p.key, "bundle": p.bundle, "trusted": store.is_approved(&p.bundle) }))
        .collect();
    let token = crate::plan::encode(&payload);
    Ok(json!({
        "plan": {
            "workspace": workspace,
            "id": id,
            "version": pf.version,
            "digest": pf.digest,
            "effects": pf.effects,
            "trusted": trusted,
            "profiles": profiles,
            "params": params,
        },
        "plan_token": token,
    }))
}

/// Activates a playbook after a successful trial or explicit confirmation (spec
/// 8.3): lifecycle -> active, digest -> approved (agent-generated). Also works
/// with the global scope (otherwise a global draft could never be activated).
pub fn playbook_approve(
    root: &Path,
    id: &str,
    version: Option<&str>,
    scope: &str,
) -> Result<Value, ToolError> {
    let definition_parent = match scope {
        "project" => root.join(".apb"),
        "global" => apb_core::store::global_playbooks_parent()
            .ok_or_else(|| ToolError::Engine("no global config dir".into()))?,
        other => return Err(ToolError::Engine(format!("unknown scope `{other}`"))),
    };
    let reg = Registry::open_dir(&definition_parent).map_err(ToolError::from)?;
    let loaded = reg.load(id, version)?;
    let digest = apb_core::scope::digest_str(&loaded.yaml);
    let playbook_dir = definition_parent.join("playbooks").join(id);
    apb_core::trust::write_lifecycle(&playbook_dir, apb_core::trust::Lifecycle::Active)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    let mut trust = apb_core::trust::TrustStore::load();
    trust
        .approve(&digest, id, apb_core::trust::OriginKind::AgentGenerated)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(
        json!({ "id": id, "version": loaded.version, "scope": scope, "lifecycle": "active", "trusted": true }),
    )
}
