//! Playbook minor-version machinery: the single point where the core emits
//! a version number (spec 10.2). Version folders are immutable; each change creates
//! a new version via atomic rename from a temporary folder.
//!
//! Note on YAML: `create_version` serializes the playbook back through
//! serde (canonical YAML), not preserving the original text verbatim, so
//! comments and formatting of the input YAML do not survive writing. For
//! the editor this is expected (source of truth - model), but hand edits with
//! comments will lose formatting.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::fsutil::atomic_write;
use crate::registry::{Registry, is_frozen_dir, is_safe_segment};
use crate::schema::{Playbook, SchemaError};
use crate::validate::{Issue, Severity, ValidationContext, validate};

const MAX_RENAME_ATTEMPTS: u32 = 100;

#[derive(Debug, thiserror::Error)]
pub enum VersioningError {
    #[error("playbook `{0}` not found")]
    NotFound(String),
    #[error("{}", crate::validate::render_issues(.0))]
    Validation(Vec<Issue>),
    #[error("schema error: {0}")]
    Schema(String),
    #[error("version conflict: {0}")]
    Conflict(String),
    #[error("playbook `{0}` is frozen: definition changes are disabled")]
    Frozen(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct VersionProvenance {
    pub created_by: String,
    pub run_id: Option<String>,
    pub classification: Option<String>,
    pub promoted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotePolicy {
    OnSuccess,
    AfterNSuccesses(u32),
    Manual,
    Always,
}

fn advance_minor_pair(major: u32, minor: u32) -> Result<(u32, u32), ()> {
    match minor.checked_add(1) {
        Some(next_minor) => Ok((major, next_minor)),
        None => major
            .checked_add(1)
            .map(|next_major| (next_major, 0))
            .ok_or(()),
    }
}

/// Next available minor version from `base` = `X.Y.Z`: candidate `X.(Y+1).0`,
/// if collision with `existing` minor, increment, patch reset to 0.
/// Invalid `base` (not three numeric segments) yields safe default `1.0.0`.
/// If minor is exhausted, major is incremented (explicit safeguard against looping on `u32::MAX`).
pub fn next_minor_version(base: &str, existing: &[String]) -> String {
    let Some((mut major, minor, _)) = parse_version_triple(base) else {
        return "1.0.0".to_string();
    };
    let taken: HashSet<&str> = existing.iter().map(String::as_str).collect();
    let Ok((next_major, mut next_minor)) = advance_minor_pair(major, minor) else {
        return format!("{major}.{minor}.0");
    };
    major = next_major;
    loop {
        let candidate = format!("{major}.{next_minor}.0");
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
        let Ok((next_major, next)) = advance_minor_pair(major, next_minor) else {
            return candidate;
        };
        major = next_major;
        next_minor = next;
    }
}

/// Next available patch version from `base` = `X.Y.Z`: candidate `X.Y.(Z+1)`.
/// Invalid `base` yields safe default `1.0.0`; create_patch_version
/// separately rejects such base before creating the version.
pub fn next_patch_version(base: &str, existing: &[String]) -> String {
    let Some((major, minor, patch)) = parse_version_triple(base) else {
        return "1.0.0".to_string();
    };
    let taken: HashSet<&str> = existing.iter().map(String::as_str).collect();
    let Some(mut next_patch) = patch.checked_add(1) else {
        return next_minor_version(base, existing);
    };

    loop {
        let candidate = format!("{major}.{minor}.{next_patch}");
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
        let Some(next) = next_patch.checked_add(1) else {
            return next_minor_version(base, existing);
        };
        next_patch = next;
    }
}

/// Creates a new immutable minor version of a playbook: validation, temp build, atomic rename.
pub fn create_version(
    root: &Path,
    id: &str,
    new_yaml: &str,
    base_version: Option<&str>,
    make_current: bool,
) -> Result<String, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }

    let playbook_dir = playbooks_dir(root).join(id);
    let is_new = !playbook_dir.is_dir();

    // A frozen playbook refuses every new version. A brand-new playbook cannot
    // be frozen (it does not exist yet), so only guard existing ones.
    if !is_new && is_frozen_dir(&playbook_dir) {
        return Err(VersioningError::Frozen(id.to_string()));
    }

    let base = if is_new {
        None
    } else if let Some(v) = base_version {
        if !is_safe_segment(v) {
            return Err(VersioningError::NotFound(format!("{id}@{v}")));
        }
        Some(v.to_string())
    } else {
        Some(read_current(&playbook_dir)?)
    };

    let existing = if is_new {
        Vec::new()
    } else {
        list_version_dirs(&playbook_dir)?
    };

    let mut version = if is_new {
        "1.0.0".to_string()
    } else {
        next_minor_version(base.as_deref().unwrap_or("1.0.0"), &existing)
    };

    let mut playbook = Playbook::from_yaml(new_yaml).map_err(schema_err)?;
    playbook.version = version.clone();
    playbook.id = id.to_string();

    validate_playbook(root, &playbook)?;

    if is_new {
        fs::create_dir_all(&playbook_dir)?;
    }

    let version_path = commit_version_dir(
        &playbook_dir,
        &version,
        &playbook,
        base.as_deref(),
        VersionBump::Minor,
    )?;
    version = version_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    if let Some(base_ver) = base.as_deref() {
        copy_parent_layout(&playbook_dir, base_ver, &version)?;
    }

    write_provenance(
        root,
        id,
        &version,
        &VersionProvenance {
            created_by: "user".to_string(),
            run_id: None,
            classification: None,
            promoted: true,
        },
    )?;

    if is_new || make_current {
        atomic_write(&playbook_dir.join("current"), version.as_bytes())?;
    }

    Ok(version)
}

/// Creates immutable patch version from base version without changing current.
pub fn create_patch_version(
    root: &Path,
    id: &str,
    base_version: &str,
    new_yaml: &str,
    run_id: &str,
    classification: &str,
) -> Result<String, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    if !is_safe_segment(base_version) {
        return Err(VersioningError::NotFound(format!("{id}@{base_version}")));
    }
    if !matches!(classification, "improvement" | "workaround") {
        return Err(VersioningError::Validation(vec![Issue {
            code: "classification",
            severity: Severity::Error,
            message: format!(
                "classification `{classification}` must be `improvement` or `workaround`"
            ),
            node: None,
        }]));
    }
    if parse_version_triple(base_version).is_none() {
        return Err(VersioningError::Conflict(format!(
            "invalid version `{base_version}`"
        )));
    }

