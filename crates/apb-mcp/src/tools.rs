use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use apb_core::registry::{Registry, RegistryError, is_safe_segment};
use apb_core::validate::{Issue, Severity, ValidationContext, validate};
use apb_core::versioning::{
    VersioningError, create_patch_version, create_version, delete_playbook,
};
use apb_engine::control::Control;
use apb_engine::event::read_all;
use apb_engine::run_config::ChildExpectation;
use apb_engine::state::{FailureReason, RunState, RunStatus};
use apb_engine::{
    EngineError, RunMode, RunOptions, list_runs, plan_resume, post_supervisor_command, run,
    run_cancel, run_inspect as engine_run_inspect, stop_run, touch_heartbeat, wait_wake,
    write_supervisor_report,
};
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("engine error: {0}")]
    Engine(String),
    /// An optimistic-concurrency conflict (CAS): the profile already exists / does not
    /// exist / `expected_digest` did not match. Typed separately so that
    /// surfaces can map it to 409 without a fragile substring search in the text.
    #[error("conflict: {0}")]
    Conflict(String),
}

impl From<RegistryError> for ToolError {
    fn from(e: RegistryError) -> Self {
        match e {
            RegistryError::NotFound(w) => ToolError::NotFound(w),
            other => ToolError::Engine(other.to_string()),
        }
    }
}

impl From<EngineError> for ToolError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::NotFound(m) => ToolError::NotFound(m),
            EngineError::Registry(RegistryError::NotFound(w)) => ToolError::NotFound(w),
            EngineError::Conflict(m) => ToolError::Conflict(m),
            other => ToolError::Engine(other.to_string()),
        }
    }
}

impl From<VersioningError> for ToolError {
    fn from(e: VersioningError) -> Self {
        match e {
            VersioningError::NotFound(w) => ToolError::NotFound(w),
            VersioningError::Validation(issues) => {
                ToolError::Engine(render_validation_issues(&issues))
            }
            other => ToolError::Engine(other.to_string()),
        }
    }
}

/// Renders a validation failure as `validation failed:` followed by one line
/// per issue. Delegates to `apb_core::validate::render_issues`, the single
/// canonical rendering shared with `VersioningError::Validation`'s own
/// `Display` impl, so this surface can never drift from any other consumer
/// of the same `Vec<Issue>`.
fn render_validation_issues(issues: &[Issue]) -> String {
    apb_core::validate::render_issues(issues)
}

/// Creates a new playbook or a new minor version of an existing one.
pub fn playbook_create(root: &Path, id: &str, yaml: &str) -> Result<Value, ToolError> {
    let version = create_version(root, id, yaml, None, true)?;
    Ok(json!({ "id": id, "version": version }))
}

/// Updates an existing playbook (a new minor version). If the id does not exist - NotFound.
pub fn playbook_update(root: &Path, id: &str, yaml: &str) -> Result<Value, ToolError> {
    if !is_safe_segment(id) {
        return Err(ToolError::NotFound(id.to_string()));
    }
    let dir = root.join(".apb/playbooks").join(id);
    if !dir.is_dir() {
        return Err(ToolError::NotFound(id.to_string()));
    }
    let version = create_version(root, id, yaml, None, true)?;
    Ok(json!({ "id": id, "version": version }))
}

/// Approves the digest of a version just created locally (spec 3.1): creation
/// through the tool/CLI is a local user action, hence trusted. Best-effort:
/// a failure is not critical (the playbook will simply stay untrusted until trial/acknowledge).
/// Project scope (`root/.apb`); global creation is approved on its own path.
pub fn approve_local(root: &Path, id: &str, version: &str) {
    let yaml_path = root
        .join(".apb/playbooks")
        .join(id)
        .join(version)
        .join("playbook.yaml");
    if let Ok(yaml) = std::fs::read_to_string(&yaml_path) {
        let digest = apb_core::scope::digest_str(&yaml);
        let mut trust = apb_core::trust::TrustStore::load();
        let _ = trust.approve(&digest, id, apb_core::trust::OriginKind::LocallyApproved);
    }
}

/// Soft-deletes a playbook into trash.
pub fn playbook_delete(root: &Path, id: &str) -> Result<Value, ToolError> {
    let trashed = delete_playbook(root, id, apb_engine::event::now_millis())?;
    Ok(json!({ "trashed": trashed.to_string_lossy() }))
}

fn open(root: &Path) -> Result<Registry, ToolError> {
    Registry::open(root).map_err(ToolError::from)
}

pub fn playbook_list(root: &Path) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let list = reg.list()?;
    serde_json::to_value(list).map_err(|e| ToolError::Engine(e.to_string()))
}

/// The compact structural playbook catalog (tier 1, spec 4). Project scope
/// plus global, with trust-aware shadowing and effective effects. Does not break
/// `playbook_list` - this is a separate surface.
pub fn playbook_catalog(
    root: &Path,
    workspace_id: Option<&str>,
    revision: Option<&str>,
    limit: Option<usize>,
) -> Result<Value, ToolError> {
    let dismissed = apb_core::dismiss::active_patterns();
    Ok(crate::catalog::build(
        root,
        workspace_id,
        revision,
        limit,
        dismissed,
    ))
}

