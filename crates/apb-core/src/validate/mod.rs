//! Playbook validation: the V-coded rules a definition must satisfy before it
//! can be stored or run.
//!
//! [`validate`] is the orchestrator; each rule family lives in its own module
//! ([`graph`] for graph shape and routing, [`nodes`] for per-node fields,
//! [`connectors`] for grants, [`templates`] for reference resolution), so a
//! new rule lands next to the rules it is related to instead of at the end of
//! one long file.

mod connectors;
mod graph;
mod nodes;
mod templates;

use std::collections::{HashMap, HashSet, VecDeque};

use crate::profile::{ProfileScope, QualifiedProfileRef};
use crate::profile_store::PlaybookOrigin;
use crate::schema::{
    CacheMode, CacheSpec, EdgeCondition, FailurePolicy, FunctionsAllow, GoalCheck, Isolation,
    NodeKind, Playbook, SuccessCheck,
};

use connectors::check_connectors;
use graph::{
    check_conditions, check_cycles, check_edges, check_edges_exist, check_failure_policy,
    check_joins, check_reachability, check_start_finish, check_unique_ids,
};
use nodes::{
    check_cache, check_expected_duration, check_finish, check_goal, check_interactive,
    check_isolation, check_playbook_ref, check_scripts, check_success_check, check_trigger,
};
use templates::{check_cross_branch_reads, check_refs, check_templates};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    pub node: Option<String>,
}

/// Renders a validation failure as `validation failed:` followed by one line
/// per issue: `- <code> <severity> (node \`<id>\`): <message>`, omitting the
/// `(node ...)` segment when the issue has no node. This is the single
/// canonical rendering for a `Vec<Issue>` becoming user-facing text:
/// `VersioningError::Validation`'s `Display` goes through it (so every
/// transitive wrapper - `BundleError`, `EngineError`, ... - renders the same
/// lines for free), and the MCP layer delegates to it too, so the format
/// never drifts between surfaces.
pub fn render_issues(issues: &[Issue]) -> String {
    let mut out = String::from("validation failed:");
    for issue in issues {
        let severity = match issue.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        out.push_str("\n- ");
        out.push_str(issue.code);
        out.push(' ');
        out.push_str(severity);
        if let Some(node) = &issue.node {
            out.push_str(" (node `");
            out.push_str(node);
            out.push_str("`)");
        }
        out.push_str(": ");
        out.push_str(&issue.message);
    }
    out
}

#[derive(Debug, Default)]
pub struct ValidationReport {
    pub issues: Vec<Issue>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        !self.issues.iter().any(|i| i.severity == Severity::Error)
    }
    fn error(&mut self, code: &'static str, node: Option<&str>, msg: String) {
        self.issues.push(Issue {
            code,
            severity: Severity::Error,
            message: msg,
            node: node.map(String::from),
        });
    }
    fn warn(&mut self, code: &'static str, node: Option<&str>, msg: String) {
        self.issues.push(Issue {
            code,
            severity: Severity::Warning,
            message: msg,
            node: node.map(String::from),
        });
    }
}

#[derive(Debug, Default)]
pub struct ValidationContext {
    /// Names of the available project profiles (for a structural existence
    /// check). Full scope-aware resolution happens at run start.
    pub profiles: Vec<String>,
    /// Origin of the playbook being checked: a global playbook cannot
    /// reference a profile with `scope: project` (V14).
    pub playbook_origin: PlaybookOrigin,
}

pub fn validate(playbook: &Playbook, ctx: &ValidationContext) -> ValidationReport {
    let mut r = ValidationReport::default();
    check_unique_ids(playbook, &mut r); // V01, V02
    check_expected_duration(playbook, &mut r); // V19, V20
    check_finish(playbook, &mut r); // V21
    check_playbook_ref(playbook, &mut r); // V22
    check_connectors(playbook, &mut r); // V23, V24, V25, V26
    check_cache(playbook, &mut r); // V27, V28, V29
    check_edges(playbook, &mut r); // V30, V34
    check_interactive(playbook, &mut r); // V31, V32
    check_success_check(playbook, &mut r); // V33
    check_start_finish(playbook, &mut r); // V03, V04, V05
    check_edges_exist(playbook, &mut r); // V06
    check_failure_policy(playbook, &mut r); // V35
    if r.is_valid() {
        check_reachability(playbook, &mut r); // V07, V08
        check_conditions(playbook, &mut r); // V09, V10
        check_cycles(playbook, &mut r); // V11
        check_joins(playbook, &mut r); // V36, V37
        check_scripts(playbook, &mut r); // V12
        check_templates(playbook, &mut r); // V13
        check_cross_branch_reads(playbook, &mut r); // V38
        check_refs(playbook, ctx, &mut r); // V14, V15
        check_isolation(playbook, &mut r); // V16
        check_trigger(playbook, &mut r); // V17
        check_goal(playbook, &mut r); // V41
    }
    r
}