    let playbook_dir = playbooks_dir(root).join(id);
    if !playbook_dir.is_dir() || !playbook_dir.join(base_version).is_dir() {
        return Err(VersioningError::NotFound(format!("{id}@{base_version}")));
    }
    // Frozen playbooks reject supervisor patches too: the supervisor may only
    // intervene within the current run, never fork the definition.
    if is_frozen_dir(&playbook_dir) {
        return Err(VersioningError::Frozen(id.to_string()));
    }

    let existing = list_version_dirs(&playbook_dir)?;
    let version = next_patch_version(base_version, &existing);
    let mut playbook = Playbook::from_yaml(new_yaml).map_err(schema_err)?;
    playbook.version = version.clone();
    playbook.id = id.to_string();
    validate_playbook(root, &playbook)?;

    let version_path = commit_version_dir(
        &playbook_dir,
        &version,
        &playbook,
        Some(base_version),
        VersionBump::Patch,
    )?;
    let version = version_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    copy_parent_layout(&playbook_dir, base_version, &version)?;
    write_provenance(
        root,
        id,
        &version,
        &VersionProvenance {
            created_by: "supervisor".to_string(),
            run_id: Some(run_id.to_string()),
            classification: Some(classification.to_string()),
            promoted: false,
        },
    )?;

