//! The cloud part of a start and a rebind (`apb_core::cloud_models`): every
//! executor of every profile a run binds (primary, fallbacks and tiers, over
//! the whole tree; the background supervisor's when one runs; a node's
//! ephemeral override in place of its profile's chain) that names a cloud
//! catalog model is judged against the catalog as this machine has it, and
//! the primary executor's key variable must reach its spawn: set, kept by
//! the profile's `process_env` and not a variable an installed connector
//! references (the connector scrub drops those from every agent spawn).
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
/// `process_env` plan and not removed by the connector scrub (`scrub`, the
/// variables installed connectors reference, which every agent spawn drops).
fn key_reaches(doc: &ProfileDoc, agent: &str, model: &str, name: &str, scrub: &[String]) -> bool {
    let env = crate::adapter::SpawnEnv {
        process: crate::agent_env::plan_for_entry(doc.process_env.as_ref(), agent, model, scrub),
        scrub: scrub.to_vec(),
        ..Default::default()
    };
    env.passes(name)
}

/// The cloud verdict of one profile: `Err` with the refusal, else the
/// warnings. `scrub` is the connector scrub of the workspace
/// ([`connector_scrub`]).
pub fn check_profile(
    doc: &ProfileDoc,
    key: &str,
    snaps: &[CatalogSnapshot],
    scrub: &[String],
) -> Result<Vec<String>, Value> {
    check_pairs(doc, key, &doc.executor_pairs(), snaps, scrub)
}

/// The connector scrub of the workspace at `root`: the variables every
/// agent spawn drops.
pub fn connector_scrub(root: &Path) -> Vec<String> {
    apb_core::connector::resolve::all_referenced_env_names(root)
}

/// [`check_profile`] over the executor chain `pairs` a step runs with the
/// profile `doc`'s environment (its own chain, or the single executor a
/// run's ephemeral override puts in its place).
fn check_pairs(
    doc: &ProfileDoc,
    key: &str,
    pairs: &[(&str, &str)],
    snaps: &[CatalogSnapshot],
    scrub: &[String],
) -> Result<Vec<String>, Value> {
    let mut warnings = Vec::new();
    for (i, &(agent, model)) in pairs.iter().enumerate() {
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
        if key_reaches(doc, agent, model, key_env, scrub) {
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
                    "profile `{key}`: {agent} model `{model}` runs on {} and needs {key_env} in the environment of the agent process; set it (and keep it under `process_env: allowlist`, and out of every installed connector's account config, whose variables no agent receives)",
                    cloud_models::lookup(r.provider).map_or(r.provider, |p| p.display)
                ),
            }));
        }
        warnings.push(format!(
            "profile `{key}`: {agent} model `{model}` (a fallback or tier) needs {key_env}, which does not reach the agent process; the chain fails there if it reaches it"
        ));
    }
    Ok(warnings)
}

/// What a start binds besides its playbook's nodes: whether a background
/// supervisor agent runs (its profile is checked too), and the run's
/// overrides (a node's ephemeral executor is checked in place of its
/// profile's chain).
#[derive(Debug, Clone, Copy, Default)]
pub struct StartBinding<'a> {
    pub supervised: bool,
    pub overrides: Option<&'a apb_core::overrides::RunOverrides>,
}

/// The cloud verdict of every executor `playbook` binds: each agent node's
/// chain (or its ephemeral override), the supervisor's when `binding` says
/// one runs, and its children's (the pins a start verified).
pub fn check_tree(
    root: &Path,
    playbook: &Playbook,
    origin: PlaybookOrigin,
    children: Option<&std::collections::BTreeMap<String, ChildExpectation>>,
    binding: StartBinding<'_>,
) -> Result<Vec<String>, Value> {
    let snaps = cloud_models::catalog::load_all();
    let scrub = connector_scrub(root);
    let cx = Walk {
        root,
        snaps: &snaps,
        scrub: &scrub,
    };
    let mut warnings = Vec::new();
    cx.walk(playbook, origin, children, binding, &mut warnings)?;
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

struct Walk<'a> {
    root: &'a Path,
    snaps: &'a [CatalogSnapshot],
    scrub: &'a [String],
}

impl Walk<'_> {
    fn walk(
        &self,
        playbook: &Playbook,
        origin: PlaybookOrigin,
        children: Option<&std::collections::BTreeMap<String, ChildExpectation>>,
        binding: StartBinding<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), Value> {
        let mut checked = std::collections::BTreeSet::new();
        for n in &playbook.nodes {
            let Some(r) = n.kind.effective_profile_ref(&playbook.defaults) else {
                continue;
            };
            // A profile that does not resolve is refused on its own elsewhere.
            let Ok(p) = profile_store::resolve_profile(self.root, origin, &r) else {
                continue;
            };
            let eph = binding
                .overrides
                .and_then(|o| o.nodes.get(&n.id))
                .and_then(|o| o.ephemeral_executor.as_ref());
            if let Some(e) = eph {
                let key = format!("ephemeral/{}", n.id);
                let pairs = [(e.agent.as_str(), e.model.as_str())];
                warnings.extend(check_pairs(&p.doc, &key, &pairs, self.snaps, self.scrub)?);
                continue;
            }
            let key = format!("{}/{}", profile_store::scope_str(p.scope), p.name);
            if checked.insert(key.clone()) {
                warnings.extend(check_profile(&p.doc, &key, self.snaps, self.scrub)?);
            }
        }
        if binding.supervised
            && let Some(r) = super::profile_refs::supervisor_profile_ref(playbook)
            && let Ok(p) = profile_store::resolve_profile(self.root, origin, &r)
        {
            let key = format!("{}/{}", profile_store::scope_str(p.scope), p.name);
            if checked.insert(key.clone()) {
                warnings.extend(check_profile(&p.doc, &key, self.snaps, self.scrub)?);
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
            let Some(child) = pin.load(self.root) else {
                continue;
            };
            let child_origin = match pin.scope {
                apb_core::profile::ProfileScope::Global => PlaybookOrigin::Global,
                _ => PlaybookOrigin::Project,
            };
            // A child runs its own nodes, unsupervised and without the
            // parent's overrides.
            self.walk(
                &child,
                child_origin,
                Some(&pin.children),
                StartBinding::default(),
                warnings,
            )?;
        }
        Ok(())
    }
}

/// The refusal as one line: `<code>: <detail>`.
pub fn text(refusal: &Value) -> String {
    format!(
        "{}: {}",
        refusal["policy"].as_str().unwrap_or_default(),
        refusal["detail"].as_str().unwrap_or_default()
    )
}
