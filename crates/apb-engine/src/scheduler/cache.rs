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
    /// This node ran as a member of a concurrent batch, whose siblings share
    /// its workspace by design (#80). Changes the admission rejection reason
    /// on a fingerprint mismatch, never the admission decision itself.
    batch_member: bool,
    /// Whether a success may be stored: false for a run that processes
    /// external data ([`crate::external_data`]), whose lookups still hit an
    /// earlier run's entry but which adds nothing to the store.
    keep: bool,
}

/// What the caller knows about the execution surrounding this node's cache
/// step. Bundled rather than passed as two more arguments, because `probe`
/// already carries seven (#80).
#[derive(Default, Clone, Copy)]
pub(crate) struct ProbeContext<'a> {
    /// Pre-execution workspace fingerprint taken by the caller (the batch
    /// takes it once for the whole logical batch, via [`pre_fingerprint`]).
    /// `None` means compute it now, exactly as before this context existed.
    pub pre_fingerprint: Option<&'a str>,
    /// This node runs as a member of a concurrent batch, whose siblings write
    /// into the same workspace by design.
    pub batch_member: bool,
    /// Where the cache store lives: the execution root's `.apb/cache`. The
    /// workspace fingerprint is taken over the tree the node runs in, which
    /// is a different directory once the run has its own working tree (issue
    /// #67 item 8); the store stays in the project so a worktree never grows
    /// an `.apb` of its own. `None`: the node's workdir.
    pub store_root: Option<&'a Path>,
    /// The node's rendered `cache.key` (issue #67 item 6), set by [`probe`]:
    /// when present it stands in for the workspace fingerprint in the key.
    pub declared_key: Option<&'a str>,
}

/// The single definition of "this node participates in the cache at all":
/// caching is on for the run, the node declares `cache: auto`, and it is a
/// script or an agent_task. Shared by `prepare` and `pre_fingerprint` so the
/// two can never answer differently for the same node (#80).
fn cache_eligible<'a>(playbook: &'a Playbook, node_id: &str, cfg: &RunConfig) -> Option<&'a Node> {
    if cfg.cache == CacheRunMode::Off {
        return None;
    }
    let node = playbook.node(node_id)?;
    // A node with its own `workdir` (issue #67 item 4) runs outside the tree
    // the cache fingerprints and stores into, so it never participates.
    // A list node's output is its items' settlements, never a cache entry.
    if node.cache_mode() != CacheMode::Auto
        || node.kind.workdir_template().is_some()
        || node.list().is_some()
    {
        return None;
    }
    // A proving node (spec node-contracts D4) settles green before any
    // attempt ran, and a scoped node (D3) is checked against its scope: a
    // cache hit would settle either with no proof and no scope check in
    // this run, so neither participates, even when validation was skipped.
    if node
        .success_check
        .as_ref()
        .is_some_and(|c| c.prove().is_some())
        || node.writes.is_some()
        || node.max_changed_files.is_some()
    {
        return None;
    }
    match &node.kind {
        NodeKind::Script { .. } | NodeKind::AgentTask { .. } | NodeKind::Judge { .. } => Some(node),
        _ => None,
    }
}

// --- issue #165 Part 5: judge node cache key ---------------------------------

/// The `workspace_fingerprint` a judge node's key carries: its answer
/// depends on its state and questions, never on the working tree.
const JUDGE_FINGERPRINT: &str = "judge";

/// The rendered-state digest and the `(provider, model)` of a judge key.
type JudgeKey = (String, (String, String));

/// A judge node's key parts: the rendered state (canonical JSON) and the
/// `(provider, pinned model)` that would answer. `None` (no cache) unless
/// the run's `judge_node` use is at enforce, because below it the output is
/// a fallback, never an answer worth keeping.
fn judge_key_parts(
    playbook: &Playbook,
    node_id: &str,
    run_dir: &Path,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
) -> Result<Option<JudgeKey>, EngineError> {
    let Some(NodeKind::Judge { state: fields, .. }) = playbook.node(node_id).map(|n| &n.kind)
    else {
        return Ok(None);
    };
    let Some(decisions) = crate::manifest::read(run_dir)
        .ok()
        .flatten()
        .and_then(|m| m.decisions)
    else {
        return Ok(None);
    };
    // The snapshot and what the machine says now (the kill switch, a
    // lowered ceiling or use mode, a removed file): a cached answer routes
    // the run only while the use is still at enforce.
    if apb_core::decisions::killed_by_switch()
        || decisions.mode_for("judge_node") != apb_core::decisions::DecisionMode::Enforce
        || apb_core::decisions::live_mode(&super::judge::cfg_root(run_dir), "judge_node")
            != apb_core::decisions::DecisionMode::Enforce
    {
        return Ok(None);
    }
    let Some(primary) = decisions
        .providers
        .iter()
        .find(|p| !matches!(p.kind, apb_core::decisions::ProviderKind::LlmEmulation))
    else {
        return Ok(None);
    };
    let mut rendered = serde_json::Map::new();
    for (name, template) in fields.iter() {
        let text = render_node_prompt(
            playbook,
            run_dir,
            run_id,
            state,
            cfg,
            template,
            &playbook.context_budget(node_id),
        )?;
        rendered.insert(name.to_string(), serde_json::Value::String(text));
    }
    Ok(Some((
        apb_decide::digest::digest(&serde_json::Value::Object(rendered)),
        (
            primary.id.clone(),
            primary.model.clone().unwrap_or_default(),
        ),
    )))
}

/// Whether a judge output is a provider's answer (worth caching), not a
/// fallback or an emulation.
fn judge_output_is_answer(output: &str) -> bool {
    apb_core::schema::output_field_value(output, apb_core::judge::DECIDED_BY).is_some_and(|by| {
        by != apb_core::judge::UNAVAILABLE
            && by != apb_core::judge::DEFAULT
            && !by.starts_with("emulated:")
    })
}

// --- end judge node cache key ---------------------------------------------------

/// The pre-execution workspace fingerprint for one node, taken against the
/// tree as it stands NOW. The concurrent batch takes every member's
/// fingerprint once, before the first chunk runs, so a member's cache key
/// does not depend on which chunk it landed in (that is, on `max_parallel`,
/// #80). `None` when the node is not cache-eligible or no fingerprint can be
/// computed, exactly as `prepare`.
pub(crate) fn pre_fingerprint(
    playbook: &Playbook,
    node_id: &str,
    workdir: &Path,
    cfg: &RunConfig,
) -> Option<String> {
    let node = cache_eligible(playbook, node_id, cfg)?;
    // The same two fingerprint modes `prepare` uses: declared `inputs.files`
    // narrow it, otherwise the whole git work tree, with the node's declared
    // `outputs.files` excluded either way so its own products never shift it.
    let exclude = node
        .outputs
        .as_ref()
        .map(|o| o.files.clone())
        .unwrap_or_default();
    match node.inputs.as_ref() {
        Some(inp) if !inp.files.is_empty() => files_fingerprint(workdir, &inp.files, &exclude).ok(),
        _ => git_fingerprint(workdir, &exclude),
    }
}

