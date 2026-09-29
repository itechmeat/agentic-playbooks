//! The run policy gate (spec 9): the ONE pre-start check every launch
//! surface runs - MCP `playbook_run`, the dashboard's `POST
//! /api/playbooks/{id}/run`, and the CLI's `apb run` / `apb run --supervise`.
//! Checks lifecycle (draft/retired), `requires` applicability, digest-based
//! playbook and profile-bundle trust, connector and account trust, and the
//! sub-playbook tree, and returns a [`RunPermit`] the caller hands to the
//! engine verbatim ([`RunPermit::apply`]). A refusal is structural JSON the
//! surface passes on as-is.
//!
//! The surfaces differ in exactly one knob, `acknowledge_untrusted`: an agent
//! (MCP) must confirm with the user first and passes it only after that; a
//! person clicking Run in the dashboard or typing `apb run` IS that
//! confirmation, so those surfaces pass `true`. Connector and account trust
//! ignore the knob on every surface (secret egress).

use std::path::Path;

use crate::run_config::ChildExpectation;
use apb_core::connector::config::account_digest;
use apb_core::connector::resolve::resolve_playbook;
use apb_core::connector::secrets::missing_vars;
use apb_core::profile::ProfileScope;
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::registry::Registry;
use apb_core::schema::{Effect, NodeKind, Playbook};
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::trust::{Lifecycle, TrustStore, account_trust_id, read_lifecycle};
use serde_json::{Value, json};

/// String name of an effect, for plans/catalog.
pub fn effect_str(e: &Effect) -> &'static str {
    match e {
        Effect::FsRead => "fs_read",
        Effect::FsWrite => "fs_write",
        Effect::Network => "network",
        Effect::External => "external",
        Effect::Secrets => "secrets",
        Effect::Irreversible => "irreversible",
    }
}

/// Preflight facts for the two-phase contract (spec 7).
pub struct Preflight {
    pub version: String,
    pub digest: String,
    pub effects: Vec<String>,
    /// The sub-playbook tree the plan will run, keyed by the parent's
    /// playbook-node id, so the consent surface can show every child and its
    /// trust, not only the parent's.
    pub children: std::collections::BTreeMap<String, ChildExpectation>,
}

/// Preflight of the definition in a given root: lifecycle (draft/retired are rejected)
/// and `requires` applicability. Without trust- and cross-workspace checks - this
/// is the lower layer shared by the local gate and the cross-workspace plan.
pub fn preflight(root: &Path, id: &str, version: Option<&str>) -> Result<Preflight, Value> {
    let reg = Registry::open(root)
        .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
    let loaded = reg
        .load(id, version)
        .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
    let playbook_dir = root.join(".apb/playbooks").join(id);
    check_lifecycle(&playbook_dir, id)?;
    if let Some(req) = &loaded.playbook.requires {
        check_requires(root, req, id)?;
    }
    // The consent surface shows the WHOLE tree's effects at parent start (spec
    // C): the parent's effective effects UNION every pinned child's, recursively.
    // Reuse the same walk `check_run` uses so both derive the identical union
    // from one resolution. A cross-workspace playbook is always project-scoped
    // here; `acknowledge_untrusted: true` skips trust marking, keeping preflight
    // read-only: trust for the parent AND every child is enforced when the plan
    // executes, by running `check_run` in the target workspace.
    let origin = Origin::Project { workspace_id: None };
    let tree = resolve_tree(root, &loaded.playbook, &origin, id, true)?;
    let effects = tree
        .effects
        .iter()
        .map(|e| effect_str(e).to_string())
        .collect();
    Ok(Preflight {
        version: loaded.version.clone(),
        digest: loaded
            .trust_digest()
            .map_err(|e| json!({ "policy": "definition_unreadable", "detail": e.to_string() }))?,
        effects,
        children: tree.children,
    })
}

/// Permission to run, assembled in ONE pass of the trust check: the digest
/// of the definition and the exact map of verified profile bundles. The caller passes
/// EXACTLY this map to the engine (`expected_*`), without recomputing it separately -
/// otherwise editing a profile/skill in the window between the check and the recomputation
/// would give the engine a different set (TOCTOU).
#[derive(Debug, Clone)]
pub struct RunPermit {
    pub playbook_digest: String,
    pub profile_bundles: std::collections::BTreeMap<String, String>,
    /// Verified sub-playbook pins, keyed by THIS playbook's playbook-node id
    /// (spec C). The engine receives it verbatim and rejects drift.
    pub children: std::collections::BTreeMap<String, ChildExpectation>,
    /// Verified connector tree digests, `connector name -> tree digest` (spec
    /// 6 step 1). Covers every connector THIS playbook binds. Handed to the
    /// engine verbatim as `expected_connectors`; run-start re-verifies it with
    /// an exact bidirectional key-set match (`snapshot_connectors`).
    pub connectors: std::collections::BTreeMap<String, String>,
    /// Verified connector account digests, `"connector/account" -> account
    /// digest` (spec 6 step 1). Covers EVERY merged account of every connector
    /// the playbook uses, not only node-granted ones: any merged account is
    /// reachable via config-level behavior (default flags, later grant edits),
    /// so trust and drift detection span the full merged set. Handed to the
    /// engine verbatim as `expected_connector_accounts`.
    pub connector_accounts: std::collections::BTreeMap<String, String>,
    /// Non-fatal, consent-time warnings the caller can show the user before the
    /// run starts (finding 11 of issue #42). Currently one per bound connector
    /// that resolved to zero configured accounts: the gate permits it (it is not
    /// a secret-egress problem), but every node bound to it would fail at call
    /// time, so the user is told up front rather than only discovering it mid-run.
    /// Never a refusal channel - a refusal is an `Err(Value)` from the gate.
    pub warnings: Vec<String>,
    // --- 0.24.0 irreversible consent ---
    /// What in the tree declares `irreversible` (`playbook`, `node <id>`,
    /// `sub-playbook node <id>`); empty when the run needs no consent. The
    /// surface checks it with [`RunPermit::consent_refusal`] and the engine
    /// enforces it again at start ([`crate::consent`]).
    pub irreversible: Vec<String>,
    /// The playbook id, for the refusal text.
    pub playbook_id: String,
    // --- end 0.24.0 irreversible consent ---
}