    Ok(version)
}

/// Writes mutable provenance of version outside the immutable version folder.
pub fn write_provenance(
    root: &Path,
    id: &str,
    version: &str,
    provenance: &VersionProvenance,
) -> Result<(), VersioningError> {
    let path = provenance_path(root, id, version)?;
    let yaml =
        serde_yaml_ng::to_string(provenance).map_err(|e| VersioningError::Schema(e.to_string()))?;
    atomic_write(&path, yaml.as_bytes())?;
    Ok(())
}

/// Returns provenance of version if sidecar already exists.
pub fn read_provenance(
    root: &Path,
    id: &str,
    version: &str,
) -> Result<Option<VersionProvenance>, VersioningError> {
    let path = provenance_path(root, id, version)?;
    if !path.is_file() {
        return Ok(None);
    }
    let yaml = fs::read_to_string(path)?;
    let provenance =
        serde_yaml_ng::from_str(&yaml).map_err(|e| VersioningError::Schema(e.to_string()))?;
    Ok(Some(provenance))
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionInfo {
    pub version: String,
    pub is_current: bool,
    pub provenance: Option<VersionProvenance>,
}

/// Lists playbook versions with provenance and current marker.
/// Order matches `list_version_dirs` (lexicographic by folder name).
pub fn list_versions_with_provenance(
    root: &Path,
    id: &str,
) -> Result<Vec<VersionInfo>, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    let playbook_dir = root.join(".apb/playbooks").join(id);
    if !playbook_dir.is_dir() {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    let current = fs::read_to_string(playbook_dir.join("current"))
        .ok()
        .map(|s| s.trim().to_string());
    let mut out = Vec::new();
    for version in list_version_dirs(&playbook_dir)? {
        let is_current = current.as_deref() == Some(version.as_str());
        let provenance = read_provenance(root, id, &version)?;
        out.push(VersionInfo {
            version,
            is_current,
            provenance,
        });
    }
    Ok(out)
}

/// Changes the promote flag in mutable sidecar of a known version.
pub fn set_promoted(
    root: &Path,
    id: &str,
    version: &str,
    promoted: bool,
) -> Result<(), VersioningError> {
    let mut provenance = read_provenance(root, id, version)?
        .ok_or_else(|| VersioningError::NotFound(format!("{id}@{version}")))?;
    provenance.promoted = promoted;
    write_provenance(root, id, version, &provenance)
}

pub fn promote_version(root: &Path, id: &str, version: &str) -> Result<(), VersioningError> {
    let playbook_dir = playbook_version_dir(root, id, version)?;
    // Promotion repoints `current`, a definition change - refused while frozen.
    if is_frozen_dir(&playbooks_dir(root).join(id)) {
        return Err(VersioningError::Frozen(id.to_string()));
    }
    set_promoted(root, id, version, true)?;
    atomic_write(&playbook_dir.join("current"), version.as_bytes())?;
    Ok(())
}

/// Reads supervisor patch promotion policy from playbook description.
pub fn promote_policy(playbook: &Playbook) -> PromotePolicy {
    let Some(policy) = playbook
        .supervisor
        .as_ref()
        .and_then(|supervisor| supervisor.policy.as_ref())
    else {
        return PromotePolicy::OnSuccess;
    };
    let Ok(value) = serde_json::to_value(policy) else {
        return PromotePolicy::OnSuccess;
    };
    let Some(promotion) = value.get("promote_supervisor_patches") else {
        return PromotePolicy::OnSuccess;
    };

    match promotion {
        serde_json::Value::String(value) => match value.as_str() {
            "manual" => PromotePolicy::Manual,
            "always" => PromotePolicy::Always,
            "on_success" => PromotePolicy::OnSuccess,
            _ => PromotePolicy::OnSuccess,
        },
        serde_json::Value::Object(values) => values
            .get("after_n_successes")
            .and_then(serde_json::Value::as_u64)
            .and_then(|count| u32::try_from(count).ok())
            .map(PromotePolicy::AfterNSuccesses)
            .unwrap_or(PromotePolicy::OnSuccess),
        _ => PromotePolicy::OnSuccess,
    }
}

pub fn should_promote(
    policy: PromotePolicy,
    classification: &str,
    run_succeeded: bool,
    changed_nodes_succeeded: bool,
    prior_successes: u32,
) -> bool {
    if classification == "workaround" {
        return false;
    }

    match policy {
        PromotePolicy::Manual => false,
        PromotePolicy::Always => true,
        PromotePolicy::OnSuccess => run_succeeded && changed_nodes_succeeded,
        PromotePolicy::AfterNSuccesses(required) => {
            run_succeeded
                && changed_nodes_succeeded
                && prior_successes.saturating_add(1) >= required
        }
    }
}

/// Soft deletion of playbook: moves `.apb/playbooks/<id>` to
/// `.apb/trash/<id>-<ts_millis>`. Runs are not affected.
pub fn delete_playbook(root: &Path, id: &str, ts_millis: u128) -> Result<PathBuf, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }

    let src = playbooks_dir(root).join(id);
    if !src.is_dir() {
        return Err(VersioningError::NotFound(id.to_string()));
    }

    let trash = trash_dir(root);
    fs::create_dir_all(&trash)?;

    let trash_name = format!("{id}-{ts_millis}");
    let dst = trash.join(&trash_name);
    if dst.exists() {
        return Err(VersioningError::Conflict(format!(
            "trash entry `{trash_name}` already exists"
        )));
    }

    fs::rename(&src, &dst)?;
    Ok(dst)
}

