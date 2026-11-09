//! The cloud part of a start and a rebind (`apb_core::cloud_models`): every
//! executor of every profile a run binds (primary, fallbacks and tiers, over
//! the whole tree) that names a cloud catalog model is judged against the
//! catalog as this machine has it, and the primary executor's key variable
//! must reach its spawn.
//!
//! - a model the catalog lacks: refused `cloud_model_unknown`, naming
//!   `apb models --refresh`;
//! - a model an agent step cannot run on: refused with that finding's code
//!   (`cloud_model_no_tools`, `cloud_model_not_chat`,
//!   `cloud_effort_unsupported`);
//! - the primary executor's key does not reach its spawn: refused
//!   `cloud_key_missing`, naming the variable; a fallback's or a tier's is a
//!   start warning, since the chain may never reach it.
//!
//! Run in cli execution mode only (host mode spawns no agent CLI), before
//! the run directory is created (`scheduler::prepare`), and on the profile
//! a rebind names (`gate::check_rebind`). `acknowledge_untrusted` does not
//! bypass it.

use std::path::Path;

use apb_core::cloud_models::{self, catalog::CatalogSnapshot, check};
use apb_core::profile::ProfileDoc;
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::schema::{NodeKind, Playbook};
use serde_json::{Value, json};

use crate::run_config::ChildExpectation;

/// The refusal code of a primary executor whose key does not reach it.
pub const KEY_MISSING: &str = "cloud_key_missing";

/// The shortest key value the store-time scrub replaces.
pub const MIN_KEY_CHARS: usize = apb_core::cloud_models::MIN_KEY_CHARS;

/// Whether `policy` is one of the cloud refusal codes.
pub fn is_cloud_refusal(policy: &str) -> bool {
    matches!(
        policy,
        "cloud_model_unknown"
            | "cloud_model_no_tools"
            | "cloud_model_not_chat"
            | "cloud_effort_unsupported"
            | KEY_MISSING
    )
}

/// Whether the key variable `name` reaches a spawn of `agent` on `model`
/// for a profile `doc`: set in apb's environment, kept by the profile's
/// `process_env` plan.
fn key_reaches(doc: &ProfileDoc, agent: &str, model: &str, name: &str) -> bool {
    let env = crate::adapter::SpawnEnv {
        process: crate::agent_env::plan_for_entry(doc.process_env.as_ref(), agent, model, &[]),
        ..Default::default()
    };
    env.passes(name)
}

/// The cloud verdict of one profile: `Err` with the refusal, else the
/// warnings.
pub fn check_profile(
    doc: &ProfileDoc,
    key: &str,
    snaps: &[CatalogSnapshot],
) -> Result<Vec<String>, Value> {
    let mut warnings = Vec::new();
    for (i, (agent, model)) in doc.executor_pairs().into_iter().enumerate() {
        let Some(r) = apb_core::agents::cloud_model(agent, model) else {
            continue;
        };
        if let Some(issue) = check::check(&r, snaps) {
            return Err(json!({
                "policy": issue.code(),
                "profile": key,
                "agent": agent,
                "model": model,
                "detail": format!("profile `{key}`: {agent} model `{model}`: {}", issue.describe(&r)),
            }));
        }
        let Some(key_env) = cloud_models::lookup(r.provider).map(|p| p.key_env) else {
            continue;
        };
        if key_reaches(doc, agent, model, key_env) {
            if std::env::var(key_env).is_ok_and(|v| v.chars().count() < MIN_KEY_CHARS) {
                warnings.push(format!(
                    "profile `{key}`: {key_env} is shorter than {MIN_KEY_CHARS} characters, so apb cannot replace it in what the agent prints"
                ));
            }
            continue;
        }
        if i == 0 {
            return Err(json!({
                "policy": KEY_MISSING,
                "profile": key,
                "agent": agent,
                "model": model,
                "variable": key_env,
                "detail": format!(
                    "profile `{key}`: {agent} model `{model}` runs on {} and needs {key_env} in the environment of the agent process; set it (and keep it under `process_env: allowlist`)",
                    cloud_models::lookup(r.provider).map_or(r.provider, |p| p.display)
                ),
            }));
        }
        warnings.push(format!(
            "profile `{key}`: {agent} model `{model}` (a fallback or tier) needs {key_env}, which is not set; the chain fails there if it reaches it"
        ));
    }
    Ok(warnings)
}

/// The cloud verdict of every profile `playbook` binds, its children's
/// included (the pins a start verified).
pub fn check_tree(
    root: &Path,
    playbook: &Playbook,
    origin: PlaybookOrigin,
    children: Option<&std::collections::BTreeMap<String, ChildExpectation>>,
) -> Result<Vec<String>, Value> {
    let snaps = cloud_models::catalog::load_all();
    let mut warnings = Vec::new();
    walk(root, playbook, origin, children, &snaps, &mut warnings)?;
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn walk(
    root: &Path,
    playbook: &Playbook,
    origin: PlaybookOrigin,
    children: Option<&std::collections::BTreeMap<String, ChildExpectation>>,
    snaps: &[CatalogSnapshot],
    warnings: &mut Vec<String>,
) -> Result<(), Value> {
    for r in super::profile_refs::collect_profile_refs(playbook, false) {
        // A profile that does not resolve is refused on its own elsewhere.
        if let Ok(p) = profile_store::resolve_profile(root, origin, &r) {
            let key = format!("{}/{}", profile_store::scope_str(p.scope), p.name);
            warnings.extend(check_profile(&p.doc, &key, snaps)?);
        }
    }
    let Some(pins) = children else {
        return Ok(());
    };
    for n in &playbook.nodes {
        if !matches!(n.kind, NodeKind::Playbook { .. }) {
            continue;
        }
        let Some(pin) = pins.get(&n.id) else { continue };
        let Some(child) = pin.load(root) else {
            continue;
        };
        let child_origin = match pin.scope {
            apb_core::profile::ProfileScope::Global => PlaybookOrigin::Global,
            _ => PlaybookOrigin::Project,
        };
        walk(
            root,
            &child,
            child_origin,
            Some(&pin.children),
            snaps,
            warnings,
        )?;
    }
    Ok(())
}

/// The refusal as one line: `<code>: <detail>`.
pub fn text(refusal: &Value) -> String {
    format!(
        "{}: {}",
        refusal["policy"].as_str().unwrap_or_default(),
        refusal["detail"].as_str().unwrap_or_default()
    )
}
