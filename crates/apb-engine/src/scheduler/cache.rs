//! Node cache lookup and admission (spec 2026-07-19-node-cache-design).
//!
//! Wraps `execute_node` in the drive loop: [`prepare`] builds a per-node
//! context (returning `None` for anything not cache-eligible), `lookup`
//! probes the store, and `admit` runs the post-hoc workspace verification and
//! stores a clean result. Every failure here degrades to a miss or a
//! rejection, never a run error, so the cache can never fail a run.
use super::*;
use apb_core::cache::{
    ArtifactRef, ArtifactScope, CacheRecord, CacheStore, CachedEntry, KeyParts, Provenance,
    Verification, cache_key,
};
use apb_core::content::sha256_hex;
use apb_core::fingerprint::{files_fingerprint, git_fingerprint};
use apb_core::fsutil::atomic_write;
use apb_core::schema::{CacheMode, Node};
use apb_core::validate::build_globset;
use std::collections::HashMap;
use std::path::Component;

/// Everything the drive loop needs to look up and later admit one node's
/// cached result. Built once, before execution, so the pre-execution
/// workspace fingerprint is captured before the node can mutate anything.
pub(crate) struct NodeCacheCtx {
    /// The content-addressed cache key for this node execution.
    pub key: String,
    store: CacheStore,
    ttl: Option<u64>,
    /// Workspace fingerprint captured in `prepare`, before the node ran.
    pre_fingerprint: String,
    node_id: String,
    /// Cache record `node_type`: `"script"` or `"agent_task"`.
    node_type: &'static str,
    bundle_digest: Option<String>,
    /// Declared output globs, excluded from the post-execution fingerprint so
    /// a node's own products never count as an unexpected workspace change.
    exclude: Vec<String>,
}

