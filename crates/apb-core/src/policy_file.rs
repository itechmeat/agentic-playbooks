//! The settings file `policy.yaml`: the machine's in the config dir and a
//! project's in `.apb/`. It holds what older binaries must never misread,
//! so it lives beside `config.yaml` instead of inside it: an older apb
//! never opens it and loads its config exactly as before.
//!
//! Two sections: `notify:`, the filters of a default notification
//! subscription, and `launch:`, the launch policy of agents
//! ([`crate::launch`]):
//!
//! ```yaml
//! notify:
//!   finished: [failed, aborted]          # the outcomes a `finished` notice covers
//!   playbooks: [deploy, global:triage]   # the playbooks a default subscription covers
//! launch:
//!   agent: ask                           # every playbook, for an agent
//!   playbooks:                           # machine: exact `<workspace id>/<id>` or `global/<id>`
//!     "3f2a.../deploy": allow            # project file: `<id>` or `global:<id>`
//! ```
//!
//! The machine file sets them; a project file may only narrow them. The
//! reader is strict: an unknown section, key or value is an error that
//! names it. A present file with an error creates no default subscription
//! (the run journals the problem) and refuses launches through agent
//! channels (`policy_file_invalid`). No apb surface writes the file; `apb
//! doctor` reads it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::launch::policy::LaunchPolicy;
use crate::notify::FinishOutcome;
use crate::registry::is_safe_segment;

/// The file name, in the machine config dir and in a project's `.apb/`.
pub const POLICY_FILE: &str = "policy.yaml";

/// The prefix of a global playbook in a `playbooks` list.
pub const GLOBAL_PREFIX: &str = "global:";

/// One `policy.yaml`, read and checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyFile {
    pub notify: NotifyPolicy,
    pub launch: LaunchSection,
}

/// The `launch:` section: the default for every playbook an agent
/// launches, and per-playbook entries. Machine keys are exact
/// (`<workspace id>/<id>` or `global/<id>`), so an entry reaches one
/// playbook of one project; project keys are `<id>` or `global:<id>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LaunchSection {
    pub agent: Option<LaunchPolicy>,
    pub playbooks: BTreeMap<String, LaunchPolicy>,
}

impl LaunchSection {
    fn validate(&self, source: PolicySource) -> Result<(), String> {
        for key in self.playbooks.keys() {
            let ok = match source {
                PolicySource::Machine => key
                    .split_once('/')
                    .is_some_and(|(ws, id)| is_safe_segment(ws) && is_safe_segment(id)),
                PolicySource::Project => {
                    is_safe_segment(key.strip_prefix(GLOBAL_PREFIX).unwrap_or(key))
                }
            };
            if !ok {
                let want = match source {
                    PolicySource::Machine => "`<workspace id>/<id>` or `global/<id>`",
                    PolicySource::Project => "`<id>` or `global:<id>`",
                };
                return Err(format!(
                    "launch.playbooks: `{key}` is not a playbook key ({want})"
                ));
            }
        }
        Ok(())
    }

    /// Whether the section sets anything.
    pub fn is_empty(&self) -> bool {
        self.agent.is_none() && self.playbooks.is_empty()
    }
}

/// The `notify:` section: filters of a default subscription. Absent keys
/// filter nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifyPolicy {
    /// The run outcomes a `finished` notice is sent for.
    pub finished: Option<Vec<FinishOutcome>>,
    /// The playbooks a default subscription covers: `<id>` for a project
    /// playbook, `global:<id>` for a global one.
    pub playbooks: Option<Vec<String>>,
}

/// The playbook a run starts, as a `playbooks` filter names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybookKey<'a> {
    pub id: &'a str,
    pub global: bool,
}

impl<'a> PlaybookKey<'a> {
    /// The key of the playbook `r` names.
    pub fn of(r: &'a crate::scope::PlaybookRef) -> Self {
        PlaybookKey {
            id: &r.id,
            global: matches!(r.origin, crate::scope::Origin::Global),
        }
    }

    /// `<id>` or `global:<id>`.
    pub fn name(&self) -> String {
        if self.global {
            format!("{GLOBAL_PREFIX}{}", self.id)
        } else {
            self.id.to_string()
        }
    }
}

