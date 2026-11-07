//! The untrusted-input analysis (V179 to V182): where text from outside
//! the author's control enters a playbook, which nodes render it, and
//! which of those can send outward or reach secrets.
//!
//! The author marks the entry points with `untrusted: true` on a param, on
//! a connector binding, or on a node whose own attempt reads outside
//! text. Taint then follows the text through templates, list items,
//! continued sessions, evidence, a finish prompt without a context read,
//! and a template-form `writes`, to a fixed point. Every
//! `run.previous_success.nodes.*` value is untrusted whatever the current
//! definition says: the earlier run may have run another version whose
//! markers this one cannot know. The values are data, never instructions;
//! the prompt section that says so is the engine's.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::schema::{Effect, NodeKind, Playbook};

/// Where untrusted text enters a run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Param {
        name: String,
    },
    Connector {
        node: String,
        connector: String,
    },
    Node {
        node: String,
    },
    /// An earlier run's output of `node`, read through
    /// `run.previous_success.nodes.<node>.*`.
    EarlierRun {
        node: String,
    },
}

impl Source {
    /// `param issue_body`, `connector github on list_open`, `node fetch`,
    /// `earlier run's output of draft`.
    pub fn label(&self) -> String {
        match self {
            Source::Param { name } => format!("param {name}"),
            Source::Connector { node, connector } => {
                format!("connector {connector} on {node}")
            }
            Source::Node { node } => format!("node {node}"),
            Source::EarlierRun { node } => format!("earlier run's output of {node}"),
        }
    }
}

/// An agent that renders untrusted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Follower {
    pub node: String,
    pub sources: BTreeSet<Source>,
    /// Why it can send outward, empty when it cannot.
    pub outward: Vec<String>,
    /// What secrets it reaches, empty when none.
    pub secrets: Vec<String>,
    pub accepted: Option<String>,
}

/// What the analysis found.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    /// Every node whose output carries untrusted text, with its sources.
    pub tainted: BTreeMap<String, BTreeSet<Source>>,
    /// The followers that render untrusted text, in node order.
    pub followers: Vec<Follower>,
}

/// The effects that mean a node can send outward.
const OUTWARD_EFFECTS: [Effect; 3] = [Effect::Network, Effect::External, Effect::Irreversible];

/// Whether the node's own output carries text taint can flow through.
fn carries_text(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::AgentTask { .. }
            | NodeKind::Script { .. }
            | NodeKind::Prompt { .. }
            | NodeKind::Playbook { .. }
    ) || matches!(
        kind,
        NodeKind::Finish {
            prompt: Some(_),
            ..
        }
    )
}

/// Whether the node reads instructions (an agent that could follow them).
fn is_follower_kind(kind: &NodeKind) -> bool {
    matches!(kind, NodeKind::AgentTask { .. } | NodeKind::Playbook { .. })
        || matches!(
            kind,
            NodeKind::Finish {
                prompt: Some(_),
                ..
            }
        )
}

