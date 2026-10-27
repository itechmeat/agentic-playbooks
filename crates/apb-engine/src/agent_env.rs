//! The process environment a profile's `process_env: allowlist` gives one
//! agent: the plan over apb's own environment (see
//! `apb_core::process_env`), and the `agent_env_applied` event an attempt
//! journals with it. Names only: no value is read here.

use std::sync::Arc;

use apb_core::process_env::{EnvPlan, ProcessEnvPolicy, allowed_patterns, is_truthy, plan};

use crate::event::EventPayload;

/// At most this many withheld names are journaled per attempt.
pub(crate) const WITHHELD_JOURNALED: usize = 200;

/// The plan for `agent` running `model` under `policy`: the base set, the
/// agent's set, its triggered rules, the names derived from its own config
/// (codex provider keys), the profile's `allow`, minus the connector
/// `scrub`.
pub fn plan_for(agent: &str, model: &str, policy: &ProcessEnvPolicy, scrub: &[String]) -> EnvPlan {
    let agent = apb_core::detect::canonical_agent_id(agent);
    let derived = if agent == "codex" {
        crate::agent_home::provider_env_keys(crate::agent_home::source_codex_home().as_deref())
    } else {
        Vec::new()
    };
    let allowed = allowed_patterns(
        agent,
        model,
        &policy.allow,
        |name| std::env::var(name).is_ok_and(|v| is_truthy(&v)),
        &derived,
    );
    plan(
        std::env::vars_os().map(|(name, _)| name),
        &allowed,
        scrub,
        &policy.allow,
    )
}

/// The plan of a manifest entry's `process_env` for one chain step: `None`
/// when the entry inherits (no policy, `inherit`, or an old manifest).
pub(crate) fn plan_for_entry(
    policy: Option<&ProcessEnvPolicy>,
    agent: &str,
    model: &str,
    scrub: &[String],
) -> Option<Arc<EnvPlan>> {
    policy
        .filter(|p| p.is_allowlist())
        .map(|p| Arc::new(plan_for(agent, model, p, scrub)))
}

/// The `agent_env_applied` event of attempt `attempt` of `node` on `agent`
/// under `plan`.
pub(crate) fn applied_event(plan: &EnvPlan, node: &str, attempt: u32, agent: &str) -> EventPayload {
    let shown = plan.withheld.len().min(WITHHELD_JOURNALED);
    EventPayload::AgentEnvApplied {
        node: node.to_string(),
        attempt,
        agent: agent.to_string(),
        kept: u32::try_from(plan.keep.len()).unwrap_or(u32::MAX),
        withheld: plan.withheld[..shown].to_vec(),
        withheld_more: u32::try_from(plan.withheld.len() - shown).unwrap_or(u32::MAX),
        absent: plan.absent.clone(),
        item: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_caps_the_withheld_names() {
        let plan = EnvPlan {
            keep: vec!["PATH".into()],
            withheld: (0..250).map(|i| format!("V{i:03}")).collect(),
            absent: vec!["NPM_TOKEN".into()],
        };
        match applied_event(&plan, "n", 2, "codex") {
            EventPayload::AgentEnvApplied {
                kept,
                withheld,
                withheld_more,
                absent,
                ..
            } => {
                assert_eq!(kept, 1);
                assert_eq!(withheld.len(), WITHHELD_JOURNALED);
                assert_eq!(withheld_more, 50);
                assert_eq!(absent, vec!["NPM_TOKEN".to_string()]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// An explicit `inherit` and a manifest written before `process_env`
    /// existed both resume without a plan.
    #[test]
    fn inherit_and_an_old_manifest_entry_have_no_plan() {
        let yaml = "scope: project\nname: p\nprofile_digest: d\nbundle_digest: b\nsoul: ''\nsoul_requirement: any\nskills: []\nchain: []\n";
        let entry: crate::manifest::ManifestProfile = yaml_serde::from_str(yaml).unwrap();
        assert!(plan_for_entry(entry.process_env.as_ref(), "claude", "m", &[]).is_none());
        let inherit = ProcessEnvPolicy::default();
        assert!(plan_for_entry(Some(&inherit), "claude", "m", &[]).is_none());
    }
}