/// Builds the cache context for `node_id`, or `None` when the node is not
/// cache-eligible: the run disables caching, the node does not declare
/// `cache: auto`, the node is neither a script nor an agent_task, or no
/// workspace fingerprint can be computed (for example, not a git work tree).
/// Any of these paths simply skips the cache for this node.
///
/// The key's `workspace_fingerprint` excludes the node's declared
/// `outputs.files`, the same set `admit` excludes from the post-execution
/// fingerprint, so a node's own products never shift its key between runs.
///
/// `probe_ctx.pre_fingerprint`, when `Some`, is used verbatim instead of being
/// recomputed here: the concurrent batch takes it once for the whole batch
/// (#80). `probe_ctx.batch_member` is carried onto the built context so
/// `admit` can pick the right rejection reason on a mismatch.
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
    probe_ctx: ProbeContext<'_>,
) -> Option<NodeCacheCtx> {
    let node = cache_eligible(playbook, node_id, cfg)?;
    // Script and agent_task nodes participate; every other kind skips the
    // cache (ruled out by `cache_eligible` above). A script folds in its
    // script digest + runner; an agent_task folds in the caller-supplied
    // rendered prompt + bundle digest + agent/model + connector digests
    // instead (all None/empty for a script). The script body is resolved
    // from the run snapshot exactly as `run_script` does: the node's
    // `script` value already carries the `scripts/` prefix, so it joins
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
            NodeKind::Judge { .. } => (None, None, "judge"),
            _ => unreachable!("cache_eligible only returns script, agent_task or judge nodes"),
        };

    // Declared outputs are excluded from BOTH the pre-execution fingerprint
    // (which is also the key's `workspace_fingerprint`) and the post-execution
    // one in `admit`. Excluding on only one side is asymmetric: a node that
    // writes a declared output into the workspace and leaves it there would
    // shift its own key on the next run (never a hit) and drift pre vs post at
    // admit (never a re-store).
    let exclude = node
        .outputs
        .as_ref()
        .map(|o| o.files.clone())
        .unwrap_or_default();
    let fingerprint = match probe_ctx.pre_fingerprint {
        _ if node_type == "judge" => JUDGE_FINGERPRINT.to_string(),
        Some(f) => f.to_string(),
        None => match node.inputs.as_ref() {
            Some(inp) if !inp.files.is_empty() => {
                files_fingerprint(workdir, &inp.files, &exclude).ok()?
            }
            _ => git_fingerprint(workdir, &exclude)?,
        },
    };

    // A declared key replaces the workspace fingerprint in the cache key
    // (issue #67 item 6): the author has said what the result depends on, so
    // an unrelated change to the tree no longer misses. The fingerprint is
    // still taken above, because admission still requires the node to leave
    // the workspace as it found it.
    let key_basis = match probe_ctx.declared_key {
        Some(k) => format!("declared-key:{}", sha256_hex(k.as_bytes())),
        None => fingerprint.clone(),
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
        workspace_fingerprint: &key_basis,
    });

    Some(NodeCacheCtx {
        key,
        store: CacheStore::open(probe_ctx.store_root.unwrap_or(workdir)),
        ttl: node.cache_ttl_seconds(),
        pre_fingerprint: fingerprint,
        node_id: node_id.to_string(),
        node_type,
        bundle_digest: bundle_digest.map(str::to_string),
        exclude,
        batch_member: probe_ctx.batch_member,
        keep: !crate::external_data::is_flagged(run_dir),
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
    // The EFFECTIVE binding: a rebound node (issue #45 finding 5) caches under
    // its new profile's bundle, not the manifest's original.
    let entry = effective_for_node(run_dir, &manifest, node_id)
        .ok()
        .flatten()?;
    let primary = entry.chain.first()?;
    let bundle = entry.bundle_digest.clone();
    // Host execution mode (0.23.0): the host, not the profile's CLI, produces
    // the output, so a host run keys under `host` and never shares an entry
    // with a CLI run of the same node (either way round). A CLI run's key is
    // unchanged, so existing cache entries stay valid.
    let agent = if manifest.is_host_mode().ok()? {
        "host".to_string()
    } else {
        primary.agent_id.clone()
    };
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
            // A judge node reads no files and writes none.
            _ if self.node_type == "judge" => Some(JUDGE_FINGERPRINT.to_string()),
            Some(inp) if !inp.files.is_empty() => {
                files_fingerprint(workdir, &inp.files, &self.exclude).ok()
            }
            _ => git_fingerprint(workdir, &self.exclude),
        };
        if post.as_deref() != Some(self.pre_fingerprint.as_str()) {
            // A batch member shares one workspace with its siblings by design,
            // so a changed post-execution fingerprint is structurally expected
            // and is NOT evidence that this node dirtied the tree. The engine
            // cannot tell a sibling's write from this node's own undeclared
            // one, so the reason names the condition and never claims a cause.
            // Distinct string so a dashboard can filter it out of the honest
            // rejections.
            let reason = match self.batch_member {
                true => "workspace shared with concurrent batch siblings",
                false => "workspace changed during node execution",
            };
            return EventPayload::NodeCacheRejected {
                node: self.node_id.clone(),
                reason: reason.into(),
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

/// The pre-execution cache step for ONE node, resolved on the drive thread.
///
/// Both drive arms go through [`probe`] and [`settle`], so the cache flow exists
/// once: the sequential path and the concurrent batch cannot drift, and a batch
/// member that hits is never spawned (spec 2026-08-05 section 1.4).
pub(crate) enum CacheProbe {
    /// A valid cached result whose declared artifacts are already restored to
    /// the workspace. The node must NOT be executed; the caller journals the
    /// events, then finishes the node as succeeded with this output and these
    /// artifacts.
    Hit {
        output: String,
        artifacts: Vec<ArtifactRef>,
        events: Vec<EventPayload>,
    },
    /// No usable cached result: execute the node. `ctx` is `Some` only when the
    /// node is cache-eligible, and is what [`settle`] then admits through.
    Miss {
        ctx: Option<NodeCacheCtx>,
        events: Vec<EventPayload>,
    },
}

/// Looks one node up in the cache before it runs: builds its cache context
/// (rendering the agent prompt through the very helper `execute_node` uses, so
/// the key can never drift from what the agent receives), probes the store, and
/// restores a hit's declared artifacts.
///
/// A hit is only reported once its artifacts are back in the workspace: a failed
/// restore (a missing or tampered object) degrades to a miss, so a
/// `NodeCacheHit` never appears without the files it promised. A node that has
/// already finished once in this run (a loop re-execution) skips the lookup so
/// each pass runs the node again; the store side is unaffected.
///
/// The returned `events` are the caller's to append, in order, before anything
/// else it writes for this node. Every failure here degrades to a miss, never a
/// run error.
///
/// `probe_ctx` carries what the caller already knows about the surrounding
/// execution (#80): a pre-computed workspace fingerprint (the concurrent
/// batch takes one per member, once, before the first chunk runs) and whether
/// this node runs as a batch member at all, threaded into `prepare`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn probe(
    playbook: &Playbook,
    node_id: &str,
    run_dir: &Path,
    workdir: &Path,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    probe_ctx: ProbeContext<'_>,
) -> Result<CacheProbe, EngineError> {
    // A scenario run neither reads nor fills the cache.
    if crate::scenario::session::skips(run_dir, crate::scenario::BoundaryKind::NodeCache) {
        return Ok(CacheProbe::Miss {
            ctx: None,
            events: Vec::new(),
        });
    }
    // Agent-task key parts come from the run's own immutable manifest snapshot
    // (bundle digest + primary agent/model + the node's connector digests). A
    // script node leaves all of these empty.
    let mut rendered_prompt: Option<String> = None;
    let mut bundle_digest: Option<String> = None;
    let mut agent_model: Option<(String, String)> = None;
    let mut connector_digests: Vec<String> = Vec::new();
    if let Some(NodeKind::AgentTask { prompt, .. }) = playbook.node(node_id).map(|n| &n.kind)
        && let Some((bundle, agent, model, digests)) = agent_key_parts(run_dir, node_id)
    {
        rendered_prompt = Some(render_node_prompt(
            playbook,
            run_dir,
            run_id,
            state,
            cfg,
            prompt,
            &playbook.context_budget(node_id),
        )?);
        bundle_digest = Some(bundle);
        agent_model = Some((agent, model));
        connector_digests = digests;
    }
    if matches!(
        playbook.node(node_id).map(|n| &n.kind),
        Some(NodeKind::Judge { .. })
    ) {
        match judge_key_parts(playbook, node_id, run_dir, run_id, state, cfg)? {
            Some((state_digest, provider_model)) => {
                rendered_prompt = Some(state_digest);
                agent_model = Some(provider_model);
            }
            None => {
                return Ok(CacheProbe::Miss {
                    ctx: None,
                    events: Vec::new(),
                });
            }
        }
    }
    // A declared cache key, rendered unclipped so a clip note (which names
    // this run's directory) can never leak into it. A key that renders empty
    // (the value it reads was never published) skips the cache for this
    // execution rather than keying every such execution alike.
    let declared_key: Option<String> =
        match playbook.node(node_id).and_then(|n| n.cache_key_template()) {
            Some(template) => {
                let rendered = render_node_prompt(
                    playbook,
                    run_dir,
                    run_id,
                    state,
                    cfg,
                    template,
                    &apb_core::schema::ContextBudget::UNLIMITED,
                )?;
                if rendered.trim().is_empty() {
                    return Ok(CacheProbe::Miss {
                        ctx: None,
                        events: Vec::new(),
                    });
                }
                Some(rendered)
            }
            None => None,
        };
    let ctx = prepare(
        playbook,
        node_id,
        workdir,
        run_dir,
        cfg,
        rendered_prompt.as_deref(),
        bundle_digest.as_deref(),
        agent_model.as_ref().map(|(a, m)| (a.as_str(), m.as_str())),
        connector_digests,
        ProbeContext {
            declared_key: declared_key.as_deref(),
            ..probe_ctx
        },
    );
    let already_finished = state.nodes.get(node_id).is_some_and(|st| st.is_finished());
    let entry = match already_finished {
        true => None,
        false => ctx.as_ref().and_then(|c| c.lookup(cfg)),
    };
    let hit = entry.filter(|entry| {
        let c = ctx.as_ref().expect("a lookup implies a cache ctx");
        restore_artifacts(entry, c.store(), run_dir, workdir).is_ok()
    });
    match hit {
        Some(entry) => {
            let c = ctx.as_ref().expect("a hit implies a cache ctx");
            Ok(CacheProbe::Hit {
                artifacts: entry.record.artifacts.clone(),
                events: vec![EventPayload::NodeCacheHit {
                    node: node_id.to_string(),
                    key: c.key.clone(),
                    source_run: entry.record.provenance.run_id.clone(),
                }],
                output: entry.output,
            })
        }
        None => {
            let events = ctx
                .as_ref()
                .map(|c| {
                    vec![EventPayload::NodeCacheMiss {
                        node: node_id.to_string(),
                        key: c.key.clone(),
                    }]
                })
                .unwrap_or_default();
            Ok(CacheProbe::Miss { ctx, events })
        }
    }
}

/// The declared `outputs.fields` a successful output leaves out (absent,
/// `null`, or an output that is not a JSON object at all). Empty for a node
/// that declares none. The one definition behind `OutputFieldsMissing` and
/// the completion check's `missing_fields`.
pub(crate) fn missing_output_fields(node: &apb_core::schema::Node, output: &str) -> Vec<String> {
    let Some(fields) = node.outputs.as_ref().map(|o| &o.fields) else {
        return Vec::new();
    };
    if fields.is_empty() {
        return Vec::new();
    }
    let parsed: Option<serde_json::Map<String, serde_json::Value>> =
        serde_json::from_str(output).ok();
    fields
        .iter()
        .filter(|f| {
            parsed
                .as_ref()
                .is_none_or(|o| o.get(f.name()).is_none_or(|v| v.is_null()))
        })
        .map(|f| f.name().to_string())
        .collect()
}

/// The post-execution half of the per-node unit: captures the node's declared
/// output artifacts, checks the declaration, and decides cache admission.
///
/// Returns the artifacts to record on the node's `NodeFinished` plus the events
/// to journal before it, in order: a `DeliverableMissing` warning when the node
/// declared `outputs.files` and nothing was captured, then the admission event
/// (`NodeCacheStored` or `NodeCacheRejected`) for a cache-eligible node.
///
/// Capture runs for EVERY successful node that declares `outputs.files` (spec
/// 2026-08-05 section 2.6, issue #74 finding 4), with or without a cache
/// configuration: the declaration is what the playbook promises to produce, and
/// tying its capture to caching meant an author who wrote `outputs.files` without
/// `cache: auto` silently got nothing. Admission stays gated on `ctx`. A capture
/// error still rejects admission rather than storing a record that references
/// artifacts we could not read, and never fails the run.
///
/// An unsuccessful result captures nothing: a declared deliverable of a failed
/// node is not drift, and a partially written file is not a deliverable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn settle(
    ctx: Option<&NodeCacheCtx>,
    playbook: &Playbook,
    run_dir: &Path,
    workdir: &Path,
    run_id: &str,
    node_id: &str,
    status: NodeStatus,
    output: &str,
) -> (Vec<ArtifactRef>, Vec<EventPayload>) {
    if status != NodeStatus::Succeeded {
        return (Vec::new(), Vec::new());
    }
    let Some(node) = playbook.node(node_id) else {
        return (Vec::new(), Vec::new());
    };
    // The declaration verbatim. Read from the node rather than inferred from an
    // empty capture, because `capture_artifacts` returns an empty vector both for
    // "nothing declared" and for "declared, matched nothing" - and only the
    // second is drift worth reporting.
    let declared: Vec<String> = node
        .outputs
        .as_ref()
        .map(|o| o.files.clone())
        .unwrap_or_default();
    let mut events = Vec::new();
    // Declared named outputs (issue #67 item 4): a warning per execution that
    // left any of them out, never a failure.
    let missing = missing_output_fields(node, output);
    if !missing.is_empty() {
        events.push(EventPayload::OutputFieldsMissing {
            node: node_id.to_string(),
            fields: missing,
        });
    }
    if declared.is_empty() && ctx.is_none() {
        return (Vec::new(), events);
    }
    match capture_artifacts(node, run_dir, workdir) {
        Ok(captured) => {
            if !declared.is_empty() && captured.is_empty() {
                events.push(EventPayload::DeliverableMissing {
                    node: node_id.to_string(),
                    globs: declared,
                    detail: None,
                });
            }
            let artifacts = captured.iter().map(|(a, _)| a.clone()).collect();
            if let Some(ctx) = ctx
                && ctx.node_type == "judge"
                && !judge_output_is_answer(output)
            {
                events.push(EventPayload::NodeCacheRejected {
                    node: node_id.to_string(),
                    reason: "judge output is a fallback, not a provider answer".into(),
                });
            } else if ctx.is_some() && ran_as_host_fallback(run_dir, node_id) {
                // A `cli` run's key names the CLI agent; an output the host
                // produced through the execution fallback is not that agent's
                // and must not serve a later CLI run.
                events.push(EventPayload::NodeCacheRejected {
                    node: node_id.to_string(),
                    reason: "output came from the host fallback, not the CLI agent".into(),
                });
            } else if ctx.is_some_and(|c| !c.keep) {
                // A run that processes external data adds nothing to the
                // store; its lookups still hit an earlier run's entry.
                events.push(EventPayload::NodeCacheRejected {
                    node: node_id.to_string(),
                    reason: "the run processes external data and keeps nothing for later runs"
                        .into(),
                });
            } else if let Some(ctx) = ctx {
                // Scan the run log for this node's connector calls (written out
                // of band by the connector-call subprocess) and verify each
                // against the read_only set in the run's connector snapshot. A
                // script node makes none.
                let (calls_ok, had_calls) = verify_connector_calls(run_dir, node_id);
                events.push(ctx.admit(
                    workdir, run_id, playbook, output, calls_ok, had_calls, &captured,
                ));
            }
            (artifacts, events)
        }
        Err(reason) => {
            if !declared.is_empty() {
                events.push(EventPayload::DeliverableMissing {
                    node: node_id.to_string(),
                    globs: declared,
                    detail: Some(reason.clone()),
                });
            }
            if ctx.is_some() {
                events.push(EventPayload::NodeCacheRejected {
                    node: node_id.to_string(),
                    reason: format!("artifact capture failed: {reason}"),
                });
            }
            (Vec::new(), events)
        }
    }
}

/// Whether this node's latest execution handed a step to the host through
/// the execution fallback (0.23.0): an `execution_fallback` since its most
/// recent `node_started`. An unreadable log counts as yes (fail closed: no
/// store).
fn ran_as_host_fallback(run_dir: &Path, node_id: &str) -> bool {
    let Ok(events) = crate::event::read_all(run_dir) else {
        return true;
    };
    let from = events
        .iter()
        .rposition(
            |e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == node_id),
        )
        .map(|i| i + 1)
        .unwrap_or(0);
    events[from..].iter().any(
        |e| matches!(&e.payload, EventPayload::ExecutionFallback { node, .. } if node == node_id),
    )
}

