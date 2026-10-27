//! The `process env` rows of `apb doctor --run`: what a profile's
//! `process_env: allowlist` withheld from a node's agent, and when that may
//! be why the node failed. Names only, from the journal and the manifest.

use std::collections::BTreeMap;
use std::path::Path;

use super::check::{OK, RunCheck, WARN};
use crate::event::{Event, EventPayload};
use crate::manifest::RunExecutionManifest;

const SUBJECT: &str = "process env";

/// How many withheld names a row spells out.
const NAMES_SHOWN: usize = 10;

/// The latest `agent_env_applied` of one node, or of one item of a list
/// node (its attempts restart their numbers per item).
struct Applied<'a> {
    seq: u64,
    item: Option<&'a str>,
    attempt: u32,
    agent: &'a str,
    kept: u32,
    withheld: &'a [String],
    withheld_more: u32,
    absent: &'a [String],
}

impl Applied<'_> {
    fn withheld_total(&self) -> usize {
        self.withheld.len() + self.withheld_more as usize
    }

    /// How a row names the node and, for a list node, its item.
    fn subject(&self, node: &str) -> String {
        match self.item {
            Some(item) => format!("node `{node}` item `{item}`"),
            None => format!("node `{node}`"),
        }
    }
}

/// One row per node whose agent ran under an allowlist (OK, or WARN when
/// its latest such attempt failed), a hint for a `cmd:` secret helper the
/// allowlist may starve, and a WARN for allowlisted nodes whose attempts ran
/// as host tasks, where the allowlist does not apply.
pub(super) fn env_checks(run_dir: &Path, events: &[Event]) -> Vec<RunCheck> {
    let manifest = crate::manifest::read(run_dir).ok().flatten();
    let mut latest: BTreeMap<(&str, Option<&str>), Applied> = BTreeMap::new();
    for e in events {
        if let EventPayload::AgentEnvApplied {
            node,
            attempt,
            agent,
            kept,
            withheld,
            withheld_more,
            absent,
            item,
        } = &e.payload
        {
            latest.insert(
                (node, item.as_deref()),
                Applied {
                    seq: e.seq,
                    item: item.as_deref(),
                    attempt: *attempt,
                    agent,
                    kept: *kept,
                    withheld,
                    withheld_more: *withheld_more,
                    absent,
                },
            );
        }
    }
    let mut out = Vec::new();
    for ((node, _), applied) in &latest {
        let failed = attempt_failed(events, node, applied);
        out.push(if failed {
            RunCheck::new(WARN, SUBJECT, failed_line(node, applied))
        } else {
            RunCheck::new(
                OK,
                SUBJECT,
                format!(
                    "{}: process_env allowlist, {} kept, {} withheld",
                    applied.subject(node),
                    applied.kept,
                    applied.withheld_total()
                ),
            )
        });
        if let Some(m) = &manifest
            && cmd_helper_at_risk(m, events, node, applied, failed)
        {
            out.push(RunCheck::new(
                WARN,
                SUBJECT,
                format!(
                    "{}: a connector account it holds resolves a secret through a `cmd:` helper, which runs with the agent's allowlisted environment; if the helper needs variables such as PASSWORD_STORE_DIR, GNUPGHOME or DBUS_SESSION_BUS_ADDRESS, add them to the profile's process_env.allow",
                    applied.subject(node)
                ),
            ));
        }
    }
    if let Some(m) = &manifest {
        out.extend(host_rows(m, events));
    }
    out
}

/// Whether the attempt of `applied` (its node and item) finished failed or
/// timed out.
fn attempt_failed(events: &[Event], node: &str, applied: &Applied) -> bool {
    events.iter().any(|e| {
        e.seq > applied.seq
            && matches!(
                &e.payload,
                EventPayload::AttemptFinished { node: n, attempt, status, item, .. }
                    if n == node
                        && item.as_deref() == applied.item
                        && *attempt == applied.attempt
                        && (status == "failed" || status == "timed_out")
            )
    })
}

fn failed_line(node: &str, a: &Applied) -> String {
    let mut line = format!(
        "{} attempt {} ({}) failed under process_env allowlist",
        a.subject(node),
        a.attempt,
        a.agent
    );
    if a.withheld_total() > 0 {
        let shown: Vec<&str> = a
            .withheld
            .iter()
            .take(NAMES_SHOWN)
            .map(String::as_str)
            .collect();
        line.push_str(&format!("; withheld: {}", shown.join(", ")));
        let more = a.withheld_total() - shown.len();
        if more > 0 {
            line.push_str(&format!(" (and {more} more)"));
        }
    }
    if !a.absent.is_empty() {
        line.push_str(&format!(
            "; not set in the operator environment: {}",
            a.absent.join(", ")
        ));
    }
    line.push_str(". If the agent needs one, add it to the profile's process_env.allow.");
    line
}

