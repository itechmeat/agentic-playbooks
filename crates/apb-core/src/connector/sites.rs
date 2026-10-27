//! Every place a playbook binds connectors: each node's own bindings, and
//! the baseline check's connector sources under the reserved
//! [`BASELINE_SITE`]. Grant resolution, effects, the run gate, preflight and
//! the validator all walk this one list, so a binding site cannot be
//! resolved by one of them and missed by another.

use std::borrow::Cow;

use crate::baseline::{BASELINE_SITE, BaselineConnector};
use crate::schema::{ConnectorBinding, FunctionsAllow, Playbook};

impl Playbook {
    /// `(site id, bindings)` for every node, then `(BASELINE_SITE, ..)` when
    /// a baseline check reads a connector. Baseline sources of one connector
    /// merge into one binding, as a node binds a connector once.
    pub fn connector_sites(&self) -> impl Iterator<Item = (&str, Cow<'_, [ConnectorBinding]>)> {
        let nodes = self
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), Cow::Borrowed(n.kind.connector_bindings())));
        let baseline = self.baseline_bindings();
        nodes.chain((!baseline.is_empty()).then_some((BASELINE_SITE, Cow::Owned(baseline))))
    }

    /// Whether any site binds a connector.
    pub fn binds_connectors(&self) -> bool {
        self.connector_sites().any(|(_, b)| !b.is_empty())
    }

    /// The baseline check's connector sources as bindings, one per
    /// connector: the functions of every check on it, and its accounts
    /// (every account as soon as one check names none).
    fn baseline_bindings(&self) -> Vec<ConnectorBinding> {
        let mut out: Vec<ConnectorBinding> = Vec::new();
        for c in self
            .baseline_checks()
            .iter()
            .filter_map(|c| c.connector.as_ref())
        {
            let b = baseline_binding(c);
            let Some(have) = out.iter_mut().find(|x| x.name == b.name) else {
                out.push(b);
                continue;
            };
            if let (FunctionsAllow::List(fns), FunctionsAllow::List(more)) =
                (&mut have.functions, b.functions)
            {
                for f in more {
                    if !fns.contains(&f) {
                        fns.push(f);
                    }
                }
            }
            have.accounts = match (have.accounts.take(), b.accounts) {
                (Some(mut a), Some(more)) => {
                    for x in more {
                        if !a.contains(&x) {
                            a.push(x);
                        }
                    }
                    Some(a)
                }
                _ => None,
            };
        }
        out
    }
}

/// A binding site as people read it: `node `<id>``, or `the baseline
/// check` for the reserved [`BASELINE_SITE`], which is not a node.
pub fn site_label(site: &str) -> String {
    if site == BASELINE_SITE {
        "the baseline check".to_string()
    } else {
        format!("node `{site}`")
    }
}

/// The grant a baseline connector source needs, resolved at run start like
/// a node's: the one function, the one account when named, no call budget.
pub fn baseline_binding(c: &BaselineConnector) -> ConnectorBinding {
    ConnectorBinding {
        name: c.name.clone(),
        accounts: c.account.clone().map(|a| vec![a]),
        functions: FunctionsAllow::List(vec![c.function.clone()]),
        max_calls: None,
        on_unknown: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
baseline_check:
  checks:
    - id: open
      connector: { name: twenty, account: main, function: list_opportunities }
      observe: [{ id: n, type: number, path: totalCount, expect: { equals: 0 } }]
    - id: other
      connector: { name: twenty, account: main, function: list_people }
      observe: [{ id: n, type: number, path: totalCount, expect: { equals: 0 } }]
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: hi, profile: x, connectors: [jira] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

    #[test]
    fn sites_include_the_baseline_with_one_binding_per_connector() {
        let p = Playbook::from_yaml(YAML).unwrap();
        let sites: Vec<(String, Vec<ConnectorBinding>)> = p
            .connector_sites()
            .map(|(id, b)| (id.to_string(), b.into_owned()))
            .collect();
        let baseline = &sites.iter().find(|(id, _)| id == BASELINE_SITE).unwrap().1;
        assert_eq!(baseline.len(), 1);
        assert_eq!(
            baseline[0].functions,
            FunctionsAllow::List(vec!["list_opportunities".into(), "list_people".into()])
        );
        assert_eq!(baseline[0].accounts, Some(vec!["main".into()]));
        assert!(sites.iter().any(|(id, b)| id == "a" && b[0].name == "jira"));
        assert!(p.binds_connectors());
    }

    #[test]
    fn the_baseline_site_is_named_as_the_baseline_check() {
        assert_eq!(site_label(BASELINE_SITE), "the baseline check");
        assert_eq!(site_label("fix"), "node `fix`");
    }

    #[test]
    fn no_baseline_connector_means_no_baseline_site() {
        let yaml = YAML.split("baseline_check:").next().unwrap().to_string()
            + "nodes:"
            + YAML.split("nodes:").nth(1).unwrap();
        let p = Playbook::from_yaml(&yaml).unwrap();
        assert!(p.connector_sites().all(|(id, _)| id != BASELINE_SITE));
    }
}
