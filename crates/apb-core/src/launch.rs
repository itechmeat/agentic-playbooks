//! The launch policy of a playbook: whether an agent may start it on its
//! own (`allow`), only with a person's confirmation relayed by the host
//! (`ask`), or not at all (`deny`). The policy is part of the approved
//! definition (`agent_launch:`), held up by the playbook's launch floor
//! ([`floor`]) so an agent channel never loosens it, and narrowed by the
//! `launch:` sections of the machine's and the project's `policy.yaml`
//! ([`crate::policy_file`]).
//!
//! [`resolve`] combines them for one run: for each playbook of the tree,
//! the stricter of its declaration and its floor, unless the machine file
//! names that exact playbook (the machine file is the person's own, so its
//! entry replaces both); then the strictest of that, the machine default,
//! the project default and the project entry. The run's policy is the
//! strictest over the tree, with every setting that produced it named.

pub mod floor;
pub mod policy;

pub use policy::LaunchPolicy;

use serde::{Deserialize, Serialize};

/// A run's policy and every setting that produced it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectivePolicy {
    pub policy: LaunchPolicy,
    /// One English phrase per setting that holds the run at `policy`
    /// (`default` when nothing is set), sorted.
    pub sources: Vec<String>,
}

impl EffectivePolicy {
    /// No setting at all: `allow`.
    pub fn allow() -> Self {
        EffectivePolicy {
            policy: LaunchPolicy::Allow,
            sources: vec![DEFAULT_SOURCE.to_string()],
        }
    }
}

/// The authorization a run was admitted with: written once into the run's
/// manifest at start, or into a resume's `run_resumed` event, and covering
/// the run for its whole lifetime. Never holds a nonce.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    /// What was launched: `start`, `trial`, `execute_plan`, `eval`,
    /// `resume`, `resume_all`, `continue_from` or `child`.
    pub kind: String,
    /// Who launched it: `agent_mcp`, `agent_mcp:<client>`, `run_step:mcp`,
    /// `run_step:cli`, `cli_terminal`, `cli`, `dashboard`, `engine:<by>`,
    /// or for a child `child_of:<run id>`.
    pub initiator: String,
    /// How an `ask` or `deny` launch was confirmed: `terminal_prompt`,
    /// `cli_flag`, `host_relayed`, `dashboard`, `forwarded:<kind>`;
    /// absent for an `allow` launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub confirmation: Option<String>,
    /// The effective policy when it was admitted.
    pub policy: LaunchPolicy,
    /// The settings that produced it.
    #[serde(default)]
    pub sources: Vec<String>,
    /// What the run may spend on (`crate::billing`): the bases the settings
    /// allowed and the agents and bases a person confirmed. Absent in a
    /// record an older apb wrote; such a run is checked against the
    /// settings in force at each spawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub billing: Option<crate::billing::BillingRecord>,
    /// The run step this run was started from (`run_step:cli` or
    /// `run_step:mcp` initiators). Absent for every other start. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub started_by: Option<StartedBy>,
}

/// The run whose step started a run, as the new run's launch record keeps
/// it. The attempt token the step carried is never stored: the record
/// keeps the attempt number it resolved to.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartedBy {
    pub run_id: String,
    /// The registered workspace id of the starting run's project
    /// (`projects::registered_id`); absent when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub node: Option<String>,
    /// The attempt whose token the caller carried (an agent step or a host
    /// task's subagent); absent for a script step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub host_task: Option<String>,
    /// The starting run was found, created by this installation, not
    /// ended, and the node (and the token, when given) match its journal.
    pub verified: bool,
    /// 1 for a run a step of a run outside any chain started; the
    /// starter's chain + 1 after that.
    pub chain: u32,
}

/// The deepest chain of runs started from run steps.
pub const MAX_START_CHAIN: u32 = crate::schema::MAX_SUBPLAYBOOK_DEPTH as u32;

/// The source of a policy nothing sets.
pub const DEFAULT_SOURCE: &str = "default";