/// Captures a node's declared output artifacts after a successful execution.
///
/// For every glob in `outputs.files` this matches files under `workdir`
/// (`ArtifactScope::Workspace`) and under `run_dir` (`ArtifactScope::Run`),
/// recording each as an [`ArtifactRef`] (file name, scope-relative
/// forward-slash path, content digest) paired with its bytes for storage. The
/// workspace walk skips `.git` and `.apb` (mirroring the fingerprint walk), so
/// the run's own state under `.apb` never counts as a declared output; the run
/// walk skips only apb's own storage at the top of the run directory
/// ([`apb_core::result_store::INTERNAL_RUN_NAMES`]: result snapshots, stored
/// definitions, bindings, diagnostics), so an author who globs a run file
/// (`events.jsonl`, ...) gets exactly that, and neither mechanism captures
/// the other's storage. Symlinks are neither
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
    // Workspace scope skips `.git`/`.apb`; run scope skips apb's storage.
    for (root, scope, skip_internal) in [
        (workdir, ArtifactScope::Workspace, Skip::GitAndApb),
        (run_dir, ArtifactScope::Run, Skip::RunStorage),
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

/// What a walk of `outputs.files` leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Skip {
    /// The workspace: `.git` and `.apb` at any depth.
    GitAndApb,
    /// The run directory: apb's own storage at its top.
    RunStorage,
}