/// Compiles glob patterns into a `GlobSet`. Shared by the validator (V29)
/// and by later cache-fingerprinting tasks, so the compiled matcher and the
/// error reporting stay identical between validate-time and run-time. On
/// failure, returns the offending pattern rather than the underlying
/// `globset` error so callers can report exactly which glob is invalid.
pub fn build_globset(globs: &[String]) -> Result<globset::GlobSet, String> {
    let mut builder = globset::GlobSetBuilder::new();
    for g in globs {
        builder.add(globset::Glob::new(g).map_err(|_| g.clone())?);
    }
    builder.build().map_err(|_| globs.join(","))
}

#[cfg(test)]
mod connector_tests {
    use super::*;
    use crate::schema::Playbook;

    fn ctx() -> ValidationContext {
        ValidationContext::default()
    }

    fn codes(yaml: &str) -> Vec<&'static str> {
        let playbook = Playbook::from_yaml(yaml).unwrap();
        validate(&playbook, &ctx())
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.code)
            .collect()
    }

    const GOOD: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: a
    type: agent_task
    prompt: hi
    profile: x
    connectors:
      - { name: telegram, accounts: [team-bot], functions: [send_message], max_calls: 50 }
      - jira
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

    #[test]
    fn valid_connector_bindings_have_no_errors() {
        let c = codes(GOOD);
        assert!(c.is_empty(), "expected no errors, got {c:?}");
    }

    #[test]
    fn v23_invalid_connector_name_is_rejected() {
        let bad = GOOD.replace("name: telegram", "name: Telegram");
        assert!(codes(&bad).contains(&"V23"));
    }

    #[test]
    fn v23_invalid_account_entry_is_rejected() {
        let bad = GOOD.replace("accounts: [team-bot]", "accounts: [Team-Bot]");
        assert!(codes(&bad).contains(&"V23"));
    }

    #[test]
    fn v23_invalid_function_entry_is_rejected() {
        let bad = GOOD.replace("functions: [send_message]", "functions: [send-message]");
        assert!(codes(&bad).contains(&"V23"));
    }

    #[test]
    fn v24_duplicate_connector_name_is_rejected() {
        let bad = GOOD.replace("      - jira", "      - jira\n      - jira");
        assert!(codes(&bad).contains(&"V24"));
    }

    #[test]
    fn v25_duplicate_account_entry_is_rejected() {
        let bad = GOOD.replace("accounts: [team-bot]", "accounts: [team-bot, team-bot]");
        assert!(codes(&bad).contains(&"V25"));
    }

    #[test]
    fn v25_empty_function_entry_is_rejected() {
        let bad = GOOD.replace("functions: [send_message]", r#"functions: [""]"#);
        assert!(codes(&bad).contains(&"V25"));
    }

    #[test]
    fn v26_max_calls_zero_is_rejected() {
        let bad = GOOD.replace("max_calls: 50", "max_calls: 0");
        assert!(codes(&bad).contains(&"V26"));
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::schema::Playbook;

    fn ctx() -> ValidationContext {
        ValidationContext::default()
    }

    /// Wraps a `nodes:`/`edges:` YAML fragment with the schema 2 preamble
    /// every playbook needs (schema/id/name/version), following the style of
    /// `connector_tests::GOOD` above.
    fn pb_yaml(body: &str) -> Playbook {
        let yaml = format!("schema: 2\nid: p\nname: p\nversion: 1.0.0\n{body}\n");
        Playbook::from_yaml(&yaml).unwrap()
    }

    /// All issues (errors and warnings) as `(code, severity)` pairs, so a
    /// warning-only code like V28 can be asserted without losing severity.
    fn codes(pb: &Playbook) -> Vec<(&'static str, Severity)> {
        validate(pb, &ctx())
            .issues
            .iter()
            .map(|i| (i.code, i.severity))
            .collect()
    }

    #[test]
    fn v27_cache_on_uncacheable_kind() {
        let pb = pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: c, type: condition, cache: auto }
edges: []"#,
        );
        assert!(codes(&pb).contains(&("V27", Severity::Error)));
    }

    #[test]
    fn v28_ttl_without_auto_mode() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, cache: { mode: off, ttl: 1h } }
edges: []"#,
        );
        assert!(codes(&pb).contains(&("V28", Severity::Warning)));
    }

    #[test]
    fn v29_invalid_glob() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - id: a
    type: agent_task
    prompt: hi
    cache: auto
    inputs: { files: ["src/[**"] }
edges: []"#,
        );
        assert!(codes(&pb).contains(&("V29", Severity::Error)));
    }
}