/// The other `apb` executables on `PATH` beside the one running: an apb
/// older than this release ignores every launch setting (`agent_launch`,
/// the floors, `policy.yaml`), so the policy holds only while every apb on
/// the machine is this release or newer. `None` when there is no other.
pub fn other_binaries_line() -> Option<String> {
    let me = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok());
    let path = std::env::var_os("PATH")?;
    let mut others: Vec<String> = std::env::split_paths(&path)
        .map(|d| d.join("apb"))
        .filter(|p| p.is_file())
        .filter_map(|p| std::fs::canonicalize(&p).ok())
        .filter(|p| Some(p) != me.as_ref())
        .map(|p| p.display().to_string())
        .collect();
    others.sort();
    others.dedup();
    (!others.is_empty()).then(|| {
        format!(
            "other apb binaries on PATH: {}; an apb older than this release ignores every launch setting, so the launch policy holds only while each of them is this release or newer",
            others.join(", ")
        )
    })
}

/// One playbook of a run's tree, as [`resolve`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The playbook's id.
    pub id: String,
    /// Whether it comes from the global store.
    pub global: bool,
    /// The workspace that owns its definition (`None` for a global one, or
    /// a project without an id yet).
    pub workspace_id: Option<String>,
    /// What the run's version declares.
    pub declared: Option<LaunchPolicy>,
    /// Its key's floor (a record, or the scan of its store).
    pub floor: Option<LaunchPolicy>,
}

impl Member {
    /// `<id>` or `global:<id>`, how the project file and the sources name it.
    pub fn label(&self) -> String {
        if self.global {
            format!("{}{}", crate::policy_file::GLOBAL_PREFIX, self.id)
        } else {
            self.id.clone()
        }
    }

    /// The exact machine key: `<workspace id>/<id>` or `global/<id>`.
    pub fn machine_key(&self) -> Option<String> {
        if self.global {
            Some(format!("{}/{}", floor::GLOBAL_SCOPE, self.id))
        } else {
            self.workspace_id
                .as_ref()
                .map(|ws| format!("{ws}/{}", self.id))
        }
    }
}

