//! `apb setup plan`: what setup would do here, read only. It discovers the
//! agents, the user's subscriptions, the shipped playbooks and skills in the
//! global store, the role profiles, the hosts the MCP server can be
//! registered in, a project's standing block and the sandbox, and proposes
//! the actions. The digest covers the actions and the state they depend
//! on, so `apply` runs exactly the plan the agent read.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use apb_core::bundled::{self, Independence, InstallState, RolePlan, SkippedAgent};
use apb_core::models_table::Subscription;
use apb_core::profile::{ProfileScope, QualifiedProfileRef};
use apb_core::profile_store::{PlaybookOrigin, resolve_profile};

use apb_core::agents::HookRegistration;
use apb_core::agents::McpRegistration as Registration;

use super::hook::{self, HookState};
use super::hosts::{self, HostState};
use super::observe::{self, Suggestion, TrackerSeen};
use super::standing::{self, BlockState};

/// The version of the plan's JSON shape.
pub const PLAN_SCHEMA: u32 = 1;

/// What one action does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    InstallPlaybook,
    UpgradePlaybook,
    InstallSkill,
    UpgradeSkill,
    WriteRoleProfile,
    RegisterMcp,
    /// The end-of-turn save check (`apb hook stop`) in a host's settings.
    RegisterHook,
    WriteStandingBlock,
    /// A person asked for the shipped content over a changed default.
    ReplaceModifiedPlaybook,
    ReplaceModifiedSkill,
}

impl ActionKind {
    /// The stable name (the JSON spelling).
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::InstallPlaybook => "install_playbook",
            ActionKind::UpgradePlaybook => "upgrade_playbook",
            ActionKind::InstallSkill => "install_skill",
            ActionKind::UpgradeSkill => "upgrade_skill",
            ActionKind::WriteRoleProfile => "write_role_profile",
            ActionKind::RegisterMcp => "register_mcp",
            ActionKind::RegisterHook => "register_hook",
            ActionKind::WriteStandingBlock => "write_standing_block",
            ActionKind::ReplaceModifiedPlaybook => "replace_modified_playbook",
            ActionKind::ReplaceModifiedSkill => "replace_modified_skill",
        }
    }
}

/// One proposed action.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Action {
    pub kind: ActionKind,
    pub target: String,
    /// One sentence for a person.
    pub detail: String,
    /// `apb setup remove` undoes it (a replace by giving the person's copy
    /// back).
    pub reversible: bool,
    pub undo: String,
    /// The state the action depends on and what it writes.
    pub params: Value,
}

/// Why apply refuses this plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Refusal {
    pub code: String,
    pub message: String,
}

/// Something worth saying that is not an action.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Warning {
    pub code: String,
    pub detail: String,
}

/// A host entry setup cannot add safely: the person adds it by hand.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ManualStep {
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub why: String,
    /// The exact text to add.
    pub snippet: String,
}

/// Which changed defaults a person asked to replace with the shipped
/// content (`apb setup plan --replace-modified [<name>...]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ReplaceModified {
    /// None: a changed default only gets an `update_available` warning.
    #[default]
    None,
    /// Every changed default playbook and skill.
    All,
    /// These names only (playbook ids or skill names).
    Only(Vec<String>),
}

impl ReplaceModified {
    /// From the flag: absent, bare (every one) or with names.
    pub fn from_flag(names: Option<Vec<String>>) -> Self {
        match names {
            None => ReplaceModified::None,
            Some(n) if n.is_empty() => ReplaceModified::All,
            Some(n) => ReplaceModified::Only(n),
        }
    }

    fn wants(&self, name: &str) -> bool {
        match self {
            ReplaceModified::None => false,
            ReplaceModified::All => true,
            ReplaceModified::Only(names) => names.iter().any(|n| n == name),
        }
    }

    /// The flag that asks for `name`, for a hint.
    fn hint(name: &str) -> String {
        format!("apb setup plan --replace-modified {name}")
    }
}