/// Builds the cache context for `node_id`, or `None` when the node is not
/// cache-eligible: the run disables caching, the node does not declare
/// `cache: auto`, the node is neither a script nor an agent_task, or no
/// workspace fingerprint can be computed (for example, not a git work tree).
/// Any of these paths simply skips the cache for this node.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare(
    playbook: &Playbook,
    node_id: &str,
    workdir: &Path,
    run_dir: &Path,
    cfg: &RunConfig,
    rendered_prompt: Option<&str>,
    bundle_digest: Option<&str>,
    agent_model: Option<(&str, &str)>,
    connector_digests: Vec<String>,
) -> Option<NodeCacheCtx> {
    if cfg.cache == CacheRunMode::Off {
        return None;
    }
    let node = playbook.node(node_id)?;
    if node.cache_mode() != CacheMode::Auto {
        return None;
    }
    // Script and agent_task nodes participate; every other kind skips the
    // cache. A script folds in its script digest + runner; an agent_task folds
    // in the caller-supplied rendered prompt + bundle digest + agent/model +
    // connector digests instead (all None/empty for a script). The script body
    // is resolved from the run snapshot exactly as `run_script` does: the
    // node's `script` value already carries the `scripts/` prefix, so it joins
    // directly onto `run_dir` (no extra `scripts` segment).
    let (script_digest, runner, node_type): (Option<String>, Option<String>, &'static str) =
        match &node.kind {
            NodeKind::Script { script, runner, .. } => {
                let script_bytes = std::fs::read(run_dir.join(script)).ok()?;
                (
                    Some(sha256_hex(&script_bytes)),
                    Some(runner.clone()),
                    "script",
                )
            }
            NodeKind::AgentTask { .. } => (None, None, "agent_task"),
            _ => return None,
        };

    let exclude = node
        .outputs
        .as_ref()
        .map(|o| o.files.clone())
        .unwrap_or_default();
    let fingerprint = match node.inputs.as_ref() {
        Some(inp) if !inp.files.is_empty() => files_fingerprint(workdir, &inp.files, &[]).ok()?,
        _ => git_fingerprint(workdir, &[])?,
    };

    let node_def = serde_json::to_string(node).ok()?;
    let key = cache_key(&KeyParts {
        format: apb_core::cache::CACHE_FORMAT,
        node_def: &node_def,
        script_digest: script_digest.as_deref(),
        runner: runner.as_deref(),
        rendered_prompt,
        bundle_digest,
        agent: agent_model.map(|(a, _)| a),
        model: agent_model.map(|(_, m)| m),
        connector_digests,
        workspace_fingerprint: &fingerprint,
    });

    Some(NodeCacheCtx {
        key,
        store: CacheStore::open(workdir),
        ttl: node.cache_ttl_seconds(),
        pre_fingerprint: fingerprint,
        node_id: node_id.to_string(),
        node_type,
        bundle_digest: bundle_digest.map(str::to_string),
        exclude,
    })
}

/// The agent_task key parts read from the run's immutable manifest snapshot:
/// the profile bundle digest, the primary executor's agent and model, and the
/// node's connector digests (deduped and sorted). All come from the run's own
/// snapshot, never live profile/connector files (anti-TOCTOU: the manifest is
/// the truth for the run). `None` when there is no manifest or the node has no
/// profile binding, which simply skips the cache for the node.
pub(crate) fn agent_key_parts(
    run_dir: &Path,
    node_id: &str,
) -> Option<(String, String, String, Vec<String>)> {
    let manifest = crate::manifest::read(run_dir).ok().flatten()?;
    let entry = manifest.for_node(node_id)?;
    let primary = entry.chain.first()?;
    let bundle = entry.bundle_digest.clone();
    let agent = primary.agent_id.clone();
    let model = primary.model.clone();
    let mut digests: Vec<String> = manifest
        .grants_for(node_id)
        .iter()
        .filter_map(|g| manifest.connector(&g.connector).map(|c| c.digest.clone()))
        .collect();
    digests.sort();
    digests.dedup();
    Some((bundle, agent, model, digests))
}

/// Classifies the connector calls a just-executed node actually made, reading
/// the run's event log and the run's connector snapshot. Returns
/// `(connector_calls_ok, had_connector_calls)`: `ok` is true only when every
/// reached `ConnectorCall` names a `read_only` function of its connector; an
/// unknown connector or an unreadable/unparsable snapshot counts as NOT ok
/// (fail closed). Only calls since the node's most recent `NodeStarted` are
/// considered, so a resume never re-judges a prior execution's calls. No calls
/// yields `(true, false)`, matching a script node (which makes none).
pub(crate) fn verify_connector_calls(run_dir: &Path, node_id: &str) -> (bool, bool) {
    // The connector-call subprocess appends `ConnectorCall` events straight to
    // events.jsonl (out of band from `execute_node`'s returned events), so the
    // run log - not the returned `evs` - is the source of truth here.
    let events = match crate::event::read_all(run_dir) {
        Ok(e) => e,
        // Cannot read the log to verify: fail closed (no store), never a run error.
        Err(_) => return (false, false),
    };
    let from = events
        .iter()
        .rposition(
            |e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == node_id),
        )
        .map(|i| i + 1)
        .unwrap_or(0);
    let calls: Vec<(&str, &str)> = events[from..]
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ConnectorCall {
                node_id: n,
                connector,
                function,
                ..
            } if n == node_id => Some((connector.as_str(), function.as_str())),
            _ => None,
        })
        .collect();
    if calls.is_empty() {
        return (true, false);
    }
    let mut read_only: HashMap<&str, Option<Vec<String>>> = HashMap::new();
    let mut ok = true;
    for (connector, function) in &calls {
        let set = read_only
            .entry(connector)
            .or_insert_with(|| load_read_only(run_dir, connector));
        let is_read_only = set
            .as_ref()
            .is_some_and(|fns| fns.iter().any(|f| f == function));
        if !is_read_only {
            ok = false;
        }
    }
    (ok, true)
}

/// Loads the `read_only` function set for `connector` from the run snapshot
/// (`run_dir/connectors/<name>.yaml`), the very file `connector_call` reads
/// during execution. `None` on a missing or unparsable snapshot (an unknown
/// connector), so the caller fails closed.
fn load_read_only(run_dir: &Path, connector: &str) -> Option<Vec<String>> {
    let path = run_dir.join("connectors").join(format!("{connector}.yaml"));
    let yaml = std::fs::read_to_string(&path).ok()?;
    let doc = apb_core::connector::def::ConnectorDoc::from_yaml(&yaml, connector).ok()?;
    Some(doc.read_only_functions())
}

impl NodeCacheCtx {
    /// The cache store this context reads and writes, so the drive loop can
    /// restore a hit's artifacts through the very store the lookup used.
    pub(crate) fn store(&self) -> &CacheStore {
        &self.store
    }

    /// Probes the store for a still-valid cached result. `Refresh` always
    /// skips the lookup (never a hit) so a fresh run overwrites stale entries.
    pub(crate) fn lookup(&self, cfg: &RunConfig) -> Option<CachedEntry> {
        if cfg.cache == CacheRunMode::Refresh {
            return None;
        }
        self.store.load(&self.key, unix_now())
    }