/// The gate for an agent resuming an existing run (MCP `run_resume`). A
/// resume executes what the run directory holds - its playbook snapshot, its
/// scripts copy, its manifest - so it gets the consent a start gets:
/// - the directory must carry this installation's stamp
///   ([`apb_core::run_origin`]): a run directory that came with a repository
///   is refused outright (`run_not_created_locally`), acknowledged or not;
/// - the snapshot's digest (its `playbook.yaml` plus its `scripts/`, the same
///   [`apb_core::scope::definition_digest`] a start pins) must be approved,
///   unless the caller acknowledged after confirming with the user.
pub fn check_resume(root: &Path, run_id: &str, acknowledge_untrusted: bool) -> Result<(), Value> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(json!({ "policy": "not_found", "detail": format!("run `{run_id}`") }));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(json!({ "policy": "not_found", "detail": format!("run `{run_id}`") }));
    }
    if !apb_core::run_origin::verify(&run_dir, run_id) {
        return Err(json!({
            "policy": "run_not_created_locally",
            "run_id": run_id,
            "detail": "this run directory was not created by apb on this machine (it may have come with the repository); it cannot be resumed through MCP. Start the playbook again instead",
        }));
    }
    let yaml = std::fs::read_to_string(run_dir.join("playbook.yaml"))
        .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
    let digest = apb_core::scope::definition_digest(&yaml, &run_dir)
        .map_err(|e| json!({ "policy": "snapshot_unreadable", "detail": e.to_string() }))?;
    let id = apb_core::schema::Playbook::from_yaml(&yaml)
        .map(|p| p.id)
        .unwrap_or_default();
    check_digest_trust(&id, &digest, acknowledge_untrusted)
}

// --- 0.24.0 irreversible consent ---
/// What a surface must show the person before it may consent: the playbook,
/// the trust digest of what will run, and the consent sources. Built from a
/// start's permit ([`RunPermit::consent_need`]) or from a run directory on
/// resume ([`resume_consent_need`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentNeed {
    pub playbook_id: String,
    pub digest: String,
    pub sources: Vec<String>,
}

impl ConsentNeed {
    /// The nonce a confirmation echoes (see [`crate::consent::consent_nonce`]).
    pub fn nonce(&self) -> String {
        crate::consent::consent_nonce(&self.digest, &self.sources)
    }

    /// Checks a confirmation against what this need showed (see
    /// [`crate::consent::check_confirmation`]).
    pub fn check(
        &self,
        confirmation: Option<&crate::consent::Confirmation>,
    ) -> Result<Option<&'static str>, Value> {
        crate::consent::check_confirmation(
            &self.playbook_id,
            &self.digest,
            &self.sources,
            confirmation,
        )
    }
}

/// Whether resuming run `run_id` needs a fresh consent to irreversible
/// effects, and to what. `Ok(None)` when the run's snapshot needs no consent,
/// or its manifest records one that covers the sources and the directory
/// carries this installation's origin stamp. A manifest consent of a run
/// directory apb did not create here is never honoured: it may have come
/// with the repository. Such a consent is dropped from the manifest when the
/// tree needs none, so no sub-playbook inherits it.
pub fn resume_consent_need(root: &Path, run_id: &str) -> Result<Option<ConsentNeed>, Value> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(json!({ "policy": "not_found", "detail": format!("run `{run_id}`") }));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    let yaml = std::fs::read_to_string(run_dir.join("playbook.yaml"))
        .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
    let playbook = Playbook::from_yaml(&yaml)
        .map_err(|e| json!({ "policy": "snapshot_unreadable", "detail": e.to_string() }))?;
    let pins = crate::run_config::read_run_config(&run_dir)
        .ok()
        .and_then(|c| c.expected_children);
    // Children resolve from the run's own origin, as the run spawns them.
    let origin = crate::scheduler::parent_run_origin(&run_dir);
    let sources = consent_sources(root, &playbook, &origin, pins.as_ref());
    let local = apb_core::run_origin::verify(&run_dir, run_id);
    let recorded = crate::manifest::read(&run_dir)
        .ok()
        .flatten()
        .and_then(|m| m.consent)
        .filter(|c| c.irreversible);
    if sources.is_empty() {
        if recorded.is_some() && !local {
            crate::manifest::replace_consent(&run_dir, run_id, None)
                .map_err(|e| json!({ "policy": "manifest_unwritable", "detail": e.to_string() }))?;
        }
        return Ok(None);
    }
    if local && recorded.is_some_and(|c| sources.iter().all(|s| c.sources.contains(s))) {
        return Ok(None);
    }
    let digest = apb_core::scope::definition_digest(&yaml, &run_dir)
        .map_err(|e| json!({ "policy": "snapshot_unreadable", "detail": e.to_string() }))?;
    Ok(Some(ConsentNeed {
        playbook_id: playbook.id,
        digest,
        sources,
    }))
}

/// Records the consent `by` gave to `need` in the manifest of run `run_id`
/// before it resumes, so its sub-playbooks inherit it and a later resume
/// asks no more.
pub fn record_resume_consent(
    root: &Path,
    run_id: &str,
    need: &ConsentNeed,
    by: &str,
) -> Result<(), Value> {
    let run_dir = root.join(".apb/runs").join(run_id);
    let consent = crate::consent::RunConsent {
        sources: need.sources.clone(),
        ..crate::consent::RunConsent::irreversible(by)
    };
    crate::manifest::replace_consent(&run_dir, run_id, Some(consent))
        .map_err(|e| json!({ "policy": "manifest_unwritable", "detail": e.to_string() }))
}

/// The resume gate for irreversible effects in one call, for a surface that
/// already holds the person's confirmation (MCP `run_resume`): nothing to do,
/// the consent recorded (with a deprecation note for a bare `true`), or the
/// structured refusal with the sources and the `consent_nonce`.
pub fn check_resume_consent(
    root: &Path,
    run_id: &str,
    confirmation: Option<&crate::consent::Confirmation>,
    by: &str,
) -> Result<Option<&'static str>, Value> {
    let Some(need) = resume_consent_need(root, run_id)? else {
        return Ok(None);
    };
    let note = need.check(confirmation).map_err(|mut refusal| {
        refusal["resume"] = json!(true);
        refusal
    })?;
    record_resume_consent(root, run_id, &need, by)?;
    Ok(note)
}
// --- end 0.24.0 irreversible consent ---

/// One-pass walk of a playbook's sub-playbook tree (spec C), shared by the local
/// run gate (`check_run`) and the cross-workspace consent surface (`preflight`)
/// so both derive the SAME children pins and recursive effects union from a
/// single resolution instead of duplicating it.
struct TreeResolution {
    /// Node-id -> verified child pin for THIS playbook (recursive).
    children: std::collections::BTreeMap<String, ChildExpectation>,
    /// Union of the parent's effective effects and every pinned child's
    /// effective effects (recursively). Rendered with `effect_str`, matching
    /// `Preflight::effects`.
    effects: std::collections::BTreeSet<Effect>,
    /// `<scope>/<name>` keys of child profile bundles that are not approved
    /// (empty when `acknowledge_untrusted` is set).
    untrusted: Vec<String>,
}