/// Combines the tree's declarations and floors with the settings files.
pub fn resolve(members: &[Member], settings: &crate::policy_file::Policies) -> EffectivePolicy {
    let mut found: Vec<(LaunchPolicy, String)> = Vec::new();
    let machine = &settings.machine.launch;
    let project = &settings.project.launch;
    for m in members {
        let label = m.label();
        let entry = m
            .machine_key()
            .and_then(|k| machine.playbooks.get(&k).map(|p| (k, *p)));
        match entry {
            Some((key, p)) => {
                found.push((p, format!("machine policy.yaml launch.playbooks `{key}`")));
            }
            None => {
                if let Some(p) = m.declared {
                    found.push((p, format!("playbook `{label}` declares agent_launch")));
                }
                if let Some(p) = m.floor {
                    found.push((p, format!("launch floor of `{label}`")));
                }
            }
        }
        if let Some(p) = project.playbooks.get(&label) {
            found.push((
                *p,
                format!("project policy.yaml launch.playbooks `{label}`"),
            ));
        }
    }
    if let Some(p) = machine.agent {
        found.push((p, "machine policy.yaml launch.agent".to_string()));
    }
    if let Some(p) = project.agent {
        found.push((p, "project policy.yaml launch.agent".to_string()));
    }
    let policy = found.iter().map(|(p, _)| *p).max().unwrap_or_default();
    let mut sources: Vec<String> = found
        .into_iter()
        .filter(|(p, _)| *p == policy)
        .map(|(_, s)| s)
        .collect();
    sources.sort();
    sources.dedup();
    if sources.is_empty() {
        sources.push(DEFAULT_SOURCE.to_string());
    }
    EffectivePolicy { policy, sources }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy_file::{LaunchSection, Policies, PolicyFile};
    use LaunchPolicy::*;

    fn member(id: &str, declared: Option<LaunchPolicy>, floor: Option<LaunchPolicy>) -> Member {
        Member {
            id: id.into(),
            global: false,
            workspace_id: Some("ws1".into()),
            declared,
            floor,
        }
    }

    fn settings(machine: LaunchSection, project: LaunchSection) -> Policies {
        Policies {
            machine: PolicyFile {
                launch: machine,
                ..Default::default()
            },
            project: PolicyFile {
                launch: project,
                ..Default::default()
            },
        }
    }

    fn record() -> LaunchRecord {
        LaunchRecord {
            kind: "start".into(),
            initiator: "run_step:cli".into(),
            confirmation: None,
            policy: LaunchPolicy::Allow,
            sources: vec![DEFAULT_SOURCE.into()],
            billing: None,
            started_by: None,
        }
    }

    /// A record an older apb wrote has no `started_by` and decodes; one
    /// with it round-trips and never names a token.
    #[test]
    fn a_launch_record_with_and_without_started_by_reads() {
        let old = r#"{"kind":"start","initiator":"cli","policy":"allow","sources":["default"]}"#;
        let r: LaunchRecord = serde_json::from_str(old).unwrap();
        assert_eq!(r.started_by, None);
        assert!(!serde_json::to_string(&r).unwrap().contains("started_by"));
        let with = LaunchRecord {
            started_by: Some(StartedBy {
                run_id: "t-1".into(),
                workspace_id: Some("ws1".into()),
                node: Some("triage".into()),
                attempt: Some(2),
                host_task: None,
                verified: true,
                chain: 1,
            }),
            ..record()
        };
        let text = serde_json::to_string(&with).unwrap();
        assert!(!text.contains("token"), "{text}");
        assert_eq!(serde_json::from_str::<LaunchRecord>(&text).unwrap(), with);
        assert_eq!(MAX_START_CHAIN, 5);
    }

    #[test]
    fn nothing_set_is_allow_by_default() {
        let e = resolve(&[member("p", None, None)], &Policies::default());
        assert_eq!(e, EffectivePolicy::allow());
    }

    #[test]
    fn the_floor_holds_a_looser_declaration() {
        let e = resolve(
            &[member("p", Some(Allow), Some(Deny))],
            &Policies::default(),
        );
        assert_eq!(e.policy, Deny);
        assert_eq!(e.sources, vec!["launch floor of `p`"]);
    }

    #[test]
    fn an_exact_machine_entry_replaces_declaration_and_floor() {
        let mut m = LaunchSection::default();
        m.playbooks.insert("ws1/p".into(), Allow);
        let e = resolve(
            &[member("p", Some(Deny), Some(Deny))],
            &settings(m, LaunchSection::default()),
        );
        assert_eq!(e.policy, Allow);
        assert_eq!(
            e.sources,
            vec!["machine policy.yaml launch.playbooks `ws1/p`"]
        );
    }

    #[test]
    fn a_machine_entry_for_another_workspace_does_not_apply() {
        let mut m = LaunchSection::default();
        m.playbooks.insert("ws2/p".into(), Allow);
        let e = resolve(
            &[member("p", Some(Deny), None)],
            &settings(m, LaunchSection::default()),
        );
        assert_eq!(e.policy, Deny);
    }

    #[test]
    fn the_project_file_only_narrows() {
        let mut p = LaunchSection::default();
        p.playbooks.insert("p".into(), Allow);
        p.agent = Some(Allow);
        let e = resolve(
            &[member("p", Some(Ask), None)],
            &settings(LaunchSection::default(), p.clone()),
        );
        assert_eq!(e.policy, Ask);
        p.playbooks.insert("p".into(), Deny);
        let e = resolve(
            &[member("p", Some(Ask), None)],
            &settings(LaunchSection::default(), p),
        );
        assert_eq!(e.policy, Deny);
        assert_eq!(e.sources, vec!["project policy.yaml launch.playbooks `p`"]);
    }

    #[test]
    fn the_machine_default_applies_to_every_playbook() {
        let m = LaunchSection {
            agent: Some(Ask),
            ..Default::default()
        };
        let e = resolve(
            &[member("p", None, None)],
            &settings(m, LaunchSection::default()),
        );
        assert_eq!(e.policy, Ask);
        assert_eq!(e.sources, vec!["machine policy.yaml launch.agent"]);
    }

    #[test]
    fn a_child_deny_makes_the_run_deny() {
        let mut child = member("c", Some(Deny), None);
        child.global = true;
        child.workspace_id = None;
        let e = resolve(
            &[member("p", Some(Allow), None), child],
            &Policies::default(),
        );
        assert_eq!(e.policy, Deny);
        assert_eq!(e.sources, vec!["playbook `global:c` declares agent_launch"]);
    }

    #[test]
    fn a_global_member_is_keyed_machine_wide() {
        let mut m = LaunchSection::default();
        m.playbooks.insert("global/c".into(), Ask);
        let child = Member {
            id: "c".into(),
            global: true,
            workspace_id: None,
            declared: None,
            floor: None,
        };
        let e = resolve(&[child], &settings(m, LaunchSection::default()));
        assert_eq!(e.policy, Ask);
    }
}