/// Whether node `node` holds an account whose secret comes from a `cmd:`
/// helper and either the allowlisted attempt failed or, after it started, a
/// call of the same list item through such an account failed the way a starved helper fails
/// (`auth` or `config`).
fn cmd_helper_at_risk(
    m: &RunExecutionManifest,
    events: &[Event],
    node: &str,
    applied: &Applied,
    failed: bool,
) -> bool {
    let cmd_accounts: Vec<(&str, &str)> = m
        .grants_for(node)
        .iter()
        .filter_map(|g| m.connector(&g.connector).map(|c| (g, c)))
        .flat_map(|(g, c)| {
            c.accounts
                .iter()
                .filter(|a| g.accounts.contains(&a.name) && !a.cmd.is_empty())
                .map(move |a| (c.name.as_str(), a.name.as_str()))
        })
        .collect();
    if cmd_accounts.is_empty() {
        return false;
    }
    failed
        || events.iter().any(|e| {
            e.seq > applied.seq
                && matches!(
                    &e.payload,
                    EventPayload::ConnectorCall { node_id, connector, account, outcome, item, .. }
                        if node_id == node
                            && item.as_deref() == applied.item
                            && (outcome == "auth" || outcome == "config")
                            && cmd_accounts.contains(&(connector.as_str(), account.as_str()))
                )
        })
}