#[cfg(test)]
mod interactive_tests {
    use super::*;
    use crate::schema::Playbook;

    fn ctx() -> ValidationContext {
        ValidationContext::default()
    }

    /// Wraps a `nodes:`/`edges:` YAML fragment with the schema 2 preamble,
    /// mirroring `cache_tests::pb_yaml` above.
    fn pb_yaml(body: &str) -> Playbook {
        let yaml = format!("schema: 2\nid: p\nname: p\nversion: 1.0.0\n{body}\n");
        Playbook::from_yaml(&yaml).unwrap()
    }

    /// All issues as `(code, severity)` pairs, mirroring `cache_tests::codes`.
    fn codes(pb: &Playbook) -> Vec<(&'static str, Severity)> {
        validate(pb, &ctx())
            .issues
            .iter()
            .map(|i| (i.code, i.severity))
            .collect()
    }

    #[test]
    fn v31_companion_field_without_interactive_true() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, answer_by: supervisor }
edges: []"#,
        );
        assert!(codes(&pb).contains(&("V31", Severity::Error)));
        let report = validate(&pb, &ctx());
        let issue = report
            .issues
            .iter()
            .find(|i| i.code == "V31")
            .expect("expected a V31 error");
        assert!(
            issue.message.contains("interactive: true"),
            "V31 message must mention `interactive: true`, got: {}",
            issue.message
        );
    }

    #[test]
    fn v32_default_answer_without_timeout() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, interactive: true, default_answer: "x" }
edges: []"#,
        );
        assert!(codes(&pb).contains(&("V32", Severity::Error)));
        let report = validate(&pb, &ctx());
        let issue = report
            .issues
            .iter()
            .find(|i| i.code == "V32")
            .expect("expected a V32 error");
        assert!(
            issue.message.contains("question_timeout_seconds"),
            "V32 message must mention `question_timeout_seconds`, got: {}",
            issue.message
        );
    }

    #[test]
    fn well_formed_interactive_node_has_no_v31_or_v32() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, interactive: true, question_timeout_seconds: 60, default_answer: "x" }
edges: []"#,
        );
        let c = codes(&pb);
        assert!(!c.contains(&("V31", Severity::Error)), "got {c:?}");
        assert!(!c.contains(&("V32", Severity::Error)), "got {c:?}");
    }
}

#[cfg(test)]
mod success_check_tests {
    use super::*;
    use crate::schema::Playbook;

    fn ctx() -> ValidationContext {
        ValidationContext::default()
    }

    fn pb_yaml(body: &str) -> Playbook {
        let yaml = format!("schema: 2\nid: p\nname: p\nversion: 1.0.0\n{body}\n");
        Playbook::from_yaml(&yaml).unwrap()
    }

    fn error_codes(pb: &Playbook) -> Vec<&'static str> {
        validate(pb, &ctx())
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.code)
            .collect()
    }

    #[test]
    fn v33_success_check_on_non_agent_task_is_rejected() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: sc, type: script, script: "scripts/x.sh", runner: sh, success_check: { marker: "DONE" } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: sc }
  - { from: sc, to: done }"#,
        );
        assert!(error_codes(&pb).contains(&"V33"), "expected V33");
    }

    #[test]
    fn v33_empty_marker_is_rejected() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, success_check: { marker: "" } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: done }"#,
        );
        assert!(error_codes(&pb).contains(&"V33"), "expected V33");
    }

    #[test]
    fn v33_whitespace_only_marker_is_rejected() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, success_check: { marker: "   " } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: done }"#,
        );
        assert!(error_codes(&pb).contains(&"V33"), "expected V33");
    }

    #[test]
    fn valid_marker_on_agent_task_has_no_v33() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, success_check: { marker: "WAVE-COMPLETE" } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: done }"#,
        );
        let c = error_codes(&pb);
        assert!(!c.contains(&"V33"), "got {c:?}");
    }

    #[test]
    fn valid_script_form_on_agent_task_has_no_v33_or_v12() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, success_check: "scripts/check.sh" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: done }"#,
        );
        let c = error_codes(&pb);
        assert!(!c.contains(&"V33"), "got {c:?}");
        assert!(!c.contains(&"V12"), "got {c:?}");
    }

    #[test]
    fn v12_script_form_outside_scripts_dir_is_rejected() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, success_check: "check.sh" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: done }"#,
        );
        assert!(error_codes(&pb).contains(&"V12"), "expected V12");
    }
}

#[cfg(test)]
mod join_value_tests {
    use super::*;
    use crate::schema::Playbook;

    fn pb_yaml(body: &str) -> Playbook {
        let yaml = format!("schema: 2\nid: p\nname: p\nversion: 1.0.0\n{body}\n");
        Playbook::from_yaml(&yaml).unwrap()
    }

