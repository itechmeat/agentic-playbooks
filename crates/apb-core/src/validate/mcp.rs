//! The binding rules of MCP connectors (V300 to V303): a binding that
//! grants every function warns (V300), an interpreter or launcher command
//! warns (V301), an MCP connector at an engine site is refused (V302), and
//! an http MCP connector whose auth header is one the transport sets is
//! refused (V303).

use super::*;

/// V300 (warning): a binding of an MCP connector (its `ConnectorFacts`
/// carry `mcp_transport`) with no `functions` list grants every tool the
/// manifest maps, destructive ones included. V301 (warning): the
/// connector's stdio `command` is an interpreter or a launcher
/// (`MCP_LAUNCHERS`), whose approval covers that program file only, not
/// the script or package it runs. V302 (error): an MCP connector bound at an engine
/// site (the baseline check, the follow-up check, a list item check or an
/// inbox wait).
/// V303 (error): an http MCP connector whose `auth: { kind: header }`
/// names a header the transport sets itself
/// (`ConnectorFacts.mcp_reserved_header`), at every binding site.
/// Silent when `ctx.connectors` is empty or does not know the connector.
pub(super) fn check_mcp_bindings(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    use crate::connector::sites::{SiteKind, site_label};
    for site in playbook.connector_sites() {
        let site_id: &str = site.id.as_ref();
        let label = site_label(&site);
        let mut seen = HashSet::new();
        for b in site.bindings.iter() {
            let Some(facts) = ctx.connectors.get(&b.name) else {
                continue;
            };
            if facts.mcp_transport.is_none() || !seen.insert(b.name.as_str()) {
                continue;
            }
            let name = &b.name;
            if let Some(header) = &facts.mcp_reserved_header {
                r.error(
                    "V303",
                    Some(site_id),
                    format!(
                        "node `{site_id}` binds MCP connector `{name}`, whose auth header `{header}` is one the HTTP transport sets itself; name another header"
                    ),
                );
            }
            // The site walk says what a site is: only a node's own site
            // may bind an MCP connector. A list item check, an inbox wait
            // and the engine's own sites run from the drive or a waker,
            // where no MCP server is started.
            if !matches!(site.kind, SiteKind::Node) {
                r.error(
                    "V302",
                    Some(site_id),
                    format!(
                        "{label} reads MCP connector `{name}`; an MCP server is started only for a node's own call in this version (the baseline check, the follow-up check, list item checks and inbox waits run from the drive or a waker)"
                    ),
                );
                continue;
            }
            if b.functions == FunctionsAllow::All {
                r.warn(
                    "V300",
                    Some(site_id),
                    format!(
                        "node `{site_id}` grants every function of MCP connector `{name}`; name the functions it needs (`functions: [..]` or `functions: read_only`)"
                    ),
                );
            }
            if let Some(launcher) = &facts.mcp_launcher {
                r.warn(
                    "V301",
                    Some(site_id),
                    format!(
                        "node `{site_id}` binds MCP connector `{name}`, whose command `{launcher}` is an interpreter or a launcher: its approval covers the `{launcher}` program file only, not the script or package it runs; point the command at the installed server program instead"
                    ),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::def::ConnectorDoc;
    use crate::connector::resolve::ConnectorFacts;

    /// An MCP connector with one read and one write.
    const TRACKER: &str = r#"name: tracker
version: 0.1.0
mcp: { transport: stdio, command: tracker-mcp-server }
functions:
  - name: search_issues
    description: Search issues
    read_only: true
    mcp: { tool: search_issues }
  - name: create_comment
    description: Add a comment
    mcp: { tool: create_comment }
    idempotency: { mode: none }
"#;

    fn facts(yaml: &str) -> ConnectorFacts {
        ConnectorFacts::of_manifest(&ConnectorDoc::from_yaml(yaml, "tracker").unwrap())
    }

    fn ctx_with(f: ConnectorFacts) -> ValidationContext {
        let mut ctx = ValidationContext::default();
        ctx.connectors.insert("tracker".to_string(), f);
        ctx
    }

    fn issues(playbook: &str, ctx: &ValidationContext) -> Vec<Issue> {
        let p = Playbook::from_yaml(playbook).unwrap();
        let mut r = ValidationReport::default();
        check_mcp_bindings(&p, ctx, &mut r);
        r.issues
    }

    fn of_code<'a>(issues: &'a [Issue], code: &str) -> Vec<&'a Issue> {
        issues.iter().filter(|i| i.code == code).collect()
    }

    /// A node `a` binding the connector with `binding`.
    fn node_playbook(binding: &str) -> String {
        format!(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - id: a
    type: agent_task
    prompt: hi
    profile: x
    connectors:
      - {binding}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: a }}
  - {{ from: a, to: done }}
"#
        )
    }

    #[test]
    fn v300_warns_for_a_binding_without_functions() {
        let found = issues(
            &node_playbook("{ name: tracker }"),
            &ctx_with(facts(TRACKER)),
        );
        let v300 = of_code(&found, "V300");
        assert_eq!(v300.len(), 1, "{found:?}");
        assert_eq!(v300[0].severity, Severity::Warning);
        assert_eq!(v300[0].node.as_deref(), Some("a"));
        assert_eq!(
            v300[0].message,
            "node `a` grants every function of MCP connector `tracker`; name the functions it needs (`functions: [..]` or `functions: read_only`)"
        );
    }

    #[test]
    fn v300_is_silent_with_a_functions_list() {
        let found = issues(
            &node_playbook("{ name: tracker, functions: [search_issues] }"),
            &ctx_with(facts(TRACKER)),
        );
        assert!(of_code(&found, "V300").is_empty(), "{found:?}");
    }

    #[test]
    fn v300_is_silent_with_read_only() {
        let found = issues(
            &node_playbook("{ name: tracker, functions: read_only }"),
            &ctx_with(facts(TRACKER)),
        );
        assert!(of_code(&found, "V300").is_empty(), "{found:?}");
    }

    #[test]
    fn v300_is_silent_without_facts() {
        let found = issues(
            &node_playbook("{ name: tracker }"),
            &ValidationContext::default(),
        );
        assert!(found.is_empty(), "{found:?}");
        // An HTTP connector of that name is no MCP connector either.
        let http = ConnectorFacts {
            functions: vec!["search_issues".into()],
            ..ConnectorFacts::default()
        };
        let found = issues(&node_playbook("{ name: tracker }"), &ctx_with(http));
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn v301_warns_for_a_launcher_command() {
        let npx = TRACKER.replace("command: tracker-mcp-server", "command: npx");
        let found = issues(
            &node_playbook("{ name: tracker, functions: [search_issues] }"),
            &ctx_with(facts(&npx)),
        );
        let v301 = of_code(&found, "V301");
        assert_eq!(v301.len(), 1, "{found:?}");
        assert_eq!(v301[0].severity, Severity::Warning);
        assert_eq!(
            v301[0].message,
            "node `a` binds MCP connector `tracker`, whose command `npx` is an interpreter or a launcher: its approval covers the `npx` program file only, not the script or package it runs; point the command at the installed server program instead"
        );
    }

    #[test]
    fn v301_is_silent_for_a_server_program() {
        let found = issues(
            &node_playbook("{ name: tracker, functions: [search_issues] }"),
            &ctx_with(facts(TRACKER)),
        );
        assert!(of_code(&found, "V301").is_empty(), "{found:?}");
    }

    fn assert_v302(found: &[Issue], label: &str) {
        let v302 = of_code(found, "V302");
        assert_eq!(v302.len(), 1, "{found:?}");
        assert_eq!(v302[0].severity, Severity::Error);
        assert_eq!(
            v302[0].message,
            format!(
                "{label} reads MCP connector `tracker`; an MCP server is started only for a node's own call in this version (the baseline check, the follow-up check, list item checks and inbox waits run from the drive or a waker)"
            )
        );
        // V302 answers alone at an engine site.
        assert!(of_code(found, "V300").is_empty(), "{found:?}");
    }

    #[test]
    fn v302_refuses_an_mcp_baseline_read() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
baseline_check:
  checks:
    - id: open
      connector: { name: tracker, account: main, function: search_issues }
      observe: [{ id: n, type: number, path: total, expect: { equals: 0 } }]
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: hi, profile: x }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;
        assert_v302(
            &issues(yaml, &ctx_with(facts(TRACKER))),
            "the baseline check",
        );
    }

    #[test]
    fn v302_refuses_an_mcp_item_check() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: search
    type: agent_task
    prompt: Find issues.
    profile: x
    outputs: { fields: [issues] }
  - id: triage
    type: agent_task
    prompt: Triage the issue named in the item section.
    profile: x
    list:
      items:
        from: { node: search, field: issues }
        max_items: 10
      batch: { size: 2, parallel: 1, retries: 0 }
      check:
        connector: { name: tracker, function: search_issues, accept_when: { path: total, exists: true } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: search }
  - { from: search, to: triage }
  - { from: triage, to: done }