/// Names of folders in `.apb/trash/`. If trash directory doesn't exist - empty list.
pub fn list_trash(root: &Path) -> Result<Vec<String>, VersioningError> {
    let trash = trash_dir(root);
    if !trash.is_dir() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in fs::read_dir(&trash)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// Saves canvas layout for version. Layout is mutable: overwriting
/// existing `layouts/<version>.yaml` is allowed (unlike version folders).
pub fn save_layout(
    root: &Path,
    id: &str,
    version: &str,
    layout_yaml: &str,
) -> Result<(), VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    if !is_safe_segment(version) {
        return Err(VersioningError::NotFound(format!("{id}@{version}")));
    }

    let playbook_dir = playbooks_dir(root).join(id);
    if !playbook_dir.is_dir() {
        return Err(VersioningError::NotFound(id.to_string()));
    }

    let _: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(layout_yaml).map_err(|e| VersioningError::Schema(e.to_string()))?;

    let path = playbook_dir.join("layouts").join(format!("{version}.yaml"));
    atomic_write(&path, layout_yaml.as_bytes())?;
    Ok(())
}

/// Structural and textual diff of two versions of the same playbook.
#[derive(Debug, Serialize)]
pub struct VersionDiff {
    pub nodes_added: Vec<String>,
    pub nodes_removed: Vec<String>,
    pub nodes_changed: Vec<String>,
    pub edges_added: Vec<String>,
    pub edges_removed: Vec<String>,
    pub yaml_diff: String,
}