/// Collects `dir`'s regular files as `root`-relative forward-slash paths.
///
/// Mirrors `fingerprint::walk`: `DirEntry::file_type` (no-follow) decides
/// recursion, so a symlink is never followed and a symlink cycle cannot cause
/// unbounded recursion; symlinks themselves are neither recursed nor captured.
/// `skip` names what is not collected (see [`Skip`]).
fn walk(root: &Path, dir: &Path, skip: Skip, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let file_type = entry.file_type()?;
        if skip == Skip::RunStorage
            && dir == root
            && name
                .to_str()
                .is_some_and(|n| apb_core::result_store::INTERNAL_RUN_NAMES.contains(&n))
        {
            continue;
        }
        if file_type.is_dir() {
            if skip == Skip::GitAndApb && (name == ".git" || name == ".apb") {
                continue;
            }
            walk(root, &path, skip, out)?;
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

// --- the work a list skip compares (incremental lists, D2) ---

/// The label of the work fingerprint's canonical form.
const WORK_LABEL: &str = "list-skip/1";
/// The keyed-tag domain of a sensitive run input inside the work.
const WORK_PARAM_DOMAIN: &str = "apb-list-skip-param-v1";

/// What one list node's work is computed from, read when its visit starts.
pub(crate) struct WorkSource<'a> {
    pub run_dir: &'a Path,
    pub events: &'a [crate::event::Event],
    pub playbook: &'a Playbook,
    pub node: &'a Node,
    pub cfg: &'a RunConfig,
    /// The effective non-secret settings of the run's decision runner;
    /// `None` when the run has no decisions block.
    pub decisions: Option<&'a apb_core::decisions::EffectiveDecisions>,
    /// The directory the node's step executes in, as [`execution_dir`]
    /// gives it (the run's tree or the project root it falls back to, under
    /// the node's own `workdir`), or why it does not resolve.
    pub exec: &'a Result<VisitExecution, String>,
}

/// Everything a list visit's item execution reads from outside the
/// definition, resolved once as the visit starts ([`visit_execution`]): the
/// canonical directory its steps run in, every script runtime its steps and
/// checks run as the canonical program that is exec'd, the directory and
/// `PATH` its checks run with, the checks' policy flags and the MCP pins of
/// its granted accounts. The work fingerprint is computed from this value,
/// and every item of a skip visit executes with this value (`ItemExec.exec`):
/// no execution site of such an item resolves any of it again. A node with
/// `isolation` cannot declare `skip` (V288), so no sandbox is part of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct VisitExecution {
    /// The canonical directory the steps execute in.
    pub dir: PathBuf,
    /// The canonical directory the item checks execute in: the drive's tree
    /// (the run's worktree, else the project root), never the node's own
    /// `workdir`.
    pub check_dir: PathBuf,
    /// The `PATH` the steps and checks receive, as apb's process held it
    /// when the visit started; `None` when it had none. The children get
    /// this value, whatever apb's `PATH` is later. No other inherited
    /// variable is captured (a stated limit: the profile's `process_env`
    /// bounds what an agent inherits).
    pub path_env: Option<String>,
    /// The item checks' policy as the visit derives it from the run's
    /// options and the definition: whether a script or connector check
    /// must leave the tree as it found it (`guard_tree`, one worker only),
    /// and whether a report scan may trust the tree (`holds_tree`). The
    /// checks run from these captured flags.
    pub check_policy: CheckPolicy,
    /// The runner keys the steps and checks use, resolved.
    pub runtimes: std::collections::BTreeMap<String, crate::script::ResolvedRunner>,
    /// `<connector>/<account>` of every account granted at the node or its
    /// item check -> the `pin_digest` of the MCP tool approval the machine's
    /// pin store holds for it (`None`: no pin, an HTTP connector or an MCP
    /// account never approved). Every call checks the live server against
    /// the pin store; a settlement that finds the store's pins differ from
    /// these stamps nothing (`ListRun::pins_held`).
    pub mcp_pins: std::collections::BTreeMap<String, Option<String>>,
}

/// The MCP pins of the accounts granted at `node` and its item check, as
/// the machine's pin store holds them now.
pub(crate) fn mcp_pins(
    run_dir: &Path,
    node: &Node,
) -> Result<std::collections::BTreeMap<String, Option<String>>, String> {
    let Some(manifest) =
        crate::manifest::read(run_dir).map_err(|e| format!("skip_needs_manifest: {e}"))?
    else {
        return Ok(Default::default());
    };
    let mut keys = Vec::new();
    for site in [node.id.clone(), apb_core::list::check_site(&node.id)] {
        for g in manifest.grants_for(&site) {
            for a in &g.accounts {
                keys.push((g.connector.clone(), a.clone()));
            }
        }
    }
    if keys.is_empty() {
        return Ok(Default::default());
    }
    let store = apb_core::connector::mcp::pins::PinStore::load()
        .map_err(|e| format!("skip_mcp_pins_unreadable: {e}"))?;
    Ok(keys
        .into_iter()
        .map(|(c, a)| {
            let pin = store.server(&c, &a).map(|p| p.pin_digest.clone());
            (apb_core::connector::mcp::pins::server_key(&c, &a), pin)
        })
        .collect())
}

/// The concurrency cap in force for this run.
///
/// Precedence follows the `max_retries` chain (`node.rs`: the authored value
/// first, then a default, then a constant): `Playbook.defaults.max_parallel`,
/// then the run's own cap, then [`apb_core::schema::DEFAULT_MAX_PARALLEL`]. `RunOptions` and
/// `RunConfig` are ONE slot here - `prepare` copies the invocation value into the
/// persisted run config, which is what a detached driver reads back, so the cap
/// survives a re-drive.
///
/// A declared `0` reads as `1` (serialize) rather than as "admit nothing", which
/// would wedge the batch.
pub(crate) fn resolved_max_parallel(playbook: &Playbook, cfg: &RunConfig) -> usize {
    playbook
        .defaults
        .max_parallel
        .or(cfg.max_parallel)
        .map(|n| n.max(1))
        .unwrap_or(apb_core::schema::DEFAULT_MAX_PARALLEL)
}

/// Whether a run holds its working tree: one of its nodes takes the tree
/// lock and it does not share the workdir. The one predicate the drive takes
/// the tree lock by, and the baseline and list item checks trust a JUnit
/// report by (`entry::holds_tree` re-exports it).
pub(crate) fn holds_tree(playbook: &Playbook, cfg: &RunConfig) -> bool {
    !cfg.shared_workdir && playbook.nodes.iter().any(|n| n.kind.takes_workdir_lock())
}

/// The item checks' policy flags a visit captures ([`VisitExecution`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CheckPolicy {
    pub guard_tree: bool,
    pub holds_tree: bool,
}

/// Resolves the [`VisitExecution`] of `node` once: `Err` with the skip
/// refusal's reason when a part does not resolve.
#[allow(clippy::too_many_arguments)]
pub(crate) fn visit_execution(
    playbook: &Playbook,
    node: &Node,
    tree: &Path,
    run_dir: &Path,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    workers: usize,
) -> Result<VisitExecution, String> {
    let dir = execution_dir(playbook, &node.id, tree, run_dir, run_id, state, cfg)?;
    let check_dir = tree
        .canonicalize()
        .map_err(|e| format!("skip_workdir_unresolved: {}: {e}", tree.display()))?;
    Ok(VisitExecution {
        dir,
        check_dir,
        path_env: captured_path(std::env::var("PATH"))?,
        // Sibling items change the tree while one is checked: the tree
        // guard holds only with one worker.
        check_policy: CheckPolicy {
            guard_tree: workers == 1,
            holds_tree: holds_tree(playbook, cfg) && workers == 1,
        },
        runtimes: runtimes(run_dir, node)?,
        mcp_pins: mcp_pins(run_dir, node)?,
    })
}

/// The `PATH` a visit captures from apb's own: `None` when it has none,
/// `Err` with the skip refusal's reason when it holds bytes a `String`
/// cannot carry. Such a value is never captured as absent: the children
/// would inherit it while the work fingerprint named no `PATH`.
fn captured_path(path: Result<String, std::env::VarError>) -> Result<Option<String>, String> {
    match path {
        Ok(p) => Ok(Some(p)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(
            "skip_path_unrepresentable: apb's PATH is not valid Unicode, so the work fingerprint cannot name it"
                .to_string(),
        ),
    }
}

/// Where agent_task and script nodes without their own `workdir` run: the
/// run's working tree once it is resolved (issue #67 item 8), else a
/// scenario run's directory, else the execution root.
pub(crate) fn tree_dir(state: &RunState, run_dir: &Path, root: &Path) -> PathBuf {
    state
        .worktree
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| crate::scenario::session::workdir(run_dir))
        .unwrap_or_else(|| root.to_path_buf())
}

/// The directory `node` executes in, canonical: the node's own `workdir`
/// rendered over `tree` (the run's working tree, else the execution root
/// the drive falls back to) through the same resolution and renderer the
/// node's execution uses ([`super::node_workdir::resolve`] over the
/// template render of `render_node_prompt`). `Err` with the
/// skip refusal's reason when it does not resolve.
pub(crate) fn execution_dir(
    playbook: &Playbook,
    node_id: &str,
    tree: &Path,
    run_dir: &Path,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
) -> Result<PathBuf, String> {
    let unresolved = |why: String| format!("skip_workdir_unresolved: {why}");
    // The one template render every prompt goes through, as
    // `render_node_prompt` gives it to the node's own execution.
    let dir = super::node_workdir::resolve(playbook, node_id, tree, |t| {
        let budget = playbook.context_budget(node_id);
        let inputs = crate::judge_eval::JudgeInputs::from_state(
            playbook, run_dir, run_id, state, cfg, &budget,
        )?;
        Ok(inputs.render(t, run_dir, budget.output_max_bytes))
    })
    .map_err(|e| unresolved(e.to_string()))?
    .map_err(unresolved)?;
    dir.canonicalize()
        .map_err(|e| unresolved(format!("{}: {e}", dir.display())))
}