    fn issues(pb: &Playbook) -> Vec<(&'static str, Severity)> {
        validate(pb, &ValidationContext::default())
            .issues
            .iter()
            .map(|i| (i.code, i.severity))
            .collect()
    }

    fn messages(pb: &Playbook, code: &str) -> Vec<String> {
        validate(pb, &ValidationContext::default())
            .issues
            .iter()
            .filter(|i| i.code == code)
            .map(|i| i.message.clone())
            .collect()
    }

    /// The classic diamond, with each merge edge's `join` suffix supplied by the
    /// caller (`", join: all"`, or `""` for a barrier-less fan-in).
    fn diamond(join_a: &str, join_b: &str) -> Playbook {
        pb_yaml(&format!(
            r#"
nodes:
  - {{ id: s, type: start }}
  - {{ id: a, type: prompt, prompt: "a" }}
  - {{ id: b, type: prompt, prompt: "b" }}
  - {{ id: j, type: prompt, prompt: "j" }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: s, to: a }}
  - {{ from: s, to: b }}
  - {{ from: a, to: j{join_a} }}
  - {{ from: b, to: j{join_b} }}
  - {{ from: j, to: done }}"#
        ))
    }

    #[test]
    fn v36_unknown_join_value_is_an_error() {
        let pb = diamond(", join: al", ", join: all");
        assert!(
            issues(&pb).contains(&("V36", Severity::Error)),
            "expected V36 for `join: al`, got {:?}",
            issues(&pb)
        );
        let msgs = messages(&pb, "V36");
        assert!(
            msgs.iter().any(|m| m.contains("al") && m.contains("all")),
            "V36 must name the bad value and the expected ones, got: {msgs:?}"
        );
    }

    #[test]
    fn valid_join_values_have_no_join_issues() {
        for pb in [diamond(", join: all", ", join: all"), diamond("", "")] {
            let c = issues(&pb);
            assert!(!c.iter().any(|(code, _)| *code == "V36"), "got {c:?}");
            assert!(!c.iter().any(|(code, _)| *code == "V37"), "got {c:?}");
        }
    }

    #[test]
    fn v37_mixed_join_modes_on_one_node_warn() {
        let pb = diamond(", join: all", ", join: any");
        let c = issues(&pb);
        assert!(
            c.contains(&("V37", Severity::Warning)),
            "expected V37 for mixed all/any, got {c:?}"
        );
        assert!(
            !c.iter().any(|(code, _)| *code == "V36"),
            "valid values must not be V36, got {c:?}"
        );
        let msgs = messages(&pb, "V37");
        assert!(
            msgs.iter()
                .any(|m| m.contains("j") && m.contains("all") && m.contains("any")),
            "V37 must name the node and both modes, got: {msgs:?}"
        );
    }

    #[test]
    fn v37_does_not_fire_across_different_nodes() {
        // `join: all` into one node and `join: any` into another is not a mix.
        let pb = pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j1, type: prompt, prompt: "j1" }
  - { id: j2, type: prompt, prompt: "j2" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: s, to: b }
  - { from: a, to: j1, join: all }
  - { from: b, to: j1, join: all }
  - { from: a, to: j2, join: any }
  - { from: b, to: j2, join: any }
  - { from: j1, to: done }
  - { from: j2, to: done }"#,
        );
        let c = issues(&pb);
        assert!(!c.iter().any(|(code, _)| *code == "V37"), "got {c:?}");
    }
}

#[cfg(test)]
mod cross_branch_read_tests {
    use super::*;
    use crate::schema::Playbook;

    fn pb_yaml(body: &str) -> Playbook {
        let yaml = format!("schema: 2\nid: p\nname: p\nversion: 1.0.0\n{body}\n");
        Playbook::from_yaml(&yaml).unwrap()
    }

    fn issues(pb: &Playbook) -> Vec<(&'static str, Severity)> {
        validate(pb, &ValidationContext::default())
            .issues
            .iter()
            .map(|i| (i.code, i.severity))
            .collect()
    }

    fn v38(pb: &Playbook) -> Vec<(Option<String>, String)> {
        validate(pb, &ValidationContext::default())
            .issues
            .iter()
            .filter(|i| i.code == "V38")
            .map(|i| (i.node.clone(), i.message.clone()))
            .collect()
    }

    fn assert_no_v38(pb: &Playbook) {
        let found = v38(pb);
        assert!(found.is_empty(), "expected no V38, got {found:?}");
    }