/// Compares two versions: structurally (nodes/edges) and line by line (YAML).
pub fn version_diff(
    root: &Path,
    id: &str,
    from: &str,
    to: &str,
) -> Result<VersionDiff, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    if !is_safe_segment(from) {
        return Err(VersioningError::NotFound(format!("{id}@{from}")));
    }
    if !is_safe_segment(to) {
        return Err(VersioningError::NotFound(format!("{id}@{to}")));
    }

    let base = playbooks_dir(root).join(id);
    if !base.is_dir() {
        return Err(VersioningError::NotFound(id.to_string()));
    }

    let from_yaml = read_version_yaml(&base, id, from)?;
    let to_yaml = read_version_yaml(&base, id, to)?;

    let from_playbook = Playbook::from_yaml(&from_yaml).map_err(schema_err)?;
    let to_playbook = Playbook::from_yaml(&to_yaml).map_err(schema_err)?;

    let from_nodes: HashMap<&str, &crate::schema::Node> = from_playbook
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();
    let to_nodes: HashMap<&str, &crate::schema::Node> = to_playbook
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();

    let mut nodes_added = Vec::new();
    let mut nodes_removed = Vec::new();
    let mut nodes_changed = Vec::new();

    for node_id in to_nodes.keys() {
        if !from_nodes.contains_key(node_id) {
            nodes_added.push((*node_id).to_string());
        }
    }
    for node_id in from_nodes.keys() {
        if !to_nodes.contains_key(node_id) {
            nodes_removed.push((*node_id).to_string());
        }
    }
    for (node_id, from_node) in &from_nodes {
        if let Some(to_node) = to_nodes.get(node_id) {
            let from_ser = serde_json::to_value(from_node)
                .map_err(|e| VersioningError::Schema(e.to_string()))?;
            let to_ser = serde_json::to_value(to_node)
                .map_err(|e| VersioningError::Schema(e.to_string()))?;
            if from_ser != to_ser {
                nodes_changed.push((*node_id).to_string());
            }
        }
    }
    nodes_added.sort();
    nodes_removed.sort();
    nodes_changed.sort();

    let from_edges: HashSet<String> = from_playbook
        .edges
        .iter()
        .map(|e| format!("{}->{}", e.from, e.to))
        .collect();
    let to_edges: HashSet<String> = to_playbook
        .edges
        .iter()
        .map(|e| format!("{}->{}", e.from, e.to))
        .collect();

    let mut edges_added: Vec<String> = to_edges.difference(&from_edges).cloned().collect();
    let mut edges_removed: Vec<String> = from_edges.difference(&to_edges).cloned().collect();
    edges_added.sort();
    edges_removed.sort();

    let yaml_diff = line_diff(&from_yaml, &to_yaml);

    Ok(VersionDiff {
        nodes_added,
        nodes_removed,
        nodes_changed,
        edges_added,
        edges_removed,
        yaml_diff,
    })
}

fn read_version_yaml(base: &Path, id: &str, version: &str) -> Result<String, VersioningError> {
    let path = base.join(version).join("playbook.yaml");
    if !path.is_file() {
        return Err(VersioningError::NotFound(format!("{id}@{version}")));
    }
    Ok(fs::read_to_string(path)?)
}