/// The effects of `playbook` and of every sub-playbook it runs, recursively,
/// resolved the way the run gate resolves them (read-only about trust).
/// `None` when the tree does not resolve. The review auto-decision (issue
/// #165 Part 14.4) reads it at run time for an ungated run (no pins) and
/// refuses on `None`; a gated run reads [`pinned_tree_effects`] instead.
pub(crate) fn tree_effects(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
) -> Option<std::collections::BTreeSet<Effect>> {
    resolve_tree(root, playbook, origin, &playbook.id, true)
        .ok()
        .map(|t| t.effects)
}

/// The effects of `playbook` and of every sub-playbook the run's gate pinned
/// (`expected_children`), recursively, each read from its PINNED version: the
/// version a gated run will actually execute, not whatever is current now. A
/// child re-versioned after the gate (a new `current` with other declared
/// effects) must not change what the review auto-decision weighs. `None` when a
/// sub-playbook node has no pin, a pinned version no longer resolves, or its
/// content no longer matches the pinned digest: the caller refuses then
/// (fail-closed), exactly as the run would refuse to spawn that child.
pub(crate) fn pinned_tree_effects(
    root: &Path,
    playbook: &Playbook,
    pins: &std::collections::BTreeMap<String, ChildExpectation>,
) -> Option<std::collections::BTreeSet<Effect>> {
    let mut effects = apb_core::effects::effective(playbook);
    collect_pinned_effects(root, playbook, pins, &mut effects)?;
    Some(effects)
}

/// Walks `playbook`'s sub-playbook nodes along `pins` (finite: the gate built
/// the pin tree with cycle detection, and the walk follows the pins).
fn collect_pinned_effects(
    root: &Path,
    playbook: &Playbook,
    pins: &std::collections::BTreeMap<String, ChildExpectation>,
    effects: &mut std::collections::BTreeSet<Effect>,
) -> Option<()> {
    for n in &playbook.nodes {
        if !matches!(n.kind, NodeKind::Playbook { .. }) {
            continue;
        }
        let pin = pins.get(&n.id)?;
        let loaded = load_pinned(root, pin)?;
        effects.extend(apb_core::effects::effective(&loaded));
        collect_pinned_effects(root, &loaded, &pin.children, effects)?;
    }
    Some(())
}

/// The definition a sub-playbook pin names, at its pinned version; `None`
/// when it no longer resolves or no longer matches the pinned digest.
fn load_pinned(root: &Path, pin: &ChildExpectation) -> Option<Playbook> {
    let origin = match pin.scope {
        ProfileScope::Global => Origin::Global,
        _ => Origin::Project { workspace_id: None },
    };
    let cref = PlaybookRef {
        origin,
        id: pin.id.clone(),
        version: Some(pin.version.clone()),
    };
    let resolved = apb_core::store::resolve(root, &cref).ok()?;
    if resolved.digest != pin.playbook_digest {
        return None;
    }
    Registry::open_dir(&resolved.definition_parent)
        .ok()?
        .load(&resolved.id, Some(&resolved.version))
        .ok()
        .map(|l| l.playbook)
}

// --- 0.24.0 irreversible consent ---
/// The effects of one pinned sub-playbook's tree, at the pinned versions
/// (the consent gate names each irreversible child node).
pub(crate) fn pinned_child_effects(
    root: &Path,
    pin: &ChildExpectation,
) -> Option<std::collections::BTreeSet<Effect>> {
    let loaded = load_pinned(root, pin)?;
    pinned_tree_effects(root, &loaded, &pin.children)
}

/// The effects of the sub-playbook node `node_id` of `playbook`, resolved
/// live the way an ungated run resolves it; `None` when it does not resolve.
/// A scope candidate whose registry or definition fails to load is skipped,
/// not taken as the answer for the whole lookup.
pub(crate) fn live_child_effects(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    node_id: &str,
) -> Option<std::collections::BTreeSet<Effect>> {
    let node = playbook.nodes.iter().find(|n| n.id == node_id)?;
    let NodeKind::Playbook { playbook: pref, .. } = &node.kind else {
        return None;
    };
    for cand in apb_core::scope::scope_candidates(pref.scope, origin) {
        let cref = PlaybookRef {
            origin: cand.clone(),
            id: pref.id.clone(),
            version: None,
        };
        let Ok(resolved) = apb_core::store::resolve(root, &cref) else {
            continue;
        };
        let Some(loaded) = Registry::open_dir(&resolved.definition_parent)
            .ok()
            .and_then(|reg| reg.load(&resolved.id, Some(&resolved.version)).ok())
        else {
            continue;
        };
        return tree_effects(root, &loaded.playbook, &cand);
    }
    None
}