    /// Two parallel branches with no barrier between them: `b` reading `a` is a
    /// race, the two run concurrently.
    #[test]
    fn v38_read_of_a_parallel_sibling_warns() {
        let pb = pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b needs {{nodes.a.output}}" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: s, to: b }
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: done }"#,
        );
        let found = v38(&pb);
        assert_eq!(found.len(), 1, "expected exactly one V38, got {found:?}");
        assert_eq!(found[0].0.as_deref(), Some("b"), "V38 owner is the reader");
        assert!(
            found[0].1.contains("nodes.a.output") && found[0].1.contains('b'),
            "V38 must name the reference and the reader, got: {found:?}"
        );
        assert!(
            issues(&pb).contains(&("V38", Severity::Warning)),
            "V38 is a warning, not an error"
        );
    }

    /// The barrier-less diamond's merge point is an implicit all-join (Task 1),
    /// so it waits for both branches: reading both is safe.
    #[test]
    fn read_at_an_implicit_join_is_clean() {
        assert_no_v38(&pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "{{nodes.a.output}} + {{nodes.b.report}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: s, to: b }
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: done }"#,
        ));
    }

    /// Behind an explicit `join: all` the read is safe even one hop further
    /// downstream of the barrier.
    #[test]
    fn read_behind_an_explicit_join_is_clean() {
        assert_no_v38(&pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: after, type: prompt, prompt: "{{nodes.a.output}} and {{nodes.b.output}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: s, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: after }
  - { from: after, to: done }"#,
        ));
    }

    /// `join: any` fires on first arrival, so the other branch may still be
    /// running when the join node reads it.
    #[test]
    fn v38_read_at_an_any_join_warns() {
        let pb = pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: j, type: prompt, prompt: "{{nodes.a.output}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: s, to: b }
  - { from: a, to: j, join: any }
  - { from: b, to: j, join: any }
  - { from: j, to: done }"#,
        );
        let found = v38(&pb);
        assert_eq!(
            found.len(),
            1,
            "expected one V38 at an any-join, got {found:?}"
        );
        assert_eq!(found[0].0.as_deref(), Some("j"));
    }

    /// A linear chain guarantees every upstream node finished.
    #[test]
    fn linear_chain_read_is_clean() {
        assert_no_v38(&pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b saw {{nodes.a.output}}" }
  - { id: done, type: finish, outcome: success, prompt: "sum of {{nodes.a.output}} and {{nodes.b.output}}" }
edges:
  - { from: s, to: a }
  - { from: a, to: b }
  - { from: b, to: done }"#,
        ));
    }

    /// An either-or conditional fork merging back: the merge legitimately
    /// references both branches even though only one of them ran.
    #[test]
    fn either_or_conditional_merge_reading_both_branches_is_clean() {
        assert_no_v38(&pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: gate, type: condition }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: m, type: prompt, prompt: "{{nodes.a.output}} or {{nodes.b.output}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: gate }
  - { from: gate, to: a, condition: { type: node_status, node: gate, equals: success } }
  - { from: gate, to: b, condition: { type: node_status, node: gate, equals: failure } }
  - { from: a, to: m }
  - { from: b, to: m }
  - { from: m, to: done }"#,
        ));
    }

    /// A loop-carried read (reader and source inside one cycle) keeps
    /// first-arrival semantics on purpose and is an authoring idiom, not a race.
    #[test]
    fn loop_carried_read_is_clean() {
        assert_no_v38(&pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: work, type: prompt, prompt: "improve on {{nodes.review.output}}" }
  - { id: review, type: prompt, prompt: "review" }
  - { id: gate, type: condition, max_loops: 3 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: work }
  - { from: work, to: review }
  - { from: review, to: gate }
  - { from: gate, to: work, condition: { type: node_status, node: gate, equals: failure } }
  - { from: gate, to: done, condition: { type: node_status, node: gate, equals: success } }"#,
        ));
    }

    /// The `defaults.on_failure` handler is entered from anywhere in the graph,
    /// so no drawn edge describes what preceded it and V38 stays silent. This is
    /// a SILENCE rule: a regression here is a false warning, not a missed one.
    #[test]
    fn v38_silent_for_the_on_failure_handler() {
        let pb = pb_yaml(
            r#"
defaults: { on_failure: h }
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: h, type: prompt, prompt: "handler reads {{nodes.a.output}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: s, to: h }
  - { from: a, to: done }
  - { from: h, to: done }"#,
        );
        assert_no_v38(&pb);
    }

    /// A node reading its own output is loop-carried by definition, never racy.
    #[test]
    fn v38_silent_for_a_self_read() {
        let pb = pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a rereads {{nodes.a.output}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: done }"#,
        );
        assert_no_v38(&pb);
    }

    /// An unknown source is V13's business. V38 defers rather than piling a
    /// second, less useful diagnosis on the same line.
    #[test]
    fn v38_silent_for_an_unknown_source_already_v13() {
        let pb = pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: b, type: prompt, prompt: "b needs {{nodes.missing.output}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: b }
  - { from: b, to: done }"#,
        );
        assert!(
            issues(&pb).contains(&("V13", Severity::Error)),
            "an unknown template source must still be V13"
        );
        assert_no_v38(&pb);
    }

    /// Not a sibling race but a strictly backwards read: `owner` is a genuine
    /// ancestor of `source`, so `source` cannot possibly have run yet.
    #[test]
    fn v38_read_of_a_strictly_downstream_node_warns() {
        let pb = pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: owner, type: prompt, prompt: "owner needs {{nodes.source.output}}" }
  - { id: source, type: prompt, prompt: "source" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: owner }
  - { from: owner, to: source }
  - { from: source, to: done }"#,
        );
        let found = v38(&pb);
        assert_eq!(found.len(), 1, "expected exactly one V38, got {found:?}");
        assert_eq!(
            found[0].0.as_deref(),
            Some("owner"),
            "V38 owner is the reader"
        );
    }

    /// The either-or merge is already pinned for the IMPLICIT acyclic fan-in.
    /// The explicit `join: all` branch must produce the same silence.
    #[test]
    fn either_or_merge_reading_both_branches_behind_explicit_join_all_is_clean() {
        let pb = pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: gate, type: condition }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: m, type: prompt, prompt: "merge {{nodes.a.output}} and {{nodes.b.output}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: gate }
  - { from: gate, to: a, condition: { type: node_status, node: gate, equals: success } }
  - { from: gate, to: b, condition: { type: node_status, node: gate, equals: failure } }
  - { from: a, to: m, join: all }
  - { from: b, to: m, join: all }
  - { from: m, to: done }"#,
        );
        assert_no_v38(&pb);
    }

    /// The old remedy told the author to put `join: all` on the edges into the
    /// reader, but the shape this rule most often catches gives the reader ONE
    /// incoming edge, and a join needs at least two to do anything at all.
    #[test]
    fn v38_message_does_not_advise_join_all_on_a_single_input_reader() {
        let pb = pb_yaml(
            r#"
nodes:
  - { id: s, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b needs {{nodes.a.output}}" }
  - { id: j, type: prompt, prompt: "j" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: s, to: b }
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: done }"#,
        );
        let found = v38(&pb);
        assert_eq!(found.len(), 1, "expected exactly one V38, got {found:?}");
        let msg = &found[0].1;
        assert!(
            msg.contains("nodes.a.output"),
            "the message names the read: {msg}"
        );
        assert!(
            !msg.contains("join: all"),
            "V38 must not advise a join on a reader with one incoming edge: {msg}"
        );
        assert!(
            msg.contains("route the read behind"),
            "V38 must name the remedy that works: {msg}"
        );
    }
}