/// Line-by-line unified-like diff via LCS (without external crates).
fn line_diff(from: &str, to: &str) -> String {
    let a: Vec<&str> = from.lines().collect();
    let b: Vec<&str> = to.lines().collect();
    let n = a.len();
    let m = b.len();

    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            if a[i] == b[j] {
                dp[i][j] = dp[i + 1][j + 1] + 1;
            } else {
                dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    let mut out = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(format!(" {}", a[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(format!("-{}", a[i]));
            i += 1;
        } else {
            out.push(format!("+{}", b[j]));
            j += 1;
        }
    }
    while i < n {
        out.push(format!("-{}", a[i]));
        i += 1;
    }
    while j < m {
        out.push(format!("+{}", b[j]));
        j += 1;
    }
    out.join("\n")
}

/// Restores playbook from trash: `<id>-<ts>` -> `.apb/playbooks/<id>`.
/// If `<id>` already exists - `Conflict`.
pub fn restore_playbook(root: &Path, trash_name: &str) -> Result<String, VersioningError> {
    if !is_safe_segment(trash_name) {
        return Err(VersioningError::NotFound(trash_name.to_string()));
    }

    let id = id_from_trash_name(trash_name)
        .ok_or_else(|| VersioningError::NotFound(trash_name.to_string()))?;

    let src = trash_dir(root).join(trash_name);
    if !src.is_dir() {
        return Err(VersioningError::NotFound(trash_name.to_string()));
    }

    let dst = playbooks_dir(root).join(&id);
    if dst.exists() {
        return Err(VersioningError::Conflict(format!(
            "playbook `{id}` already exists"
        )));
    }

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&src, &dst)?;
    Ok(id)
}

fn playbooks_dir(root: &Path) -> PathBuf {
    root.join(".apb/playbooks")
}

/// Creates draft playbook version 1.0.0 directly in the parent directory
/// (`<parent>/playbooks/`), without binding to the project root. Point for capturing in
/// chosen scope (spec 8.3): project `.apb` or global config dir.
/// Error `Conflict` if id already exists in this scope (dedup before write).
pub fn create_draft_in(
    parent: &Path,
    id: &str,
    yaml: &str,
    origin: crate::profile_store::PlaybookOrigin,
) -> Result<String, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    fs::create_dir_all(parent.join("playbooks"))?;
    let playbook_dir = parent.join("playbooks").join(id);
    let mut playbook = Playbook::from_yaml(yaml).map_err(schema_err)?;
    playbook.version = "1.0.0".to_string();
    playbook.id = id.to_string();

    let reg = Registry::open_dir(parent).map_err(|e| VersioningError::NotFound(e.to_string()))?;
    // Origin is passed explicitly: global draft with `scope: project` should
    // fail V14 immediately, not only at runtime.
    let ctx = ValidationContext {
        profiles: reg.profiles(),
        playbook_origin: origin,
    };
    let report = validate(&playbook, &ctx);
    let errors: Vec<Issue> = report
        .issues
        .into_iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    if !errors.is_empty() {
        return Err(VersioningError::Validation(errors));
    }

    // Atomic collision check: create the playbook folder non-recursively -
    // if it already exists, it's a duplicate (no TOCTOU between exists() and write).
    match fs::create_dir(&playbook_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(VersioningError::Conflict(format!(
                "playbook `{id}` already exists in this scope"
            )));
        }
        Err(e) => return Err(e.into()),
    }

    let canonical =
        serde_yaml_ng::to_string(&playbook).map_err(|e| VersioningError::Schema(e.to_string()))?;
    let vdir = playbook_dir.join("1.0.0");
    fs::create_dir_all(&vdir)?;
    atomic_write(&vdir.join("playbook.yaml"), canonical.as_bytes())?;
    atomic_write(&playbook_dir.join("current"), b"1.0.0")?;
    Ok("1.0.0".to_string())
}

fn provenance_path(root: &Path, id: &str, version: &str) -> Result<PathBuf, VersioningError> {
    let playbook_dir = playbook_version_dir(root, id, version)?;
    Ok(playbook_dir.join("meta").join(format!("{version}.yaml")))
}

fn playbook_version_dir(root: &Path, id: &str, version: &str) -> Result<PathBuf, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    if !is_safe_segment(version) {
        return Err(VersioningError::NotFound(format!("{id}@{version}")));
    }

    let playbook_dir = playbooks_dir(root).join(id);
    if !playbook_dir.is_dir() || !playbook_dir.join(version).is_dir() {
        return Err(VersioningError::NotFound(format!("{id}@{version}")));
    }
    Ok(playbook_dir)
}

fn trash_dir(root: &Path) -> PathBuf {
    root.join(".apb/trash")
}

/// Extracts `id` from trash folder name `<id>-<ts_millis>` (part before last `-`).
fn id_from_trash_name(trash_name: &str) -> Option<String> {
    let (id, ts) = trash_name.rsplit_once('-')?;
    if id.is_empty() || ts.is_empty() {
        return None;
    }
    if !ts.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !is_safe_segment(id) {
        return None;
    }
    Some(id.to_string())
}