/// The registry of the user's workspaces (spec 6): current, global, and other projects.
pub fn projects_list() -> Result<Value, ToolError> {
    let entries = apb_core::projects::list_active();
    serde_json::to_value(entries).map_err(|e| ToolError::Engine(e.to_string()))
}

/// Tier 2 (spec 4): playbook authoring details. Pulled only when
/// creating/reworking one; it does not enter a normal session.
pub fn playbook_howto() -> Result<Value, ToolError> {
    Ok(json!({ "howto": include_str!("../../../docs/HOWTO-authoring.md") }))
}

/// A "looks like a secret" heuristic (spec 8.3): a crude scan without a regex crate.
/// Catches `key: value` with an indicator key and a value of length >= 8 with no
/// whitespace, as well as long (>= 32) contiguous base64/hex-like tokens.
/// Returns a masked fragment. This is an extra safety net, not a
/// guarantee: the main contract is that the host does not put secrets into the synopsis.
fn secret_like(text: &str) -> Option<String> {
    const KEYS: [&str; 6] = [
        "api_key", "apikey", "api-key", "secret", "token", "password",
    ];
    for raw in text.lines() {
        let line = raw.trim();
        let lower = line.to_lowercase();
        // Look for the indicator key anywhere in the line (robust to a JSON wrapper
        // like `"note":"api_key: ..."`), then take the value after the nearest ':'/'='.
        // We take offsets and slice using the same `lower` string throughout - otherwise on Unicode
        // (to_lowercase can change length) a byte index from `lower` could
        // point into the middle of a character in `line` and panic.
        for key in KEYS {
            let Some(kpos) = lower.find(key) else {
                continue;
            };
            let after = &lower[kpos + key.len()..];
            if let Some(sep) = after.find([':', '=']) {
                let val = after[sep + 1..].trim();
                let val: &str = val
                    .split(|c: char| {
                        c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '}'
                    })
                    .find(|s| !s.is_empty())
                    .unwrap_or("");
                if val.len() >= 8 {
                    return Some(mask(val));
                }
            }
        }
        for tok in line.split(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
            if tok.len() >= 32
                && tok
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
            {
                return Some(mask(tok));
            }
        }
    }
    None
}

fn mask(s: &str) -> String {
    let head: String = s.chars().take(3).collect();
    format!("{head}...({} chars)", s.len())
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Accepts an action synopsis and creates a draft playbook from it in the chosen
/// scope (spec 8.3). Draft: does not pass the run gate until it goes through trial
/// or explicit confirmation. Secrets and duplicates are rejected before writing.
pub fn playbook_capture(
    root: &Path,
    synopsis: &Value,
    selected_scope: &str,
    yaml: &str,
) -> Result<Value, ToolError> {
    // Secret scan over the synopsis and over the yaml (spec 8.3).
    let synopsis_text = serde_json::to_string(synopsis).unwrap_or_default();
    for src in [synopsis_text.as_str(), yaml] {
        if let Some(m) = secret_like(src) {
            return Ok(json!({ "rejected": "secret_like_value", "match": m }));
        }
    }

    // Take the id from the yaml itself (the canonical source).
    let parsed = apb_core::schema::Playbook::from_yaml(yaml)
        .map_err(|e| ToolError::Engine(format!("invalid yaml: {e}")))?;
    let id = parsed.id.clone();

    let parent = match selected_scope {
        "project" => root.join(".apb"),
        "global" => apb_core::store::global_playbooks_parent()
            .ok_or_else(|| ToolError::Engine("no global config dir".into()))?,
        other => return Err(ToolError::Engine(format!("unknown scope `{other}`"))),
    };

    // Dedup: a close trigger among the existing ones (both scopes). An exact
    // match of the normalized when string -> possible_duplicate.
    let catalog = crate::catalog::build(root, None, None, None, Vec::new());
    if let Some(entries) = catalog["entries"].as_array() {
        let new_whens: Vec<String> = synopsis
            .get("trigger")
            .and_then(|t| t.get("when"))
            .and_then(|w| w.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(normalize)).collect())
            .unwrap_or_default();
        for e in entries {
            let existing: Vec<String> = e
                .get("trigger")
                .and_then(|t| t.get("when"))
                .and_then(|w| w.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(normalize)).collect())
                .unwrap_or_default();
            if new_whens.iter().any(|w| existing.contains(w)) {
                return Ok(json!({ "rejected": "possible_duplicate", "candidate": e["ref"] }));
            }
        }
    }

    // Draft creation. A Conflict from core -> duplicate_id.
    let origin = if selected_scope == "global" {
        apb_core::profile_store::PlaybookOrigin::Global
    } else {
        apb_core::profile_store::PlaybookOrigin::Project
    };
    let version = match apb_core::versioning::create_draft_in(&parent, &id, yaml, origin) {
        Ok(v) => v,
        Err(apb_core::versioning::VersioningError::Conflict(_)) => {
            return Ok(json!({ "rejected": "duplicate_id", "id": id }));
        }
        Err(e) => return Err(ToolError::from(e)),
    };

    // Mark it draft and write provenance. The digest is NOT approved - capture is not a
    // local approval (spec 8.3).
    let playbook_dir = parent.join("playbooks").join(&id);
    apb_core::trust::write_lifecycle(&playbook_dir, apb_core::trust::Lifecycle::Draft)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    let provenance = json!({
        "created_by": "agent-capture",
        "title": synopsis.get("title").and_then(|t| t.as_str()).unwrap_or(""),
    });
    let _ = apb_core::fsutil::atomic_write(
        &playbook_dir.join("provenance.json"),
        provenance.to_string().as_bytes(),
    );

    Ok(json!({
        "id": id,
        "version": version,
        "scope": selected_scope,
        "lifecycle": "draft",
        "trusted": false,
        "provenance": provenance,
    }))
}