#[cfg(test)]
mod duplicate_route_tests {
    use super::*;
    use crate::schema::Playbook;

    fn ctx() -> ValidationContext {
        ValidationContext::default()
    }

    fn pb_yaml(body: &str) -> Playbook {
        let yaml = format!("schema: 2\nid: p\nname: p\nversion: 1.0.0\n{body}\n");
        Playbook::from_yaml(&yaml).unwrap()
    }

    fn error_codes(pb: &Playbook) -> Vec<&'static str> {
        validate(pb, &ctx())
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.code)
            .collect()
    }

    fn v34_messages(pb: &Playbook) -> Vec<String> {
        validate(pb, &ctx())
            .issues
            .iter()
            .filter(|i| i.code == "V34")
            .map(|i| i.message.clone())
            .collect()
    }

    /// The issue-45 finding 7 shape: two edges from `finalize` both matching
    /// `failure`, pointing at different targets. First-match would send a
    /// failed finalize into the success finish.
    #[test]
    fn v34_identical_failure_conditions_different_targets() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: finalize, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
  - { id: aborted, type: finish, outcome: failure }
edges:
  - { from: s, to: finalize }
  - { from: finalize, to: done, condition: { type: node_status, node: finalize, equals: failure } }
  - { from: finalize, to: aborted, condition: { type: node_status, node: finalize, equals: failure } }"#,
        );
        assert!(error_codes(&pb).contains(&"V34"), "expected V34");
        let msgs = v34_messages(&pb);
        assert!(
            msgs.iter()
                .any(|m| m.contains("finalize") && m.contains("done") && m.contains("aborted")),
            "V34 must name the node and both targets, got: {msgs:?}"
        );
    }

    /// Success vs failure are different conditions; that is the normal branch shape.
    #[test]
    fn different_conditions_to_different_targets_is_ok() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: finalize, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
  - { id: aborted, type: finish, outcome: failure }