/// Something setup never does: the person decides it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NeedsPerson {
    pub kind: &'static str,
    pub detail: &'static str,
    pub how: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentSeen {
    pub agent: String,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemSeen {
    pub name: String,
    pub shipped_version: String,
    #[serde(flatten)]
    pub state: InstallState,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoleSeen {
    pub name: String,
    pub agent: String,
    pub model: String,
    pub fallbacks: Vec<bundled::RoleFallback>,
    pub skills: Vec<String>,
    /// A global profile of that name exists.
    pub present: bool,
    /// The agent the existing profile names, when it exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub present_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RolesSeen {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub independence: Option<Independence>,
    pub profiles: Vec<RoleSeen>,
    pub skipped: Vec<SkippedAgent>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectSeen {
    pub dir: PathBuf,
    pub standing_block: BlockState,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SandboxSeen {
    pub available: bool,
    pub detail: String,
}

/// What the plan read.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Observed {
    pub agents: Vec<AgentSeen>,
    pub subscriptions: Vec<Subscription>,
    pub models_table_as_of: String,
    pub playbooks: Vec<ItemSeen>,
    pub skills: Vec<ItemSeen>,
    pub role_profiles: RolesSeen,
    pub hosts: Vec<HostState>,
    /// The hosts that run apb's end-of-turn save check.
    pub hooks: Vec<HookState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectSeen>,
    pub sandbox: SandboxSeen,
    /// Work trackers the project uses, for information only: apb depends
    /// on none of them.
    pub trackers: Vec<TrackerSeen>,
}

/// The whole plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plan {
    pub schema: u32,
    /// sha256 over the actions and the refusal.
    pub digest: String,
    /// No refusal: apply would run the actions.
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
    pub actions: Vec<Action>,
    pub warnings: Vec<Warning>,
    /// Host entries the person adds by hand: setup writes nothing there.
    pub manual: Vec<ManualStep>,
    pub needs_person: Vec<NeedsPerson>,
    /// Review helpers: one installed here is `required` (a step of every
    /// review cycle); one not installed is an optional suggestion. Setup
    /// only detects them and never installs anything.
    pub suggestions: Vec<HelperSuggestion>,
    pub observed: Observed,
    /// The role profiles to write, with their skills.
    #[serde(skip)]
    pub(crate) roles: Option<RolePlan>,
}

/// One review helper in the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HelperSuggestion {
    #[serde(flatten)]
    pub helper: Suggestion,
    /// True when one of its examples is on PATH on this machine and answers
    /// its version check (`observe::suggestions`): the
    /// review cycle (the `apb-review` skill) must then run it, and a run
    /// that fails is reported as a blocking failure for the implementer or
    /// the person to fix, never a silent skip. False leaves it an optional
    /// suggestion.
    pub required: bool,
}

impl std::ops::Deref for HelperSuggestion {
    type Target = Suggestion;

    fn deref(&self) -> &Suggestion {
        &self.helper
    }
}

/// The plan's helpers: each detected one becomes required.
fn helper_suggestions(helpers: Vec<Suggestion>) -> Vec<HelperSuggestion> {
    helpers
        .into_iter()
        .map(|helper| HelperSuggestion {
            required: !helper.found.is_empty(),
            helper,
        })
        .collect()
}

/// What setup never does on its own.
pub const NEEDS_PERSON: &[NeedsPerson] = &[
    NeedsPerson {
        kind: "launch_floor",
        detail: "Lowering a playbook's launch floor loosens a guarantee.",
        how: "the person runs apb launch floor <id> --lower-to <policy> at a terminal",
    },
    NeedsPerson {
        kind: "feedback_loop",
        detail: "The feedback-loop block lets agents file issues about apb.",
        how: "only the person, at a terminal: apb init (or apb init --feedback-loop yes there); refused without a terminal and inside a run step",
    },
    NeedsPerson {
        kind: "notify_target",
        detail: "A notification target sends messages to people.",
        how: "the person sets it: apb notify --via <connector> --to <address>",
    },
    NeedsPerson {
        kind: "save_nudge_model",
        detail: "The end-of-turn save check can ask the configured decision provider whether the last request a person typed in a host session is recurring. Turned on, that request leaves the machine: redacted, at most 2000 characters, at most 100 requests a day, sent to the provider in decisions.yaml. Off by default; without it the check uses only the task's shape and the host agent's own judgement.",
        how: "the person adds to decisions.yaml: mode: advise, uses: { save_nudge: { mode: advise } } and requests in privacy.send",
    },
    NeedsPerson {
        kind: "trust_fork",
        detail: "An adapted copy of a default an agent saves is approved as locally authored, like any playbook an agent writes; it never gets the shipped trust or automatic upgrades, and it cannot use an apb- id.",
        how: "the person reviews it before running it: apb adopt <id> --scope global, and apb trust revoke to withdraw the approval",
    },
];

const UNDO: &str = "apb setup remove";

fn action(kind: ActionKind, target: &str, detail: String, params: Value) -> Action {
    Action {
        kind,
        target: target.to_string(),
        detail,
        reversible: true,
        undo: UNDO.to_string(),
        params,
    }
}

fn warning(code: &str, detail: String) -> Warning {
    Warning {
        code: code.to_string(),
        detail,
    }
}

/// The global profile `name`: its agent, when it exists.
pub(crate) fn existing_profile(root: &Path, name: &str) -> Option<String> {
    existing_role(root, name).map(|(agent, _)| agent)
}

/// The global profile `name`: its agent and the names of its skills.
fn existing_role(root: &Path, name: &str) -> Option<(String, Vec<String>)> {
    let r = QualifiedProfileRef {
        name: name.to_string(),
        scope: ProfileScope::Global,
    };
    resolve_profile(root, PlaybookOrigin::Global, &r)
        .ok()
        .map(|p| {
            let skills = p.doc.skills.into_iter().map(|s| s.name).collect();
            (p.doc.executor.agent, skills)
        })
}

/// The digest of `actions` and `refusal`: serde_json writes objects with
/// sorted keys and structs in declaration order, so the bytes are
/// canonical.
pub(crate) fn digest(actions: &[Action], refusal: Option<&Refusal>) -> String {
    let bytes =
        serde_json::to_vec(&json!({ "actions": actions, "refusal": refusal })).unwrap_or_default();
    apb_core::content::sha256_hex(&bytes)
}

/// The shipped playbooks: what is seen and what to do.
fn playbooks(
    replace: &ReplaceModified,
    actions: &mut Vec<Action>,
    warnings: &mut Vec<Warning>,
) -> Vec<ItemSeen> {
    let mut seen = Vec::new();
    for p in bundled::playbooks() {
        let state = bundled::playbook_state(&p);
        let params = json!({ "version": p.version, "observed": state });
        match &state {
            InstallState::Absent => actions.push(action(
                ActionKind::InstallPlaybook,
                &p.id,
                format!("install the default playbook {} {} into the global store", p.id, p.version),
                params,
            )),
            InstallState::Pristine { installed } => actions.push(action(
                ActionKind::UpgradePlaybook,
                &p.id,
                format!("upgrade the unmodified default playbook {} {installed} -> {}", p.id, p.version),
                params,
            )),
            InstallState::Modified { installed } if replace.wants(&p.id) => {
                actions.push(Action {
                    undo: format!(
                        "{UNDO} moves current back to {installed} (or apb version use {} {installed} --scope global)",
                        p.id
                    ),
                    ..action(
                        ActionKind::ReplaceModifiedPlaybook,
                        &p.id,
                        format!(
                            "replace the changed default playbook {} (at {installed}) with the shipped {}; {installed} stays in its history",
                            p.id, p.version
                        ),
                        params,
                    )
                })
            }
            InstallState::Modified { installed } => warnings.push(warning(
                "update_available",
                format!(
                    "the default playbook {} stays at {installed}, which was changed or chosen locally; the shipped {} is available ({})",
                    p.id,
                    p.version,
                    ReplaceModified::hint(&p.id)
                ),
            )),
            InstallState::Foreign => warnings.push(warning(
                "foreign_playbook",
                format!("a global playbook {} exists that apb did not install; left alone", p.id),
            )),
            InstallState::UpToDate { .. } => {}
        }
        seen.push(ItemSeen {
            name: p.id,
            shipped_version: p.version,
            state,
        });
    }
    seen
}

/// The shipped skills: what is seen and what to do.
fn skills(
    replace: &ReplaceModified,
    actions: &mut Vec<Action>,
    warnings: &mut Vec<Warning>,
) -> Vec<ItemSeen> {
    let mut seen = Vec::new();
    for s in bundled::skills() {
        let state = bundled::skill_state(&s);
        let params = json!({ "version": s.version, "observed": state });
        match &state {
            InstallState::Absent => actions.push(action(
                ActionKind::InstallSkill,
                &s.name,
                format!("install the skill {} {} into the global skills directory", s.name, s.version),
                params,
            )),
            InstallState::Pristine { installed } => actions.push(action(
                ActionKind::UpgradeSkill,
                &s.name,
                format!("upgrade the unmodified skill {} {installed} -> {}", s.name, s.version),
                params,
            )),
            InstallState::Modified { installed } if replace.wants(&s.name) => {
                actions.push(Action {
                    undo: format!(
                        "{UNDO} puts the changed copy back from its backup in the skills directory"
                    ),
                    ..action(
                        ActionKind::ReplaceModifiedSkill,
                        &s.name,
                        format!(
                            "replace the changed skill {} ({installed}) with the shipped {}; the changed copy is kept as a backup",
                            s.name, s.version
                        ),
                        params,
                    )
                })
            }
            InstallState::Modified { installed } => warnings.push(warning(
                "update_available",
                format!(
                    "the skill {} stays at {installed}, which was changed or chosen locally; the shipped {} is available ({})",
                    s.name,
                    s.version,
                    ReplaceModified::hint(&s.name)
                ),
            )),
            InstallState::Foreign => warnings.push(warning(
                "foreign_skill",
                format!("a skill {} exists that apb did not install; left alone", s.name),
            )),
            InstallState::UpToDate { .. } => {}
        }
        seen.push(ItemSeen {
            name: s.name,
            shipped_version: s.version,
            state,
        });
    }
    seen
}

/// The role profiles: what is seen, what to write and the refusal when no
/// agent can take them.
fn roles(
    root: &Path,
    catalog: &apb_core::agent_catalog::Catalog,
    skills_seen: &[ItemSeen],
    actions: &mut Vec<Action>,
    warnings: &mut Vec<Warning>,
) -> (RolesSeen, Option<RolePlan>, Option<Refusal>) {
    let mut plan = match bundled::plan_role_profiles(&catalog.agents, &catalog.table) {
        Ok(p) => p,
        Err(e) => {
            let skipped = catalog
                .agents
                .iter()
                .map(|a| SkippedAgent {
                    agent: a.agent.clone(),
                    reason: if a.installed {
                        "no default model in the models table".into()
                    } else {
                        "not installed".into()
                    },
                })
                .collect();
            let seen = RolesSeen {
                independence: None,
                profiles: Vec::new(),
                skipped,
            };
            let refusal = Refusal {
                code: e.code().to_string(),
                message: e.to_string(),
            };
            return (seen, None, Some(refusal));
        }
    };
    // A skill is carried when it is there, or this plan installs it.
    let installing: Vec<String> = actions
        .iter()
        .filter(|a| a.kind == ActionKind::InstallSkill)
        .map(|a| a.target.clone())
        .collect();
    let available = |name: &str| {
        skills_seen
            .iter()
            .any(|s| s.name == name && s.state != InstallState::Absent)
            || installing.iter().any(|t| t == name)
    };
    let mut seen = Vec::new();
    for p in &mut plan.profiles {
        p.skills = bundled::role_skills(&p.name)
            .iter()
            .filter(|n| available(n))
            .map(|n| n.to_string())
            .collect();
        let existing = existing_role(root, &p.name);
        // An earlier apb wrote the profile with fewer skills: setup never
        // rewrites a profile, so the person adds the ones it lacks.
        if let Some((_, has)) = &existing {
            let missing: Vec<&str> = p
                .skills
                .iter()
                .filter(|s| !has.contains(s))
                .map(String::as_str)
                .collect();
            if !missing.is_empty() {
                warnings.push(warning(
                    "role_profile_missing_skills",
                    format!(
                        "the global profile {} lacks the skills {} that its role now carries; add them to its skills (apb profile edit {} --scope global, or the profile_write tool), else the playbooks that bind it ask for a skill it does not have",
                        p.name,
                        missing.join(", "),
                        p.name
                    ),
                ));
            }
        }
        let present_agent = existing.map(|(agent, _)| agent);
        if present_agent.is_none() {
            actions.push(action(
                ActionKind::WriteRoleProfile,
                &p.name,
                format!(
                    "write the global profile {} on {} ({})",
                    p.name, p.agent, p.model
                ),
                json!({
                    "agent": p.agent,
                    "model": p.model,
                    "fallbacks": p.fallbacks,
                    "skills": p.skills,
                    "independence": plan.independence,
                }),
            ));
        }
        seen.push(RoleSeen {
            name: p.name.clone(),
            agent: p.agent.clone(),
            model: p.model.clone(),
            fallbacks: p.fallbacks.clone(),
            skills: p.skills.clone(),
            present: present_agent.is_some(),
            present_agent,
        });
    }
    if plan.independence == Independence::SameAgent {
        warnings.push(warning(
            "same_agent_reviewer",
            "only one agent is usable: the reviewer runs on the same agent as the main profile, in a fresh session; reviews are less independent".to_string(),
        ));
    }
    let roles_seen = RolesSeen {
        independence: Some(plan.independence),
        profiles: seen,
        skipped: plan.skipped.clone(),
    };
    (roles_seen, Some(plan), None)
}

/// The hosts: what to register and what to say.
fn host_actions(
    states: &[HostState],
    actions: &mut Vec<Action>,
    warnings: &mut Vec<Warning>,
    manual: &mut Vec<ManualStep>,
) {
    for h in states.iter().filter(|h| h.detected) {
        match h.registration {
            Registration::Absent if h.manual.is_some() => manual.push(ManualStep {
                host: h.host.clone(),
                path: h.config.clone(),
                why: h.note.clone().unwrap_or_default(),
                snippet: h.manual.clone().unwrap_or_default(),
            }),
            Registration::Absent => actions.push(action(
                ActionKind::RegisterMcp,
                &h.host,
                if h.method == "cli" {
                    format!(
                        "register the MCP server `{}` at user scope through the {} CLI",
                        hosts::SERVER_NAME,
                        h.host
                    )
                } else {
                    format!(
                        "add the MCP server `{}` to {}",
                        hosts::SERVER_NAME,
                        h.config
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    )
                },
                h.precondition(),
            )),
            Registration::Same => {}
            Registration::Different => warnings.push(warning(
                "mcp_entry_differs",
                format!(
                    "{} already has an entry `{}` with another command; left alone",
                    h.host,
                    hosts::SERVER_NAME
                ),
            )),
            Registration::Unreadable => warnings.push(warning(
                "host_config_unreadable",
                h.note
                    .clone()
                    .unwrap_or_else(|| format!("the {} config could not be read", h.host)),
            )),
        }
    }
}

/// The end-of-turn save check: what to register and what to say.
fn hook_actions(states: &[HookState], actions: &mut Vec<Action>, warnings: &mut Vec<Warning>) {
    for h in states.iter().filter(|h| h.detected) {
        let Some(path) = h.settings.as_ref().map(|p| p.display().to_string()) else {
            warnings.push(warning(
                "hook_settings_unreadable",
                format!(
                    "the {} settings path is unknown (no home directory); the save check is not registered",
                    h.host
                ),
            ));
            continue;
        };
        match h.registration {
            HookRegistration::Absent => actions.push(action(
                ActionKind::RegisterHook,
                &h.host,
                format!(
                    "register the end-of-turn save check `{}` as a Stop hook in {path} (APB_SAVE_NUDGE=off silences it; run apb setup remove before uninstalling apb, else the host reports a failing hook on every turn). It reads the session's history on this machine and sends nothing unless the person turns on the save_nudge decision use",
                    hook::HOOK_COMMAND
                ),
                h.precondition(),
            )),
            HookRegistration::Same => {}
            HookRegistration::Different => warnings.push(warning(
                "hook_entry_differs",
                format!(
                    "{path} already runs apb's Stop hook in another form; left alone"
                ),
            )),
            HookRegistration::Unreadable => warnings.push(warning(
                "hook_settings_unreadable",
                h.note
                    .clone()
                    .unwrap_or_else(|| format!("the {} settings could not be read", h.host)),
            )),
        }
    }
}

/// The standing block of the project `dir`: what is seen and what to do.
fn project_block(
    dir: &Path,
    actions: &mut Vec<Action>,
    warnings: &mut Vec<Warning>,
) -> Result<ProjectSeen, String> {
    let dir = std::fs::canonicalize(dir).map_err(|e| format!("project {}: {e}", dir.display()))?;
    let state = standing::block_state(&dir);
    if state != BlockState::Present {
        match standing::link_problem(&dir) {
            Some(why) => warnings.push(warning("standing_block_link", why)),
            None => actions.push(action(
                ActionKind::WriteStandingBlock,
                &dir.display().to_string(),
                "write the standing playbook instruction into CLAUDE.md and AGENTS.md".to_string(),
                json!({ "observed": state }),
            )),
        }
    }
    Ok(ProjectSeen {
        dir,
        standing_block: state,
    })
}

/// A name `--replace-modified` asked for that has nothing to replace.
fn replace_leftovers(replace: &ReplaceModified, actions: &[Action], warnings: &mut Vec<Warning>) {
    let ReplaceModified::Only(names) = replace else {
        return;
    };
    for n in names {
        let planned = actions.iter().any(|a| {
            &a.target == n
                && matches!(
                    a.kind,
                    ActionKind::ReplaceModifiedPlaybook | ActionKind::ReplaceModifiedSkill
                )
        });
        if !planned {
            warnings.push(warning(
                "nothing_to_replace",
                format!("{n} is not a changed default playbook or skill; nothing to replace"),
            ));
        }
    }
}

/// The plan for this machine, and for `project` when given. Read only:
/// detection may start each agent's `--version` and refresh its memo, as
/// `apb detect` does; nothing else is written. `replace` names the changed
/// defaults a person asked to replace with the shipped content. Read only,
/// so a run step may read it too (the `apb-setup` playbook does); applying
/// and removing stay refused there.
pub fn plan(
    root: &Path,
    project: Option<&Path>,
    replace: &ReplaceModified,
) -> Result<Plan, String> {
    let catalog = apb_core::agent_catalog::load(false).map_err(|e| e.to_string())?;
    let mut actions = Vec::new();
    let mut warnings = Vec::new();
    let mut manual = Vec::new();
    let playbooks = playbooks(replace, &mut actions, &mut warnings);
    let skills = skills(replace, &mut actions, &mut warnings);
    replace_leftovers(replace, &actions, &mut warnings);
    let (role_profiles, roles, refusal) =
        roles(root, &catalog, &skills, &mut actions, &mut warnings);
    let host_states = hosts::observe(&catalog.agents);
    host_actions(&host_states, &mut actions, &mut warnings, &mut manual);
    let hook_states = hook::observe(&catalog.agents);
    hook_actions(&hook_states, &mut actions, &mut warnings);
    let suggestions = helper_suggestions(observe::suggestions(root, project));
    let project = project
        .map(|dir| project_block(dir, &mut actions, &mut warnings))
        .transpose()?;
    let facts = apb_engine::sandbox::probe::host_facts();
    let (available, detail) = apb_engine::sandbox::probe::doctor_line(&facts);
    let observed = Observed {
        agents: catalog
            .agents
            .iter()
            .map(|a| AgentSeen {
                agent: a.agent.clone(),
                installed: a.installed,
                version: a.version.clone(),
            })
            .collect(),
        subscriptions: catalog.table.subscriptions.clone(),
        models_table_as_of: catalog.table.as_of.clone(),
        playbooks,
        skills,
        role_profiles,
        hosts: host_states,
        hooks: hook_states,
        trackers: observe::trackers(root, project.as_ref().map(|p| p.dir.as_path())),
        project,
        sandbox: SandboxSeen { available, detail },
    };
    Ok(Plan {
        schema: PLAN_SCHEMA,
        digest: digest(&actions, refusal.as_ref()),
        ready: refusal.is_none(),
        refusal,
        actions,
        warnings,
        manual,
        needs_person: NEEDS_PERSON.to_vec(),
        suggestions,
        observed,
        roles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(settings: Option<&str>, registration: HookRegistration) -> HookState {
        HookState {
            host: "claude".into(),
            detected: true,
            settings: settings.map(std::path::PathBuf::from),
            registration,
            note: None,
            sha256: None,
        }
    }

    /// What the plan says per hook state: one action where the hook can be
    /// registered, a warning where it cannot or is there in another form.
    #[test]
    fn the_hook_is_offered_only_where_it_can_be_written() {
        let path = Some("/h/.claude/settings.json");
        let mut hidden = state(path, HookRegistration::Absent);
        hidden.detected = false;
        let cases = [
            (
                state(path, HookRegistration::Absent),
                Some("register_hook"),
                None,
            ),
            (state(path, HookRegistration::Same), None, None),
            (
                state(path, HookRegistration::Different),
                None,
                Some("hook_entry_differs"),
            ),
            (
                state(path, HookRegistration::Unreadable),
                None,
                Some("hook_settings_unreadable"),
            ),
            (
                state(None, HookRegistration::Absent),
                None,
                Some("hook_settings_unreadable"),
            ),
            (hidden, None, None),
        ];
        for (h, action, warn) in cases {
            let (mut actions, mut warnings) = (Vec::new(), Vec::new());
            hook_actions(std::slice::from_ref(&h), &mut actions, &mut warnings);
            let kinds: Vec<&str> = actions.iter().map(|a| a.kind.as_str()).collect();
            let codes: Vec<&str> = warnings.iter().map(|w| w.code.as_str()).collect();
            assert_eq!(kinds, action.into_iter().collect::<Vec<_>>(), "{h:?}");
            assert_eq!(codes, warn.into_iter().collect::<Vec<_>>(), "{h:?}");
        }
    }
}