fn schema_err(e: SchemaError) -> VersioningError {
    VersioningError::Schema(e.to_string())
}

fn validate_playbook(root: &Path, playbook: &Playbook) -> Result<(), VersioningError> {
    let reg = Registry::open(root).map_err(|e| VersioningError::NotFound(e.to_string()))?;
    let ctx = ValidationContext {
        profiles: reg.profiles(),
        ..Default::default()
    };
    let report = validate(playbook, &ctx);
    if report.is_valid() {
        return Ok(());
    }
    let issues: Vec<Issue> = report
        .issues
        .into_iter()
        .filter(|issue| issue.severity == Severity::Error)
        .collect();
    Err(VersioningError::Validation(issues))
}

fn parse_version_triple(s: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let major = parts[0].parse().ok()?;
    let minor = parts[1].parse().ok()?;
    let patch = parts[2].parse().ok()?;
    Some((major, minor, patch))
}

fn bump_minor(version: &str) -> Result<String, VersioningError> {
    let (major, minor, _) = parse_version_triple(version)
        .ok_or_else(|| VersioningError::Conflict(format!("invalid version `{version}`")))?;
    let (next_major, next_minor) = advance_minor_pair(major, minor)
        .map_err(|()| VersioningError::Conflict(format!("version overflow for `{version}`")))?;
    Ok(format!("{next_major}.{next_minor}.0"))
}

fn bump_patch(version: &str) -> Result<String, VersioningError> {
    let (major, minor, patch) = parse_version_triple(version)
        .ok_or_else(|| VersioningError::Conflict(format!("invalid version `{version}`")))?;
    let next_patch = patch
        .checked_add(1)
        .ok_or_else(|| VersioningError::Conflict(format!("version overflow for `{version}`")))?;
    Ok(format!("{major}.{minor}.{next_patch}"))
}

fn read_current(playbook_dir: &Path) -> Result<String, VersioningError> {
    let path = playbook_dir.join("current");
    if !path.is_file() {
        return Err(VersioningError::NotFound(
            playbook_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        ));
    }
    let current = fs::read_to_string(path)?.trim().to_string();
    if !is_safe_segment(&current) {
        return Err(VersioningError::NotFound(
            playbook_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        ));
    }
    Ok(current)
}

fn list_version_dirs(playbook_dir: &Path) -> Result<Vec<String>, VersioningError> {
    let mut out = Vec::new();
    for entry in fs::read_dir(playbook_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "layouts" || name == "meta" || name.starts_with(".tmp-") {
            continue;
        }
        out.push(name);
    }
    out.sort();
    Ok(out)
}

fn temp_dir_name(version: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(".tmp-{version}-{nanos}")
}

fn playbook_yaml_for_version(
    playbook: &Playbook,
    version: &str,
) -> Result<String, VersioningError> {
    let mut playbook = playbook.clone();
    playbook.version = version.to_string();
    serde_yaml_ng::to_string(&playbook).map_err(|e| VersioningError::Schema(e.to_string()))
}

#[derive(Clone, Copy)]
enum VersionBump {
    Minor,
    Patch,
}

fn commit_version_dir(
    playbook_dir: &Path,
    initial_version: &str,
    playbook: &Playbook,
    base_version: Option<&str>,
    bump: VersionBump,
) -> Result<PathBuf, VersioningError> {
    let mut version = initial_version.to_string();

    for _ in 0..MAX_RENAME_ATTEMPTS {
        let validated_yaml = playbook_yaml_for_version(playbook, &version)?;
        let tmp = playbook_dir.join(temp_dir_name(&version));
        if tmp.exists() {
            remove_dir_all(&tmp)?;
        }
        fs::create_dir_all(tmp.join("scripts"))?;
        atomic_write(&tmp.join("playbook.yaml"), validated_yaml.as_bytes())?;

        if let Some(base) = base_version {
            let scripts_src = playbook_dir.join(base).join("scripts");
            if scripts_src.is_dir() {
                copy_dir_recursive(&scripts_src, &tmp.join("scripts"))?;
            }
        }

        let target = playbook_dir.join(&version);
        match fs::rename(&tmp, &target) {
            Ok(()) => return Ok(target),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                remove_dir_all(&tmp)?;
                version = match bump {
                    VersionBump::Minor => bump_minor(&version)?,
                    VersionBump::Patch => bump_patch(&version)?,
                };
                continue;
            }
            Err(e) => {
                let _ = remove_dir_all(&tmp);
                return Err(e.into());
            }
        }
    }

    Err(VersioningError::Conflict(format!(
        "failed to allocate version after {MAX_RENAME_ATTEMPTS} attempts"
    )))
}

