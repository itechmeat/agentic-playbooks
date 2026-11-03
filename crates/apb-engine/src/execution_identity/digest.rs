//! The executor digest of a binding and the expected key of a run.
//!
//! `profile_digest` and `bundle_digest` identify the profile content (SOUL
//! and skills) and stay what the trust gate and the run permit compare.
//! What ran is the executor: [`executor_digest`] hashes the chain, the
//! tiers and the `ephemeral` flag of a manifest entry, machine-specific
//! fields left out, so every override is its own group even when its
//! chain equals the base, and a reinstall in another path splits nothing.
//! It is a pure function of the entry, so old manifests get it too.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::overrides::RunOverrides;
use apb_core::profile_store::PlaybookOrigin;
use apb_core::schema::Playbook;
use serde::Serialize;

use crate::error::EngineError;
use crate::manifest::ManifestProfile;

pub use crate::manifest::EXECUTOR_DIGEST_PREFIX;

/// `x1:` plus the sha256 over the canonical form of `entry`: the chain in
/// order, each invocation as `(agent_id, model, spec, soul_delivery)`; the
/// tiers in manifest order, each as `(name, for, invocation or the marker
/// "executor")`; the `ephemeral` flag (`ManifestProfile::executor_digest`).
pub fn executor_digest(entry: &ManifestProfile) -> String {
    entry.executor_digest()
}

/// What a node's history must match to count for an estimate or a stats
/// group: the profile, the bundle and the executor digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct NodeExecKey {
    pub profile: String,
    pub bundle: String,
    pub executor: String,
}

/// The key of every agent node of `playbook` as a start with `overrides`
/// would bind it, resolved the way the start resolves profiles and
/// overrides (one resolver, so pre-start and started keys cannot diverge).
pub fn expected_exec_key(
    exec_root: &Path,
    origin: PlaybookOrigin,
    playbook: &Playbook,
    overrides: &RunOverrides,
) -> Result<BTreeMap<String, NodeExecKey>, EngineError> {
    let mut playbook = playbook.clone();
    overrides
        .apply(&mut playbook)
        .map_err(EngineError::Invalid)?;
    let global = apb_core::config::GlobalConfig::load().map_err(EngineError::Invalid)?;
    let mut keys = BTreeMap::new();
    for n in &playbook.nodes {
        let Some(pref) = n.kind.effective_profile_ref(&playbook.defaults) else {
            continue;
        };
        let (loaded, _, bundle) = apb_core::profile_store::compute_bundle(exec_root, origin, &pref)
            .map_err(|e| EngineError::Invalid(format!("profile `{}`: {e}", pref.name)))?;
        let eph = overrides
            .nodes
            .get(&n.id)
            .and_then(|o| o.ephemeral_executor.as_ref());
        let (chain, tiers) = crate::manifest::resolve_executor(&loaded, eph, &global)?;
        keys.insert(
            n.id.clone(),
            NodeExecKey {
                profile: loaded.profile_digest.clone(),
                bundle,
                executor: crate::manifest::executor_digest_of(&chain, &tiers, eph.is_some()),
            },
        );
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invocation::ResolvedInvocation;
    use crate::manifest::ManifestTier;

    fn invocation(agent: &str, model: &str, program: &str) -> ResolvedInvocation {
        ResolvedInvocation {
            agent_id: agent.into(),
            model: model.into(),
            spec: apb_core::config::InvocationDef {
                argv: vec!["-p".into(), "{prompt}".into()],
                prompt_via: Default::default(),
                soul: Default::default(),
                soul_flag: None,
                transport: Default::default(),
                autonomous_args: Vec::new(),
                interaction: Default::default(),
            },
            soul_delivery: Default::default(),
            canonical_executable: program.into(),
            executable_fingerprint: format!("1:{program}"),
        }
    }

    fn entry(chain: Vec<ResolvedInvocation>, ephemeral: bool) -> ManifestProfile {
        ManifestProfile {
            scope: "project".into(),
            name: "p".into(),
            profile_digest: "pd".into(),
            bundle_digest: "bd".into(),
            soul: String::new(),
            soul_requirement: Default::default(),
            skills: Vec::new(),
            chain,
            ephemeral,
            hermetic: false,
            zcode_mode: None,
            process_env: None,
            tiers: Vec::new(),
            routed_tier: None,
            cascade: 0,
        }
    }

    /// Design 2.7: an override with the base's own chain is still its own
    /// executor, and the profile and bundle digests play no part.
    #[test]
    fn an_override_equal_to_the_base_is_another_executor() {
        let base = entry(vec![invocation("claude", "haiku", "/a/claude")], false);
        let over = entry(vec![invocation("claude", "haiku", "/a/claude")], true);
        assert!(executor_digest(&base).starts_with(EXECUTOR_DIGEST_PREFIX));
        assert_ne!(executor_digest(&base), executor_digest(&over));
        let mut renamed = base.clone();
        renamed.profile_digest = "other".into();
        renamed.bundle_digest = "other".into();
        renamed.soul = "another role".into();
        assert_eq!(executor_digest(&base), executor_digest(&renamed));
    }

    /// Machine fields never split a group; a tier's invocation does.
    #[test]
    fn machine_fields_do_not_count_and_a_tier_change_does() {
        let a = entry(vec![invocation("claude", "haiku", "/a/claude")], false);
        let moved = entry(vec![invocation("claude", "haiku", "/opt/b/claude")], false);
        assert_eq!(executor_digest(&a), executor_digest(&moved));
        let tier = |model: &str| ManifestTier {
            name: "light".into(),
            for_work: "small fixes".into(),
            invocation: Some(invocation("claude", model, "/a/claude")),
        };
        let mut t1 = a.clone();
        t1.tiers = vec![tier("haiku")];
        let mut t2 = a.clone();
        t2.tiers = vec![tier("sonnet")];
        let mut t3 = a.clone();
        t3.tiers = vec![ManifestTier {
            invocation: None,
            ..tier("haiku")
        }];
        assert_ne!(executor_digest(&t1), executor_digest(&t2));
        assert_ne!(executor_digest(&t1), executor_digest(&t3));
        assert_ne!(executor_digest(&a), executor_digest(&t1));
        let mut chain = a.clone();
        chain.chain.push(invocation("codex", "gpt-5", "/a/codex"));
        assert_ne!(executor_digest(&a), executor_digest(&chain));
    }
}