    /// Post-hoc verification then store. Returns the event drive should append:
    /// `NodeCacheStored` on a clean admission, otherwise `NodeCacheRejected`
    /// with a reason. The workspace must be unchanged (declared outputs
    /// excluded) and any connector calls within the read-only set.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn admit(
        &self,
        workdir: &Path,
        run_id: &str,
        playbook: &Playbook,
        output: &str,
        connector_calls_ok: bool,
        had_connector_calls: bool,
        artifacts: &[(ArtifactRef, Vec<u8>)],
    ) -> EventPayload {
        if !connector_calls_ok {
            return EventPayload::NodeCacheRejected {
                node: self.node_id.clone(),
                reason: "connector call outside the read_only set".into(),
            };
        }
        let post = match playbook.node(&self.node_id).and_then(|n| n.inputs.as_ref()) {
            Some(inp) if !inp.files.is_empty() => {
                files_fingerprint(workdir, &inp.files, &self.exclude).ok()
            }
            _ => git_fingerprint(workdir, &self.exclude),
        };
        if post.as_deref() != Some(self.pre_fingerprint.as_str()) {
            return EventPayload::NodeCacheRejected {
                node: self.node_id.clone(),
                reason: "workspace changed during node execution".into(),
            };
        }

        let record = CacheRecord {
            format_version: apb_core::cache::CACHE_FORMAT,
            key: self.key.clone(),
            created_at_unix: unix_now(),
            node_type: self.node_type.into(),
            provenance: Provenance {
                run_id: run_id.into(),
                playbook_id: playbook.id.clone(),
                playbook_version: playbook.version.clone(),
                node_id: self.node_id.clone(),
            },
            profile_bundle_digest: self.bundle_digest.clone(),
            workspace_fingerprint: self.pre_fingerprint.clone(),
            verification: Verification {
                workspace_unchanged: true,
                connector_calls: if had_connector_calls {
                    "read_only"
                } else {
                    "none"
                }
                .into(),
            },
            output_digest: sha256_hex(output.as_bytes()),
            artifacts: artifacts.iter().map(|(a, _)| a.clone()).collect(),
            ttl_seconds: self.ttl,
        };
        match self.store.store(&record, output, artifacts) {
            Ok(()) => EventPayload::NodeCacheStored {
                node: self.node_id.clone(),
                key: self.key.clone(),
            },
            Err(e) => EventPayload::NodeCacheRejected {
                node: self.node_id.clone(),
                reason: format!("store error: {e}"),
            },
        }
    }
}

/// Captures a node's declared output artifacts after a successful execution.
///
/// For every glob in `outputs.files` this matches files under `workdir`
/// (`ArtifactScope::Workspace`) and under `run_dir` (`ArtifactScope::Run`),
/// recording each as an [`ArtifactRef`] (file name, scope-relative
/// forward-slash path, content digest) paired with its bytes for storage. The
/// workspace walk skips `.git` and `.apb` (mirroring the fingerprint walk), so
/// the run's own state under `.apb` never counts as a declared output; the run
/// walk skips nothing, so an author who globs a run file (`events.jsonl`, ...)
/// gets exactly that and nothing is special-cased away. Symlinks are neither
/// followed nor captured, so a matched path can never escape its scope root.
///
/// Returns `Err` if a matched file cannot be read or a matched path escapes
/// its scope root after normalization. The caller then rejects admission
/// rather than store a record that references artifacts it could not capture.
/// It never fails the run.
pub(crate) fn capture_artifacts(
    node: &Node,
    run_dir: &Path,
    workdir: &Path,
) -> Result<Vec<(ArtifactRef, Vec<u8>)>, String> {
    let globs = node
        .outputs
        .as_ref()
        .map(|o| o.files.as_slice())
        .unwrap_or(&[]);
    if globs.is_empty() {
        return Ok(Vec::new());
    }
    let set = build_globset(globs).map_err(|g| format!("invalid output glob `{g}`"))?;

    let mut out = Vec::new();
    // Workspace scope skips `.git`/`.apb`; run scope skips nothing.
    for (root, scope, skip_internal) in [
        (workdir, ArtifactScope::Workspace, true),
        (run_dir, ArtifactScope::Run, false),
    ] {
        let mut rels = Vec::new();
        walk(root, root, skip_internal, &mut rels)
            .map_err(|e| format!("walk {}: {e}", root.display()))?;
        rels.sort_unstable();
        for rel in rels {
            if !set.is_match(&rel) {
                continue;
            }
            if !is_safe_relative(&rel) {
                return Err(format!("artifact path `{rel}` escapes its scope root"));
            }
            let bytes = std::fs::read(root.join(&rel)).map_err(|e| format!("read {rel}: {e}"))?;
            let name = Path::new(&rel)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| rel.clone());
            out.push((
                ArtifactRef {
                    name,
                    digest: sha256_hex(&bytes),
                    scope: scope.clone(),
                    path: rel,
                },
                bytes,
            ));
        }
    }
    Ok(out)
}

