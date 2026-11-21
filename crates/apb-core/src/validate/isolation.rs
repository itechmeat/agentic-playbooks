//! Rules for a node's `isolation` (`apb_core::isolation`): V112-V115. V16,
//! the old "declared but not enforced" warning, is retired and its number
//! stays reserved.

use super::*;
use crate::isolation::{IsolationSpec, Level, paths};

/// V112 (warning): `full` with the network left implicit (it means `open`,
/// the host network). V113 (error): isolation together with `connectors`:
/// the sandbox withholds both the connector credentials and the journal the
/// send ledger writes, and an MCP connector (its facts in `ctx` carry
/// `mcp_transport`) is named with its own reason. V114 (error): an extra path that is relative, has
/// `..`, is `/` or `~`, or reaches a credential location, or a writable path
/// that reaches a persistence location. V115 (error):
/// `network`, `read_only` or `writable` with level `none`.
pub(crate) fn check_isolation(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    for n in &playbook.nodes {
        let Some(spec) = n.kind.isolation() else {
            continue;
        };
        let id = Some(n.id.as_str());
        if spec.level == Level::None {
            if spec.network.is_some() || !spec.read_only.is_empty() || !spec.writable.is_empty() {
                r.error(
                    "V115",
                    id,
                    "isolation level `none` takes no network, read_only or writable; set level full or best_effort, or remove them".to_string(),
                );
            }
            continue;
        }
        if spec.level == Level::Full && spec.network.is_none() {
            r.warn(
                "V112",
                id,
                "isolation `full` leaves the network implicit, which means `open` (the host network); state `network: open` or `network: none`".to_string(),
            );
        }
        let bindings = n.kind.connector_bindings();
        if !bindings.is_empty() {
            let mcp = bindings.iter().any(|b| {
                ctx.connectors
                    .get(&b.name)
                    .is_some_and(|f| f.mcp_transport.is_some())
            });
            let mcp_reason = if mcp {
                "; an MCP connector also starts a server program or reaches a remote server with those credentials, which the sandbox cannot broker in this version"
            } else {
                ""
            };
            r.error(
                "V113",
                id,
                format!(
                    "isolation `{}` cannot be combined with connectors: the sandbox withholds the connector credentials and the journal the send ledger writes{mcp_reason}",
                    spec.level.as_str()
                ),
            );
        }
        check_paths(spec, id, r);
    }
}

/// V114 over both extra path lists; a writable path is also kept off the
/// persistence locations.
fn check_paths(spec: &IsolationSpec, id: Option<&str>, r: &mut ValidationReport) {
    type Check = fn(&str) -> Result<(), paths::PathProblem>;
    let lists: [(&str, &Vec<String>, Check); 2] = [
        ("read_only", &spec.read_only, paths::check_extra),
        ("writable", &spec.writable, paths::check_writable),
    ];
    for (field, list, check) in lists {
        for p in list {
            if let Err(why) = check(p) {
                r.error("V114", id, format!("isolation {field} path `{p}` {why}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::resolve::ConnectorFacts;

    /// Node `a` isolated, binding connector `tracker`.
    const ISOLATED: &str = r#"
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
    isolation: { level: full, network: none }
    connectors: [tracker]
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

    const HTTP_MESSAGE: &str = "isolation `full` cannot be combined with connectors: the sandbox withholds the connector credentials and the journal the send ledger writes";

    fn v113(facts: Option<ConnectorFacts>) -> Vec<Issue> {
        let mut ctx = ValidationContext::default();
        if let Some(f) = facts {
            ctx.connectors.insert("tracker".to_string(), f);
        }
        let p = Playbook::from_yaml(ISOLATED).unwrap();
        let mut r = ValidationReport::default();
        check_isolation(&p, &ctx, &mut r);
        r.issues.into_iter().filter(|i| i.code == "V113").collect()
    }

    #[test]
    fn v113_names_the_mcp_reason() {
        let found = v113(Some(ConnectorFacts {
            mcp_transport: Some("stdio".into()),
            ..ConnectorFacts::default()
        }));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].severity, Severity::Error);
        assert_eq!(
            found[0].message,
            format!(
                "{HTTP_MESSAGE}; an MCP connector also starts a server program or reaches a remote server with those credentials, which the sandbox cannot broker in this version"
            )
        );
    }

    #[test]
    fn v113_keeps_its_message_for_an_http_connector() {
        for facts in [None, Some(ConnectorFacts::default())] {
            let found = v113(facts);
            assert_eq!(found.len(), 1, "{found:?}");
            assert_eq!(found[0].message, HTTP_MESSAGE);
        }
    }
}