/// The untrusted-input analysis of `playbook`, to a fixed point.
pub fn analyze(playbook: &Playbook) -> Analysis {
    let any_untrusted_param = playbook.params.iter().any(|p| p.untrusted);
    let mut tainted: BTreeMap<String, BTreeSet<Source>> = BTreeMap::new();

    // The texts a node's taint reads: prompts and instructions (with judge
    // state fields, which never taint but never hurt), and the node's
    // template-form `writes`.
    let texts_of = |id: &str| -> Vec<String> {
        crate::schema::template_texts(playbook)
            .into_iter()
            .filter(|(owner, _)| *owner == id)
            .map(|(_, text)| text.to_string())
            .collect()
    };

    // A node renders untrusted text (its inputs) versus its OUTPUT
    // carrying taint: the two come apart where the kind does not carry
    // text (a judge answers from fixed options), and rule 3 reads the
    // output map, so a judge's answer stays clean while its own state
    // fields may read untrusted text.
    let mut output: BTreeMap<String, BTreeSet<Source>> = BTreeMap::new();
    loop {
        let mut changed = false;
        for node in &playbook.nodes {
            let mut sources: BTreeSet<Source> = tainted.get(&node.id).cloned().unwrap_or_default();
            let before = sources.len();

            // Rule 1: the node's own attempt reads outside text.
            if node.untrusted {
                sources.insert(Source::Node {
                    node: node.id.clone(),
                });
            }
            // Rule 2: one of its connector bindings returns outside text.
            for b in node.kind.connector_bindings() {
                if b.untrusted {
                    sources.insert(Source::Connector {
                        node: node.id.clone(),
                        connector: b.name.clone(),
                    });
                }
            }
            // Rules 3 and 8: what the node's templates and its
            // template-form `writes` read.
            let mut texts = texts_of(&node.id);
            if let Some(crate::schema::WriteScope::Template(t)) = &node.writes {
                texts.push(t.clone());
            }
            for text in &texts {
                for cap in crate::template::refs(text) {
                    let parts: Vec<&str> = cap.split('.').collect();
                    match parts.as_slice() {
                        ["params", name]
                            if playbook
                                .params
                                .iter()
                                .any(|p| &p.name == name && p.untrusted) =>
                        {
                            sources.insert(Source::Param {
                                name: (*name).to_string(),
                            });
                        }
                        [
                            "nodes",
                            x,
                            "output" | "report" | "rejected_output" | "seen",
                            ..,
                        ] => {
                            if let Some(src) = output.get(*x) {
                                sources.extend(src.iter().cloned());
                            }
                        }
                        // The earlier run may have run another version
                        // whose markers this definition cannot know.
                        ["run", "previous_success", "nodes", x, ..] => {
                            sources.insert(Source::EarlierRun {
                                node: (*x).to_string(),
                            });
                        }
                        // The recorded context holds params and node
                        // outputs, so any untrusted input in the run
                        // reaches it.
                        ["run", "context" | "retro"] => {
                            if any_untrusted_param {
                                for p in playbook.params.iter().filter(|p| p.untrusted) {
                                    sources.insert(Source::Param {
                                        name: p.name.clone(),
                                    });
                                }
                            }
                            for src in output.values() {
                                sources.extend(src.iter().cloned());
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Rule 4: a list node's items come from a tainted node.
            if let Some(list) = &node.list
                && let Some(from) = &list.items.from
                && let Some(src) = output.get(&from.node)
            {
                sources.extend(src.iter().cloned());
            }
            // Rule 5: a continued session holds the source node's text.
            if let NodeKind::AgentTask {
                continue_session: Some(source),
                ..
            } = &node.kind
                && let Some(src) = output.get(source)
            {
                sources.extend(src.iter().cloned());
            }
            // Rule 6: evidence reads a tainted node's snapshots.
            if let Some(e) = &node.evidence {
                for from in &e.from {
                    if let Some(src) = output.get(from) {
                        sources.extend(src.iter().cloned());
                    }
                }
            }
            // Rule 7: a finish prompt without a context read gets the
            // terminal context appended, so anything untrusted in the run
            // reaches it.
            if let NodeKind::Finish {
                prompt: Some(_), ..
            } = &node.kind
                && !crate::template::reads_recorded_context(&texts_of(&node.id).join("\n"))
            {
                if any_untrusted_param {
                    for p in playbook.params.iter().filter(|p| p.untrusted) {
                        sources.insert(Source::Param {
                            name: p.name.clone(),
                        });
                    }
                }
                for src in output.values() {
                    sources.extend(src.iter().cloned());
                }
            }

            if sources.len() != before {
                changed = true;
            }
            if !sources.is_empty() {
                tainted.insert(node.id.clone(), sources.clone());
                // A node whose inputs are untrusted and whose kind carries
                // text taints its own output for the next reader.
                if carries_text(&node.kind) {
                    output.insert(node.id.clone(), sources.clone());
                }
            }
        }
        if !changed {
            break;
        }
    }

    // The followers, in node order.
    // Every text a follower renders: its templates and a template-form
    // `writes`, whose rendered globs reach its prompt (rule 8).
    let reads_of = |id: &str| -> Vec<String> {
        let mut texts = texts_of(id);
        if let Some(n) = playbook.nodes.iter().find(|n| n.id == id)
            && let Some(crate::schema::WriteScope::Template(t)) = &n.writes
        {
            texts.push(t.clone());
        }
        texts
    };
    let mut followers = Vec::new();
    for node in &playbook.nodes {
        let Some(sources) = tainted.get(&node.id) else {
            continue;
        };
        if sources.is_empty() || !is_follower_kind(&node.kind) {
            continue;
        }
        followers.push(Follower {
            node: node.id.clone(),
            sources: sources.clone(),
            outward: outward_reasons(playbook, node),
            secrets: secret_entries(playbook, node, &reads_of(&node.id)),
            accepted: node.accept_untrusted.clone(),
        });
    }
    Analysis { tainted, followers }
}

/// Why `node` can send outward, in the fixed order of the design: its
/// isolation, its connectors, its declared effects, and what a finish or
/// playbook node runs.
fn outward_reasons(playbook: &Playbook, node: &crate::schema::Node) -> Vec<String> {
    let mut out = Vec::new();
    if let NodeKind::AgentTask { isolation, .. } = &node.kind {
        let sandboxed = matches!(
            isolation,
            Some(iso)
                if iso.level == crate::isolation::Level::Full
                    && iso.network == Some(crate::isolation::Network::None)
        );
        let binds_connector = !node.kind.connector_bindings().is_empty();
        let declares_outward = node.effects.iter().any(|e| OUTWARD_EFFECTS.contains(e));
        if !sandboxed || binds_connector || declares_outward {
            out.push("runs with network access".to_string());
        }
    }
    for b in node.kind.connector_bindings() {
        out.push(format!("binds connector {}", b.name));
    }
    for e in &node.effects {
        if OUTWARD_EFFECTS.contains(e) {
            out.push(format!("declares effect {}", effect_name(e)));
        }
    }
    if matches!(
        node.kind,
        NodeKind::Finish {
            prompt: Some(_),
            ..
        }
    ) {
        out.push("runs an agent with network access".to_string());
    }
    if let NodeKind::Playbook { playbook, .. } = &node.kind {
        out.push(format!(
            "runs playbook {}, which can do anything",
            playbook.id
        ));
    }
    let _ = playbook;
    out
}

fn effect_name(e: &Effect) -> &'static str {
    match e {
        Effect::FsRead => "fs_read",
        Effect::FsWrite => "fs_write",
        Effect::Network => "network",
        Effect::External => "external",
        Effect::Irreversible => "irreversible",
        Effect::Secrets => "secrets",
    }
}

/// What secrets `node` reaches, in the fixed order: the secret params its
/// texts render, its connectors, and a declared `secrets` effect.
fn secret_entries(
    playbook: &Playbook,
    node: &crate::schema::Node,
    texts: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for text in texts {
        for cap in crate::template::refs(text) {
            let parts: Vec<&str> = cap.split('.').collect();
            if let ["params", name] = parts.as_slice()
                && playbook.params.iter().any(|p| &p.name == name && p.secret)
                && seen.insert((*name).to_string())
            {
                out.push(format!("secret param {name}"));
            }
        }
    }
    for b in node.kind.connector_bindings() {
        out.push(format!("connector {} with its account", b.name));
    }
    let declares_secrets = node.effects.contains(&Effect::Secrets)
        || playbook
            .effects
            .iter()
            .any(|e| *e == Effect::Secrets && matches!(node.kind, NodeKind::Playbook { .. }));
    if declares_secrets {
        out.push("declares effect secrets".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playbook(nodes: &str, extra: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: {{ profile: dev }}\n{extra}nodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: done, type: finish, outcome: success }}\nedges: []\n"
        ))
        .unwrap()
    }

    fn tainted_ids(a: &Analysis) -> Vec<String> {
        a.tainted
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Each marker alone taints its node.
    #[test]
    fn each_marker_alone_taints() {
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: \"x {{params.body}}\" }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n",
        ));
        assert_eq!(tainted_ids(&a), ["w"]);
        assert!(a.tainted["w"].contains(&Source::Param {
            name: "body".into()
        }));

        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: gh, untrusted: true }] }\n",
            "",
        ));
        assert!(a.tainted["w"].contains(&Source::Connector {
            node: "w".into(),
            connector: "gh".into()
        }));

        let a = analyze(&playbook(
            "  - { id: w, type: script, script: s, runner: sh, untrusted: true }\n",
            "",
        ));
        assert!(a.tainted["w"].contains(&Source::Node { node: "w".into() }));
    }

    /// `a -> b -> c` through outputs taints `c` even when the playbook
    /// declares the nodes out of dependency order.
    #[test]
    fn taint_flows_through_outputs_to_a_fixed_point() {
        let a = analyze(&playbook(
            "  - { id: c, type: agent_task, prompt: \"{{nodes.b.output}}\" }\n  - { id: b, type: agent_task, prompt: \"{{nodes.a.output}}\" }\n  - { id: a, type: agent_task, prompt: p, connectors: [{ name: gh, untrusted: true }] }\n",
            "",
        ));
        for id in ["a", "b", "c"] {
            assert!(!a.tainted[id].is_empty(), "{id} is not tainted");
        }
        let c = &a.tainted["c"];
        assert!(c.contains(&Source::Connector {
            node: "a".into(),
            connector: "gh".into()
        }));
        // The followers come in node order (c first, as declared).
        let order: Vec<String> = a.followers.iter().map(|f| f.node.clone()).collect();
        assert_eq!(order, ["c", "b", "a"]);
    }

    /// `run.context` is tainted by an untrusted param alone; any earlier
    /// run's output is untrusted whatever the current definition says.
    #[test]
    fn the_run_context_and_an_earlier_output_taint() {
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: \"{{run.context}}\" }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n",
        ));
        assert!(a.tainted["w"].contains(&Source::Param {
            name: "body".into()
        }));

        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: \"{{run.previous_success.nodes.x.output}}\" }\n",
            "",
        ));
        assert!(a.tainted["w"].contains(&Source::EarlierRun { node: "x".into() }));
    }

    /// A judge's answer carries no taint: a prompt reading it stays clean.
    #[test]
    fn a_judges_answer_carries_no_taint() {
        let a = analyze(&playbook(
            "  - { id: j, type: judge, state: { s: \"{{nodes.w.output}}\" }, questions: { q: { type: noul, instructions: i } } }\n  - { id: r, type: agent_task, prompt: \"{{nodes.j.output}}\" }\n  - { id: w, type: agent_task, prompt: p, connectors: [{ name: gh, untrusted: true }] }\n",
            "",
        ));
        assert!(
            a.tainted.get("r").map(|s| s.is_empty()).unwrap_or(true),
            "the reader of a judge answer is tainted: {:?}",
            a.tainted.get("r")
        );
    }

    /// A finish prompt without a context read gets the terminal context
    /// appended, so it is tainted while anything in the run is.
    #[test]
    fn a_finish_prompt_without_a_context_read_is_tainted_while_anything_is() {
        let nodes = "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: gh, untrusted: true }] }\n  - { id: fin, type: finish, outcome: success, prompt: \"done\" }\n";
        let a = analyze(&playbook(nodes, ""));
        assert!(!a.tainted["fin"].is_empty());
        // A finish prompt that places a clean node's output itself keeps
        // its choice: no terminal context is appended, and the read is of
        // a clean node.
        let nodes = "  - { id: ok, type: prompt, prompt: clean }\n  - { id: w, type: agent_task, prompt: p, connectors: [{ name: gh, untrusted: true }] }\n  - { id: fin, type: finish, outcome: success, prompt: \"{{nodes.ok.output}}\" }\n";
        let a = analyze(&playbook(nodes, ""));
        assert!(a.tainted.get("fin").map(|s| s.is_empty()).unwrap_or(true));
    }

    /// A template-form `writes` reading untrusted text taints the node; a
    /// glob-form `writes` reading nothing does not.
    #[test]
    fn a_template_form_writes_taints_and_a_glob_form_does_not() {
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: p, writes: \"{{params.body}}\" }\n  - { id: g, type: agent_task, prompt: p, writes: [\"src/**\"] }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n",
        ));
        assert!(!a.tainted["w"].is_empty());
        assert!(a.tainted.get("g").map(|s| s.is_empty()).unwrap_or(true));
    }

    /// The outward reasons, in their fixed order.
    #[test]
    fn the_outward_reasons() {
        // Unisolated: runs with network access.
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: \"{{params.body}}\" }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n",
        ));
        assert_eq!(a.followers[0].outward, ["runs with network access"]);

        // Best effort: no sandbox, so still network.
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: \"{{params.body}}\", isolation: best_effort }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n",
        ));
        assert_eq!(a.followers[0].outward, ["runs with network access"]);

        // Full with the network open: still outward.
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: \"{{params.body}}\", isolation: { level: full, network: open } }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n",
        ));
        assert_eq!(a.followers[0].outward, ["runs with network access"]);

        // Full with no network, no connector, no outward effect: the node
        // still renders untrusted text, but nothing it can send outward
        // with.
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: \"{{params.body}}\", isolation: { level: full, network: none } }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n",
        ));
        assert_eq!(a.followers.len(), 1);
        assert!(a.followers[0].outward.is_empty());

        // A connector makes even a sandboxed agent outward.
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: \"{{params.body}}\", isolation: { level: full, network: none }, connectors: [{ name: gh }] }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n",
        ));
        assert_eq!(
            a.followers[0].outward,
            ["runs with network access", "binds connector gh"]
        );

        // A declared outward effect.
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: \"{{params.body}}\", isolation: { level: full, network: none }, effects: [irreversible] }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n",
        ));
        assert_eq!(
            a.followers[0].outward,
            ["runs with network access", "declares effect irreversible"]
        );

        // A finish with a prompt runs an agent.
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: p, connectors: [{ name: gh, untrusted: true }] }\n  - { id: fin, type: finish, outcome: success, prompt: \"{{run.context}}\" }\n",
            "",
        ));
        let fin = a.followers.iter().find(|f| f.node == "fin").unwrap();
        assert_eq!(fin.outward, ["runs an agent with network access"]);

        // A playbook node runs another playbook.
        let a = analyze(&playbook(
            "  - { id: sub, type: playbook, playbook: other, instruction: \"x {{params.body}}\" }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n",
        ));
        assert_eq!(
            a.followers[0].outward,
            ["runs playbook other, which can do anything"]
        );
    }

    /// The secrets entries, in their fixed order.
    #[test]
    fn the_secrets_entries() {
        let a = analyze(&playbook(
            "  - { id: w, type: agent_task, prompt: \"{{params.body}} {{params.token}}\", connectors: [{ name: gh }], effects: [secrets] }\n",
            "params:\n  - { name: body, type: text, untrusted: true }\n  - { name: token, type: text, secret: true }\n",
        ));
        // The node reads an untrusted param and a secret one.
        let w = a.followers.iter().find(|f| f.node == "w").unwrap();
        assert_eq!(
            w.secrets,
            [
                "secret param token".to_string(),
                "connector gh with its account".to_string(),
                "declares effect secrets".to_string(),
            ]
        );
    }

    /// The labels of every source.
    #[test]
    fn the_source_labels() {
        assert_eq!(
            Source::Param {
                name: "issue_body".into()
            }
            .label(),
            "param issue_body"
        );
        assert_eq!(
            Source::Connector {
                node: "list_open".into(),
                connector: "github".into()
            }
            .label(),
            "connector github on list_open"
        );
        assert_eq!(
            Source::Node {
                node: "fetch".into()
            }
            .label(),
            "node fetch"
        );
        assert_eq!(
            Source::EarlierRun {
                node: "draft".into()
            }
            .label(),
            "earlier run's output of draft"
        );
    }
}