"#;
        assert_v302(
            &issues(yaml, &ctx_with(facts(TRACKER))),
            "the item check of node `triage`",
        );
    }

    #[test]
    fn v302_refuses_an_mcp_inbox_wait() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: wait
    type: wait
    timeout_seconds: 60
    wait_for: { type: inbox, connector: tracker, account: main }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: wait }
  - { from: wait, to: done }
"#;
        let pb = Playbook::from_yaml(yaml).unwrap();
        let report = crate::validate::validate(&pb, &ctx_with(facts(TRACKER)));
        let v285: Vec<_> = report
            .issues
            .iter()
            .filter(|issue| issue.code == "V285")
            .collect();
        assert_eq!(v285.len(), 1, "{:?}", report.issues);
        assert_eq!(v285[0].node.as_deref(), Some("wait"));
        assert!(
            v285[0]
                .message
                .contains("is an MCP connector and cannot declare `webhook:`"),
            "{}",
            v285[0].message
        );

        let v302: Vec<_> = report
            .issues
            .iter()
            .filter(|issue| issue.code == "V302")
            .collect();
        assert_eq!(v302.len(), 1, "{:?}", report.issues);
        assert_eq!(v302[0].node.as_deref(), Some("wait#inbox"));
        assert!(
            v302[0]
                .message
                .starts_with("the inbox wait of node `wait` reads MCP connector"),
            "{}",
            v302[0].message
        );
    }

    #[test]
    fn v302_refuses_an_mcp_follow_up() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
