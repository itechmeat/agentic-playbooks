//! The server-side run policy gate (spec 9). Checks what cannot be
//! trusted to the host model's discipline: lifecycle, digest-based trust,
//! the cross-workspace boundary, and applicability preflight. Returns a structural
//! refusal (JSON) that the tool hands back to the agent as-is.

use std::path::Path;

use apb_core::config::program_in_path;
use apb_core::connector::config::account_digest;
use apb_core::connector::resolve::resolve_playbook;
use apb_core::connector::secrets::missing_vars;
use apb_core::profile::ProfileScope;
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::registry::Registry;
use apb_core::schema::{Effect, NodeKind, Playbook};
use apb_core::scope::{Origin, PlaybookRef, digest_str};
use apb_core::trust::{Lifecycle, TrustStore, account_trust_id, read_lifecycle};
use apb_engine::run_config::ChildExpectation;
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

/// The two connector permit maps the gate produces: `connector name -> tree
/// digest` and `"connector/account" -> account digest`. Handed to the engine
/// verbatim as `expected_connectors` / `expected_connector_accounts`.
pub type ConnectorPermitMaps = (
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeMap<String, String>,
);

/// Preflight facts for the two-phase contract (spec 7).
pub struct Preflight {
    pub version: String,
    pub digest: String,
    pub effects: Vec<String>,
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
    // here; `acknowledge_untrusted: true` skips trust marking (trust is enforced
    // separately at execute-plan time), keeping preflight read-only.
    let origin = Origin::Project { workspace_id: None };
    let tree = resolve_tree(root, &loaded.playbook, &origin, id, true)?;
    let effects = tree
        .effects
        .iter()
        .map(|e| effect_str(e).to_string())
        .collect();
    Ok(Preflight {
        version: loaded.version.clone(),
        digest: digest_str(&loaded.yaml),
        effects,
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
}

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
    let digest = digest_str(&loaded.yaml);
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

    // Connector trust (spec 6 step 1, 7): resolve every bound connector,
    // check env presence, and gate both connector and account digests. Unlike
    // profile/playbook trust, `acknowledge_untrusted` does NOT bypass this -
    // connector and account trust guard secret egress, not content taste.
    // Returns the two permit maps handed to the engine verbatim, plus any
    // consent-time warnings (finding 11: a bound connector with zero accounts).
    let (connectors, connector_accounts, connector_warnings) =
        check_connectors(root, &loaded.playbook, acknowledge_untrusted)?;

    // Sub-playbook pins (spec C): walk the reference tree in the same gate pass,
    // detect cycles, and trust-check each child's bundles alongside the parent's.
    // The recursive effects union that this walk also accumulates is the user's
    // consent surface, exposed through `preflight` (which shares this walk); the
    // local run gate only needs the verified pins.
    let tree = resolve_tree(
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

    Ok(RunPermit {
        playbook_digest: digest,
        profile_bundles,
        children: tree.children,
        connectors,
        connector_accounts,
        warnings: connector_warnings,
    })
}

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
/// Public seam for the connector trust gate ALONE (spec 6 step 1, 7), for a
/// caller that does not go through the full `check_run` policy gate but still
/// must never start a connector-binding run with an empty (and therefore
/// vacuously-refusing, or worse silently-unverified) permit map - the
/// dashboard's `POST /api/playbooks/{id}/run` handler in `apb-server`, which
/// has no MCP tool call in front of it. Runs the EXACT SAME resolution and
/// trust checks `check_run` runs for its own connector step, so a
/// dashboard-started run gets the identical connector/account trust decision
/// an MCP-started run would. Callers must never reimplement this gate at the
/// call site - always come back through here.
pub fn connector_permit_maps(
    root: &Path,
    playbook: &Playbook,
) -> Result<ConnectorPermitMaps, Value> {
    // The public seam needs only the two maps; the consent-time warnings
    // (finding 11) are surfaced by the full `check_run` gate, so drop them here.
    let (connectors, accounts, _warnings) = check_connectors(root, playbook, false)?;
    Ok((connectors, accounts))
}

/// The two permit maps plus the consent-time warnings (finding 11) that
/// [`check_connectors`] produces in one pass.
type ConnectorCheck = (
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeMap<String, String>,
    Vec<String>,
);

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

        // Zero-account connector (finding 11 of issue #42): an empty
        // `expected_connector_accounts` for a bound connector passes the gate
        // silently today and every node bound to it then fails at call time
        // (nothing to call). Surface it as a consent-time warning rather than a
        // hard refusal - a missing account is a configuration gap, not the
        // secret-egress problem the connector/account trust gate guards.
        if resolved.accounts.is_empty() {
            warnings.push(format!(
                "connector `{name}` is bound but has no configured accounts; nodes that call it will fail until an account is added"
            ));
        }

        for account in &resolved.accounts {
            let id = account_trust_id(name, &account.name);
            let adigest = account_digest(account);
            if !store.is_approved(&adigest) && !unapproved_accounts.contains(&id) {
                unapproved_accounts.push(id.clone());
                account_fields.insert(id.clone(), account_display(account));
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
/// field holds only its raw `{{env.VAR}}` reference in the config, never the
/// resolved secret, so the whole `fields` map plus the `default` flag can be
/// shown. This mirrors exactly what the account digest pins.
fn account_display(account: &apb_core::connector::config::Account) -> Value {
    let fields: serde_json::Map<String, Value> = account
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    json!({ "default": account.default, "fields": Value::Object(fields) })
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

/// A safe relative path name: not absolute and without `..` components.
/// Protection against `requires.files` serving as an existence oracle for
/// arbitrary files (especially in foreign prepare_run before trust is
/// confirmed) - see spec 5.2.
fn is_safe_relative(p: &str) -> bool {
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        return false;
    }
    path.components().all(|c| {
        !matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::Prefix(_)
                | std::path::Component::RootDir
        )
    })
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
    let mut missing: Vec<String> = Vec::new();
    for f in &req.files {
        if !is_safe_relative(f) {
            return Err(json!({ "policy": "requires_unsafe_path", "id": id, "path": f }));
        }
        if !root.join(f).exists() {
            missing.push(format!("file:{f}"));
        }
    }
    for c in &req.commands {
        if c.contains('/') || c.contains('\\') {
            return Err(json!({ "policy": "requires_unsafe_command", "id": id, "command": c }));
        }
        if !program_in_path(c) {
            missing.push(format!("command:{c}"));
        }
    }
    if !missing.is_empty() {
        return Err(json!({ "policy": "requires_unmet", "id": id, "missing": missing }));
    }
    Ok(())
}