/// Records the user's decline of the suggestion to save a playbook (spec 8.2):
/// "no, and don't suggest that again". The pattern is an English kebab-slug.
pub fn suggestion_dismiss(pattern: &str, ttl_days: Option<u64>) -> Result<Value, ToolError> {
    apb_core::dismiss::record(pattern, ttl_days).map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(json!({ "dismissed": pattern }))
}

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
        let scratch = std::env::temp_dir().join(format!(
            "apb-trial-{}-{}",
            id,
            apb_engine::event::now_millis()
        ));
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
    let now = apb_engine::event::now_millis() as u64;
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

/// Read-only listing of installed connectors for an authoring agent (spec 12):
/// each installed connector with its version, storefront summary, connector
/// trust state, the function names it exposes (with description and the
/// read_only / deprecated marks), and the configured account names - enough to
/// write a node `connectors` binding. Never returns account field values or env
/// values (a secret-marked field holds an `{{env.VAR}}` reference, which we
/// still do not surface here: names only).
pub fn connectors_list(root: &Path) -> Result<Value, ToolError> {
    let trust = apb_core::trust::TrustStore::load();
    let approved_ids = trust.approved_record_ids(apb_core::trust::Kind::Connector);
    let mut out = Vec::new();
    for summary in apb_core::connector::store::list() {
        let Ok(loaded) = apb_core::connector::store::load(&summary.name) else {
            // A connector that vanished or stopped parsing between listing and
            // load is simply skipped, matching `store::list`'s own tolerance.
            continue;
        };
        let functions: Vec<Value> = loaded
            .doc
            .functions
            .iter()
            .map(|f| {
                json!({
                    "name": f.name,
                    "description": f.description,
                    "read_only": f.read_only,
                    "deprecated": f.deprecated,
                })
            })
            .collect();
        // Account NAMES only (never fields/env). Best-effort: a broken account
        // config yields an empty account list, not a failed listing.
        let accounts: Vec<String> = apb_core::connector::config::load_merged(root, &summary.name)
            .map(|accts| accts.into_iter().map(|a| a.name).collect())
            .unwrap_or_default();
        let trust_state = if trust.is_approved(&loaded.digest) {
            "approved"
        } else if approved_ids.iter().any(|id| id == &summary.name) {
            "changed"
        } else {
            "unapproved"
        };
        out.push(json!({
            "name": summary.name,
            "version": summary.version,
            "summary": summary.meta.summary,
            "trust": trust_state,
            "functions": functions,
            "accounts": accounts,
        }));
    }
    Ok(json!({ "connectors": out }))
}

pub fn playbook_get(root: &Path, id: &str, version: Option<&str>) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let loaded = reg.load(id, version)?;
    Ok(json!({
        "id": id,
        "version": loaded.version,
        "yaml": loaded.yaml,
        "playbook": loaded.playbook,
        "layout": loaded.layout,
    }))
}

pub fn playbook_validate(root: &Path, id: &str) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let loaded = reg.load(id, None)?;
    let ctx = ValidationContext {
        profiles: reg.profiles(),
        ..Default::default()
    };
    let report = validate(&loaded.playbook, &ctx);
    let issues: Vec<Value> = report.issues.iter().map(|i| json!({
        "code": i.code,
        "severity": match i.severity { Severity::Error => "error", Severity::Warning => "warning" },
        "message": i.message,
        "node": i.node,
    })).collect();
    Ok(json!({ "valid": report.is_valid(), "issues": issues }))
}

/// Resolves the run directory, uniformly rejecting an unsafe run_id (path traversal)
/// and a missing run as NotFound.
fn resolve_run_dir(root: &Path, run_id: &str) -> Result<std::path::PathBuf, ToolError> {
    if !is_safe_segment(run_id) {
        return Err(ToolError::NotFound(format!("run `{run_id}`")));
    }
    let dir = root.join(".apb/runs").join(run_id);
    if !dir.is_dir() {
        return Err(ToolError::NotFound(format!("run `{run_id}`")));
    }
    Ok(dir)
}