impl NotifyPolicy {
    fn validate(&self) -> Result<(), String> {
        if self.finished.as_ref().is_some_and(Vec::is_empty) {
            return Err(
                "notify.finished names no outcome; remove `finished` from policy.yaml (to stop end-of-run notices, leave `finished` out of `notify.on` in config.yaml)"
                    .into(),
            );
        }
        if let Some(list) = &self.playbooks {
            if list.is_empty() {
                return Err("notify.playbooks names no playbook".into());
            }
            for entry in list {
                let id = entry.strip_prefix(GLOBAL_PREFIX).unwrap_or(entry);
                if !is_safe_segment(id) {
                    return Err(format!(
                        "notify.playbooks: `{entry}` is not a playbook id or `global:<id>`"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whether `playbook` passes the `playbooks` filter.
    pub fn covers(&self, playbook: &PlaybookKey) -> bool {
        self.playbooks
            .as_ref()
            .is_none_or(|list| list.iter().any(|e| *e == playbook.name()))
    }

    /// The `finished` filter, every outcome when absent.
    pub fn finished(&self) -> Vec<FinishOutcome> {
        self.finished
            .clone()
            .unwrap_or_else(|| FinishOutcome::ALL.to_vec())
    }
}

/// The machine's and the project's files, each read and checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policies {
    pub machine: PolicyFile,
    pub project: PolicyFile,
}

/// Which file failed to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicySource {
    Machine,
    Project,
}

/// A file that is present and does not read: the problem names the file
/// and what is wrong in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyError {
    pub source: PolicySource,
    pub message: String,
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The machine file's path; `None` without a config dir.
pub fn machine_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join(POLICY_FILE))
}

/// The project file's path in the workspace at `root`.
pub fn project_path(root: &Path) -> PathBuf {
    root.join(".apb").join(POLICY_FILE)
}

impl Policies {
    /// Both files of the workspace at `root`; an absent file filters
    /// nothing.
    pub fn load(root: &Path) -> Result<Self, PolicyError> {
        Self::load_in(Some(root))
    }

    /// Both files, or only the machine's without a workspace.
    pub fn load_in(root: Option<&Path>) -> Result<Self, PolicyError> {
        Self::load_with(root, machine_path().as_deref())
    }

    /// Both files, the machine's read from `machine` (a config dir's
    /// `policy.yaml`; `None` without one).
    pub fn load_with(root: Option<&Path>, machine: Option<&Path>) -> Result<Self, PolicyError> {
        let machine = match machine {
            Some(p) => read(p, PolicySource::Machine).map_err(|message| PolicyError {
                source: PolicySource::Machine,
                message,
            })?,
            None => PolicyFile::default(),
        };
        let project = match root {
            Some(root) => {
                read(&project_path(root), PolicySource::Project).map_err(|message| PolicyError {
                    source: PolicySource::Project,
                    message,
                })?
            }
            None => PolicyFile::default(),
        };
        let policies = Policies { machine, project };
        // Each file reads on its own; together they must leave an outcome,
        // or a run that asks for `finished` would quietly never hear of it.
        if policies.finished().is_empty() {
            return Err(PolicyError {
                source: PolicySource::Project,
                message: format!(
                    "notify.finished of `{}` keeps no outcome the machine's policy.yaml allows",
                    root.map(project_path).unwrap_or_default().display()
                ),
            });
        }
        Ok(policies)
    }

    /// Whether a default subscription covers `playbook`: both files'
    /// `playbooks` filters pass it.
    pub fn covers(&self, playbook: &PlaybookKey) -> bool {
        self.machine.notify.covers(playbook) && self.project.notify.covers(playbook)
    }

    /// The machine's `finished` filter narrowed by the project's.
    pub fn finished(&self) -> Vec<FinishOutcome> {
        let project = self.project.notify.finished();
        self.machine
            .notify
            .finished()
            .into_iter()
            .filter(|o| project.contains(o))
            .collect()
    }
}

/// One file at `path`: absent or empty is the default; anything present
/// must read whole.
fn read(path: &Path, source: PolicySource) -> Result<PolicyFile, String> {
    let raw = match crate::fsutil::read_settings_file(path, crate::fsutil::SETTINGS_FILE_MAX_BYTES)
    {
        Ok(Some(raw)) => raw,
        Ok(None) => return Ok(PolicyFile::default()),
        Err(e) => return Err(format!("cannot read `{}`: {e}", path.display())),
    };
    parse_as(&raw, source).map_err(|e| format!("invalid `{}`: {e}", path.display()))
}

/// The machine file's text, checked.
pub fn parse(raw: &str) -> Result<PolicyFile, String> {
    parse_as(raw, PolicySource::Machine)
}

fn parse_as(raw: &str, source: PolicySource) -> Result<PolicyFile, String> {
    if raw.trim().is_empty() {
        return Ok(PolicyFile::default());
    }
    let doc: yaml_serde::Value = yaml_serde::from_str(raw).map_err(|e| e.to_string())?;
    let file: PolicyFile = match doc {
        yaml_serde::Value::Null => return Ok(PolicyFile::default()),
        yaml_serde::Value::Mapping(_) => yaml_serde::from_value(doc).map_err(|e| e.to_string())?,
        _ => return Err("the top level must be a mapping".into()),
    };
    file.notify.validate()?;
    file.launch.validate(source)?;
    Ok(file)
}

/// The doctor line: `(ok, detail)`, not ok when a present file does not
/// read. Names each file and what it sets.
pub fn doctor_line(root: &Path) -> (bool, String) {
    let mut ok = true;
    let mut parts = Vec::new();
    let files = [
        ("machine", machine_path(), PolicySource::Machine),
        ("project", Some(project_path(root)), PolicySource::Project),
    ];
    for (label, path, source) in files {
        let Some(path) = path else {
            parts.push(format!("{label}: no config dir"));
            continue;
        };
        if !path.exists() {
            parts.push(format!("{label}: absent"));
            continue;
        }
        match read(&path, source) {
            Ok(file) => parts.push(format!("{label}: {}", summary(&file))),
            Err(e) => {
                ok = false;
                parts.push(format!(
                    "{label}: {e}; no default notification subscription and no agent launch until it is fixed"
                ));
            }
        }
    }
    if ok && let Err(e) = Policies::load(root) {
        ok = false;
        parts.push(format!(
            "{e}; no default notification subscription until it is fixed"
        ));
    }
    (ok, parts.join("; "))
}

fn summary(file: &PolicyFile) -> String {
    let n = &file.notify;
    let mut sets = Vec::new();
    if let Some(f) = &n.finished {
        sets.push(format!(
            "notify.finished [{}]",
            f.iter().map(|o| o.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(p) = &n.playbooks {
        sets.push(format!("notify.playbooks [{}]", p.join(", ")));
    }
    let l = &file.launch;
    if let Some(a) = l.agent {
        sets.push(format!("launch.agent {a}"));
    }
    if !l.playbooks.is_empty() {
        sets.push(format!(
            "launch.playbooks [{}]",
            l.playbooks
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if sets.is_empty() {
        "valid, sets nothing".into()
    } else {
        format!("valid, {}", sets.join(", "))
    }
}
