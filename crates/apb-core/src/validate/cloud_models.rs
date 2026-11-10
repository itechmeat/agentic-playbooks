//! The node rules of cloud catalog models (`crate::cloud_models`): every
//! node that binds a profile (an `agent_task`, a list `agent_task`, a
//! finish that composes an answer, a judge's emulation profile) is checked
//! against the executor chain of that profile, resolved as run start
//! resolves `scope: auto` (project, then global).
//!
//! - V220 (warning): the chain reaches a catalog model that has no tool
//!   calling or is not a chat model. A warning, not an error: run
//!   preparation rejects every validator error whatever the execution mode,
//!   and a run in host mode spawns no agent CLI. The profile finding
//!   (`cloud_model_no_tools`) is the error, and a cli start refuses.
//! - V221 (warning): the chain spans more than one privacy class.
//! - V222 (warning): the chain names a cloud model the catalog does not
//!   list.
//!
//! Both context facts are empty by default, so a caller that builds a bare
//! context stays silent.

use super::*;
use crate::agents::{chain_privacy, cloud_model as model_ref};
use crate::cloud_models::{check, privacy};
use crate::profile::ProfileScope;

/// The key of a profile in [`ValidationContext::profile_chains`].
pub fn chain_key(scope: &str, name: &str) -> String {
    format!("{scope}:{name}")
}

