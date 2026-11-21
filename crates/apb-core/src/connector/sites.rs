//! Every place a playbook binds connectors: each node's own bindings, a
//! list node's connector item check under `<node>#check`, and the
//! connector sources of the baseline check under the reserved
//! [`BASELINE_SITE`] and of the follow-up check under the reserved
//! [`FOLLOW_UP_SITE`]. Grant resolution, effects, the run gate, preflight
//! and the validator all walk this one list, so a binding site cannot be
//! resolved by one of them and missed by another.
//!
//! Sites whose id starts with [`ENGINE_SITE_PREFIX`] belong to the engine:
//! [`BASELINE_SITE`], [`FOLLOW_UP_SITE`] and [`NOTIFY_SITE`]. No node may
//! take such an id (V68), and no caller outside the engine may call under
//! one.

use std::borrow::Cow;

use crate::baseline::{BASELINE_SITE, BaselineConnector};
use crate::follow_up::FOLLOW_UP_SITE;
use crate::list::{ConnectorCheck, check_site};
use crate::schema::{ConnectorBinding, FunctionsAllow, NodeKind, Playbook};

/// What the id of every site the engine owns starts with.
pub const ENGINE_SITE_PREFIX: char = '@';

/// The site run notifications are sent and journaled under. It binds no
/// connector in the playbook: a notification's target is the person's own
/// subscription, trust-checked against the digests pinned when it was set.
pub const NOTIFY_SITE: &str = "@notify";

/// Whether `site` belongs to the engine rather than to a node.
pub fn is_engine_site(site: &str) -> bool {
    site.starts_with(ENGINE_SITE_PREFIX)
}

/// One binding site: its id, what owns it and the connectors it binds.
pub struct Site<'a> {
    /// The site's id: a node's own id, a derived site id
    /// (`<node>#check`, `<node>#inbox`) or a reserved engine id.
    pub id: Cow<'a, str>,
    /// What owns the site.
    pub kind: SiteKind,
    /// The connectors the site binds.
    pub bindings: Cow<'a, [ConnectorBinding]>,
}

/// What owns a binding site. The site walk says what a site is; no code
/// infers a kind from an id, so an ordinary node whose id ends in `#check`
/// or `#inbox` stays a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteKind {
    /// A node's own site: the node binds the connectors itself.
    Node,
    /// A list node's item check: the site id is `<node>#check`.
    ItemCheck { node: String },
    /// An inbox wait's site: the site id is `<node>#inbox`.
    InboxWait { node: String },
    /// A site the engine owns: the baseline check, the follow-up check and
    /// the notifications.
    Engine,
}