edges:
  - { from: s, to: finalize }
  - { from: finalize, to: done, condition: { type: node_status, node: finalize, equals: success } }
  - { from: finalize, to: aborted, condition: { type: node_status, node: finalize, equals: failure } }"#,
        );
        let c = error_codes(&pb);
        assert!(!c.contains(&"V34"), "got {c:?}");
    }

    /// Identical condition and the same target is redundant, not contradictory.
    #[test]
    fn identical_condition_same_target_is_ok() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: done, condition: { type: node_status, node: a, equals: success } }
  - { from: a, to: done, condition: { type: node_status, node: a, equals: success } }"#,
        );
        let c = error_codes(&pb);
        assert!(
            !c.contains(&"V34"),
            "same-target duplicates must not be V34, got {c:?}"
        );
    }

    /// Several unconditional edges from one node are parallel fan-out, not V34.
    #[test]
    fn parallel_unconditional_edges_are_ok() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: b, type: agent_task, prompt: hi }
  - { id: join, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: s, to: b }
  - { from: a, to: join, join: all }
  - { from: b, to: join, join: all }
  - { from: join, to: done }"#,
        );
        let c = error_codes(&pb);
        assert!(
            !c.contains(&"V34"),
            "parallel fan-out must not be V34, got {c:?}"
        );
    }

    /// Two fallback edges with different targets: first-match takes only the first.
    #[test]
    fn v34_two_fallbacks_different_targets() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
  - { id: aborted, type: finish, outcome: failure }
edges:
  - { from: s, to: a }
  - { from: a, to: done, fallback: true }
  - { from: a, to: aborted, fallback: true }"#,
        );
        assert!(
            error_codes(&pb).contains(&"V34"),
            "expected V34 for dual fallbacks"
        );
    }

    /// An unconditional edge makes every conditional edge from the same node
    /// unreachable under first-match routing.
    #[test]
    fn v34_unconditional_shadows_conditional() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
  - { id: aborted, type: finish, outcome: failure }
edges:
  - { from: s, to: a }
  - { from: a, to: done }
  - { from: a, to: aborted, condition: { type: node_status, node: a, equals: failure } }"#,
        );
        assert!(
            error_codes(&pb).contains(&"V34"),
            "expected V34 for shadowing"
        );
        let msgs = v34_messages(&pb);
        assert!(
            msgs.iter().any(|m| m.contains("unreachable")),
            "V34 shadowing message must say unreachable, got: {msgs:?}"
        );
    }
}

/// The `output_field` condition variant (spec 2026-08-05 section 2.5): it names
/// a source node exactly like `node_status` and `output_match` do, so it must be
/// covered by the same V10 source checks (existence and happens-before) and take
/// part in V34 first-match duplicate detection.
#[cfg(test)]
mod output_field_condition_tests {
    use super::*;
    use crate::schema::Playbook;

    fn pb_yaml(body: &str) -> Playbook {
        let yaml = format!("schema: 2\nid: p\nname: p\nversion: 1.0.0\n{body}\n");
        Playbook::from_yaml(&yaml).unwrap()
    }

    fn error_codes(pb: &Playbook) -> Vec<&'static str> {
        validate(pb, &ValidationContext::default())
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.code)
            .collect()
    }

    /// `check` routes on one field of `verify`'s structured output. `SOURCE` is
    /// the node the two conditions read, `SECOND` the second edge's `equals`.
    fn graph(source: &str, second: &str) -> Playbook {
        pb_yaml(&format!(
            r#"
defaults: {{ profile: x }}
nodes:
  - {{ id: s, type: start }}
  - {{ id: verify, type: agent_task, prompt: hi }}
  - {{ id: check, type: condition }}
  - {{ id: fix, type: agent_task, prompt: hi }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: s, to: verify }}
  - {{ from: verify, to: check }}
  - {{ from: check, to: fix, condition: {{ type: output_field, node: {source}, field: verdict, equals: failed }} }}
  - {{ from: check, to: done, condition: {{ type: output_field, node: {source}, field: verdict, equals: {second} }} }}
  - {{ from: fix, to: done }}"#
        ))
    }

    #[test]
    fn a_valid_output_field_route_raises_nothing() {
        let c = error_codes(&graph("verify", "ok"));
        assert!(c.is_empty(), "expected a clean playbook, got {c:?}");
    }

    #[test]
    fn v10_output_field_referencing_an_unknown_node() {
        let c = error_codes(&graph("nope", "ok"));
        assert!(
            c.contains(&"V10"),
            "an output_field condition on an unknown node must be V10, got {c:?}"
        );
    }

    #[test]
    fn v10_output_field_referencing_a_node_that_cannot_run_first() {
        // `fix` only runs AFTER `check` routes to it, so its output can never be
        // available to `check`'s own routing decision.
        let c = error_codes(&graph("fix", "ok"));
        assert!(
            c.contains(&"V10"),
            "an output_field condition on a downstream node must be V10, got {c:?}"
        );
    }

    #[test]
    fn v34_two_identical_output_field_conditions_to_different_targets() {
        // Same node, field AND value: first-match takes only the first edge, so
        // the second target is unreachable.
        let c = error_codes(&graph("verify", "failed"));
        assert!(
            c.contains(&"V34"),
            "identical output_field routes to different targets must be V34, got {c:?}"
        );
    }
}