/// Collects `dir`'s regular files as `root`-relative forward-slash paths.
///
/// Mirrors `fingerprint::walk`: `DirEntry::file_type` (no-follow) decides
/// recursion, so a symlink is never followed and a symlink cycle cannot cause
/// unbounded recursion; symlinks themselves are neither recursed nor captured.
/// When `skip_internal`, the `.git` and `.apb` directories are not descended.
fn walk(
    root: &Path,
    dir: &Path,
    skip_internal: bool,
    out: &mut Vec<String>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if skip_internal && (name == ".git" || name == ".apb") {
                continue;
            }
            walk(root, &path, skip_internal, out)?;
        } else if file_type.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Restores a hit's declared artifacts to the workspace before the hit is
/// taken. Each stored relative path is validated as safe (no absolute path, no
/// `..` traversal) BEFORE any write, then the bytes are read digest-verified
/// from the store and atomically written under the scope root. Any failure (an
/// unsafe path, a missing or tampered object, a write error) returns `Err`,
/// and the caller degrades the hit to a miss. It never fails the run.
pub(crate) fn restore_artifacts(
    entry: &CachedEntry,
    store: &CacheStore,
    run_dir: &Path,
    workdir: &Path,
) -> Result<(), String> {
    for a in &entry.record.artifacts {
        if !is_safe_relative(&a.path) {
            return Err(format!("unsafe artifact path `{}`", a.path));
        }
        let bytes = store
            .read_object(&a.digest)
            .ok_or_else(|| format!("artifact object missing or tampered: {}", a.name))?;
        let root = match a.scope {
            ArtifactScope::Run => run_dir,
            ArtifactScope::Workspace => workdir,
        };
        atomic_write(&root.join(&a.path), &bytes)
            .map_err(|e| format!("write artifact `{}`: {e}", a.path))?;
    }
    Ok(())
}

/// True only for a non-empty relative path built entirely of normal segments
/// (and `.`): rejects absolute paths and any `..` traversal, so a path joined
/// onto its scope root can never escape into a sibling tree.
fn is_safe_relative(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let p = Path::new(path);
    if p.is_absolute() {
        return false;
    }
    p.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::cache::CACHE_FORMAT;

    /// Builds a minimal cache entry whose single artifact carries `path`, so a
    /// restore exercises the path-safety guard before touching the store.
    fn entry_with_path(path: &str) -> CachedEntry {
        CachedEntry {
            record: CacheRecord {
                format_version: CACHE_FORMAT,
                key: "sha256:k".into(),
                created_at_unix: 0,
                node_type: "script".into(),
                provenance: Provenance {
                    run_id: "r".into(),
                    playbook_id: "p".into(),
                    playbook_version: "1.0.0".into(),
                    node_id: "n".into(),
                },
                profile_bundle_digest: None,
                workspace_fingerprint: "fp".into(),
                verification: Verification {
                    workspace_unchanged: true,
                    connector_calls: "none".into(),
                },
                output_digest: "sha256:o".into(),
                artifacts: vec![ArtifactRef {
                    name: "x".into(),
                    digest: "sha256:d".into(),
                    scope: ArtifactScope::Workspace,
                    path: path.into(),
                }],
                ttl_seconds: None,
            },
            output: String::new(),
        }
    }

    #[test]
    fn restore_rejects_parent_traversal_and_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let store = CacheStore::open(root);
        for bad in [
            "../escape.txt",
            "/etc/passwd",
            "a/../../b.txt",
            "sub/../../../x",
            "",
        ] {
            let entry = entry_with_path(bad);
            assert!(
                restore_artifacts(&entry, &store, root, root).is_err(),
                "path `{bad}` must be rejected"
            );
        }
        // A rejected restore must never create a file outside the scope root.
        assert!(!root.parent().unwrap().join("escape.txt").exists());
    }
}