impl Playbook {
    /// `(site id, bindings)` for every node, then `(<node>#check, ..)` for
    /// every list node whose item check reads a connector, then
    /// `(<node>#inbox, ..)` for every inbox wait node, then
    /// `(BASELINE_SITE, ..)` when a baseline check reads a connector and
    /// `(FOLLOW_UP_SITE, ..)` when the follow-up's check does. Sources of
    /// one connector on one site merge into one binding, as a node binds a
    /// connector once.
    pub fn connector_sites(&self) -> impl Iterator<Item = Site<'_>> {
        let nodes = self.nodes.iter().map(|n| Site {
            id: Cow::Borrowed(n.id.as_str()),
            kind: SiteKind::Node,
            bindings: Cow::Borrowed(n.kind.connector_bindings()),
        });
        let checks = self.nodes.iter().filter_map(|n| {
            let c = n.list()?.connector_check()?;
            Some(Site {
                id: Cow::Owned(check_site(&n.id)),
                kind: SiteKind::ItemCheck { node: n.id.clone() },
                bindings: Cow::Owned(vec![check_binding(c)]),
            })
        });
        // One site per inbox wait: the connector and exactly the one
        // account, no function. Owned like the `<node>#check` sites, so
        // the gate's trust check, the permit and the manifest snapshot
        // cover the wait with no change (inbox-waits D4 "Authorization").
        let inbox_waits = self.nodes.iter().filter_map(|n| {
            let NodeKind::Wait {
                wait_for: crate::schema::WaitFor::Inbox(w),
                ..
            } = &n.kind
            else {
                return None;
            };
            Some(Site {
                id: Cow::Owned(crate::inbox_wait::inbox_site(&n.id)),
                kind: SiteKind::InboxWait { node: n.id.clone() },
                bindings: Cow::Owned(vec![crate::inbox_wait::binding(w)]),
            })
        });
        let baseline = self.baseline_bindings();
        let follow_up: Option<ConnectorBinding> = self
            .follow_up
            .as_ref()
            .and_then(|f| f.check.connector.as_ref())
            .map(baseline_binding);
        nodes
            .chain(checks)
            .chain(inbox_waits)
            .chain((!baseline.is_empty()).then_some(Site {
                id: Cow::Borrowed(BASELINE_SITE),
                kind: SiteKind::Engine,
                bindings: Cow::Owned(baseline),
            }))
            .chain(follow_up.map(|b| Site {
                id: Cow::Borrowed(FOLLOW_UP_SITE),
                kind: SiteKind::Engine,
                bindings: Cow::Owned(vec![b]),
            }))
    }

    /// Whether any site binds a connector.
    pub fn binds_connectors(&self) -> bool {
        self.connector_sites().any(|s| !s.bindings.is_empty())
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

/// A binding site as people read it: `node `<id>`` for a node's own site,
/// `the item check of node `<id>`` for a list node's check, `the inbox wait
/// of node `<id>`` for an inbox wait's site, and the engine's own name for
/// the reserved sites (the baseline check, the follow-up check and the
/// notifications), which are not nodes.
pub fn site_label(site: &Site<'_>) -> String {
    match &site.kind {
        SiteKind::Node => format!("node `{}`", site.id),
        SiteKind::Engine => engine_site_label(&site.id),
        other => kind_label(other),
    }
}

/// What owns a site as people read it.
pub fn kind_label(kind: &SiteKind) -> String {
    match kind {
        SiteKind::Node => "a node".to_string(),
        SiteKind::ItemCheck { node } => format!("the item check of node `{node}`"),
        SiteKind::InboxWait { node } => format!("the inbox wait of node `{node}`"),
        SiteKind::Engine => "the engine".to_string(),
    }
}

/// A site as the consent sources name it: the node that owns it and the
/// kind it is, without the quoting the sources carry (`node <id>`, `the
/// item check of node <id>`, `the inbox wait of node <id>`, and the
/// engine's own names, which are not nodes).
pub fn source_label(site: &Site<'_>) -> String {
    match &site.kind {
        SiteKind::Node => format!("node {}", site.id),
        SiteKind::ItemCheck { node } => format!("the item check of node {node}"),
        SiteKind::InboxWait { node } => format!("the inbox wait of node {node}"),
        SiteKind::Engine => engine_source_label(&site.id),
    }
}

/// The engine's own sites as the consent sources name them: `baseline
/// check`, `follow-up check` and `notifications`, none of them a node.
pub fn engine_source_label(site: &str) -> String {
    if site == BASELINE_SITE {
        "baseline check".to_string()
    } else if site == FOLLOW_UP_SITE {
        "follow-up check".to_string()
    } else if site == NOTIFY_SITE {
        "notifications".to_string()
    } else {
        format!("node {site}")
    }
}

/// The engine's own sites as people read them: `the baseline check`, `the
/// follow-up check` and `notifications`, which are not nodes.
pub fn engine_site_label(site: &str) -> String {
    if site == BASELINE_SITE {
        "the baseline check".to_string()
    } else if site == FOLLOW_UP_SITE {
        "the follow-up check".to_string()
    } else if site == NOTIFY_SITE {
        "notifications".to_string()
    } else {
        format!("node `{site}`")
    }
}

/// The grant a list node's connector item check needs: its one function,
/// its one account when named, no call budget.
pub fn check_binding(c: &ConnectorCheck) -> ConnectorBinding {
    single_function_binding(&c.name, c.account.as_deref(), &c.function)
}

/// The grant a baseline or follow-up connector source needs, resolved at
/// run start like a node's: the one function, the one account when named,
/// no call budget.
pub fn baseline_binding(c: &BaselineConnector) -> ConnectorBinding {
    single_function_binding(&c.name, c.account.as_deref(), &c.function)
}

/// One function of one connector, on one account when named, no budget.
fn single_function_binding(name: &str, account: Option<&str>, function: &str) -> ConnectorBinding {
    ConnectorBinding {
        name: name.to_string(),
        accounts: account.map(|a| vec![a.to_string()]),
        functions: FunctionsAllow::List(vec![function.to_string()]),
        max_calls: None,
        on_unknown: None,
        untrusted: false,
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
            .map(|s| (s.id.into_owned(), s.bindings.into_owned()))
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
        assert_eq!(engine_site_label(BASELINE_SITE), "the baseline check");
        assert_eq!(engine_site_label(NOTIFY_SITE), "notifications");
        assert!(is_engine_site(NOTIFY_SITE) && is_engine_site(BASELINE_SITE));
        assert!(!is_engine_site("build"));
        assert_eq!(engine_site_label("fix"), "node `fix`");
    }

    #[test]
    fn no_baseline_connector_means_no_baseline_site() {
        let yaml = YAML.split("baseline_check:").next().unwrap().to_string()
            + "nodes:"
            + YAML.split("nodes:").nth(1).unwrap();
        let p = Playbook::from_yaml(&yaml).unwrap();
        assert!(p.connector_sites().all(|s| s.id != BASELINE_SITE));
    }

    /// A follow-up whose check reads a connector, with its own account.
    const FOLLOW_YAML: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
follow_up:
  after: 3d
  check:
    id: deal_won
    description: the deal moved to won
    connector: { name: crm, account: main, function: get_deal }
    observe: [{ id: stage, type: field, path: deal.stage, expect: { equals: won } }]
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: hi, profile: x }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

    #[test]
    fn sites_include_the_follow_up_with_its_binding() {
        let p = Playbook::from_yaml(FOLLOW_YAML).unwrap();
        let sites: Vec<(String, Vec<ConnectorBinding>)> = p
            .connector_sites()
            .map(|s| (s.id.into_owned(), s.bindings.into_owned()))
            .collect();
        let follow_up = &sites.iter().find(|(id, _)| id == FOLLOW_UP_SITE).unwrap().1;
        assert_eq!(follow_up.len(), 1);
        assert_eq!(follow_up[0].name, "crm");
        assert_eq!(
            follow_up[0].functions,
            FunctionsAllow::List(vec!["get_deal".into()])
        );
        assert_eq!(follow_up[0].accounts, Some(vec!["main".into()]));
        // One site per kind: the follow-up's site is its own, even next to
        // a baseline and nodes on the same connector.
        assert_eq!(
            sites.iter().filter(|(id, _)| id == FOLLOW_UP_SITE).count(),
            1
        );
        assert!(p.binds_connectors());

        // No connector in the check, no follow-up site.
        let script = FOLLOW_YAML
            .replace(
                "    connector: { name: crm, account: main, function: get_deal }\n",
                "    script: scripts/deal.sh\n",
            )
            .replace(
                "    observe: [{ id: stage, type: field, path: deal.stage, expect: { equals: won } }]\n",
                "    observe: [{ id: stage, type: number, path: deal.stage, expect: { equals: 1 } }]\n",
            );
        let p = Playbook::from_yaml(&script).unwrap();
        assert!(p.connector_sites().all(|s| s.id != FOLLOW_UP_SITE));
        // No block, no site.
        let plain = FOLLOW_YAML.split("follow_up:").next().unwrap().to_string()
            + "nodes:"
            + FOLLOW_YAML.split("\nnodes:").nth(1).unwrap();
        let p = Playbook::from_yaml(&plain).unwrap();
        assert!(p.connector_sites().all(|s| s.id != FOLLOW_UP_SITE));
    }

    #[test]
    fn the_follow_up_site_is_named_as_the_follow_up_check() {
        assert_eq!(engine_site_label(FOLLOW_UP_SITE), "the follow-up check");
        assert!(is_engine_site(FOLLOW_UP_SITE));
        // It stays an engine site of its own, not the baseline's.
        assert_ne!(FOLLOW_UP_SITE, BASELINE_SITE);
        assert_eq!(engine_site_label(BASELINE_SITE), "the baseline check");
    }

    /// An inbox wait's site is its own `<node>#inbox`, carrying the wait's
    /// binding (the connector, exactly the one account, no function); the
    /// wait node's own plain site binds nothing, and the label names the
    /// wait (inbox-waits D4 "Authorization").
    #[test]
    fn the_inbox_site_is_its_own_site() {
        let p = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: w, type: wait, wait_for: { type: inbox, connector: echo-hooks, account: main }, timeout_seconds: 60 }\nedges: []\n",
        )
        .unwrap();
        let sites: Vec<(String, Vec<ConnectorBinding>)> = p
            .connector_sites()
            .map(|s| (s.id.into_owned(), s.bindings.into_owned()))
            .collect();
        let site = sites.iter().find(|(id, _)| id == "w#inbox").unwrap();
        assert_eq!(site.1.len(), 1);
        let crate::schema::NodeKind::Wait {
            wait_for: crate::schema::WaitFor::Inbox(w),
            ..
        } = &p.node("w").unwrap().kind
        else {
            panic!("the node is an inbox wait");
        };
        assert_eq!(site.1[0], crate::inbox_wait::binding(w));
        assert_eq!(site.1[0].functions, FunctionsAllow::List(vec![]));
        assert_eq!(site.1[0].accounts, Some(vec!["main".into()]));
        assert!(site.1[0].untrusted);
        // The wait node's own site exists (every node's does) and binds
        // nothing: the wait calls no function.
        let own = sites.iter().find(|(id, _)| id == "w").unwrap();
        assert!(own.1.is_empty());
    }

    /// The site walk says what a site is: a node whose id ends in `#inbox`
    /// or `#check` is an ordinary node, not a derived site. No code infers
    /// a kind from an id.
    #[test]
    fn a_node_named_like_a_derived_site_is_its_own_site() {
        let p = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: fetch#inbox, type: agent_task, prompt: hi, profile: x, connectors: [jira] }\n  - { id: x#check, type: agent_task, prompt: hi, profile: x }\nedges: []\n",
        )
        .unwrap();
        let sites: Vec<Site<'_>> = p.connector_sites().collect();
        let fetch = sites.iter().find(|s| s.id == "fetch#inbox").unwrap();
        assert_eq!(fetch.kind, SiteKind::Node);
        assert_eq!(site_label(fetch), "node `fetch#inbox`");
        let check = sites.iter().find(|s| s.id == "x#check").unwrap();
        assert_eq!(check.kind, SiteKind::Node);
        assert_eq!(site_label(check), "node `x#check`");
        // No derived site exists beside them: no wait named `fetch` and
        // no list node named `x`.
        assert!(
            !sites.iter().any(|s| matches!(
                s.kind,
                SiteKind::InboxWait { .. } | SiteKind::ItemCheck { .. }
            )),
            "{:?}",
            sites.iter().map(|s| (&s.id, &s.kind)).collect::<Vec<_>>()
        );
    }

    /// The derived sites carry their own kind and label.
    #[test]
    fn a_derived_site_names_its_owner() {
        let p = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: w, type: wait, wait_for: { type: inbox, connector: echo-hooks, account: main }, timeout_seconds: 60 }\n  - id: a\n    type: script\n    script: s.sh\n    runner: sh\n    list:\n      items: { inline: [one] }\n      check: { connector: { name: imap, function: get_message, accept_when: { path: uid, exists: true } } }\nedges: []\n",
        )
        .unwrap();
        let sites: Vec<Site<'_>> = p.connector_sites().collect();
        let inbox = sites.iter().find(|s| s.id == "w#inbox").unwrap();
        assert_eq!(
            inbox.kind,
            SiteKind::InboxWait {
                node: "w".to_string()
            }
        );
        assert_eq!(site_label(inbox), "the inbox wait of node `w`");
        let check = sites.iter().find(|s| s.id == "a#check").unwrap();
        assert_eq!(
            check.kind,
            SiteKind::ItemCheck {
                node: "a".to_string()
            }
        );
        assert_eq!(site_label(check), "the item check of node `a`");
        // The consent sources name the same owner, without the quoting.
        assert_eq!(source_label(inbox), "the inbox wait of node w");
        assert_eq!(source_label(check), "the item check of node a");
        // The engine's own sites are not nodes.
        assert_eq!(engine_source_label(BASELINE_SITE), "baseline check");
        assert_eq!(engine_source_label(FOLLOW_UP_SITE), "follow-up check");
        assert_eq!(engine_source_label(NOTIFY_SITE), "notifications");
    }
}