#[allow(clippy::too_many_arguments)]
pub fn playbook_run(
    root: &Path,
    id: &str,
    version: Option<&str>,
    params: BTreeMap<String, String>,
    instruction: Option<String>,
    expected_digest: Option<String>,
    expected_profile_bundles: Option<BTreeMap<String, String>>,
    expected_children: Option<BTreeMap<String, ChildExpectation>>,
    expected_connectors: BTreeMap<String, String>,
    expected_connector_accounts: BTreeMap<String, String>,
    continued_from: Option<String>,
) -> Result<Value, ToolError> {
    let opts = RunOptions {
        instruction,
        params,
        allow_shared_workdir: false,
        mode: RunMode::Autonomous,
        supervisor_expected: false,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
        expected_digest,
        expected_profile_bundles,
        parent_run: None,
        continued_from,
        depth: 0,
        expected_children,
        expected_connectors,
        expected_connector_accounts,
        cache: Default::default(),
    };
    let res = run(root, id, version, opts)?;
    Ok(json!({ "run_id": res.run_id, "outcome": res.outcome.as_str() }))
}

/// A non-blocking run start for a regular (non-supervised) MCP client:
/// starts the playbook (autonomous) and returns run_id immediately. The client
/// then polls `run_status`/`run_events` and resolves reviews via `review_decide`.
/// Needed because some hosts (e.g. ChatGPT Apps) have a tool-call timeout of
/// ~60s, while a run can take minutes (design doc, section 13.5).
///
/// The run is driven by a DETACHED process, not a thread of this one: the
/// policy gate, permit verification and manifest snapshot all complete here,
/// in-process, and only the drive loop is handed across - so an `apb mcp`
/// bound to a chat session that dies no longer takes the run with it.
#[allow(clippy::too_many_arguments)]
pub fn playbook_run_background(
    root: &Path,
    id: &str,
    version: Option<&str>,
    params: BTreeMap<String, String>,
    instruction: Option<String>,
    expected_digest: Option<String>,
    expected_profile_bundles: Option<BTreeMap<String, String>>,
    expected_children: Option<BTreeMap<String, ChildExpectation>>,
    expected_connectors: BTreeMap<String, String>,
    expected_connector_accounts: BTreeMap<String, String>,
    continued_from: Option<String>,
) -> Result<Value, ToolError> {
    let opts = RunOptions {
        instruction,
        params,
        allow_shared_workdir: false,
        mode: RunMode::Autonomous,
        supervisor_expected: false,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
        expected_digest,
        expected_profile_bundles,
        parent_run: None,
        continued_from,
        depth: 0,
        expected_children,
        expected_connectors,
        expected_connector_accounts,
        cache: Default::default(),
    };
    let run_id = apb_engine::start_detached(root, id, version, opts)?;
    Ok(json!({ "run_id": run_id }))
}

pub fn runs_list(root: &Path) -> Result<Value, ToolError> {
    let runs = list_runs(root)?;
    serde_json::to_value(runs).map_err(|e| ToolError::Engine(e.to_string()))
}