/// Everything a run was configured with that decides how one list node's
/// items are worked and checked: parts 1 to 12 (8 retired), each taken whole, with only the
/// contents of the world outside. The one comparison of work: the skip
/// digest, the patch refusal and the rebind refusal read it, and it shares
/// no code with the node cache key.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct WorkInputs {
    label: &'static str,
    /// Part 1: the digest of the whole definition in force.
    definition: String,
    /// Part 2: the digest of the run's `scripts/` copy as pinned at start.
    scripts: String,
    /// Part 3: the canonical start request filled from the run config.
    request: serde_json::Value,
    /// Part 4: what the manifest resolved for the run and the node.
    resolved: serde_json::Value,
    /// Part 5: the effective decision configuration, or `none`.
    decisions: serde_json::Value,
    /// Part 6: the canonical directory the node's step executes in, the
    /// project-root fallback and the node's own `workdir` included.
    target: String,
    /// Part 7: every script runtime the node's step and checks run, as the
    /// run resolved it once ([`crate::script::resolved_runner`]): the same
    /// value runs.
    runtimes: std::collections::BTreeMap<String, crate::script::ResolvedRunner>,
    /// Part 10: the canonical directory the item checks run in.
    check_target: String,
    /// Part 11: the `PATH` the steps and checks receive.
    path_env: Option<String>,
    /// Part 12: the item checks' policy flags.
    check_policy: CheckPolicy,
    /// Part 9: the MCP pins of the granted accounts
    /// ([`VisitExecution::mcp_pins`]); empty without a grant.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    mcp_pins: std::collections::BTreeMap<String, Option<String>>,
}

/// The runner keys `node`'s step and its checks run scripts under: a
/// script node's `runner`, and `sh` for a script `success_check` or a
/// script item check.
fn runner_keys(node: &Node) -> std::collections::BTreeSet<String> {
    let mut keys = std::collections::BTreeSet::new();
    if let apb_core::schema::NodeKind::Script { runner, .. } = &node.kind {
        keys.insert(runner.clone());
    }
    if node
        .success_check
        .as_ref()
        .is_some_and(|c| c.script_path().is_some())
    {
        keys.insert("sh".to_string());
    }
    if node
        .list()
        .and_then(|l| l.check.as_ref())
        .and_then(|c| c.source())
        .is_some_and(|s| matches!(s, apb_core::list::CheckSource::Script(_)))
    {
        keys.insert("sh".to_string());
    }
    keys
}

/// Part 7 (see [`WorkInputs::runtimes`]).
fn runtimes(
    run_dir: &Path,
    node: &Node,
) -> Result<std::collections::BTreeMap<String, crate::script::ResolvedRunner>, String> {
    runner_keys(node)
        .into_iter()
        .map(|k| {
            crate::script::resolved_runner(run_dir, &k)
                .map(|r| (k, r))
                .map_err(|e| format!("skip_runtime_unresolved: {e}"))
        })
        .collect()
}

/// Why a runtime of `runtimes` is no longer the file the run resolved: its
/// identity read now differs from the one resolved (a program replaced at
/// the same path while the run held its resolution). `None` while every
/// one holds. Read as a visit's work is computed and at every settlement,
/// so neither a skip nor a stamp rests on a runtime that changed.
pub(crate) fn runtime_changed(
    runtimes: &std::collections::BTreeMap<String, crate::script::ResolvedRunner>,
) -> Option<String> {
    runtimes
        .iter()
        .find(|(_, r)| apb_core::detect::fingerprint(Path::new(&r.path)) != r.identity)
        .map(|(k, r)| {
            format!(
                "{}: the runtime of runner `{k}` at {} changed after the run resolved it",
                crate::list::skip::RUNTIME_CHANGED,
                r.path
            )
        })
}

/// The six parts of one list node's work (see [`WorkInputs`]). `Err` with
/// the skip refusal's reason when a part does not read.
pub(crate) fn work_inputs(src: &WorkSource<'_>) -> Result<WorkInputs, String> {
    let exec = src.exec.as_ref().map_err(Clone::clone)?;
    if let Some(why) = runtime_changed(&exec.runtimes) {
        return Err(why);
    }
    Ok(WorkInputs {
        label: WORK_LABEL,
        definition: definition_in_force(src.run_dir, src.events)?,
        scripts: pinned_scripts(src.events)?,
        request: run_request(src.run_dir, src.playbook, src.cfg)?,
        resolved: resolved_work(src.run_dir, src.node)?,
        decisions: src
            .decisions
            .map(|d| serde_json::to_value(d).unwrap_or(serde_json::Value::Null))
            .unwrap_or_else(|| serde_json::Value::String("none".into())),
        target: exec.dir.to_string_lossy().into_owned(),
        runtimes: exec.runtimes.clone(),
        mcp_pins: exec.mcp_pins.clone(),
        check_target: exec.check_dir.to_string_lossy().into_owned(),
        path_env: exec.path_env.clone(),
        check_policy: exec.check_policy,
    })
}

/// SHA-256 hex over the canonical JSON of the work.
pub(crate) fn work_fingerprint(inputs: &WorkInputs) -> String {
    let json = serde_json::to_value(inputs).unwrap_or(serde_json::Value::Null);
    // `serde_json` keeps object keys sorted (no `preserve_order`), so the
    // compact form is canonical.
    sha256_hex(json.to_string().as_bytes())
}

/// Part 1: the latest journaled digest of the definition in force, else the
/// digest of the run's snapshot bytes.
fn definition_in_force(run_dir: &Path, events: &[crate::event::Event]) -> Result<String, String> {
    if let Some(d) = crate::run_definitions::boundaries(events)
        .into_iter()
        .rev()
        .find_map(|b| b.definition)
    {
        return Ok(d);
    }
    std::fs::read(run_dir.join("playbook.yaml"))
        .map(|b| crate::run_definitions::digest(&b))
        .map_err(|e| format!("skip_definition_unreadable: {e}"))
}

/// Part 2: the scripts digest `run_provenance` pinned.
pub(crate) fn pinned_scripts(events: &[crate::event::Event]) -> Result<String, String> {
    events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunProvenance { scripts_digest, .. } => scripts_digest.clone(),
            _ => None,
        })
        .ok_or_else(|| "skip_scripts_unreadable: the run pinned no scripts digest".to_string())
}

/// Part 3: [`start_request_of`] with the removed fields taken out, the
/// output limits added and every sensitive value as its keyed tag only. A
/// param is declared secret when the run's secret set holds it (the
/// manifest's `secret_params` and the snapshot's declarations, read
/// strictly: a set that does not read refuses) or the definition in force
/// declares it, the classification every redaction of the run uses.
fn run_request(
    run_dir: &Path,
    playbook: &Playbook,
    cfg: &RunConfig,
) -> Result<serde_json::Value, String> {
    let mut declared = crate::run_redact::secret_names_strict(run_dir)
        .map_err(|e| format!("skip_secret_set_unreadable: {}", e.sentence("the run")))?;
    declared.extend(crate::run_redact::declared_secret(playbook));
    let tag = |name: &str, value: &str| -> Result<serde_json::Value, String> {
        let bytes = serde_json::to_vec(&(name, value)).unwrap_or_default();
        apb_core::run_origin::tag_if_keyed(WORK_PARAM_DOMAIN, &bytes)
            .map(|t| serde_json::json!({ "keyed": t }))
            .ok_or_else(|| {
                "skip_no_key: this installation has no key, so a sensitive input cannot be compared"
                    .to_string()
            })
    };
    let fields = start_request_of(cfg);
    let mut v = serde_json::to_value(&fields).map_err(|e| e.to_string())?;
    let map = v.as_object_mut().expect("a struct serializes as an object");
    for removed in [
        "v",
        "version",
        "worktree",
        "supervision",
        "cache",
        "max_parallel",
        "max_patches_per_run",
        "deadline_ms",
        "budget",
        "unattended",
        "allow_shared_workdir",
        "continued_from",
        "external_data",
        "execution",
    ] {
        map.remove(removed);
    }
    let mut params = serde_json::Map::new();
    for (name, value) in &fields.params {
        let sensitive =
            apb_core::run_params::sensitive(declared.contains(name.as_str()), name, value);
        let entry = match sensitive {
            Some(_) => tag(name, value)?,
            None => serde_json::Value::String(value.clone()),
        };
        params.insert(name.clone(), entry);
    }
    map.insert("params".into(), serde_json::Value::Object(params));
    if let Some(instruction) = &fields.instruction
        && apb_core::run_params::sensitive(false, "instruction", instruction).is_some()
    {
        map.insert("instruction".into(), tag("instruction", instruction)?);
    }
    map.insert(
        "output_limits".into(),
        serde_json::to_value(cfg.output_limits).unwrap_or(serde_json::Value::Null),
    );
    Ok(v)
}

