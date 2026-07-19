//! Node cache lookup and admission (spec 2026-07-19-node-cache-design).
//!
//! Wraps `execute_node` in the drive loop: [`prepare`] builds a per-node
//! context (returning `None` for anything not cache-eligible), `lookup`
//! probes the store, and `admit` runs the post-hoc workspace verification and
//! stores a clean result. Every failure here degrades to a miss or a
//! rejection, never a run error, so the cache can never fail a run.
use super::*;
use apb_core::cache::{
    ArtifactRef, CacheRecord, CacheStore, CachedEntry, KeyParts, Provenance, Verification,
    cache_key,
};
use apb_core::content::sha256_hex;
use apb_core::fingerprint::{files_fingerprint, git_fingerprint};
use apb_core::schema::CacheMode;

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
    /// Cache record `node_type`; always `"script"` in this task.
    node_type: &'static str,
    bundle_digest: Option<String>,
    /// Declared output globs, excluded from the post-execution fingerprint so
    /// a node's own products never count as an unexpected workspace change.
    exclude: Vec<String>,
}

/// Builds the cache context for `node_id`, or `None` when the node is not
/// cache-eligible: the run disables caching, the node does not declare
/// `cache: auto`, the node is not a script (the agent_task arm lands in Task
/// 6), or no workspace fingerprint can be computed (for example, not a git
/// work tree). Any of these paths simply skips the cache for this node.
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
    // Task 5 caches script nodes only; agent_task is Task 6.
    let (script, runner) = match &node.kind {
        NodeKind::Script { script, runner, .. } => (script, runner),
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

    // The script is resolved from the run snapshot exactly as `run_script`
    // does: the node's `script` value already carries the `scripts/` prefix,
    // so it joins directly onto `run_dir` (no extra `scripts` segment).
    let script_bytes = std::fs::read(run_dir.join(script)).ok()?;
    let script_digest = sha256_hex(&script_bytes);
    let node_def = serde_json::to_string(node).ok()?;
    let key = cache_key(&KeyParts {
        format: apb_core::cache::CACHE_FORMAT,
        node_def: &node_def,
        script_digest: Some(script_digest.as_str()),
        runner: Some(runner.as_str()),
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
        node_type: "script",
        bundle_digest: bundle_digest.map(str::to_string),
        exclude,
    })
}

impl NodeCacheCtx {
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

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