pub fn run_status(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let events = read_all(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    let state = RunState::fold(&events);
    // Liveness overlay (Task 9). The fold is pure and replayable; these three
    // read the machine's process table at request time, which is precisely why
    // they are applied here rather than folded into `RunState`.
    //
    // The incident behind them: a crashed attempt kept reporting an in-flight
    // node for 19 minutes and `run_status` carried no timestamps at all, so
    // "is it stuck or working?" could not be answered from the API. `lost`
    // answers the first question, `node_times` the second.
    let lost = apb_engine::liveness::lost_nodes(&events);
    let node_times = apb_engine::liveness::node_times(&events);
    let driver_alive = apb_engine::liveness::driver_alive(&dir, run_id);
    let nodes: BTreeMap<String, String> = state
        .nodes
        .iter()
        .map(|(k, v)| {
            let status = if lost.contains(k) {
                apb_engine::liveness::LOST.to_string()
            } else {
                v.as_str().to_string()
            };
            (k.clone(), status)
        })
        .collect();
    let progress = apb_engine::progress::from_run_dir(&dir, &events);
    // Lifted out of `progress` to the top level (spec 2026-07-20-interactive-
    // nodes, Task 8): callers that only care about the pending question
    // (`run_answer`'s caller, the web) do not have to drill into `progress`.
    // `progress` itself still carries it too (`progress.pending_question`),
    // unchanged.
    let cfg = apb_engine::run_config::read_run_config(&dir).unwrap_or_default();
    let pending_question = progress.as_ref().and_then(|p| p.pending_question.clone());
    // Lifted to the top level like `pending_question` (issue #42 finding 4):
    // a human_review gate must be first-class here so an intermediary that
    // calls `run_status` is forced to see the pending decision, its options,
    // and how to answer - the gate no longer waits silently forever.
    let pending_review = progress.as_ref().and_then(|p| p.pending_review.clone());
    let answer = apb_engine::progress::run_answer(&dir, &events);
    let children: Vec<Value> = events
        .iter()
        .filter_map(|e| match &e.payload {
            apb_engine::event::EventPayload::ChildRunStarted { node_id, run_id } => {
                let child_dir = dir.parent().map(|p| p.join(run_id));
                let status = child_dir
                    .and_then(|d| read_all(&d).ok())
                    .map(|ev| RunState::fold(&ev).run_status.as_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                Some(json!({ "node_id": node_id, "run_id": run_id, "status": status }))
            }
            _ => None,
        })
        .collect();
    // The verbatim reason behind a `failed` run (issue #42 finding 3): every
    // scheduler/prepare path that fails a run now appends a `RunError` before
    // its terminal `run_finished(failed)`, so an operator reads why directly
    // from run_status instead of grepping events.jsonl by hand. `None` for
    // anything other than a failed run, and for a failed run whose log
    // predates this fix (no `RunError` was ever appended for it).
    let failure_reason = (state.run_status == RunStatus::Failed)
        .then(|| state.failure_reason.as_ref().map(FailureReason::display))
        .flatten();
    Ok(json!({
        "run_id": run_id,
        "run_status": state.run_status.as_str(),
        "nodes": nodes,
        "node_times": node_times,
        "driver_alive": driver_alive,
        "outputs": state.outputs,
        "progress": progress,
        "pending_question": pending_question,
        "pending_review": pending_review,
        "answer": answer,
        "children": children,
        "continued_from": cfg.continued_from,
        "superseded_by": cfg.superseded_by,
        "failure_reason": failure_reason,
    }))
}

pub fn run_events(root: &Path, run_id: &str, from_seq: Option<u64>) -> Result<Value, ToolError> {
    let dir = resolve_run_dir(root, run_id)?;
    let events = read_all(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    let from = from_seq.unwrap_or(0);
    let filtered: Vec<&_> = events.iter().filter(|e| e.seq >= from).collect();
    Ok(
        json!({ "events": serde_json::to_value(filtered).map_err(|e| ToolError::Engine(e.to_string()))? }),
    )
}

fn node_kind_label(kind: &apb_core::schema::NodeKind) -> &'static str {
    use apb_core::schema::NodeKind::*;
    match kind {
        Start => "start",
        AgentTask { .. } => "agent_task",
        Script { .. } => "script",
        Prompt { .. } => "prompt",
        Condition { .. } => "condition",
        HumanReview { .. } => "human_review",
        Wait { .. } => "wait",
        Finish { .. } => "finish",
        Playbook { .. } => "playbook",
    }
}

/// Per-node expected vs measured durations for calibration (spec 5). Measured
/// comes from the run's events; expected from the playbook version bound to
/// the run. The maintaining agent uses this to update estimates via
/// playbook_update; the engine never rewrites the playbook.
pub(crate) fn build_duration_table_from(
    playbook: &apb_core::schema::Playbook,
    measured: &BTreeMap<String, u64>,
) -> Vec<Value> {
    playbook
        .nodes
        .iter()
        .map(|n| {
            json!({
                "node": n.id,
                "kind": node_kind_label(&n.kind),
                "expected_seconds": n.expected_seconds(),
                "measured_seconds": measured.get(&n.id),
            })
        })
        .collect()
}

pub fn run_report(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    // There is no supervisor agent in Phase 3: the report is a light state
    // summary. The full supervisor report is Phase 4. events.jsonl is read once
    // and the playbook snapshot parsed once here; a failing events read
    // propagates as a ToolError rather than masquerading as an empty duration
    // table (B7). The base object mirrors `run_status`'s JSON shape exactly.
    let dir = resolve_run_dir(root, run_id)?;
    let events = read_all(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    let state = RunState::fold(&events);
    let pb = apb_engine::progress::load_run_playbook(&dir);
    let progress = pb
        .as_ref()
        .map(|p| apb_engine::progress::compute(p, &events));
    let answer = apb_engine::progress::run_answer(&dir, &events);

    let nodes: BTreeMap<String, String> = state
        .nodes
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().to_string()))
        .collect();
    let mut base = json!({
        "run_id": run_id,
        "run_status": state.run_status.as_str(),
        "nodes": nodes,
        "outputs": state.outputs,
        "progress": progress,
        "answer": answer,
    });

    // duration_table is always present (empty when there is no snapshot), as
    // before; it is now built from the single events read above.
    let table = match &pb {
        Some(playbook) => {
            let measured = apb_engine::progress::node_durations_seconds(&events);
            build_duration_table_from(playbook, &measured)
        }
        None => Vec::new(),
    };
    if let Some(obj) = base.as_object_mut() {
        obj.insert("duration_table".into(), json!(table));
    }
    Ok(base)
}

pub fn run_resume(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    allow_environment_drift: bool,
) -> Result<Value, ToolError> {
    // Compute the resume decision up front so the ack reports where and why the
    // run resumes. This must run BEFORE the drive: once the run reaches a
    // terminal state, an argument-free `plan_resume` would refuse it.
    let decision = plan_resume(root, run_id, from_node)?;
    // The drive itself happens in a separate OS process: this session may be a
    // chat host that dies at any moment, and a resumed run must not die with
    // it. The ack is what the caller gets back, immediately - the run's
    // progress is read afterwards through `run_status` / `run_events`.
    // A stop still sitting unapplied in the control queue is consumed by the
    // resumed drive BEFORE it executes anything, so the run stops again
    // immediately. Read it before spawning the driver (afterwards the driver
    // races us to consume it) and say so in the ack, or the caller sees a
    // successful resume followed by a run that never moved.
    let pending_stop =
        apb_engine::control::pending_stop_seq(&root.join(".apb/runs").join(run_id))?.is_some();
    // The drift preflight runs inside resume_detached_with: a drift the caller
    // did not allow is returned as an Err HERE (issue #45 finding 3), instead
    // of the old detached spawn whose child failed its own check on null stdio
    // and left this ack reporting `detached: true` for a run that never moved.
    apb_engine::resume_detached_with(root, run_id, from_node, allow_environment_drift)?;
    let mut ack = json!({
        "run_id": run_id,
        "resumed_from": decision.start_node,
        "reason": decision.reason.as_str(),
        "detached": true,
    });
    if allow_environment_drift && let Some(obj) = ack.as_object_mut() {
        obj.insert(
            "note".into(),
            json!(
                "environment drift override accepted: an agent binary changed since run start, and resume is proceeding anyway; the accepted drift is recorded in the run event log"
            ),
        );
    }
    if pending_stop && let Some(obj) = ack.as_object_mut() {
        obj.insert("stops_on_pending_abort".into(), json!(true));
        obj.insert(
            "note".into(),
            json!(
                "a stop was still pending on this run, so this resume applies it and the run stops again without executing anything; call run_resume once more to continue past it"
            ),
        );
    }
    Ok(ack)
}

/// Starts a playbook in supervised mode without waiting for it to finish, on a
/// detached driver process (see `playbook_run_background`). The supervisor
/// access token is minted by the server layer (Phase 4b, Task 3), not this
/// function.
#[allow(clippy::too_many_arguments)]
pub fn playbook_run_supervised(
    root: &Path,
    id: &str,
    version: Option<&str>,
    params: BTreeMap<String, String>,
    instruction: Option<String>,
    expected_digest: Option<String>,
    expected_profile_bundles: Option<BTreeMap<String, String>>,
    expected_children: Option<BTreeMap<String, ChildExpectation>>,
    expected_connectors: BTreeMap<String, String>,
    expected_connector_accounts: BTreeMap<String, String>,
    continued_from: Option<String>,
) -> Result<Value, ToolError> {
    // supervise:"self" does not spawn a separate supervisor agent process - the supervisor here is the same
    // MCP session that called playbook_run, hence supervisor_expected: false
    // (heartbeat oversight in drive does not touch this path).
    let opts = RunOptions {
        instruction,
        params,
        allow_shared_workdir: false,
        mode: RunMode::Supervised,
        supervisor_expected: false,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
        expected_digest,
        expected_profile_bundles,
        parent_run: None,
        continued_from,
        depth: 0,
        expected_children,
        expected_connectors,
        expected_connector_accounts,
        cache: Default::default(),
    };
    let run_id = apb_engine::start_detached(root, id, version, opts)?;
    Ok(json!({ "run_id": run_id }))
}

/// Blockingly waits for the next wake (or a timeout/run completion) and
/// returns it along with a fresh status. `wake: null` means the run
/// has already finished, or the wait timed out - the agent decides for itself whether to
/// keep looping.
pub fn supervisor_wait_event(
    root: &Path,
    run_id: &str,
    after_seq: Option<u64>,
    timeout_ms: Option<u64>,
) -> Result<Value, ToolError> {
    // A liveness mark for the background supervisor before the blocking wait:
    // a signal that the process watching the run is still alive and polling.
    touch_heartbeat(root, run_id)?;
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(25_000));
    let wake = wait_wake(root, run_id, after_seq, timeout)?;
    let status = run_status(root, run_id)?;
    // Surface the pending human-review gate here too (issue #42 finding 4): a
    // supervisor that wakes on a run must see the gate and its owner-facing
    // instruction so it relays the decision to the user rather than blocking.
    Ok(json!({
        "wake": wake,
        "run_status": status["run_status"],
        "pending_review": status["pending_review"],
    }))
}