/// The connector functions flagged `irreversible: true` that node `n` is
/// granted, as `node <id> (connector <name>: <fn>, ...)` sources. A
/// connector that is not installed or does not load adds nothing here: the
/// connector trust gate refuses such a run before it starts.
fn connector_sources(n: &apb_core::schema::Node) -> Vec<String> {
    let mut out = Vec::new();
    for b in n.kind.connector_bindings() {
        let Ok(loaded) = apb_core::connector::store::load(&b.name) else {
            continue;
        };
        let irreversible = loaded.doc.irreversible_functions();
        let fns = apb_core::validate::granted_functions(b, &irreversible);
        if !fns.is_empty() {
            out.push(format!(
                "node {} (connector {}: {})",
                n.id,
                b.name,
                fns.iter()
                    .map(|f| f.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    out
}

/// Why a start of `playbook` needs consent: the sources in the playbook
/// itself (its own and its nodes' `irreversible` declarations, and each
/// granted connector function flagged `irreversible: true`) plus
/// `sub-playbook node <id>` for each sub-playbook node whose tree declares
/// `irreversible`. Empty when the run needs no consent. With pins (a gated
/// run) each child is read at its pinned version; without them (an ungated
/// start) as it resolves now; an unpinned child that does not resolve counts
/// as needing consent (fail closed at start rather than halfway through).
pub fn consent_sources(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    pins: Option<&std::collections::BTreeMap<String, ChildExpectation>>,
) -> Vec<String> {
    let mut out = crate::consent::own_sources(playbook);
    for n in &playbook.nodes {
        out.extend(connector_sources(n));
        if !matches!(n.kind, NodeKind::Playbook { .. }) {
            continue;
        }
        let child_irreversible = match pins {
            // A gated run never starts a child without a pin that loads (the
            // spawn fails closed), so a missing pin adds nothing here.
            Some(pins) => pins.get(&n.id).is_some_and(|pin| {
                pinned_child_effects(root, pin).is_some_and(|e| e.contains(&Effect::Irreversible))
            }),
            // An ungated run resolves the child live when it gets there; one
            // that does not resolve now counts as needing consent.
            None => live_child_effects(root, playbook, origin, &n.id)
                .is_none_or(|e| e.contains(&Effect::Irreversible)),
        };
        if child_irreversible {
            out.push(format!("sub-playbook node {}", n.id));
        }
    }
    out
}

// --- end 0.24.0 irreversible consent ---

/// Seeds the effects union and cycle path with the parent itself, then walks and
/// verifies its sub-playbook tree once. `parent_id`/`origin` identify the parent
/// for cycle detection and `auto` scope resolution of its children.
fn resolve_tree(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    parent_id: &str,
    acknowledge_untrusted: bool,
) -> Result<TreeResolution, Value> {
    let mut effects: std::collections::BTreeSet<Effect> = apb_core::effects::effective(playbook);
    let parent_scope = if matches!(origin, Origin::Global) {
        "global"
    } else {
        "project"
    };
    let mut path: Vec<(String, String)> = vec![(parent_scope.to_string(), parent_id.to_string())];
    let mut untrusted: Vec<String> = Vec::new();
    let children = collect_children(
        root,
        playbook,
        origin,
        acknowledge_untrusted,
        &mut path,
        &mut untrusted,
        &mut effects,
    )?;
    Ok(TreeResolution {
        children,
        effects,
        untrusted,
    })
}

impl RunPermit {
    /// Hands the permit to the engine verbatim: the digest, the verified
    /// profile bundles, the child pins and the connector maps become the
    /// run's `expected_*` pins, so the engine refuses any drift between this
    /// check and the run's snapshot (anti-TOCTOU). The one exception is the
    /// profile-bundle map of a run with non-empty `overrides`: the gate sees
    /// the definition, not the ephemeral executor, so combining the two would
    /// be a false key-set mismatch (see the invariant in `build_run_manifest`);
    /// such a run keeps every other pin.
    pub fn apply(self, opts: &mut crate::RunOptions) {
        let has_overrides = opts.overrides.as_ref().is_some_and(|o| !o.is_empty());
        opts.expected_digest = Some(self.playbook_digest);
        opts.expected_profile_bundles = (!has_overrides).then_some(self.profile_bundles);
        opts.expected_children = Some(self.children);
        opts.expected_connectors = self.connectors;
        opts.expected_connector_accounts = self.connector_accounts;
    }

    // --- 0.24.0 irreversible consent ---
    /// Whether a start of this tree needs consent to irreversible effects.
    pub fn needs_consent(&self) -> bool {
        !self.irreversible.is_empty()
    }

    /// What a surface must show before it may consent; `None` when the run
    /// needs no consent.
    pub fn consent_need(&self) -> Option<ConsentNeed> {
        self.needs_consent().then(|| ConsentNeed {
            playbook_id: self.playbook_id.clone(),
            digest: self.playbook_digest.clone(),
            sources: self.irreversible.clone(),
        })
    }

    /// The nonce a confirmation echoes to bind the consent to this tree
    /// (see [`crate::consent::consent_nonce`]).
    pub fn consent_nonce(&self) -> String {
        crate::consent::consent_nonce(&self.playbook_digest, &self.irreversible)
    }

    /// Checks a surface's confirmation against this tree: `Ok(None)` when the
    /// run needs no consent or the confirmation echoes this tree's nonce,
    /// `Ok(Some(note))` for a deprecated bare `true`, and the structured
    /// refusal (`policy: irreversible_requires_confirmation`, the sources,
    /// the `consent_nonce`, what to do) otherwise.
    pub fn check_confirmation(
        &self,
        confirmation: Option<&crate::consent::Confirmation>,
    ) -> Result<Option<&'static str>, Value> {
        if !self.needs_consent() {
            return Ok(None);
        }
        crate::consent::check_confirmation(
            &self.playbook_id,
            &self.playbook_digest,
            &self.irreversible,
            confirmation,
        )
    }

    /// The structured refusal for a start of an irreversible tree without
    /// consent; `Ok` when the run needs no consent or has it. Unlike
    /// [`RunPermit::check_confirmation`] it takes a consent already granted
    /// (the engine's own view), so no nonce is checked.
    pub fn consent_refusal(
        &self,
        consent: Option<&crate::consent::RunConsent>,
    ) -> Result<(), Value> {
        if !self.needs_consent() || consent.is_some_and(|c| c.irreversible) {
            return Ok(());
        }
        self.check_confirmation(None).map(|_| ())
    }
    // --- end 0.24.0 irreversible consent ---
}

/// Checks whether a run is permitted. `Ok(RunPermit)` - the run may proceed (digest +
/// verified bundle map); `Err(value)` - a structural policy refusal.
/// `supervised` - whether the run will actually spawn an EXTERNAL supervisor
/// agent (CLI `--supervise`). Only then does the supervisor profile enter the
/// verified bundle set (and the engine snapshot), matching the manifest. All
/// current MCP paths (autonomous, supervise:"self") do not spawn an external supervisor
/// agent - they pass `false`.
pub fn check_run(
    root: &Path,
    wref: &PlaybookRef,
    acknowledge_untrusted: bool,
    supervised: bool,
) -> Result<RunPermit, Value> {
    // Cross-workspace: a direct run in a foreign workspace is forbidden, only through
    // the two-phase contract (spec 7). This path bypasses it, hence the refusal.
    if matches!(
        wref.origin,
        Origin::Project {
            workspace_id: Some(_)
        }
    ) {
        return Err(json!({
            "policy": "cross_workspace_requires_plan",
            "detail": "use playbook_prepare_run / playbook_execute_plan for another workspace",
        }));
    }

    let definition_parent = match &wref.origin {
        Origin::Global => match apb_core::store::global_playbooks_parent() {
            Some(p) => p,
            None => return Err(json!({ "policy": "not_found", "detail": "no global config dir" })),
        },
        Origin::Project { .. } => root.join(".apb"),
    };
    let reg = match Registry::open_dir(&definition_parent) {
        Ok(r) => r,
        Err(e) => return Err(json!({ "policy": "not_found", "detail": e.to_string() })),
    };
    let loaded = match reg.load(&wref.id, wref.version.as_deref()) {
        Ok(l) => l,
        Err(e) => return Err(json!({ "policy": "not_found", "detail": e.to_string() })),
    };

    // Lifecycle: draft/retired does not run through the normal path - only via trial.
    let playbook_dir = definition_parent.join("playbooks").join(&wref.id);
    check_lifecycle(&playbook_dir, &wref.id)?;

    // Digest-based trust: unapproved content requires an explicit acknowledge.
    let digest = loaded
        .trust_digest()
        .map_err(|e| json!({ "policy": "definition_unreadable", "detail": e.to_string() }))?;
    check_run_loaded(
        root,
        wref,
        &loaded,
        digest.clone(),
        acknowledge_untrusted,
        supervised,
    )
    .map_err(|refusal| with_consent_hint(root, &loaded.playbook, &wref.origin, &digest, refusal))
}

/// A trust refusal of a tree that also needs consent to irreversible
/// effects names those sources and their `consent_nonce` too, so a host asks
/// the person one question that covers both and passes both answers
/// (`acknowledge_untrusted` and `confirm_irreversible`) in one retry. The
/// sources are resolved live, since the tree's pins are not verified yet.
fn with_consent_hint(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    digest: &str,
    mut refusal: Value,
) -> Value {
    let is_trust = refusal
        .get("policy")
        .and_then(Value::as_str)
        .is_some_and(|p| p.starts_with("untrusted_") && p.ends_with("_requires_acknowledge"));
    if !is_trust {
        return refusal;
    }
    let sources = consent_sources(root, playbook, origin, None);
    if !sources.is_empty() {
        refusal["irreversible"] = json!(sources);
        refusal["consent_nonce"] = json!(crate::consent::consent_nonce(digest, &sources));
    }
    refusal
}

/// The rest of [`check_run`] once the definition is loaded and its digest known.
fn check_run_loaded(
    root: &Path,
    wref: &PlaybookRef,
    loaded: &apb_core::registry::LoadedPlaybook,
    digest: String,
    acknowledge_untrusted: bool,
    supervised: bool,
) -> Result<RunPermit, Value> {
    check_digest_trust(&wref.id, &digest, acknowledge_untrusted)?;

    // Profile bundle trust (spec 5.1): the profile plus the actual content of its
    // skills are trusted as a unit. An unapproved bundle requires acknowledge.
    // The returned map is exactly what was verified, and it is what goes to the engine.
    let profile_bundles = check_profile_bundles(
        root,
        &loaded.playbook,
        &wref.origin,
        acknowledge_untrusted,
        supervised,
    )?;

    // Connector trust (spec 6 step 1, 7) for this playbook, then the
    // sub-playbook pins (spec C) - ONE walk, see `walk_connectors_and_tree`.
    let GateWalk {
        connectors,
        connector_accounts,
        warnings: connector_warnings,
        tree,
    } = walk_connectors_and_tree(
        root,
        &loaded.playbook,
        &wref.origin,
        &wref.id,
        acknowledge_untrusted,
    )?;
    if !tree.untrusted.is_empty() {
        return Err(json!({
            "policy": "untrusted_profile_requires_acknowledge",
            "profiles": tree.untrusted,
            "detail": "a sub-playbook binds an untrusted profile bundle; run again with acknowledge_untrusted: true after user confirmation",
        }));
    }

    // Applicability preflight (spec 5.2), in the execution root (current project).
    if let Some(req) = &loaded.playbook.requires {
        check_requires(root, req, &wref.id)?;
    }

    let irreversible = consent_sources(root, &loaded.playbook, &wref.origin, Some(&tree.children));
    Ok(RunPermit {
        playbook_digest: digest,
        profile_bundles,
        children: tree.children,
        connectors,
        connector_accounts,
        warnings: connector_warnings,
        irreversible,
        playbook_id: wref.id.clone(),
    })
}

/// One pass of the two walks a run start needs: the connector trust gate for
/// the playbook itself, then its sub-playbook tree, in one refusal order
/// (connector refusals before tree refusals).
struct GateWalk {
    connectors: std::collections::BTreeMap<String, String>,
    connector_accounts: std::collections::BTreeMap<String, String>,
    warnings: Vec<String>,
    tree: TreeResolution,
}

fn walk_connectors_and_tree(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    playbook_id: &str,
    acknowledge_untrusted: bool,
) -> Result<GateWalk, Value> {
    // Connector trust (spec 6 step 1, 7): resolve every bound connector, check
    // env presence, and gate both connector and account digests. Unlike
    // profile/playbook trust, `acknowledge_untrusted` does NOT bypass this -
    // connector and account trust guard secret egress, not content taste.
    // Yields the two permit maps handed to the engine verbatim, plus any
    // consent-time warnings (finding 11: a bound connector with zero accounts).
    let (connectors, connector_accounts, warnings) =
        check_connectors(root, playbook, acknowledge_untrusted)?;
    // Sub-playbook pins (spec C): walk the reference tree in the same pass,
    // detect cycles, and trust-check each child's bundles alongside the
    // parent's. The recursive effects union that this walk also accumulates is
    // the user's consent surface, exposed through `preflight` (which shares the
    // same walk); a run gate only needs the verified pins.
    let tree = resolve_tree(root, playbook, origin, playbook_id, acknowledge_untrusted)?;
    Ok(GateWalk {
        connectors,
        connector_accounts,
        warnings,
        tree,
    })
}

/// The two permit maps plus the consent-time warnings (finding 11) that
/// [`check_connectors`] produces in one pass.
type ConnectorCheck = (
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeMap<String, String>,
    Vec<String>,
);

/// Connector trust gate (spec 6 step 1, 7). For a playbook that binds any
/// connector: resolves every connector once, verifies every required secret
/// env var resolves (early failure per spec 6 step 1), then gates both the
/// connector tree digest and every merged account digest against the trust
/// store. Returns the two permit maps on success:
/// - `connector name -> tree digest`
/// - `"connector/account" -> account digest`, covering EVERY merged account
///   of every connector the playbook uses (not only node-granted ones).
///
/// `acknowledge_untrusted` is accepted for signature symmetry with the other
/// gate checks but is DELIBERATELY NOT consulted: connector and account trust
/// guard secret egress (a foreign `connector.yaml` or a redirected account
/// `base_url` can exfiltrate a token), so unlike playbook/profile trust they
/// are never bypassable by an acknowledge. Approval happens out of band via
/// the trust store (the CLI/UI approve flows).
fn check_connectors(
    root: &Path,
    playbook: &Playbook,
    acknowledge_untrusted: bool,
) -> Result<ConnectorCheck, Value> {
    // Deliberately ignored - see the doc comment above (secret egress).
    let _ = acknowledge_untrusted;

    let binds = playbook
        .nodes
        .iter()
        .any(|n| !n.kind.connector_bindings().is_empty());
    if !binds {
        return Ok((
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            Vec::new(),
        ));
    }

    // 1. Resolve every bound connector against the live files.
    let resolution = match resolve_playbook(root, playbook) {
        Ok(r) => r,
        Err(errors) => {
            return Err(json!({ "policy": "connector_unresolved", "errors": errors }));
        }
    };

    // 2. Env presence over the union of every used connector's required env.
    let mut required: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for resolved in resolution.connectors.values() {
        for var in &resolved.required_env {
            required.insert(var.clone());
        }
    }
    let required: Vec<String> = required.into_iter().collect();
    let missing = missing_vars(root, &required);
    if !missing.is_empty() {
        return Err(json!({ "policy": "connector_env_missing", "missing": missing }));
    }

    // 3. + 4. Trust: connector tree digest first (a changed folder is a bigger
    // deal than an account), then every merged account digest.
    let store = TrustStore::load();
    let mut connectors: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut accounts: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut untrusted_connectors: Vec<String> = Vec::new();
    let mut unapproved_accounts: Vec<String> = Vec::new();
    let mut account_fields = serde_json::Map::new();
    let mut warnings: Vec<String> = Vec::new();

    for (name, resolved) in &resolution.connectors {
        let digest = resolved.loaded.digest.clone();
        if !store.is_approved(&digest) {
            untrusted_connectors.push(name.clone());
        }
        connectors.insert(name.clone(), digest);

        // Zero-account connector (finding 11 of issue #42; enriched by finding 1
        // of issue #56): an empty `expected_connector_accounts` for a bound
        // connector passes the gate silently today and every node bound to it
        // then fails at call time (nothing to call). Surface it as a
        // consent-time warning rather than a hard refusal - a missing account
        // is a configuration gap, not the secret-egress problem the
        // connector/account trust gate guards. Name the binding node ids and
        // the specific functions those nodes call so a supervisor can see
        // exactly what would fail.
        if resolved.accounts.is_empty() {
            let mut node_ids: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            let mut functions: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            for (node_id, grants) in &resolution.grants {
                for grant in grants {
                    if grant.connector == *name {
                        node_ids.insert(node_id.clone());
                        for f in &grant.functions {
                            functions.insert(f.clone());
                        }
                    }
                }
            }
            if node_ids.is_empty() {
                // Defensive fallback: a bound connector should always have at
                // least one grant, but prefer the connector-only wording over
                // empty brackets if resolution and grants somehow diverge.
                warnings.push(format!(
                    "connector `{name}` is bound but has no configured accounts; nodes that call it will fail until an account is added"
                ));
            } else {
                let nodes = node_ids.into_iter().collect::<Vec<_>>().join(", ");
                let funcs = functions.into_iter().collect::<Vec<_>>().join(", ");
                warnings.push(format!(
                    "connector `{name}` is bound but has no configured accounts; nodes [{nodes}] call functions [{funcs}] and will fail until an account is added"
                ));
            }
        }

        for account in &resolved.accounts {
            let id = account_trust_id(name, &account.name);
            let adigest = account_digest(account);
            if !store.is_approved(&adigest) && !unapproved_accounts.contains(&id) {
                unapproved_accounts.push(id.clone());
                account_fields.insert(id.clone(), account_display(&resolved.loaded.doc, account));
            }
            accounts.insert(id, adigest);
        }
    }

    if !untrusted_connectors.is_empty() {
        return Err(json!({
            "policy": "untrusted_connector_requires_approve",
            "connectors": untrusted_connectors,
            "detail": "approve the connector digest via the approve surface; acknowledge_untrusted does not bypass connector trust",
        }));
    }
    if !unapproved_accounts.is_empty() {
        return Err(json!({
            "policy": "unapproved_connector_account",
            "accounts": unapproved_accounts,
            "fields": Value::Object(account_fields),
            "detail": "approve the account digest via the approve surface; acknowledge_untrusted does not bypass account trust",
        }));
    }

    Ok((connectors, accounts, warnings))
}

/// Non-secret display of an account for an approval prompt (spec 7: the user
/// sees the concrete fields they approve). Every value is safe: a secret-marked
/// field holds only its raw `{{env.VAR}}` or `{{cmd:...}}` reference in the
/// config, never the resolved secret, so the whole `fields` map plus the
/// `default` flag can be shown. This mirrors exactly what the account digest
/// pins. `cmd` names each secret read from a command, with the command line:
/// approving the account authorizes apb to run it.
fn account_display(
    doc: &apb_core::connector::def::ConnectorDoc,
    account: &apb_core::connector::config::Account,
) -> Value {
    let fields: serde_json::Map<String, Value> = account
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    json!({
        "default": account.default,
        "fields": Value::Object(fields),
        "cmd": apb_core::connector::config::cmd_refs(doc, account),
    })
}

/// Recursively collects and verifies the sub-playbook pins of `playbook`.
/// `origin` is where THIS playbook's definition came from (drives `scope: auto`
/// resolution of its children: parent origin first, then global, mirroring
/// profile scope resolution). `path` holds the `(scope, id)` pairs on the
/// current branch for cycle detection; a repeated pair is a cycle. On an
/// untrusted child bundle the key is pushed to `untrusted` (the caller turns a
/// non-empty list into the standard refusal). `effects` accumulates the union of
/// every pinned child's effective effects. Returns the node-id -> ChildExpectation
/// map for `playbook`.
#[allow(clippy::too_many_arguments)]
fn collect_children(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    acknowledge_untrusted: bool,
    path: &mut Vec<(String, String)>,
    untrusted: &mut Vec<String>,
    effects: &mut std::collections::BTreeSet<Effect>,
) -> Result<std::collections::BTreeMap<String, ChildExpectation>, Value> {
    let mut out = std::collections::BTreeMap::new();
    for n in &playbook.nodes {
        let NodeKind::Playbook { playbook: pref, .. } = &n.kind else {
            continue;
        };
        // Scope resolution shared with the engine (`scope_candidates`): an
        // explicit scope pins the origin; `auto` prefers the parent's origin,
        // then global. The first candidate in which the child resolves wins.
        let candidates = apb_core::scope::scope_candidates(pref.scope, origin);
        let mut resolved_opt = None;
        for cand in &candidates {
            let cref = PlaybookRef {
                origin: cand.clone(),
                id: pref.id.clone(),
                version: None,
            };
            if let Ok(r) = apb_core::store::resolve(root, &cref) {
                resolved_opt = Some((cand.clone(), r));
                break;
            }
        }
        let Some((child_origin, resolved)) = resolved_opt else {
            return Err(json!({
                "policy": "not_found",
                "detail": format!(
                    "sub-playbook `{}` (node `{}`) did not resolve in any candidate scope",
                    pref.id, n.id
                ),
            }));
        };
        let scope_str = apb_core::scope::origin_scope_label(&child_origin);
        let pair = (scope_str.to_string(), resolved.id.clone());
        if path.contains(&pair) {
            let mut cycle: Vec<String> = path.iter().map(|(s, i)| format!("{s}/{i}")).collect();
            cycle.push(format!("{scope_str}/{}", resolved.id));
            return Err(json!({ "policy": "sub_playbook_cycle", "cycle": cycle }));
        }
        // Load the child definition to walk its own children + collect bundles.
        let reg = Registry::open_dir(&resolved.definition_parent)
            .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
        let loaded = reg
            .load(&resolved.id, Some(&resolved.version))
            .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
        // Recursive gate (C1): every child runs through the SAME pipeline the
        // parent gets in `check_run` - lifecycle (draft/retired), digest-based
        // trust, and `requires` applicability - so a draft/retired/untrusted or
        // inapplicable child cannot be reached through a parent that passed its
        // own gate. Refusals carry the child id (and digest for trust) so the
        // caller can tell WHICH playbook in the tree refused. Trust is gated by
        // `acknowledge_untrusted` exactly as for the parent, which is how
        // `preflight` (acknowledge = true) still enforces lifecycle/requires on
        // children while staying read-only about trust.
        let child_playbook_dir = resolved
            .definition_parent
            .join("playbooks")
            .join(&resolved.id);
        check_lifecycle(&child_playbook_dir, &resolved.id)?;
        check_digest_trust(&resolved.id, &resolved.digest, acknowledge_untrusted)?;
        if let Some(req) = &loaded.playbook.requires {
            check_requires(root, req, &resolved.id)?;
        }
        // Connector trust for the child, the SAME gate the parent gets in
        // `check_run`: a child binding an untrusted connector (or an
        // unapproved/changed account, or a missing secret env var) is refused
        // here, naming the connector/account. acknowledge_untrusted does NOT
        // bypass it (secret egress). The returned maps are intentionally NOT
        // merged into the parent's permit: the engine verifies EACH run's
        // connector maps with an exact bidirectional key-set match
        // (`snapshot_connectors`), and a sub-playbook child executes as its own
        // run - handing the parent run a child's connector keys would refuse
        // the parent as "expected but unused". Instead the child's OWN verified
        // maps ride the pin (finding 2 of issue #42): they are computed here in
        // the same single gate pass and threaded verbatim into the child spawn's
        // `expected_connectors`/`expected_connector_accounts`, so a sub-playbook
        // that binds connectors is reachable under a gated run. The zero-account
        // warnings are the parent-run consent surface only, so a child's are
        // dropped here.
        let (child_connectors, child_connector_accounts, _child_warnings) =
            check_connectors(root, &loaded.playbook, acknowledge_untrusted)?;
        // Fold this child's effective effects into the consented union.
        effects.extend(apb_core::effects::effective(&loaded.playbook));
        let worigin = if matches!(child_origin, Origin::Global) {
            PlaybookOrigin::Global
        } else {
            PlaybookOrigin::Project
        };
        // Child profile bundles (nodes + finish-with-prompt), trust-checked.
        let mut bundles = std::collections::BTreeMap::new();
        let store = TrustStore::load();
        for r in collect_profile_refs(&loaded.playbook, false) {
            match profile_store::compute_bundle(root, worigin, &r) {
                Ok((lp, _pairs, bundle)) => {
                    let key = format!("{}/{}", profile_store::scope_str(lp.scope), lp.name);
                    if !acknowledge_untrusted
                        && !store.is_approved(&bundle)
                        && !untrusted.contains(&key)
                    {
                        untrusted.push(key.clone());
                    }
                    bundles.insert(key, bundle);
                }
                Err(e) => {
                    return Err(json!({ "policy": "profile_unresolved", "detail": e.to_string() }));
                }
            }
        }
        // Recurse into the child's own sub-playbook nodes on the current branch.
        path.push(pair);
        let grand = collect_children(
            root,
            &loaded.playbook,
            &child_origin,
            acknowledge_untrusted,
            path,
            untrusted,
            effects,
        )?;
        path.pop();

        // Typed scope (review I2): the pin records the resolved origin, never
        // `Auto`. Built from `child_origin` (already resolved to Global or
        // Project), so `ProfileScope::Auto` cannot appear by construction.
        let child_scope = match &child_origin {
            Origin::Global => ProfileScope::Global,
            Origin::Project { .. } => ProfileScope::Project,
        };
        out.insert(
            n.id.clone(),
            ChildExpectation {
                id: resolved.id.clone(),
                scope: child_scope,
                version: resolved.version.clone(),
                playbook_digest: resolved.digest.clone(),
                profile_bundles: bundles,
                connectors: child_connectors,
                connector_accounts: child_connector_accounts,
                children: grand,
            },
        );
    }
    Ok(out)
}

/// Collects every profile reference a playbook binds, accounting for defaults.
/// A reference comes from each node that has an effective profile - both
/// `agent_task` nodes and `finish` nodes that carry a `prompt` (a finish
/// prompt is an executor step, see `NodeKind::effective_profile_ref`) - and,
/// when `supervised`, the supervisor profile. The trust decision on these
/// bundles is made by the caller (`check_profile_bundles` / `collect_children`).
///
/// Does not account for the run-local ephemeral executor (`overrides`): the gate only sees
/// the playbook definition. This is safe only because the surfaces do NOT
/// combine `overrides` with the trust gate (`expected_profile_bundles`) - see
/// the invariant in `build_run_manifest`. Otherwise a node's profile key with an ephemeral
/// override would end up in the permit but not in the snapshot, producing a false key-set mismatch.
///
/// `supervised` - whether the run will spawn an external supervisor agent. The supervisor profile
/// (supervisor.profile OR defaults.profile, even without a section) enters the set
/// ONLY when `supervised: true` - the same rule as `build_run_manifest`,
/// otherwise the permit's key set would diverge from the snapshot.
pub fn collect_profile_refs(
    playbook: &Playbook,
    supervised: bool,
) -> Vec<apb_core::profile::QualifiedProfileRef> {
    let mut refs = Vec::new();
    for n in &playbook.nodes {
        if let Some(p) = n.kind.effective_profile_ref(&playbook.defaults) {
            refs.push(p);
        }
    }
    if supervised
        && let Some(p) = playbook
            .supervisor
            .as_ref()
            .and_then(|s| s.profile.clone())
            .or_else(|| playbook.defaults.profile.clone())
    {
        refs.push(p);
    }
    refs
}

/// Pairs of `(<scope>/<name>, bundle_digest)` for a project-scope playbook's profiles.
/// Best-effort is safe: a skipped profile causes a key-set mismatch
/// in the engine (exact-match), i.e. a refusal. For the local-project and foreign-
/// project paths.
pub fn playbook_profile_bundles(
    root: &Path,
    id: &str,
    version: Option<&str>,
    supervised: bool,
) -> Vec<(String, String)> {
    playbook_profile_bundles_for(
        &root.join(".apb"),
        root,
        id,
        version,
        PlaybookOrigin::Project,
        supervised,
    )
}

/// Origin-aware variant: the definition is read from `def_parent`, profiles
/// are resolved with the given origin (a global playbook sees only global
/// profiles). `exec_root` is the execution root (for project skills).
pub fn playbook_profile_bundles_for(
    def_parent: &Path,
    exec_root: &Path,
    id: &str,
    version: Option<&str>,
    origin: PlaybookOrigin,
    supervised: bool,
) -> Vec<(String, String)> {
    let Ok(reg) = Registry::open_dir(def_parent) else {
        return vec![];
    };
    let Ok(loaded) = reg.load(id, version) else {
        return vec![];
    };
    let mut out = Vec::new();
    for r in collect_profile_refs(&loaded.playbook, supervised) {
        if let Ok((lp, _pairs, bundle)) = profile_store::compute_bundle(exec_root, origin, &r) {
            out.push((
                format!("{}/{}", profile_store::scope_str(lp.scope), lp.name),
                bundle,
            ));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Trust gate for a single profile bundle a mid-run rebind switches a node to
/// (issue #45 finding 5). Resolves the profile plus the live content of its
/// skills, computes the bundle digest, and refuses an unapproved bundle with the
/// SAME `untrusted_profile_requires_acknowledge` refusal run start uses (unless
/// the caller acknowledged after user confirmation). On success returns the
/// verified bundle digest, which the engine re-verifies from the run snapshot at
/// apply time (anti-TOCTOU pin) - the caller passes it verbatim, never a
/// separately recomputed one. `origin` must be the run's own origin so `auto`
/// scope resolves exactly as the run's node profiles did.
pub fn check_rebind(
    root: &Path,
    origin: PlaybookOrigin,
    name: &str,
    scope: ProfileScope,
    acknowledge_untrusted: bool,
) -> Result<String, Value> {
    let r = apb_core::profile::QualifiedProfileRef {
        name: name.to_string(),
        scope,
    };
    match profile_store::compute_bundle(root, origin, &r) {
        Ok((loaded, _pairs, bundle)) => {
            let key = format!("{}/{}", profile_store::scope_str(loaded.scope), loaded.name);
            if !acknowledge_untrusted && !TrustStore::load().is_approved(&bundle) {
                return Err(json!({
                    "policy": "untrusted_profile_requires_acknowledge",
                    "profiles": [key],
                    "detail": "run again with acknowledge_untrusted: true after user confirmation",
                }));
            }
            Ok(bundle)
        }
        Err(e) => Err(json!({ "policy": "profile_unresolved", "detail": e.to_string() })),
    }
}

fn check_profile_bundles(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    acknowledge_untrusted: bool,
    supervised: bool,
) -> Result<std::collections::BTreeMap<String, String>, Value> {
    let worigin = match origin {
        Origin::Global => PlaybookOrigin::Global,
        _ => PlaybookOrigin::Project,
    };
    let refs = collect_profile_refs(playbook, supervised);
    let mut verified: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    if refs.is_empty() {
        return Ok(verified);
    }
    let store = TrustStore::load();
    let mut untrusted: Vec<String> = Vec::new();
    for r in refs {
        match profile_store::compute_bundle(root, worigin, &r) {
            Ok((loaded, _pairs, bundle)) => {
                let key = format!("{}/{}", profile_store::scope_str(loaded.scope), loaded.name);
                if !acknowledge_untrusted
                    && !store.is_approved(&bundle)
                    && !untrusted.contains(&key)
                {
                    untrusted.push(key.clone());
                }
                // The map VERIFIED by this same pass - the engine will receive it as
                // expected (permit), not a freshly recomputed one (closes TOCTOU).
                verified.insert(key, bundle);
            }
            Err(e) => {
                return Err(json!({ "policy": "profile_unresolved", "detail": e.to_string() }));
            }
        }
    }
    if !untrusted.is_empty() {
        return Err(json!({
            "policy": "untrusted_profile_requires_acknowledge",
            "profiles": untrusted,
            "detail": "run again with acknowledge_untrusted: true after user confirmation",
        }));
    }
    Ok(verified)
}

/// Lifecycle gate shared by the parent (`check_run` / `preflight`) and every
/// sub-playbook child (`collect_children`): a draft or retired definition
/// refuses with the SAME policy keys the parent uses, carrying `id` so the
/// caller can tell WHICH playbook in the tree refused.
fn check_lifecycle(playbook_dir: &Path, id: &str) -> Result<(), Value> {
    match read_lifecycle(playbook_dir) {
        Lifecycle::Active => Ok(()),
        Lifecycle::Draft => Err(json!({ "policy": "draft_requires_trial", "id": id })),
        Lifecycle::Retired => Err(json!({ "policy": "retired_not_runnable", "id": id })),
    }
}

/// Digest-based trust gate shared by the parent and every child: an unapproved
/// definition digest refuses unless `acknowledge_untrusted` (the caller
/// confirmed with the user). `id`/`digest` name the offending playbook so a
/// tree refusal points at the exact child. Gating on `acknowledge_untrusted` is
/// what lets `preflight` (which passes `true`) stay read-only and skip child
/// trust while still enforcing lifecycle and `requires`.
fn check_digest_trust(id: &str, digest: &str, acknowledge_untrusted: bool) -> Result<(), Value> {
    if !acknowledge_untrusted && !TrustStore::load().is_approved(digest) {
        return Err(json!({
            "policy": "untrusted_requires_acknowledge",
            "id": id,
            "digest": digest,
            "detail": "run again with acknowledge_untrusted: true after user confirmation",
        }));
    }
    Ok(())
}

/// Checks `requires` applicability: files - only safe relative
/// paths inside the root; commands - only program names (no path separators).
fn check_requires(root: &Path, req: &apb_core::schema::Requires, id: &str) -> Result<(), Value> {
    use apb_core::preflight::{RequiresRefusal, requires_unmet};
    match requires_unmet(root, req) {
        Ok(missing) if missing.is_empty() => Ok(()),
        Ok(missing) => Err(json!({ "policy": "requires_unmet", "id": id, "missing": missing })),
        Err(RequiresRefusal::UnsafePath(f)) => {
            Err(json!({ "policy": "requires_unsafe_path", "id": id, "path": f }))
        }
        Err(RequiresRefusal::UnsafeCommand(c)) => {
            Err(json!({ "policy": "requires_unsafe_command", "id": id, "command": c }))
        }
    }
}