/// The canonical start request of a run, filled from its persisted config:
/// the struct literal, so a field the struct gains is filled here before it
/// compiles.
fn start_request_of(cfg: &RunConfig) -> apb_core::start_key::StartRequestFields {
    apb_core::start_key::StartRequestFields {
        v: apb_core::start_key::REQUEST_V,
        version: None,
        params: cfg.params.clone(),
        instruction: cfg.instruction.clone(),
        worktree: None,
        overrides: cfg
            .overrides
            .as_ref()
            .and_then(|o| serde_json::to_vec(o).ok())
            .map(|b| sha256_hex(&b)),
        execution: None,
        supervision: apb_core::start_key::label(&cfg.mode),
        cache: apb_core::start_key::label(&cfg.cache),
        max_parallel: cfg.max_parallel,
        max_patches_per_run: cfg.max_patches_per_run,
        context_max_bytes: cfg.context_max_bytes,
        context_compact_model: cfg.context_compact_model.clone(),
        deadline_ms: None,
        budget: None,
        unattended: cfg.unattended,
        allow_shared_workdir: cfg.shared_workdir,
        continued_from: cfg.continued_from.clone(),
        external_data: false,
    }
}

/// An invocation as part 4 holds it: whole, without the executable's
/// fingerprint (an agent CLI updated in place is the world changing).
fn invocation_work(inv: &crate::invocation::ResolvedInvocation) -> serde_json::Value {
    let mut v = serde_json::to_value(inv).unwrap_or(serde_json::Value::Null);
    if let Some(m) = v.as_object_mut() {
        m.remove("executable_fingerprint");
    }
    v
}

/// The fields of a resolved profile that are not work: the tier the engine
/// routed an attempt to and the cascade counter (routing follows the
/// configured chain and tiers, which stay inside).
const PROFILE_NOT_WORK: [&str; 2] = ["routed_tier", "cascade"];