/// A full run summary for the observer (status, nodes, context.md, wakes, actions, events).
pub fn sv_run_inspect(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    Ok(engine_run_inspect(root, run_id)?)
}

pub fn node_retry(
    root: &Path,
    run_id: &str,
    node: &str,
    prompt_override: Option<String>,
) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::Retry {
            node: node.to_string(),
            prompt_override,
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

pub fn run_continue_from(root: &Path, run_id: &str, node: &str) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::ContinueFrom {
            node: node.to_string(),
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

pub fn run_pause(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(root, run_id, Control::Pause)?;
    Ok(json!({ "posted_seq": seq }))
}

pub fn run_abort(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    run_cancel(root, run_id)?;
    Ok(json!({ "ok": true }))
}

/// Stops a run and reports what that took: signaling a live driver (whose
/// watcher interrupts the in-flight node), finalizing a run whose driver is
/// gone, or nothing at all for an already terminal run. Unlike
/// `supervisor_run_abort` this needs no supervisor session - it is the
/// operator-facing stop, the same one `apb stop` calls.
pub fn run_stop(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    let outcome = stop_run(root, run_id)?;
    Ok(json!({ "run_id": run_id, "outcome": outcome.as_str() }))
}

pub fn context_append(root: &Path, run_id: &str, note: &str) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::ContextAppend {
            note: note.to_string(),
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

/// Requests interruption of the run's currently RUNNING attempt (finding 7 of
/// issue #42, third item of issue #40). Posts `Control::Interrupt`; the
/// attempt's own poll loop observes it live, SIGKILLs the agent, and journals
/// `attempt_interrupted`. The killed attempt is journaled failed, so ordinary
/// retry/fallback/patch then proceeds at the next attempt boundary - the point
/// being a supervisor can now force the attempt boundary of a wedged attempt
/// (typically after a stall anomaly woke it) rather than waiting out a hang that
/// may never end. Unlike `run_abort` this does NOT stop the run. An interrupt
/// with no attempt running is a harmless no-op. The response reports
/// `posted_seq`; the resulting `control_received`/`attempt_interrupted` events
/// are visible via `supervisor_run_inspect` and `run_events`, so a supervisor
/// can confirm the message was received live.
pub fn interrupt_attempt(
    root: &Path,
    run_id: &str,
    reason: Option<&str>,
) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::Interrupt {
            reason: reason.unwrap_or("supervisor interrupt").to_string(),
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

/// Reports cycle progress for the run's currently executing node group. Posts
/// a `Control::Progress` command; drive stamps the node and appends the
/// `RunProgress` event (single-writer). Callable by the executing agent or the
/// supervisor.
pub fn run_progress_report(
    root: &Path,
    run_id: &str,
    done: u64,
    total: u64,
    label: Option<String>,
) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(root, run_id, Control::Progress { done, total, label })?;
    Ok(json!({ "posted_seq": seq }))
}

/// Creates a patch version of the playbook from patched YAML and posts a run
/// migration command. Writes no events - drive will write them when applying
/// `Control::Patch` (single-writer). The patch's base is the run's active version.
pub fn playbook_patch(
    root: &Path,
    run_id: &str,
    yaml: &str,
    classification: &str,
    continue_from: &str,
) -> Result<Value, ToolError> {
    if !matches!(classification, "improvement" | "workaround") {
        return Err(ToolError::Engine(format!(
            "invalid classification `{classification}`"
        )));
    }
    let (id, base_version) = apb_engine::scheduler::run_playbook_ref(root, run_id)?;
    let version = create_patch_version(root, &id, &base_version, yaml, run_id, classification)?;
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::Patch {
            version: version.clone(),
            classification: classification.to_string(),
            continue_from: continue_from.to_string(),
        },
    )?;
    Ok(json!({ "version": version, "posted_seq": seq }))
}

/// Answers a pending interactive question on a run (spec
/// 2026-07-20-interactive-nodes): writes a command into the run's
/// answers.jsonl channel via `apb_engine::post_answer`. `node` omitted
/// resolves to the single pending question. The `answer_by` policy (a node
/// declaring `answer_by: human` rejects `answered_by: "supervisor"`, with an
/// error instructing the supervisor to relay the question to the user) is
/// enforced inside `post_answer`, not here - every facade (this MCP tool,
/// `apb answer`, the web API) shares that one enforcement point, so it
/// cannot be bypassed by a facade that forgets to check it.
pub fn run_answer(
    root: &Path,
    run_id: &str,
    node: Option<&str>,
    answer: &str,
    answered_by: &str,
) -> Result<Value, ToolError> {
    let run_dir = resolve_run_dir(root, run_id)?;
    let seq = apb_engine::post_answer(&run_dir, node, answer, answered_by)?;
    Ok(json!({ "posted_seq": seq }))
}

/// A human_review node decision: writes a command into the run's reviews.jsonl channel.
/// A regular run tool (not supervised): takes run_id directly.
pub fn review_decide(
    root: &Path,
    run_id: &str,
    node: &str,
    decision: &str,
    note: &str,
) -> Result<Value, ToolError> {
    if !is_safe_segment(run_id) {
        return Err(ToolError::NotFound(run_id.to_string()));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(ToolError::NotFound(run_id.to_string()));
    }
    let seq = apb_engine::post_review(
        &run_dir,
        apb_engine::ReviewCommand {
            node: node.to_string(),
            decision: decision.to_string(),
            note: note.to_string(),
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

/// Writes the supervisor's final report to `runs/<run_id>/supervisor/report.md`.
pub fn supervisor_report(root: &Path, run_id: &str, text: &str) -> Result<Value, ToolError> {
    write_supervisor_report(root, run_id, text)?;
    Ok(json!({ "ok": true }))
}

/// Extracts the capability list from `playbook.supervisor.policy.capabilities`.
/// Distinguishes an absent key (default) from a present one (exact value):
/// - key absent -> default `["observe", "retry", "patch_playbook"]`
///   (all implemented capabilities, see spec 9.5: the default is all)
/// - key present as a sequence -> its strings (empty if empty)
/// - key present as a scalar string -> a single-element list
/// - key present as another type -> empty (deny all)
pub fn supervisor_capabilities(
    root: &Path,
    id: &str,
    version: Option<&str>,
) -> Result<Vec<String>, ToolError> {
    let reg = open(root)?;
    let loaded = reg.load(id, version)?;

    let caps = match loaded
        .playbook
        .supervisor
        .as_ref()
        .and_then(|s| s.policy.as_ref())
        .and_then(|p| p.get("capabilities"))
    {
        None => vec![
            "observe".to_string(),
            "retry".to_string(),
            "patch_playbook".to_string(),
        ],
        Some(v) if v.is_sequence() => v
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        Some(v) if v.as_str().is_some() => {
            vec![v.as_str().unwrap().to_string()]
        }
        Some(_) => Vec::new(),
    };

    // A frozen playbook cannot be patched, so never advertise `patch_playbook`:
    // the supervisor still observes and retries within the current run, but the
    // definition is off the table (enforced in core too, this just keeps the
    // advertised capability honest).
    let caps = if reg.is_frozen(id) {
        caps.into_iter().filter(|c| c != "patch_playbook").collect()
    } else {
        caps
    };

    Ok(caps)
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn run_progress_report_posts_a_command() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        // minimal events + playbook so resolve_run_dir + run_status succeed
        std::fs::write(
            run_dir.join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n",
        )
        .unwrap();
        let out = run_progress_report(tmp.path(), "r1", 2, 5, Some("x".into())).unwrap();
        assert!(out.get("posted_seq").is_some());
        let control = std::fs::read_to_string(run_dir.join("control.jsonl")).unwrap();
        assert!(control.contains("\"cmd\":\"progress\""));
    }

    #[test]
    fn run_report_includes_duration_table() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n").unwrap();
        std::fs::write(run_dir.join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n{\"seq\":1,\"ts\":1000,\"type\":\"node_started\",\"node\":\"a\",\"attempt\":1}\n{\"seq\":2,\"ts\":6000,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n").unwrap();
        let out = run_report(tmp.path(), "r1").unwrap();
        let table = out
            .get("duration_table")
            .and_then(|v| v.as_array())
            .unwrap();
        let a = table.iter().find(|e| e["node"] == "a").unwrap();
        assert_eq!(a["expected_seconds"], 100);
        assert_eq!(a["measured_seconds"], 5);
    }

    /// Issue #42 finding 3: `run_status` must expose the terminal error for a
    /// failed run directly, rather than making an operator open events.jsonl
    /// by hand to find the `run_error` event.
    #[test]
    fn run_status_exposes_failure_reason_for_a_failed_run() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":0,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":1000,"type":"node_started","node":"a","attempt":1}"#,
                "\n",
                r#"{"seq":2,"ts":2000,"type":"node_finished","node":"a","status":"failed","attempt":1,"output":"boom"}"#,
                "\n",
                r#"{"seq":3,"ts":2500,"type":"run_error","node":"a","reason":"node `a` has no outgoing edge and is not finish"}"#,
                "\n",
                r#"{"seq":4,"ts":3000,"type":"run_finished","outcome":"failed"}"#,
                "\n",
            ),
        )
        .unwrap();
        let out = run_status(tmp.path(), "r1").unwrap();
        assert_eq!(out["run_status"], "failed");
        let reason = out["failure_reason"]
            .as_str()
            .expect("failure_reason must be a string for a failed run with a recorded RunError");
        assert!(reason.contains("no outgoing edge"));
        assert!(reason.contains("node `a`"));
    }

    /// `failure_reason` stays absent (JSON `null`) for a run that is not
    /// failed - it must not appear on a succeeded/running/paused run.
    #[test]
    fn run_status_omits_failure_reason_for_a_succeeded_run() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("events.jsonl"),
            concat!(
                r#"{"seq":0,"ts":0,"type":"run_started","playbook":"p","version":"1.0.0"}"#,
                "\n",
                r#"{"seq":1,"ts":1000,"type":"run_finished","outcome":"succeeded"}"#,
                "\n",
            ),
        )
        .unwrap();
        let out = run_status(tmp.path(), "r1").unwrap();
        assert_eq!(out["run_status"], "succeeded");
        assert!(out["failure_reason"].is_null());
    }

    #[test]
    fn run_report_propagates_unreadable_events() {
        // B7: an unreadable/corrupt event log surfaces as an error, not an
        // empty duration table masquerading as "no measurements".
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n").unwrap();
        std::fs::write(run_dir.join("events.jsonl"), "this is not json\n").unwrap();
        let err = run_report(tmp.path(), "r1").unwrap_err();
        assert!(matches!(err, ToolError::Engine(_)), "got {err:?}");
    }
}