pub(super) fn check_cloud_models(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    if ctx.profile_chains.is_empty() || ctx.cloud.is_empty() {
        return;
    }
    for n in &playbook.nodes {
        let Some(pref) = n.kind.effective_profile_ref(&playbook.defaults) else {
            continue;
        };
        let scopes: &[&str] = match (pref.scope, ctx.playbook_origin) {
            (ProfileScope::Project, _) => &["project"],
            (ProfileScope::Global, _) => &["global"],
            (ProfileScope::Auto, PlaybookOrigin::Project) => &["project", "global"],
            (ProfileScope::Auto, PlaybookOrigin::Global) => &["global"],
        };
        let Some((key, chain)) = scopes.iter().find_map(|s| {
            let k = chain_key(s, &pref.name);
            ctx.profile_chains.get(&k).map(|c| (k, c))
        }) else {
            continue;
        };
        for (agent, model) in chain {
            let Some(mr) = model_ref(agent, model) else {
                continue;
            };
            match check::check(&mr, &ctx.cloud) {
                Some(issue @ (check::CloudIssue::NoTools | check::CloudIssue::NotChat)) => r.warn(
                    "V220",
                    Some(&n.id),
                    format!(
                        "node `{}` binds profile `{key}`, whose {agent} model `{model}` cannot run an agent step: {}",
                        n.id,
                        issue.describe(&mr)
                    ),
                ),
                Some(issue @ check::CloudIssue::Unknown { .. }) => r.warn(
                    "V222",
                    Some(&n.id),
                    format!(
                        "node `{}` binds profile `{key}`, whose {agent} model `{model}`: {}",
                        n.id,
                        issue.describe(&mr)
                    ),
                ),
                _ => {}
            }
        }
        let summary = chain_privacy(
            chain.iter().map(|(a, m)| (a.as_str(), m.as_str())),
            &ctx.cloud,
        );
        if !summary.warnings.is_empty() {
            r.warn(
                "V221",
                Some(&n.id),
                format!(
                    "node `{}` binds profile `{key}`, whose executor chain spans privacy classes ({}): a fallback or tier may run the node on a less private class",
                    n.id,
                    privacy::describe(&summary.classes)
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_models::{catalog, near_ai::NEAR_AI};
    use crate::validate::{Severity, validate};

    fn ctx(chains: &[(&str, &[(&str, &str)])]) -> ValidationContext {
        ValidationContext {
            profiles: vec!["main".into()],
            profile_chains: chains
                .iter()
                .map(|(k, c)| {
                    (
                        k.to_string(),
                        c.iter()
                            .map(|(a, m)| (a.to_string(), m.to_string()))
                            .collect(),
                    )
                })
                .collect(),
            cloud: vec![catalog::embedded(&NEAR_AI)],
            ..Default::default()
        }
    }

    fn pb(nodes: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: done }}\n"
        ))
        .unwrap()
    }

    fn codes(p: &Playbook, c: &ValidationContext) -> Vec<(&'static str, Severity, Option<String>)> {
        validate(p, c)
            .issues
            .into_iter()
            .filter(|i| i.code.starts_with("V22"))
            .map(|i| (i.code, i.severity, i.node))
            .collect()
    }

    const VL: &[(&str, &str)] = &[("dsh", "nearai/Qwen/Qwen3-VL-30B-A3B-Instruct")];
    const TASK: &str = "  - { id: w, type: agent_task, prompt: x }\n";

    #[test]
    fn v220_on_an_agent_task_a_list_node_and_a_judge_emulation() {
        let c = ctx(&[("project:main", VL)]);
        let got = codes(&pb(TASK), &c);
        assert_eq!(got, [("V220", Severity::Warning, Some("w".into()))]);
        let msg = validate(&pb(TASK), &c)
            .issues
            .into_iter()
            .find(|i| i.code == "V220")
            .unwrap()
            .message;
        assert!(
            msg.contains("Qwen/Qwen3-VL-30B-A3B-Instruct") && msg.contains("no tool calling"),
            "{msg}"
        );
        let list = "  - { id: w, type: agent_task, prompt: x, list: { items: { inline: [ { id: a } ] } } }\n";
        assert!(
            codes(&pb(list), &c)
                .iter()
                .any(|(code, ..)| *code == "V220")
        );
        let judge = "  - id: w\n    type: judge\n    state: { s: x }\n    questions: { q: { type: noul, instructions: i } }\n    on_unavailable: emulate\n";
        let j = pb(judge);
        assert!(j.nodes[1].kind.effective_profile_ref(&j.defaults).is_some());
        assert!(codes(&j, &c).iter().any(|(code, ..)| *code == "V220"));
    }

    #[test]
    fn v222_for_an_unknown_id_and_v221_for_a_mixed_chain() {
        let c = ctx(&[("project:main", &[("dsh", "nearai/Qwen/Qwen3.8-72B")])]);
        assert_eq!(codes(&pb(TASK), &c)[0].0, "V222");
        let mixed: &[(&str, &str)] = &[
            ("dsh", "nearai/Qwen/Qwen3.8-27B"),
            ("dsh", "nearai/anthropic/claude-sonnet-5-5"),
        ];
        let c = ctx(&[("project:main", mixed)]);
        let issues = validate(&pb(TASK), &c).issues;
        let v221 = issues.iter().find(|i| i.code == "V221").unwrap();
        assert_eq!(v221.severity, Severity::Warning);
        assert!(
            v221.message.contains("confidential_tee -> incognito"),
            "{}",
            v221.message
        );
    }

    #[test]
    fn nothing_for_claude_an_empty_context_or_a_good_chain() {
        let c = ctx(&[("project:main", &[("claude", "claude-sonnet-5-5")])]);
        assert!(codes(&pb(TASK), &c).is_empty());
        assert!(codes(&pb(TASK), &ValidationContext::default()).is_empty());
        let c = ctx(&[("project:main", &[("dsh", "nearai/Qwen/Qwen3.8-27B@high")])]);
        assert!(codes(&pb(TASK), &c).is_empty());
    }

    #[test]
    fn a_global_scope_ref_is_resolved() {
        let c = ctx(&[("global:main", VL)]);
        let p = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: x, profile: { name: main, scope: global } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n",
        )
        .unwrap();
        assert!(codes(&p, &c).iter().any(|(code, ..)| *code == "V220"));
        // Auto falls through to the global profile when no project one exists.
        assert!(
            codes(&pb(TASK), &c)
                .iter()
                .any(|(code, ..)| *code == "V220")
        );
    }
}