fn copy_parent_layout(
    playbook_dir: &Path,
    base_version: &str,
    new_version: &str,
) -> Result<(), VersioningError> {
    let src = playbook_dir
        .join("layouts")
        .join(format!("{base_version}.yaml"));
    if !src.is_file() {
        return Ok(());
    }
    let content = fs::read_to_string(&src)?;
    let dst = playbook_dir
        .join("layouts")
        .join(format!("{new_version}.yaml"));
    atomic_write(&dst, content.as_bytes())?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn remove_dir_all(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const VALID: &str = include_str!("../tests/fixtures/valid.yaml");

    #[test]
    fn commit_version_dir_rewrites_yaml_on_collision_bump() {
        let dir = tempfile::tempdir().unwrap();
        let playbook_dir = dir.path().join("apb");
        fs::create_dir_all(playbook_dir.join("1.0.0/scripts")).unwrap();
        fs::create_dir_all(playbook_dir.join("1.1.0")).unwrap();
        fs::write(playbook_dir.join("1.1.0/occupant"), "taken").unwrap();

        let mut playbook = Playbook::from_yaml(VALID).unwrap();
        playbook.id = "implement-task".into();
        playbook.version = "1.1.0".into();

        let path = commit_version_dir(
            &playbook_dir,
            "1.1.0",
            &playbook,
            Some("1.0.0"),
            VersionBump::Minor,
        )
        .unwrap();
        assert_eq!(path.file_name().unwrap().to_string_lossy(), "1.2.0");

        let yaml = fs::read_to_string(path.join("playbook.yaml")).unwrap();
        assert!(yaml.contains("version: 1.2.0"));
        assert!(!yaml.contains("version: 1.1.0"));
    }

    #[test]
    fn frozen_playbook_refuses_new_versions_and_patches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        crate::registry::init_project(root).unwrap();

        // A first version exists (this playbook is now an existing one).
        create_version(root, "implement-task", VALID, None, true).unwrap();
        let pb_dir = playbooks_dir(root).join("implement-task");
        assert!(pb_dir.join("1.0.0").is_dir());

        // Freeze it.
        fs::write(pb_dir.join(crate::registry::FROZEN_MARKER), "1").unwrap();

        // A new minor version is refused.
        let e1 = create_version(root, "implement-task", VALID, None, true).unwrap_err();
        assert!(matches!(e1, VersioningError::Frozen(_)), "got {e1:?}");

        // A supervisor patch is refused too.
        let e2 = create_patch_version(root, "implement-task", "1.0.0", VALID, "run1", "workaround")
            .unwrap_err();
        assert!(matches!(e2, VersioningError::Frozen(_)), "got {e2:?}");

        // Promotion (repoints current) is refused too.
        let e3 = promote_version(root, "implement-task", "1.0.0").unwrap_err();
        assert!(matches!(e3, VersioningError::Frozen(_)), "got {e3:?}");

        // Unfreezing lets a new version through again.
        fs::remove_file(pb_dir.join(crate::registry::FROZEN_MARKER)).unwrap();
        create_version(root, "implement-task", VALID, None, true).unwrap();
    }
}
