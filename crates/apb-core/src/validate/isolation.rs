//! Rules for a node's `isolation` (`apb_core::isolation`): V112-V115. V16,
//! the old "declared but not enforced" warning, is retired and its number
//! stays reserved.

use super::*;
use crate::isolation::{IsolationSpec, Level, paths};

/// V112 (warning): `full` with the network left implicit (it means `open`,
/// the host network). V113 (error): isolation together with `connectors`:
/// the sandbox withholds both the connector credentials and the journal the
/// send ledger writes. V114 (error): an extra path that is relative, has
/// `..`, is `/` or `~`, or reaches a credential location, or a writable path
/// that reaches a persistence location. V115 (error):
/// `network`, `read_only` or `writable` with level `none`.
pub(crate) fn check_isolation(playbook: &Playbook, r: &mut ValidationReport) {
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
        if !n.kind.connector_bindings().is_empty() {
            r.error(
                "V113",
                id,
                format!(
                    "isolation `{}` cannot be combined with connectors: the sandbox withholds the connector credentials and the journal the send ledger writes",
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