/// The executor part of the work of one resolved profile: the manifest's
/// resolved profile whole, minus [`PROFILE_NOT_WORK`], each invocation of
/// its chain and tiers without its executable fingerprint. A field the
/// profile gains later is inside by construction.
pub(crate) fn profile_work(p: &crate::manifest::ManifestProfile) -> serde_json::Value {
    let mut v = serde_json::to_value(p).unwrap_or(serde_json::Value::Null);
    if let Some(m) = v.as_object_mut() {
        for name in PROFILE_NOT_WORK {
            m.remove(name);
        }
        m.insert(
            "chain".into(),
            serde_json::Value::Array(p.chain.iter().map(invocation_work).collect()),
        );
        if let Some(serde_json::Value::Array(tiers)) = m.get_mut("tiers") {
            for (t, tier) in tiers.iter_mut().zip(&p.tiers) {
                if let Some(o) = t.as_object_mut() {
                    o.insert(
                        "invocation".into(),
                        tier.invocation
                            .as_ref()
                            .map(invocation_work)
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
        }
    }
    v
}

/// The connector part of the work at one binding site: each grant with its
/// connector's definition digest and every granted account's identity and
/// snapshotted digest.
fn grants_work(manifest: &crate::manifest::RunExecutionManifest, site: &str) -> serde_json::Value {
    let grants: Vec<serde_json::Value> = manifest
        .grants_for(site)
        .iter()
        .map(|g| {
            let connector = manifest.connector(&g.connector);
            let accounts: Vec<serde_json::Value> = g
                .accounts
                .iter()
                .map(|a| {
                    let digest = connector
                        .and_then(|c| c.accounts.iter().find(|x| &x.name == a))
                        .map(|x| x.digest.clone());
                    serde_json::json!({ "name": a, "digest": digest })
                })
                .collect();
            serde_json::json!({
                "grant": g,
                "connector_digest": connector.map(|c| c.digest.clone()),
                "accounts": accounts,
            })
        })
        .collect();
    serde_json::Value::Array(grants)
}

/// Whether `node` reads anything the manifest resolved: an executor or a
/// connector at the node or at its item check.
fn needs_manifest(node: &Node) -> bool {
    matches!(node.kind, NodeKind::AgentTask { .. })
        || !node.kind.connector_bindings().is_empty()
        || node
            .list()
            .and_then(|l| l.check.as_ref())
            .is_some_and(|c| c.connector.is_some())
}

/// Part 4 as the run is bound now.
fn resolved_work(run_dir: &Path, node: &Node) -> Result<serde_json::Value, String> {
    let manifest =
        crate::manifest::read(run_dir).map_err(|e| format!("skip_needs_manifest: {e}"))?;
    let Some(manifest) = manifest else {
        if needs_manifest(node) {
            return Err(format!(
                "skip_needs_manifest: node {} needs the run's manifest and the run has none",
                node.id
            ));
        }
        return Ok(serde_json::Value::String("none".into()));
    };
    let profile = rebind::effective_for_node(run_dir, &manifest, &node.id)
        .map_err(|e| format!("skip_needs_manifest: {e}"))?;
    if profile.is_none() && matches!(node.kind, NodeKind::AgentTask { .. }) {
        return Err(format!(
            "skip_needs_manifest: the manifest resolved no profile for node {}",
            node.id
        ));
    }
    Ok(resolved_with(&manifest, node, profile.as_ref()))
}

/// Part 4 over `manifest` with `profile` as the node's executor (the bound
/// one, or a rebind's candidate).
pub(crate) fn resolved_with(
    manifest: &crate::manifest::RunExecutionManifest,
    node: &Node,
    profile: Option<&crate::manifest::ManifestProfile>,
) -> serde_json::Value {
    serde_json::json!({
        "execution": manifest
            .execution
            .as_ref()
            .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null))
            .unwrap_or_else(|| serde_json::Value::String("none".into())),
        "profile": profile.map(profile_work),
        "node": grants_work(manifest, &node.id),
        "check": grants_work(manifest, &apb_core::list::check_site(&node.id)),
    })
}

/// [`work_inputs`] with part 4 replaced: the fingerprint a rebind to
/// `candidate` would give the node.
pub(crate) fn work_with_profile(
    src: &WorkSource<'_>,
    candidate: &crate::manifest::ManifestProfile,
) -> Result<WorkInputs, String> {
    let mut inputs = work_inputs(src)?;
    let manifest = crate::manifest::read(src.run_dir)
        .map_err(|e| format!("skip_needs_manifest: {e}"))?
        .ok_or_else(|| "skip_needs_manifest: the run has no manifest".to_string())?;
    inputs.resolved = resolved_with(&manifest, src.node, Some(candidate));
    Ok(inputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::cache::CACHE_FORMAT;

    /// A resolved execution in `dir` with no runtimes.
    fn exec_at(dir: &str) -> Result<VisitExecution, String> {
        Ok(VisitExecution {
            dir: PathBuf::from(dir),
            check_dir: PathBuf::from(dir),
            path_env: None,
            check_policy: CheckPolicy {
                guard_tree: true,
                holds_tree: false,
            },
            runtimes: Default::default(),
            mcp_pins: Default::default(),
        })
    }

    /// An absent `PATH` is captured as absent, a value as it is, and one
    /// that is not Unicode refuses the visit's execution instead of
    /// passing for an absent one.
    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_unicode_refuses_the_capture() {
        use std::os::unix::ffi::OsStringExt;
        assert_eq!(captured_path(Ok("/bin".into())), Ok(Some("/bin".into())));
        assert_eq!(captured_path(Err(std::env::VarError::NotPresent)), Ok(None));
        let raw = std::ffi::OsString::from_vec(b"/opt/\xff/bin".to_vec());
        let why = captured_path(Err(std::env::VarError::NotUnicode(raw))).unwrap_err();
        assert!(why.starts_with("skip_path_unrepresentable"), "{why}");
    }

    fn ev(seq: u64, v: serde_json::Value) -> crate::event::Event {
        let mut v = v;
        v["seq"] = seq.into();
        v["ts"] = 0.into();
        serde_json::from_value(v).unwrap()
    }

    /// The definition of the work tests: an agent list `l` and three params,
    /// one declared secret.
    const WORK_PLAYBOOK: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nparams:\n  - { name: token, type: text, secret: true }\n  - { name: api_token, type: text }\n  - { name: note, type: text }\n  - { name: city, type: text }\nnodes:\n  - { id: start, type: start }\n  - { id: l, type: agent_task, prompt: p, profile: main, list: { items: { inline: [a] } } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: l }\n  - { from: l, to: done }\n";

    /// A credential-shaped value (a prefixed access token).
    const CREDENTIAL: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

    /// A resolved profile with one chain invocation and one tier.
    fn profile() -> crate::manifest::ManifestProfile {
        let inv = serde_json::json!({
            "agent_id": "claude", "model": "haiku", "spec": { "argv": ["-p", "--", "{prompt}"] },
            "soul_delivery": "native", "canonical_executable": "/opt/claude", "executable_fingerprint": "10:1",
        });
        serde_json::from_value(serde_json::json!({
            "scope": "project", "name": "main", "profile_digest": "sha256:p", "bundle_digest": "sha256:bundle",
            "soul": "", "soul_requirement": "any", "skills": [], "chain": [inv.clone()],
            "tiers": [{ "name": "cheap", "for": "routine", "invocation": inv }],
            "routed_tier": "cheap", "cascade": 2,
        }))
        .unwrap()
    }

    /// A manifest for node `l`: execution mode `mode`, the profile, and one
    /// granted connector account whose snapshotted digest is `account`.
    fn manifest_of(mode: &str, account: &str) -> crate::manifest::RunExecutionManifest {
        let p = profile();
        let mut m = crate::manifest::RunExecutionManifest {
            execution: Some(serde_json::from_value(serde_json::json!({ "mode": mode })).unwrap()),
            node_bindings: [("l".to_string(), p.key())].into(),
            profiles: vec![p],
            ..Default::default()
        };
        m.connectors.push(crate::manifest::ManifestConnector {
            doubled_accounts: Vec::new(),
            name: "slack".into(),
            digest: "sha256:connector".into(),
            accounts: vec![crate::manifest::ManifestAccount {
                name: "acme".into(),
                default: true,
                fields: [("workspace".to_string(), "acme".to_string())].into(),
                env: BTreeMap::new(),
                cmd: BTreeMap::new(),
                digest: account.into(),
            }],
        });
        m.connector_grants.insert(
            "l".into(),
            vec![crate::manifest::ManifestConnectorGrant {
                connector: "slack".into(),
                accounts: vec!["acme".into()],
                functions: vec!["post".into()],
                max_calls: None,
                on_unknown: None,
            }],
        );
        m
    }

    fn decisions() -> apb_core::decisions::EffectiveDecisions {
        serde_json::from_value(serde_json::json!({
            "mode": "enforce", "timeout_ms": 1000, "providers": [],
            "budget": {}, "privacy": {}, "uses": {},
        }))
        .unwrap()
    }

    /// A run directory with the definition, the journal's definition and
    /// scripts pin, and `manifest` written.
    fn run_with(
        manifest: &crate::manifest::RunExecutionManifest,
    ) -> (tempfile::TempDir, Vec<crate::event::Event>) {
        let run = tempfile::tempdir().unwrap();
        std::fs::write(run.path().join("playbook.yaml"), WORK_PLAYBOOK).unwrap();
        crate::manifest::write(run.path(), manifest).unwrap();
        let events = vec![
            ev(
                0,
                serde_json::json!({ "type": "run_started", "playbook": "p", "version": "1.0.0", "definition": "sha256:def" }),
            ),
            ev(
                1,
                serde_json::json!({ "type": "run_provenance", "scripts_digest": "sha256:scripts" }),
            ),
        ];
        (run, events)
    }

    /// The work fingerprint of the run under `manifest` and `cfg`.
    fn fingerprint_of(manifest: &crate::manifest::RunExecutionManifest, cfg: &RunConfig) -> String {
        let (run, events) = run_with(manifest);
        let playbook = Playbook::from_yaml(WORK_PLAYBOOK).unwrap();
        let src = WorkSource {
            run_dir: run.path(),
            events: &events,
            playbook: &playbook,
            node: playbook.node("l").unwrap(),
            cfg,
            decisions: None,
            exec: &exec_at("/tree"),
        };
        work_fingerprint(&work_inputs(&src).unwrap())
    }

    /// The previous success ran in `cli` mode, the next run in `host` mode
    /// with every other input equal: the work fingerprint differs, so every
    /// item would run again. At the fingerprint: host mode refuses list
    /// nodes at start (`scheduler/prepare.rs`, "list nodes need execution:
    /// cli"), so no run can show it end to end.
    #[test]
    fn a_cli_to_host_change_changes_the_work_fingerprint() {
        let cfg = RunConfig::default();
        let cli = fingerprint_of(&manifest_of("cli", "sha256:a1"), &cfg);
        assert_eq!(cli, fingerprint_of(&manifest_of("cli", "sha256:a1"), &cfg));
        assert_ne!(cli, fingerprint_of(&manifest_of("host", "sha256:a1"), &cfg));
        assert_ne!(
            cli,
            fingerprint_of(&manifest_of("cli+guard", "sha256:a1"), &cfg)
        );
    }

    /// A granted account's field changes its snapshotted digest, and the
    /// work fingerprint with it (the table's account row: the agent stub
    /// calls no connector).
    #[test]
    fn a_granted_account_field_changes_the_work_fingerprint() {
        let cfg = RunConfig::default();
        assert_ne!(
            fingerprint_of(&manifest_of("cli", "sha256:a1"), &cfg),
            fingerprint_of(&manifest_of("cli", "sha256:a2"), &cfg)
        );
    }

    /// Every part of the work over its serialized form, each with a value:
    /// the definition, the pin, the request with its removals, the manifest's
    /// execution block, the profile whole without its executable fingerprint
    /// and its routing fields, the grants with the account digest, the
    /// decision settings, and the tree.
    #[test]
    fn work_inputs_takes_each_part() {
        let (run, events) = run_with(&manifest_of("cli", "sha256:a1"));
        let playbook = Playbook::from_yaml(WORK_PLAYBOOK).unwrap();
        let cfg = RunConfig {
            params: [("city".to_string(), "Lyon".to_string())].into(),
            instruction: Some("produce summaries".into()),
            context_max_bytes: Some(4096),
            max_parallel: Some(3),
            continued_from: Some("older".into()),
            ..RunConfig::default()
        };
        let d = decisions();
        let src = WorkSource {
            run_dir: run.path(),
            events: &events,
            playbook: &playbook,
            node: playbook.node("l").unwrap(),
            cfg: &cfg,
            decisions: Some(&d),
            exec: &exec_at("/src/tree"),
        };
        let v = serde_json::to_value(work_inputs(&src).unwrap()).unwrap();
        assert_eq!(v["definition"], "sha256:def");
        assert_eq!(v["scripts"], "sha256:scripts");
        let req = &v["request"];
        assert_eq!(req["params"]["city"], "Lyon");
        assert_eq!(req["instruction"], "produce summaries");
        assert_eq!(req["context_max_bytes"], 4096);
        assert!(req.get("output_limits").is_some());
        for removed in [
            "v",
            "version",
            "worktree",
            "supervision",
            "cache",
            "max_parallel",
            "max_patches_per_run",
            "deadline_ms",
            "budget",
            "unattended",
            "allow_shared_workdir",
            "continued_from",
            "external_data",
            "execution",
        ] {
            assert!(req.get(removed).is_none(), "{removed} is removed: {req}");
        }
        let resolved = &v["resolved"];
        assert_eq!(resolved["execution"]["mode"], "cli");
        let profile = &resolved["profile"];
        assert_eq!(profile["bundle_digest"], "sha256:bundle");
        assert_eq!(profile["chain"][0]["canonical_executable"], "/opt/claude");
        assert!(profile["chain"][0].get("executable_fingerprint").is_none());
        assert!(
            profile["tiers"][0]["invocation"]
                .get("executable_fingerprint")
                .is_none()
        );
        assert!(profile.get("routed_tier").is_none() && profile.get("cascade").is_none());
        assert_eq!(resolved["node"][0]["accounts"][0]["digest"], "sha256:a1");
        assert_eq!(resolved["node"][0]["connector_digest"], "sha256:connector");
        assert_eq!(v["decisions"]["mode"], "enforce");
        assert_eq!(v["target"], "/src/tree");
    }

    /// Each case `run_params::sensitive` names (a declared secret, a
    /// secret-shaped name, a credential-shaped value) and an instruction
    /// that looks like a credential enter the work only as the
    /// installation-keyed tag: never the value, never its bare hash. Without
    /// a key the work does not compute (`skip_no_key`) and no key is made.
    #[test]
    fn sensitive_inputs_enter_only_as_keyed_tags() {
        apb_core::process_mode::testing::in_plain_child(
            "scheduler::cache::tests::sensitive_inputs_enter_only_as_keyed_tags",
            || {
                let cfg_dir = tempfile::tempdir().unwrap();
                // SAFETY: a child process runs this one test on one thread.
                unsafe { std::env::set_var("APB_CONFIG_DIR", cfg_dir.path()) };
                let values = [
                    ("token", "declared-secret-value"),
                    ("api_token", "named-secret-value"),
                    ("note", CREDENTIAL),
                ];
                assert!(apb_core::run_params::sensitive(true, "token", values[0].1).is_some());
                assert!(apb_core::run_params::sensitive(false, "api_token", values[1].1).is_some());
                assert!(apb_core::run_params::sensitive(false, "note", CREDENTIAL).is_some());
                let cfg = RunConfig {
                    params: values
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                    instruction: Some(CREDENTIAL.into()),
                    ..RunConfig::default()
                };
                let (run, events) = run_with(&manifest_of("cli", "sha256:a1"));
                let playbook = Playbook::from_yaml(WORK_PLAYBOOK).unwrap();
                let src = WorkSource {
                    run_dir: run.path(),
                    events: &events,
                    playbook: &playbook,
                    node: playbook.node("l").unwrap(),
                    cfg: &cfg,
                    decisions: None,
                    exec: &exec_at("/tree"),
                };
                // No installation key: refused, and none created.
                let no_key = work_inputs(&src).unwrap_err();
                assert!(no_key.starts_with("skip_no_key"), "{no_key}");
                assert!(
                    std::fs::read_dir(cfg_dir.path()).map_or(0, |d| d.count()) == 0,
                    "no key is created"
                );
                apb_core::run_origin::ensure_key().unwrap();
                let v = serde_json::to_value(work_inputs(&src).unwrap()).unwrap();
                let req = &v["request"];
                for (name, _) in values {
                    assert!(req["params"][name]["keyed"].is_string(), "{name}: {req}");
                }
                assert!(req["instruction"]["keyed"].is_string(), "{req}");
                let text = v.to_string();
                assert!(!text.contains(CREDENTIAL), "the instruction never appears");
                for (_, value) in values {
                    assert!(!text.contains(value), "the value never appears");
                    let bare = sha256_hex(value.as_bytes());
                    assert!(
                        !text.contains(bare.trim_start_matches("sha256:")),
                        "never its bare hash"
                    );
                }
            },
        );
    }

    /// The directory a node executes in, as its work holds it: the tree the
    /// run falls back to when it has no worktree (the project root),
    /// canonical, and a node's own `workdir` rendered over it; a template
    /// that renders to no directory refuses the work.
    #[test]
    fn the_execution_dir_is_the_tree_or_the_nodes_own_workdir() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("sub-a")).unwrap();
        std::fs::create_dir_all(root.path().join("sub-b")).unwrap();
        let run = tempfile::tempdir().unwrap();
        let yaml = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nparams:\n  - { name: dir, type: text }\nnodes:\n  - { id: start, type: start }\n  - { id: plain, type: script, script: s.sh, runner: sh, list: { items: { inline: [a] } } }\n  - { id: own, type: script, script: s.sh, runner: sh, workdir: \"{{params.dir}}\", list: { items: { inline: [a] } } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: plain }\n  - { from: plain, to: own }\n  - { from: own, to: done }\n";
        let pb = Playbook::from_yaml(yaml).unwrap();
        let state = RunState::default();
        let at = |node: &str, dir: &str| {
            let cfg = RunConfig {
                params: [("dir".to_string(), dir.to_string())].into(),
                ..RunConfig::default()
            };
            execution_dir(&pb, node, root.path(), run.path(), "r", &state, &cfg)
        };
        let canon = |p: &Path| p.canonicalize().unwrap();
        assert_eq!(at("plain", "sub-a").unwrap(), canon(root.path()));
        assert_eq!(
            at("own", "sub-a").unwrap(),
            canon(&root.path().join("sub-a"))
        );
        assert_ne!(at("own", "sub-a").unwrap(), at("own", "sub-b").unwrap());
        assert!(
            at("own", "missing")
                .unwrap_err()
                .starts_with("skip_workdir_unresolved")
        );
    }

    /// The MCP approval of a granted account is part of the work: the
    /// visit's resolved execution reads its `pin_digest` from the machine's pin
    /// store, and another approval of the same account gives another work.
    #[test]
    fn a_granted_accounts_mcp_pin_is_part_of_the_work() {
        apb_core::process_mode::testing::in_plain_child(
            "scheduler::cache::tests::a_granted_accounts_mcp_pin_is_part_of_the_work",
            || {
                let cfg_dir = tempfile::tempdir().unwrap();
                // SAFETY: a child process runs this one test on one thread.
                unsafe { std::env::set_var("APB_CONFIG_DIR", cfg_dir.path()) };
                let pin = |digest: &str| {
                    let store = serde_json::json!({ "schema_version": 1, "servers": { "slack/acme": {
                        "connector_digest": "sha256:c", "account_digest": "sha256:a", "command": "srv",
                        "program": "/opt/srv", "program_digest": "sha256:p", "tools": {},
                        "pin_digest": digest, "approved_at_ms": 1 } } });
                    std::fs::write(
                        cfg_dir
                            .path()
                            .join(apb_core::connector::mcp::pins::PIN_STORE_FILE),
                        store.to_string(),
                    )
                    .unwrap();
                };
                let (run, events) = run_with(&manifest_of("cli", "sha256:a1"));
                let playbook = Playbook::from_yaml(WORK_PLAYBOOK).unwrap();
                let node = playbook.node("l").unwrap();
                let cfg = RunConfig::default();
                let work = || {
                    let mut exec = exec_at("/tree").unwrap();
                    exec.mcp_pins = mcp_pins(run.path(), node).unwrap();
                    let exec = Ok(exec);
                    let src = WorkSource {
                        run_dir: run.path(),
                        events: &events,
                        playbook: &playbook,
                        node,
                        cfg: &cfg,
                        decisions: None,
                        exec: &exec,
                    };
                    work_fingerprint(&work_inputs(&src).unwrap())
                };
                pin("sha256:pin-1");
                assert_eq!(
                    mcp_pins(run.path(), node).unwrap().get("slack/acme"),
                    Some(&Some("sha256:pin-1".to_string()))
                );
                let first = work();
                assert_eq!(first, work());
                pin("sha256:pin-2");
                assert_ne!(first, work(), "another approval is other work");
            },
        );
    }

    /// A param the run's secret set holds (the manifest's `secret_params`,
    /// kept from an earlier declaration the definition in force no longer
    /// makes) enters the work as a keyed tag, as every redaction of the run
    /// treats it; a secret set that does not read refuses the work.
    #[test]
    fn the_runs_secret_set_classifies_the_params() {
        apb_core::process_mode::testing::in_plain_child(
            "scheduler::cache::tests::the_runs_secret_set_classifies_the_params",
            || {
                let cfg_dir = tempfile::tempdir().unwrap();
                // SAFETY: a child process runs this one test on one thread.
                unsafe { std::env::set_var("APB_CONFIG_DIR", cfg_dir.path()) };
                apb_core::run_origin::ensure_key().unwrap();
                let mut manifest = manifest_of("cli", "sha256:a1");
                manifest.secret_params = vec!["city".into()];
                let (run, events) = run_with(&manifest);
                let playbook = Playbook::from_yaml(WORK_PLAYBOOK).unwrap();
                let cfg = RunConfig {
                    params: [("city".to_string(), "Lyon".to_string())].into(),
                    ..RunConfig::default()
                };
                let src = WorkSource {
                    run_dir: run.path(),
                    events: &events,
                    playbook: &playbook,
                    node: playbook.node("l").unwrap(),
                    cfg: &cfg,
                    decisions: None,
                    exec: &exec_at("/tree"),
                };
                let v = serde_json::to_value(work_inputs(&src).unwrap()).unwrap();
                assert!(v["request"]["params"]["city"]["keyed"].is_string(), "{v}");
                assert!(!v.to_string().contains("Lyon"));
                std::fs::write(run.path().join("playbook.yaml"), "params: [ not a list").unwrap();
                let err = work_inputs(&src).unwrap_err();
                assert!(err.starts_with("skip_secret_set_unreadable"), "{err}");
            },
        );
    }

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

    /// A scoped node (spec node-contracts D3) and a proving node (D4) get
    /// no cache context, on the sequential probe and on a batch member's
    /// alike: a hit would settle either with no scope check or no proof in
    /// this run, even when validation was skipped. A plain node does.
    #[test]
    fn prepare_gives_a_scoped_or_proving_node_no_context() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = RunConfig {
            cache: CacheRunMode::Auto,
            ..RunConfig::default()
        };
        for (extra, participates) in [
            ("", true),
            (", writes: [\"src/**\"]", false),
            (", max_changed_files: 3", false),
            (
                ", success_check: { script: scripts/x.sh, prove_fails_first: true }",
                false,
            ),
        ] {
            let pb = apb_core::schema::Playbook::from_yaml(&format!(
                "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: {{ profile: x }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: do, cache: auto{extra} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: done }}\n"
            ))
            .unwrap();
            for batch_member in [false, true] {
                let ctx = prepare(
                    &pb,
                    "w",
                    dir.path(),
                    dir.path(),
                    &cfg,
                    Some("do"),
                    Some("bundle"),
                    Some(("claude", "haiku")),
                    Vec::new(),
                    ProbeContext {
                        pre_fingerprint: Some("fp"),
                        batch_member,
                        store_root: None,
                        declared_key: None,
                    },
                );
                assert_eq!(
                    ctx.is_some(),
                    participates,
                    "{extra:?} batch_member={batch_member}"
                );
            }
        }
    }
}