/// A WARN per node bound to an allowlist profile whose attempts ran as
/// host tasks: the host's subagent runs in the host process, untouched.
fn host_rows(m: &RunExecutionManifest, events: &[Event]) -> Vec<RunCheck> {
    let mut nodes: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptStarted { node, agent, .. }
                if agent == crate::attempt_models::HOST_AGENT
                    && m.for_node(node)
                        .and_then(|p| p.process_env.as_ref())
                        .is_some_and(|p| p.is_allowlist()) =>
            {
                Some(node.as_str())
            }
            _ => None,
        })
        .collect();
    nodes.sort_unstable();
    nodes.dedup();
    nodes
        .into_iter()
        .map(|node| {
            RunCheck::new(
                WARN,
                SUBJECT,
                format!(
                    "node `{node}`: its profile sets process_env allowlist, but its attempts ran as host tasks, where the allowlist is not applied"
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_dir(journal: &str, manifest: Option<&str>) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("events.jsonl"), journal).unwrap();
        if let Some(m) = manifest {
            std::fs::write(tmp.path().join("manifest.yaml"), m).unwrap();
        }
        tmp
    }

    fn checks(journal: &str, manifest: Option<&str>) -> Vec<RunCheck> {
        let dir = run_dir(journal, manifest);
        let events = crate::event::read_all(dir.path()).unwrap();
        env_checks(dir.path(), &events)
    }

    const APPLIED: &str = r#"{"seq":0,"ts":1,"type":"agent_env_applied","node":"build","attempt":2,"agent":"codex","kept":23,"withheld":["AWS_PROFILE","GH_TOKEN"],"withheld_more":39,"absent":["NPM_TOKEN"]}"#;

    /// [`APPLIED`] at `seq` for the list item `item`.
    fn item_applied(seq: u64, item: &str) -> String {
        APPLIED
            .replace("\"seq\":0", &format!("\"seq\":{seq}"))
            .replace("\"absent\"", &format!("\"item\":\"{item}\",\"absent\""))
    }

    #[test]
    fn a_failed_attempt_names_the_withheld_and_absent_names() {
        let journal = format!(
            "{APPLIED}\n{}\n",
            r#"{"seq":1,"ts":2,"type":"attempt_finished","node":"build","attempt":2,"status":"failed"}"#
        );
        let rows = checks(&journal, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, WARN);
        let d = &rows[0].detail;
        assert!(d.contains("attempt 2 (codex) failed"), "{d}");
        assert!(
            d.contains("withheld: AWS_PROFILE, GH_TOKEN (and 39 more)"),
            "{d}"
        );
        assert!(
            d.contains("not set in the operator environment: NPM_TOKEN"),
            "{d}"
        );
        assert!(d.contains("process_env.allow"), "{d}");
    }

    #[test]
    fn a_succeeded_attempt_is_an_ok_row() {
        let journal = format!(
            "{APPLIED}\n{}\n",
            r#"{"seq":1,"ts":2,"type":"attempt_finished","node":"build","attempt":2,"status":"succeeded"}"#
        );
        let rows = checks(&journal, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, OK);
        assert_eq!(
            rows[0].detail,
            "node `build`: process_env allowlist, 23 kept, 41 withheld"
        );
    }

    #[test]
    fn no_allowlist_means_no_rows() {
        let journal = r#"{"seq":0,"ts":1,"type":"node_started","node":"a","attempt":1}"#;
        assert!(checks(&format!("{journal}\n"), None).is_empty());
    }

    const MANIFEST: &str = "profiles:\n- scope: project\n  name: p\n  profile_digest: d\n  bundle_digest: b\n  soul: ''\n  soul_requirement: any\n  skills: []\n  chain: []\n  process_env:\n    policy: allowlist\nnode_bindings:\n  build: project/p\nconnectors:\n- name: vault\n  digest: c\n  accounts:\n  - name: main\n    default: true\n    fields: {}\n    env: {}\n    cmd:\n      token: pass show vault\n    digest: a\nconnector_grants:\n  build:\n  - connector: vault\n    accounts: [main]\n    functions: [get]\n    max_calls: null\n";

    #[test]
    fn host_attempts_of_an_allowlisted_node_warn() {
        let journal = r#"{"seq":0,"ts":1,"type":"attempt_started","node":"build","attempt":1,"agent":"host"}"#;
        let rows = checks(&format!("{journal}\n"), Some(MANIFEST));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].status, WARN);
        assert!(rows[0].detail.contains("ran as host tasks"));
    }

    fn hinted(outcome: &str, account: &str) -> bool {
        let journal = format!(
            "{APPLIED}\n{{\"seq\":1,\"ts\":2,\"type\":\"connector_call\",\"node_id\":\"build\",\"connector\":\"vault\",\"function\":\"get\",\"account\":\"{account}\",\"url\":\"\",\"outcome\":\"{outcome}\",\"duration_ms\":1}}\n"
        );
        checks(&journal, Some(MANIFEST))
            .iter()
            .any(|r| r.status == WARN && r.detail.contains("`cmd:` helper"))
    }

    /// Only an `auth` or `config` failure through the `cmd:` account itself
    /// points at the helper; a rate limit or another account does not.
    #[test]
    fn a_failing_cmd_helper_account_gets_the_allow_hint() {
        assert!(hinted("auth", "main"));
        assert!(hinted("config", "main"));
        assert!(!hinted("rate_limited", "main"));
        assert!(!hinted("auth", "other"));
    }

    /// Two items of a list node run side by side and only item `a` has an
    /// `auth` failure through the `cmd:` account: only `a` gets the hint.
    #[test]
    fn the_cmd_hint_follows_the_item_of_the_failed_call() {
        let call = |seq: u64, item: &str, outcome: &str| {
            format!(
                "{{\"seq\":{seq},\"ts\":2,\"type\":\"connector_call\",\"node_id\":\"build\",\"connector\":\"vault\",\"function\":\"get\",\"account\":\"main\",\"url\":\"\",\"outcome\":\"{outcome}\",\"duration_ms\":1,\"item\":\"{item}\"}}"
            )
        };
        let journal = [
            item_applied(0, "a"),
            item_applied(1, "b"),
            call(2, "b", "ok"),
            call(3, "a", "auth"),
        ]
        .join("\n");
        let hinted: Vec<String> = checks(&format!("{journal}\n"), Some(MANIFEST))
            .into_iter()
            .filter(|r| r.status == WARN && r.detail.contains("`cmd:` helper"))
            .map(|r| r.detail)
            .collect();
        assert_eq!(hinted.len(), 1, "{hinted:?}");
        assert!(
            hinted[0].starts_with("node `build` item `a`:"),
            "{}",
            hinted[0]
        );
    }

    /// Items of a list node restart their attempt numbers: each gets its
    /// own row, judged by its own attempt.
    #[test]
    fn list_items_get_a_row_each() {
        let finished = |seq: u64, item: &str, status: &str| {
            format!(
                "{{\"seq\":{seq},\"ts\":2,\"type\":\"attempt_finished\",\"node\":\"build\",\"attempt\":2,\"status\":\"{status}\",\"item\":\"{item}\"}}"
            )
        };
        let journal = [
            item_applied(0, "a"),
            item_applied(1, "b"),
            finished(2, "b", "succeeded"),
            finished(3, "a", "failed"),
        ]
        .join("\n");
        let rows = checks(&format!("{journal}\n"), None);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].status, WARN);
        assert!(
            rows[0]
                .detail
                .starts_with("node `build` item `a` attempt 2"),
            "{}",
            rows[0].detail
        );
        assert_eq!(rows[1].status, OK);
        assert!(
            rows[1].detail.starts_with("node `build` item `b`:"),
            "{}",
            rows[1].detail
        );
    }
}