#[cfg(test)]
mod widened_condition_checks_tests {
    use super::*;
    use crate::schema::Playbook;

    fn pb_yaml(body: &str) -> Playbook {
        let yaml = format!("schema: 2\nid: p\nname: p\nversion: 1.0.0\n{body}\n");
        Playbook::from_yaml(&yaml).unwrap()
    }

    fn codes(pb: &Playbook) -> Vec<(&'static str, Severity)> {
        validate(pb, &ValidationContext::default())
            .issues
            .iter()
            .map(|i| (i.code, i.severity))
            .collect()
    }

    /// The conditions hang off `verify`, an agent_task, not off a condition node.
    /// `SOURCE` is the node the two conditions read.
    fn agent_owned(source: &str) -> Playbook {
        pb_yaml(&format!(
            r#"
defaults: {{ profile: x }}
nodes:
  - {{ id: s, type: start }}
  - {{ id: verify, type: agent_task, prompt: hi, expected_duration: 5m }}
  - {{ id: fix, type: agent_task, prompt: hi, expected_duration: 5m }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: s, to: verify }}
  - {{ from: verify, to: fix, condition: {{ type: output_field, node: {source}, field: verdict, equals: failed }} }}
  - {{ from: verify, to: done, condition: {{ type: output_field, node: {source}, field: verdict, equals: ok }} }}
  - {{ from: fix, to: done }}"#
        ))
    }

    #[test]
    fn a_valid_output_field_route_off_an_agent_task_raises_nothing() {
        let c = codes(&agent_owned("verify"));
        assert!(c.is_empty(), "expected a clean playbook, got {c:?}");
    }

    #[test]
    fn v40_output_field_off_an_agent_task_referencing_an_unknown_node() {
        let c = codes(&agent_owned("nope"));
        assert!(
            c.contains(&("V40", Severity::Warning)),
            "an unknown condition source off a non-condition node must warn V40, got {c:?}"
        );
        assert!(
            !c.iter().any(|(code, _)| *code == "V10"),
            "V10 stays an error reserved for condition nodes, got {c:?}"
        );
    }

    #[test]
    fn v40_output_field_off_an_agent_task_referencing_a_downstream_node() {
        // `fix` only runs after `verify` routes to it, so its output can never
        // be available to `verify`'s own routing decision.
        let c = codes(&agent_owned("fix"));
        assert!(
            c.contains(&("V40", Severity::Warning)),
            "a downstream condition source off a non-condition node must warn V40, got {c:?}"
        );
    }

    #[test]
    fn v39_node_status_off_an_agent_task_missing_failure_branch_warns() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: verify, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: verify }
  - { from: verify, to: done, condition: { type: node_status, node: verify, equals: success } }"#,
        );
        let c = codes(&pb);
        assert!(
            c.contains(&("V39", Severity::Warning)),
            "node_status branches off a non-condition node that cover only one outcome must warn V39, got {c:?}"
        );
    }

    #[test]
    fn v10_and_v40_do_not_both_fire_on_the_same_condition_node() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: verify, type: agent_task, prompt: hi }
  - { id: check, type: condition }
  - { id: fix, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: verify }
  - { from: verify, to: check }
  - { from: check, to: fix, condition: { type: output_field, node: nope, field: verdict, equals: failed } }
  - { from: check, to: done, condition: { type: output_field, node: nope, field: verdict, equals: ok } }
  - { from: fix, to: done }"#,
        );
        let c = codes(&pb);
        assert!(
            c.iter()
                .any(|(code, sev)| *code == "V10" && *sev == Severity::Error),
            "a condition node keeps the V10 error, got {c:?}"
        );
        assert!(
            !c.iter().any(|(code, _)| *code == "V40"),
            "the owner-kind branch is exclusive, not additive, got {c:?}"
        );
    }

    /// A playbook with no conditional edges anywhere must be untouched by the
    /// widening: the loop now visits every node, so the early skip matters.
    #[test]
    fn a_plain_linear_playbook_gains_no_new_warning() {
        let pb = pb_yaml(
            r#"
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, expected_duration: 5m }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: done }"#,
        );
        let c = codes(&pb);
        assert!(c.is_empty(), "expected a clean playbook, got {c:?}");
    }
}