follow_up:
  after: 3d
  check:
    id: still_open
    description: the issue is still open
    connector: { name: tracker, account: main, function: search_issues }
    observe: [{ id: n, type: number, path: total, expect: { equals: 0 } }]
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: hi, profile: x }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;
        assert_v302(
            &issues(yaml, &ctx_with(facts(TRACKER))),
            "the follow-up check",
        );
    }

    #[test]
    fn v302_is_silent_for_a_node_binding() {
        let found = issues(
            &node_playbook("{ name: tracker, functions: [search_issues] }"),
            &ctx_with(facts(TRACKER)),
        );
        assert!(of_code(&found, "V302").is_empty(), "{found:?}");
    }

    /// A node whose id ends in `#inbox` is an ordinary node: the site walk
    /// says what a site is, so no kind is inferred from an id. The node
    /// binds the MCP connector itself and is not refused.
    #[test]
    fn v302_is_silent_for_a_node_named_like_a_derived_site() {
        let id = "fetch#inbox";
        let yaml = format!(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - id: {id}
    type: agent_task
    prompt: hi
    profile: x
    connectors:
      - {{ name: tracker, functions: [search_issues] }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: {id} }}
  - {{ from: {id}, to: done }}
"#
        );
        let found = issues(&yaml, &ctx_with(facts(TRACKER)));
        assert!(of_code(&found, "V302").is_empty(), "{id}: {found:?}");
        // The label names the node, not a derived site.
        let v300 = of_code(&found, "V300");
        assert!(v300.is_empty(), "{id}: {found:?}");
    }

    /// A node named `x#check` is an ordinary node for the site walk, so no
    /// MCP rule reads a kind into its id. The list rule that reserves every
    /// `#check` id (V85) refuses it as before: the reservation is main's.
    #[test]
    fn v302_is_silent_for_a_node_named_x_check_and_v85_refuses_it() {
        let id = "x#check";
        let yaml = format!(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - id: {id}
    type: agent_task
    prompt: hi
    profile: x
    connectors:
      - {{ name: tracker, functions: [search_issues] }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: {id} }}
  - {{ from: {id}, to: done }}
"#
        );
        let found = issues(&yaml, &ctx_with(facts(TRACKER)));
        assert!(of_code(&found, "V302").is_empty(), "{id}: {found:?}");
        let v300 = of_code(&found, "V300");
        assert!(v300.is_empty(), "{id}: {found:?}");
        // The list rule reserves the id, as main does.
        let pb = Playbook::from_yaml(&yaml).unwrap();
        let report = crate::validate::validate(&pb, &ctx_with(facts(TRACKER)));
        let v85: Vec<_> = report
            .issues
            .iter()
            .filter(|issue| issue.code == "V85")
            .collect();
        assert_eq!(v85.len(), 1, "{id}: {:?}", report.issues);
        assert_eq!(v85[0].node.as_deref(), Some(id));
    }

    /// An http MCP connector whose header auth names `header`.
    fn http_tracker(header: &str) -> String {
        format!(
            r#"name: tracker
version: 0.1.0
mcp: {{ transport: http, url: "https://{{{{account.host}}}}/mcp", protocol: modern }}
auth: {{ kind: header, header: "{header}", value_template: "{{{{secret.token}}}}" }}
account_fields:
  - {{ name: host, required: true }}
  - {{ name: token, required: true, secret: true }}
functions:
  - name: search_issues
    description: Search issues
    read_only: true
    mcp: {{ tool: search_issues }}
"#
        )
    }

    #[test]
    fn a_reserved_auth_header_is_v303() {
        for header in ["Content-Type", "mcp-method", "HOST"] {
            let found = issues(
                &node_playbook("{ name: tracker, functions: [search_issues] }"),
                &ctx_with(facts(&http_tracker(header))),
            );
            let v303 = of_code(&found, "V303");
            assert_eq!(v303.len(), 1, "{header}: {found:?}");
            assert_eq!(v303[0].severity, Severity::Error);
            assert_eq!(v303[0].node.as_deref(), Some("a"));
            assert_eq!(
                v303[0].message,
                format!(
                    "node `a` binds MCP connector `tracker`, whose auth header `{header}` is one the HTTP transport sets itself; name another header"
                )
            );
        }
    }

    #[test]
    fn an_ordinary_auth_header_is_not_v303() {
        for header in ["Authorization", "X-Api-Key"] {
            let f = facts(&http_tracker(header));
            assert_eq!(f.mcp_reserved_header, None, "{header}");
            let found = issues(
                &node_playbook("{ name: tracker, functions: [search_issues] }"),
                &ctx_with(f),
            );
            assert!(of_code(&found, "V303").is_empty(), "{header}: {found:?}");
        }
    }
}
