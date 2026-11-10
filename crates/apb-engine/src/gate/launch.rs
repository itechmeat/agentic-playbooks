//! The launch check of the run gate: whether the one who launches a run may
//! launch it under the run's effective launch policy
//! ([`apb_core::launch`]), decided on the final permit.
//!
//! **Who launches.** [`Initiator`] names it explicitly: an agent over MCP,
//! a run step (MCP or CLI with `APB_RUN_ID` or `APB_HOST_TASK` set,
//! whatever its channel), the CLI outside a run step (at a terminal or
//! not; never a person merely because a variable is absent), the
//! dashboard, or the engine resuming a paused run on its own.
//!
//! **The decision table.**
//!
//! | effective | agent (MCP) | run step | CLI, terminal | CLI, no terminal | dashboard | engine |
//! |---|---|---|---|---|---|---|
//! | allow | runs | runs | runs | runs | runs | runs |
//! | ask | the refusal's nonce relayed by the host | refused | prompt or `--confirm-launch` | `--confirm-launch` | runs | refused |
//! | deny | refused | refused | prompt only | refused | runs | refused |
//!
//! **The nonce** binds a confirmation to what the person was shown: the
//! execution workspace, the playbook and its scope, the chosen version and
//! how it was chosen, the canonical digest of the whole permit (definition,
//! verified profile bundles, connectors and accounts, every pinned child
//! with its scope and its own verified maps), the effective policy with its
//! sources, the launch kind and the run for a resume. Anything that changes
//! one of them changes the nonce, so a retry with an old nonce is refused
//! with a fresh one.
//!
//! **The grant.** A launch that passes yields a [`LaunchGrant`]: the record
//! the run keeps (never a nonce), the permit digest and the policy state it
//! was decided on. The engine writes the record only after rechecking the
//! state against what it is about to run, so a floor or a settings file
//! changed between the check and the write refuses the launch instead of
//! authorizing it.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::launch::floor::{self, FloorKey};
use apb_core::launch::{EffectivePolicy, LaunchPolicy, LaunchRecord, Member};
use apb_core::profile::ProfileScope;
use apb_core::schema::{NodeKind, Playbook};
use apb_core::scope::Origin;
use apb_core::step_env::StepCaller;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::billing::BillingCheck;
use crate::run_config::ChildExpectation;

/// The channel a run step launched through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepChannel {
    Mcp,
    Cli,
}

/// Which of the engine's own resumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineResume {
    PauseWaker,
    QuotaProbe,
    ResumeAll,
}

impl EngineResume {
    fn as_str(self) -> &'static str {
        match self {
            EngineResume::PauseWaker => "pause_waker",
            EngineResume::QuotaProbe => "quota_probe",
            EngineResume::ResumeAll => "resume_all",
        }
    }
}

/// Who launches a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Initiator {
    /// An agent over MCP, outside any run step.
    AgentMcp { client: Option<String> },
    /// A step of a run, whatever channel it uses.
    RunStep {
        channel: StepChannel,
        run_id: Option<String>,
        host_task: Option<String>,
    },
    /// The CLI outside any run step.
    CliUnattested { terminal: bool },
    /// A person clicking in the dashboard, signed in with a key.
    Dashboard,
    /// The dashboard without keys: anything on this machine reaches it, so
    /// it counts as the CLI without a terminal (`ask` only with its nonce,
    /// `deny` refused).
    DashboardKeyless,
    /// The engine resuming a paused run on its own.
    Engine { by: EngineResume },
}

impl Initiator {
    /// The MCP server's initiator: a run step when this process is one.
    pub fn mcp(client: Option<String>) -> Self {
        Self::of_step(StepChannel::Mcp).unwrap_or(Initiator::AgentMcp { client })
    }

    /// The CLI's initiator: a run step when this process is one, else the
    /// CLI at a terminal or not.
    pub fn cli(terminal: bool) -> Self {
        Self::of_step(StepChannel::Cli).unwrap_or(Initiator::CliUnattested { terminal })
    }

    fn of_step(channel: StepChannel) -> Option<Self> {
        let step = StepCaller::from_env();
        (step.run_id.is_some() || step.host_task.is_some()).then_some(Initiator::RunStep {
            channel,
            run_id: step.run_id,
            host_task: step.host_task,
        })
    }

    /// How the record names it.
    pub fn label(&self) -> String {
        match self {
            Initiator::AgentMcp { client: Some(c) } if !c.trim().is_empty() => {
                format!("agent_mcp:{}", c.trim())
            }
            Initiator::AgentMcp { .. } => "agent_mcp".into(),
            Initiator::RunStep {
                channel: StepChannel::Mcp,
                ..
            } => "run_step:mcp".into(),
            Initiator::RunStep {
                channel: StepChannel::Cli,
                ..
            } => "run_step:cli".into(),
            Initiator::CliUnattested { terminal: true } => "cli_terminal".into(),
            Initiator::CliUnattested { terminal: false } => "cli".into(),
            Initiator::Dashboard => "dashboard".into(),
            Initiator::DashboardKeyless => "dashboard_keyless".into(),
            Initiator::Engine { by } => format!("engine:{}", by.as_str()),
        }
    }

    /// Whether a redirected config dir is held to the default one's
    /// settings for this initiator: a run step or a CLI without a terminal.
    pub fn checks_redirect(&self) -> bool {
        matches!(
            self,
            Initiator::RunStep { .. } | Initiator::CliUnattested { terminal: false }
        )
    }

    /// Whether this channel is an agent's: a damaged settings or floor file
    /// refuses it, while a person still launches with a warning.
    pub fn is_agent_channel(&self) -> bool {
        !matches!(
            self,
            Initiator::Dashboard | Initiator::CliUnattested { terminal: true }
        )
    }
}

/// What is launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchKind {
    Start,
    Trial,
    ExecutePlan,
    Eval,
    Resume { run_id: String },
    ResumeAll { run_id: String },
    ContinueFrom { run_id: String, node: String },
}

impl LaunchKind {
    /// How the record names it.
    pub fn label(&self) -> &'static str {
        match self {
            LaunchKind::Start => "start",
            LaunchKind::Trial => "trial",
            LaunchKind::ExecutePlan => "execute_plan",
            LaunchKind::Eval => "eval",
            LaunchKind::Resume { .. } => "resume",
            LaunchKind::ResumeAll { .. } => "resume_all",
            LaunchKind::ContinueFrom { .. } => "continue_from",
        }
    }

    /// The run a resume or a continue is about.
    pub fn run_id(&self) -> Option<&str> {
        match self {
            LaunchKind::Resume { run_id }
            | LaunchKind::ResumeAll { run_id }
            | LaunchKind::ContinueFrom { run_id, .. } => Some(run_id),
            _ => None,
        }
    }
}

/// How the version a start runs was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    /// The playbook's candidate, by the gate's draw.
    Candidate,
    /// `current`.
    Current,
    /// The version the caller asked for.
    Explicit,
}

impl Choice {
    pub fn as_str(self) -> &'static str {
        match self {
            Choice::Candidate => "candidate",
            Choice::Current => "current",
            Choice::Explicit => "explicit",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Choice::Candidate, Choice::Current, Choice::Explicit]
            .into_iter()
            .find(|c| c.as_str() == s)
    }
}

/// What a handoff file carried from the process that checked a launch to
/// the one that performs it (see [`crate::launch_handoff`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffGrant {
    /// The record the checking process decided.
    pub record: LaunchRecord,
    /// What it was decided for.
    pub binding: HandoffBinding,
    /// The config dir the checking process read and handed its receiver
    /// (an eval case): the receiver's config dir may differ from the default
    /// one only when it is exactly this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<std::path::PathBuf>,
}

/// What a forwarded launch must still match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoffBinding {
    /// The same permit: the receiver recomputes the nonce and compares.
    Nonce { nonce: String },
    /// The same definition (an eval case in its scratch tree, whose
    /// workspace differs): id, version and trust digest, and the digest of
    /// the verified child pins ([`children_digest`]), so a child that
    /// changed since the check is not what runs under it.
    Definition {
        id: String,
        version: String,
        digest: String,
        #[serde(default)]
        children: String,
    },
}

/// How an `ask` or `deny` launch is confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchConfirmation {
    /// A person answered the terminal question of this CLI process.
    TerminalPrompt,
    /// `--confirm-launch <nonce>`.
    CliFlag { nonce: String },
    /// MCP `confirm_launch: <nonce>`, after the host asked the person.
    HostRelayed { nonce: String },
    /// The dashboard without keys: `confirm_launch: <nonce>` in the request.
    DashboardNonce { nonce: String },
    /// A grant another process checked and handed over.
    Forwarded { handoff: Box<HandoffGrant> },
}

/// Everything the launch check needs to know about the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchContext {
    pub initiator: Initiator,
    pub confirmation: Option<LaunchConfirmation>,
    pub kind: LaunchKind,
    /// A retry's fixed version, from the refusal's start action.
    pub launch_version: Option<String>,
    /// A retry's fixed choice, from the refusal's start action.
    pub launch_choice: Option<Choice>,
    /// The start processes external data ([`crate::external_data`]): the
    /// gate never chooses a waiting candidate for it.
    pub external_data: bool,
}

impl LaunchContext {
    pub fn new(initiator: Initiator, kind: LaunchKind) -> Self {
        LaunchContext {
            initiator,
            confirmation: None,
            kind,
            launch_version: None,
            launch_choice: None,
            external_data: false,
        }
    }

    /// The same caller for a start that processes external data or not.
    pub fn with_external_data(mut self, external_data: bool) -> Self {
        self.external_data = external_data;
        self
    }

    /// A person clicking in the dashboard signed in with a key.
    pub fn dashboard(kind: LaunchKind) -> Self {
        Self::new(Initiator::Dashboard, kind)
    }

    /// A click in the dashboard: a person's when the dashboard has keys,
    /// else [`Initiator::DashboardKeyless`].
    pub fn dashboard_with(keys: bool, kind: LaunchKind) -> Self {
        let initiator = if keys {
            Initiator::Dashboard
        } else {
            Initiator::DashboardKeyless
        };
        Self::new(initiator, kind)
    }

    /// The engine's own resume of `run_id`.
    pub fn engine(by: EngineResume, run_id: &str) -> Self {
        let kind = match by {
            EngineResume::ResumeAll => LaunchKind::ResumeAll {
                run_id: run_id.to_string(),
            },
            _ => LaunchKind::Resume {
                run_id: run_id.to_string(),
            },
        };
        Self::new(Initiator::Engine { by }, kind)
    }

    /// With a confirmation.
    pub fn confirmed(mut self, confirmation: Option<LaunchConfirmation>) -> Self {
        self.confirmation = confirmation;
        self
    }

    /// With a retry's fixed version and choice. A choice that does not
    /// parse is kept as none, so the retry draws again and the nonce does
    /// not match.
    pub fn fixed(mut self, version: Option<String>, choice: Option<&str>) -> Self {
        self.launch_version = version;
        self.launch_choice = choice.and_then(Choice::parse);
        self
    }

    /// The config dir a forwarded grant handed over (an eval case's).
    pub fn handed_config_dir(&self) -> Option<&Path> {
        match &self.confirmation {
            Some(LaunchConfirmation::Forwarded { handoff }) => handoff.config_dir.as_deref(),
            _ => None,
        }
    }

    /// The same caller with no confirmation and no fixed choice: what a
    /// fresh refusal is computed with.
    pub fn unconfirmed(&self) -> Self {
        LaunchContext {
            confirmation: None,
            launch_version: None,
            launch_choice: None,
            ..self.clone()
        }
    }
}

/// A launch that passed: what the run keeps and what the engine rechecks
/// before it keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchGrant {
    pub record: LaunchRecord,
    /// The canonical permit digest the launch was decided for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permit_digest: Option<String>,
    /// The digest of the policy state that decided it; `None` for a child,
    /// which inherits its parent's authorization through its pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_state: Option<String>,
    /// The settings files were damaged and a person launched anyway: the
    /// recheck reads them the same way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tolerant: bool,
    /// The run a resume grant is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The child tree a resume of a run without pins checked: the engine
    /// persists it as the run's pins with the authorization, so the run
    /// spawns exactly these children and never resolves one live again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pins: Option<BTreeMap<String, ChildExpectation>>,
    /// The nonce the launch was decided for, the version and how it was
    /// chosen: what a detached receiver of a forwarded grant recomputes.
    /// Only ever in memory or in a handoff file, never in the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<Choice>,
    /// The billing challenge this launch was confirmed with, still open:
    /// consumed by [`Self::consume_challenge`] once the start is otherwise
    /// committed ([`super::challenge`]). Only ever in memory or in a
    /// handoff file, never in the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<super::challenge::Claim>,
}

/// The refusal of a start whose billing challenge was consumed by another
/// start, or lapsed, between its check and its commit.
pub const CHALLENGE_SPENT: &str = "billing_confirmation_spent";

impl LaunchGrant {
    /// Consumes the billing challenge this launch was confirmed with, where
    /// the start is committed: one confirmation, one start. A grant without
    /// one passes; one whose challenge another start consumed meanwhile, or
    /// that lapsed, is refused, and the start is asked again.
    pub fn consume_challenge(&self) -> Result<(), String> {
        let Some(claim) = &self.challenge else {
            return Ok(());
        };
        let launch = self.nonce.as_deref().unwrap_or_default();
        if super::challenge::redeem(&claim.root, &claim.token, launch) {
            return Ok(());
        }
        Err(format!(
            "{CHALLENGE_SPENT}: the billing confirmation was already used by another start or lapsed; start it again and confirm the billed use once more"
        ))
    }

    /// A child's grant: its parent's authorization through the pin it was
    /// spawned under.
    pub fn child(parent_run: &str, parent: &LaunchRecord) -> Self {
        LaunchGrant {
            record: LaunchRecord {
                kind: "child".into(),
                initiator: format!("child_of:{parent_run}"),
                confirmation: parent.confirmation.clone(),
                policy: parent.policy,
                sources: parent.sources.clone(),
                billing: parent.billing.clone(),
                // A child is never marked: its run step is its parent's.
                started_by: None,
            },
            permit_digest: None,
            policy_state: None,
            tolerant: false,
            run_id: None,
            pins: None,
            nonce: None,
            version: None,
            choice: None,
            challenge: None,
        }
    }
}

/// Refusal codes.
pub const ASK_POLICY: &str = "launch_requires_confirmation";
pub const DENY_POLICY: &str = "launch_requires_person";
pub const STEP_POLICY: &str = "launch_refused_in_run_step";
pub const ENGINE_POLICY: &str = "launch_refused_for_engine";
pub const CHOICE_CHANGED: &str = "launch_choice_changed";
pub const POLICY_CHANGED: &str = "launch_policy_changed";
pub const SETTINGS_INVALID: &str = "policy_file_invalid";
pub const CHILD_NOT_COVERED: &str = "launch_child_not_covered";
pub const TREE_UNVERIFIED: &str = "launch_tree_unverified";

/// Whether `policy` is one of the launch check's refusal codes.
pub fn is_launch_refusal(policy: &str) -> bool {
    matches!(
        policy,
        ASK_POLICY | DENY_POLICY | STEP_POLICY | ENGINE_POLICY | CHOICE_CHANGED | POLICY_CHANGED
    ) || policy == SETTINGS_INVALID
        || policy == crate::step_start::CHAIN_TOO_DEEP
        || policy == floor::INVALID
        || super::billing::is_billing_refusal(policy)
}

/// The tree whose policy is resolved: the playbook a run executes, where
/// its definition comes from, and its verified child pins.
#[derive(Clone, Copy)]
pub struct Tree<'a> {
    pub root: &'a Path,
    pub playbook: &'a Playbook,
    pub origin: &'a Origin,
    pub children: &'a BTreeMap<String, ChildExpectation>,
    /// Every sub-playbook node must have a pin (a gated tree); without, a
    /// node with no pin is left out (an ungated start's best reading).
    pub require_pins: bool,
}

/// The run's effective policy over `tree` and the warnings a person sees
/// when a settings or floor file does not read and `tolerant` lets the
/// launch go on without it. Without `tolerant` such a file refuses
/// (`policy_file_invalid`, `launch_floor_invalid`).
pub fn effective(tree: Tree<'_>, tolerant: bool) -> Result<(EffectivePolicy, Vec<String>), Value> {
    effective_in(tree, tolerant, apb_core::config::config_dir().as_deref())
}

/// [`effective`] with every machine input (the machine `policy.yaml`, the
/// floor records, the projects registry that binds a workspace id, the
/// global store) read from the config dir `config_dir`.
fn effective_in(
    tree: Tree<'_>,
    tolerant: bool,
    config_dir: Option<&Path>,
) -> Result<(EffectivePolicy, Vec<String>), Value> {
    let mut warnings = Vec::new();
    let machine = config_dir.map(|d| d.join(apb_core::policy_file::POLICY_FILE));
    let settings =
        match apb_core::policy_file::Policies::load_with(Some(tree.root), machine.as_deref()) {
            Ok(p) => p,
            Err(e) if tolerant => {
                warnings.push(format!(
                    "{SETTINGS_INVALID}: {e}; launched without its launch settings"
                ));
                apb_core::policy_file::Policies::default()
            }
            Err(e) => {
                return Err(json!({
                    "policy": SETTINGS_INVALID,
                    "detail": format!("{e}; agent launches are refused until it is fixed"),
                }));
            }
        };
    let floors = config_dir.map(|d| d.join(floor::FLOORS_FILE));
    let records = match floor::read_from(floors.as_deref()) {
        Ok(r) => r,
        Err(e) if tolerant => {
            warnings.push(format!("{e}; launched without the launch floors"));
            BTreeMap::new()
        }
        Err(e) => {
            return Err(json!({
                "policy": floor::INVALID,
                "detail": format!("{e}; agent launches are refused until it is fixed"),
            }));
        }
    };
    let members = members(
        tree,
        &Machine {
            config_dir,
            records: &records,
        },
    )?;
    Ok((apb_core::launch::resolve(&members, &settings), warnings))
}

/// [`effective`] for `initiator`, refusing (`policy_file_invalid`) a run
/// step or a CLI without a terminal whose config dir was redirected away
/// from the default one ([`apb_core::config::default_config_dir`]) when the
/// default one's launch settings or floors hold the tree stricter: a
/// variable is not how a launch setting is changed. An eval case whose
/// config dir is the one its `apb eval` handed over is the one exception
/// (`handed_config_dir`).
pub fn effective_for(
    tree: Tree<'_>,
    tolerant: bool,
    initiator: &Initiator,
    handed_config_dir: Option<&Path>,
) -> Result<(EffectivePolicy, Vec<String>), Value> {
    let out = effective(tree, tolerant)?;
    let checked = initiator.checks_redirect();
    if !checked || !apb_core::config::config_dir_redirected() {
        return Ok(out);
    }
    let here = apb_core::config::config_dir();
    if handed_config_dir.is_some() && handed_config_dir == here.as_deref() {
        return Ok(out);
    }
    let default = apb_core::config::default_config_dir();
    let redirected = |detail: String| {
        json!({
            "policy": SETTINGS_INVALID,
            "detail": format!(
                "the config dir is redirected to `{}` (APB_CONFIG_DIR, XDG_CONFIG_HOME or HOME) away from `{}`, {detail}; a run step or a CLI without a terminal launches only under the default config dir's launch settings",
                here.as_deref().map(|p| p.display().to_string()).unwrap_or_default(),
                default.as_deref().map(|p| p.display().to_string()).unwrap_or_default(),
            ),
        })
    };
    match effective_in(tree, false, default.as_deref()) {
        Ok((strict, _)) if strict.policy > out.0.policy => Err(redirected(format!(
            "whose launch settings hold the run at `{}` ({})",
            strict.policy,
            strict.sources.join(", ")
        ))),
        Ok(_) => Ok(out),
        Err(e) => Err(redirected(format!(
            "whose launch settings do not read ({})",
            e["detail"].as_str().unwrap_or_default()
        ))),
    }
}

/// The machine inputs a member is read with.
struct Machine<'a> {
    config_dir: Option<&'a Path>,
    records: &'a BTreeMap<String, LaunchPolicy>,
}

/// One member per playbook of the tree: the parent and every pinned child,
/// recursively, each with its declaration and its own key's floor.
fn members(tree: Tree<'_>, records: &Machine<'_>) -> Result<Vec<Member>, Value> {
    let mut out = vec![member_of(
        tree.root,
        tree.playbook,
        matches!(tree.origin, Origin::Global),
        records,
    )];
    collect_children(
        tree.root,
        tree.playbook,
        tree.children,
        records,
        tree.require_pins,
        &mut out,
    )?;
    Ok(out)
}

fn collect_children(
    root: &Path,
    playbook: &Playbook,
    pins: &BTreeMap<String, ChildExpectation>,
    records: &Machine<'_>,
    require_pins: bool,
    out: &mut Vec<Member>,
) -> Result<(), Value> {
    for n in &playbook.nodes {
        if !matches!(n.kind, NodeKind::Playbook { .. }) {
            continue;
        }
        let Some(pin) = pins.get(&n.id) else {
            if !require_pins {
                continue;
            }
            return Err(json!({
                "policy": TREE_UNVERIFIED,
                "detail": format!("sub-playbook node `{}` has no verified pin; its launch policy cannot be read", n.id),
            }));
        };
        let Some(child) = pin.load(root) else {
            return Err(json!({
                "policy": TREE_UNVERIFIED,
                "detail": format!("sub-playbook `{}` {} (node `{}`) no longer loads at its pinned digest", pin.id, pin.version, n.id),
            }));
        };
        out.push(member_of(
            root,
            &child,
            matches!(pin.scope, ProfileScope::Global),
            records,
        ));
        collect_children(root, &child, &pin.children, records, require_pins, out)?;
    }
    Ok(())
}

/// The member of one playbook: its key follows its origin (the global
/// store's machine-wide key, or the workspace at `root` that owns a project
/// playbook), never the place it runs from.
fn member_of(root: &Path, playbook: &Playbook, global: bool, machine: &Machine<'_>) -> Member {
    let id = &playbook.id;
    let (key, dir) = if global {
        (
            Some(FloorKey::global(id)),
            machine.config_dir.map(|p| p.join("playbooks").join(id)),
        )
    } else {
        (
            FloorKey::read_project_in(machine.config_dir, root, id),
            Some(root.join(".apb/playbooks").join(id)),
        )
    };
    let floor = match key
        .as_ref()
        .and_then(|k| machine.records.get(&k.as_string()))
    {
        Some(p) => Some(*p),
        None => dir.as_deref().and_then(floor::store_scan),
    };
    Member {
        id: id.clone(),
        global,
        workspace_id: match &key {
            Some(FloorKey::Project { workspace_id, .. }) => Some(workspace_id.clone()),
            _ => None,
        },
        declared: playbook.agent_launch,
        floor,
    }
}

/// The launch floor of `playbook` alone (its record, else its store's
/// scan), read without writing anything; `None` when nothing holds it.
pub fn own_floor(root: &Path, playbook: &Playbook, origin: &Origin) -> Option<LaunchPolicy> {
    let records = floor::read().unwrap_or_default();
    let config_dir = apb_core::config::config_dir();
    let machine = Machine {
        config_dir: config_dir.as_deref(),
        records: &records,
    };
    member_of(root, playbook, matches!(origin, Origin::Global), &machine).floor
}

/// The canonical digest of everything a permit authorizes.
pub fn permit_digest(
    playbook_digest: &str,
    profile_bundles: &BTreeMap<String, String>,
    connectors: &BTreeMap<String, String>,
    connector_accounts: &BTreeMap<String, String>,
    children: &BTreeMap<String, ChildExpectation>,
) -> String {
    #[derive(Serialize)]
    struct Canonical<'a> {
        playbook: &'a str,
        profile_bundles: &'a BTreeMap<String, String>,
        connectors: &'a BTreeMap<String, String>,
        connector_accounts: &'a BTreeMap<String, String>,
        children: &'a BTreeMap<String, ChildExpectation>,
    }
    let body = serde_json::to_string(&Canonical {
        playbook: playbook_digest,
        profile_bundles,
        connectors,
        connector_accounts,
        children,
    })
    .unwrap_or_default();
    apb_core::content::short_nonce("permit", &[body])
}

/// The digest of a tree's verified child pins, what a definition-bound
/// handoff carries.
pub fn children_digest(children: &BTreeMap<String, ChildExpectation>) -> String {
    apb_core::content::short_nonce(
        "children",
        &[serde_json::to_string(children).unwrap_or_default()],
    )
}

/// The digest of the policy state a decision was made on.
pub fn policy_state(effective: &EffectivePolicy) -> String {
    let mut lines = vec![effective.policy.as_str().to_string()];
    lines.extend(effective.sources.iter().cloned());
    apb_core::content::short_nonce("launch-state", &lines)
}

/// What a launch is about, for its nonce and its refusal.
#[derive(Debug, Clone)]
pub struct Subject<'a> {
    pub root: &'a Path,
    pub id: &'a str,
    pub global: bool,
    pub version: &'a str,
    pub choice: Choice,
    /// The trust digest of the definition that runs.
    pub definition_digest: &'a str,
    pub permit_digest: String,
    /// [`children_digest`] of the verified child pins.
    pub children_digest: String,
}

/// The nonce a confirmation echoes.
pub fn nonce(
    subject: &Subject<'_>,
    effective: &EffectivePolicy,
    kind: &LaunchKind,
    billing: &BillingCheck,
) -> String {
    let workspace = apb_core::workspace::read_id(subject.root).unwrap_or_else(|| {
        std::fs::canonicalize(subject.root)
            .unwrap_or_else(|_| subject.root.to_path_buf())
            .display()
            .to_string()
    });
    let mut sources = effective.sources.clone();
    sources.sort();
    let mut lines = vec![
        workspace,
        format!(
            "{}:{}",
            if subject.global { "global" } else { "project" },
            subject.id
        ),
        subject.version.to_string(),
        subject.choice.as_str().to_string(),
        subject.permit_digest.clone(),
        effective.policy.as_str().to_string(),
        sources.join("\n"),
        kind.label().to_string(),
        kind.run_id().unwrap_or_default().to_string(),
    ];
    lines.extend(billing.nonce_lines());
    apb_core::content::short_nonce("launch", &lines)
}

/// The run step a new start is made from ([`crate::step_start::resolve`]),
/// or its refusal `start_chain_too_deep`. Only a run step's new start (a
/// start, a trial, an executed plan, a continue) names one: a resume keeps
/// the record its start wrote and is never refused for its chain.
fn started_by(
    ctx: &LaunchContext,
    root: &Path,
) -> Result<Option<apb_core::launch::StartedBy>, Value> {
    let new_start = matches!(
        ctx.kind,
        LaunchKind::Start
            | LaunchKind::Trial
            | LaunchKind::ExecutePlan
            | LaunchKind::ContinueFrom { .. }
    );
    if !new_start || !matches!(ctx.initiator, Initiator::RunStep { .. }) {
        return Ok(None);
    }
    crate::step_start::resolve(root, &crate::step_start::CallerSnapshot::from_env())
}

/// The launch check: the grant, or the structured refusal with every hint
/// a retry needs. The launch policy decides first; then the billing check
/// ([`super::billing`]): `deny` refuses everyone, `ask` needs a person's
/// confirmation of the exact predicted set, which the same nonce covers.
pub fn decide(
    ctx: &LaunchContext,
    subject: &Subject<'_>,
    effective: &EffectivePolicy,
    billing: &BillingCheck,
    tolerant: bool,
) -> Result<LaunchGrant, Value> {
    if billing.required() == LaunchPolicy::Deny {
        return Err(billing_refusal(
            super::billing::BILLING_DENIED,
            ctx,
            subject,
            effective,
            billing,
            None,
        ));
    }
    let started_by = started_by(ctx, subject.root)?;
    let expected = nonce(subject, effective, &ctx.kind, billing);
    let grant = |confirmation: Option<String>, billed: Option<String>| LaunchGrant {
        record: LaunchRecord {
            kind: ctx.kind.label().to_string(),
            initiator: ctx.initiator.label(),
            confirmation,
            policy: effective.policy,
            sources: effective.sources.clone(),
            billing: Some(billing.record(billed)),
            started_by: started_by.clone(),
        },
        permit_digest: Some(subject.permit_digest.clone()),
        policy_state: Some(policy_state(effective)),
        tolerant,
        run_id: ctx.kind.run_id().map(str::to_string),
        pins: None,
        nonce: Some(expected.clone()),
        version: Some(subject.version.to_string()),
        choice: Some(subject.choice),
        challenge: None,
    };
    let nonce_matches = |n: &str| apb_core::secret::ct_eq_str(n, &expected);
    // Billed use a person must confirm covers one launch: the nonce handed
    // out is a fresh challenge ([`super::challenge`]), and a confirmation
    // echoing it on a channel that may confirm is accepted while it is open.
    // It is consumed only where the start commits (the grant's claim), so a
    // check that refuses for another reason leaves it open. Without billing
    // to confirm the nonce is the launch's own, as it always was.
    let single_use = billing.required() == LaunchPolicy::Ask;
    let echoed = (single_use && echoes_on_its_channel(ctx))
        .then(|| confirmation_nonce(ctx))
        .flatten();
    let claim = echoed
        .filter(|n| super::challenge::is_open(subject.root, n, &expected))
        .map(|n| super::challenge::Claim {
            root: subject.root.to_path_buf(),
            token: n.to_string(),
        });
    let open = claim.is_some();
    let accepted = |n: &str| {
        if single_use { open } else { nonce_matches(n) }
    };
    let offer = || {
        if single_use {
            super::challenge::issue(subject.root, &expected)
        } else {
            expected.clone()
        }
    };
    // A forwarded grant stands for the decision another process made: the
    // run keeps that process's record (its initiator and confirmation),
    // with the policy state rechecked here.
    if let Some(LaunchConfirmation::Forwarded { handoff }) = &ctx.confirmation {
        let (matches, billing_ok) = match &handoff.binding {
            // The nonce covers the predicted set whenever it needed a
            // confirmation.
            HandoffBinding::Nonce { nonce } => (nonce_matches(nonce), true),
            HandoffBinding::Definition {
                id,
                version,
                digest,
                children,
            } => (
                id == subject.id
                    && version == subject.version
                    && digest == subject.definition_digest
                    && apb_core::secret::ct_eq_str(children, &subject.children_digest)
                    // Decided for the policy then in force: a tree that
                    // became stricter since is decided afresh.
                    && effective.policy <= handoff.record.policy,
                billing.required() == LaunchPolicy::Allow
                    || handoff
                        .record
                        .billing
                        .as_ref()
                        .is_some_and(|r| billing.covered_by(r)),
            ),
        };
        if matches && billing_ok && !matches!(ctx.initiator, Initiator::RunStep { .. }) {
            let mut g = grant(None, None);
            g.record = handoff.record.clone();
            if g.record.billing.is_none() {
                g.record.billing = Some(billing.record(None));
            }
            return Ok(g);
        }
    }
    let launch =
        launch_confirmation(ctx, subject, effective, &accepted, &offer).map_err(|mut v| {
            // One question to the person covers both: the nonce already covers
            // the predicted set.
            if billing.required() != LaunchPolicy::Allow {
                v["billing"] = billing.block();
            }
            v
        })?;
    if billing.required() == LaunchPolicy::Allow {
        return Ok(grant(launch, None));
    }
    let ask_refusal = |code: &str, with_nonce: bool| {
        let offered = with_nonce.then(offer);
        Err(billing_refusal(
            code,
            ctx,
            subject,
            effective,
            billing,
            offered.as_deref(),
        ))
    };
    let billed = match billing_confirmation(ctx, &accepted) {
        Ok(label) => label,
        Err(code) => return ask_refusal(code, code == super::billing::BILLING_ASK),
    };
    let mut g = grant(launch, Some(billed.to_string()));
    g.challenge = claim;
    Ok(g)
}

/// Whether the caller passes a nonce back on the one channel its
/// initiator may confirm on (a run step and the engine never do), so a
/// nonce echoed anywhere else never consumes a challenge.
fn echoes_on_its_channel(ctx: &LaunchContext) -> bool {
    matches!(
        (&ctx.initiator, &ctx.confirmation),
        (
            Initiator::CliUnattested { .. },
            Some(LaunchConfirmation::CliFlag { .. })
        ) | (
            Initiator::AgentMcp { .. },
            Some(LaunchConfirmation::HostRelayed { .. })
        ) | (
            Initiator::Dashboard | Initiator::DashboardKeyless,
            Some(LaunchConfirmation::DashboardNonce { .. })
        )
    )
}

/// The nonce a confirmation passes back, if it is one that does.
fn confirmation_nonce(ctx: &LaunchContext) -> Option<&str> {
    match &ctx.confirmation {
        Some(
            LaunchConfirmation::CliFlag { nonce }
            | LaunchConfirmation::HostRelayed { nonce }
            | LaunchConfirmation::DashboardNonce { nonce },
        ) => Some(nonce),
        _ => None,
    }
}

/// How a person confirmed billed use, or the billing refusal code: a run
/// step and the engine never confirm it, a terminal question does at a
/// terminal, and otherwise only a nonce `nonce_ok` accepts, passed back on
/// the caller's own channel.
pub(crate) fn billing_confirmation(
    ctx: &LaunchContext,
    nonce_ok: &dyn Fn(&str) -> bool,
) -> Result<&'static str, &'static str> {
    let nonce_matches = |n: &str| nonce_ok(n);
    match (&ctx.initiator, &ctx.confirmation) {
        (Initiator::RunStep { .. }, _) => Err(super::billing::BILLING_STEP),
        (Initiator::Engine { .. }, _) => Err(super::billing::BILLING_ENGINE),
        (Initiator::CliUnattested { terminal: true }, Some(LaunchConfirmation::TerminalPrompt)) => {
            Ok("terminal_prompt")
        }
        (Initiator::CliUnattested { .. }, Some(LaunchConfirmation::CliFlag { nonce }))
            if nonce_matches(nonce) =>
        {
            Ok("cli_flag")
        }
        (Initiator::AgentMcp { .. }, Some(LaunchConfirmation::HostRelayed { nonce }))
            if nonce_matches(nonce) =>
        {
            Ok("host_relayed")
        }
        (
            Initiator::Dashboard | Initiator::DashboardKeyless,
            Some(LaunchConfirmation::DashboardNonce { nonce }),
        ) if nonce_matches(nonce) => Ok("dashboard_nonce"),
        _ => Err(super::billing::BILLING_ASK),
    }
}

/// The launch policy's half of [`decide`]: how the launch was confirmed
/// (`None` for an `allow` launch), or its refusal.
fn launch_confirmation(
    ctx: &LaunchContext,
    subject: &Subject<'_>,
    effective: &EffectivePolicy,
    nonce_ok: &dyn Fn(&str) -> bool,
    offer: &dyn Fn() -> String,
) -> Result<Option<String>, Value> {
    if effective.policy == LaunchPolicy::Allow {
        return Ok(None);
    }
    let nonce_matches = |n: &str| nonce_ok(n);
    let ask = effective.policy == LaunchPolicy::Ask;
    let confirmed_by = |label: &str| Ok(Some(label.to_string()));
    let refuse = |code: &str, with_nonce: bool| {
        let offered = with_nonce.then(offer);
        Err(refusal(code, ctx, subject, effective, offered.as_deref()))
    };
    match &ctx.initiator {
        Initiator::Dashboard => confirmed_by("dashboard"),
        Initiator::Engine { .. } => refuse(ENGINE_POLICY, false),
        Initiator::RunStep { .. } => refuse(STEP_POLICY, false),
        Initiator::AgentMcp { .. } => match &ctx.confirmation {
            Some(LaunchConfirmation::HostRelayed { nonce }) if ask && nonce_matches(nonce) => {
                confirmed_by("host_relayed")
            }
            _ if ask => refuse(ASK_POLICY, true),
            _ => refuse(DENY_POLICY, false),
        },
        Initiator::CliUnattested { terminal } => match &ctx.confirmation {
            Some(LaunchConfirmation::TerminalPrompt) if *terminal => {
                confirmed_by("terminal_prompt")
            }
            Some(LaunchConfirmation::CliFlag { nonce }) if ask && nonce_matches(nonce) => {
                confirmed_by("cli_flag")
            }
            _ if ask => refuse(ASK_POLICY, true),
            _ => refuse(DENY_POLICY, false),
        },
        Initiator::DashboardKeyless => match &ctx.confirmation {
            Some(LaunchConfirmation::DashboardNonce { nonce }) if ask && nonce_matches(nonce) => {
                confirmed_by("dashboard_nonce")
            }
            _ if ask => refuse(ASK_POLICY, true),
            _ => refuse(DENY_POLICY, false),
        },
    }
}

/// A billing refusal with the launch fields of `subject` a retry needs.
fn billing_refusal(
    code: &str,
    ctx: &LaunchContext,
    subject: &Subject<'_>,
    effective: &EffectivePolicy,
    billing: &BillingCheck,
    nonce: Option<&str>,
) -> Value {
    let mut v = super::billing::refusal(code, billing, nonce);
    v["id"] = json!(subject.id);
    v["launch_policy"] = json!(effective.policy);
    v["launch_sources"] = json!(effective.sources);
    v["launch_version"] = json!(subject.version);
    v["launch_choice"] = json!(subject.choice.as_str());
    v["launch_kind"] = json!(ctx.kind.label());
    if let Some(run) = ctx.kind.run_id() {
        v["run_id"] = json!(run);
    }
    v
}

/// The structured refusal of a launch.
fn refusal(
    policy: &str,
    ctx: &LaunchContext,
    subject: &Subject<'_>,
    effective: &EffectivePolicy,
    nonce: Option<&str>,
) -> Value {
    let detail = match policy {
        ASK_POLICY => format!(
            "playbook `{}` asks a person before an agent launches it (agent_launch `{}`: {}); ask the person, then call again with confirm_launch set to the launch_nonce and the same launch_version and launch_choice (CLI: --confirm-launch)",
            subject.id,
            effective.policy,
            effective.sources.join(", ")
        ),
        DENY_POLICY => format!(
            "only a person launches playbook `{}` (agent_launch `{}`: {}); a person starts it from a terminal, or from a dashboard signed in with a key",
            subject.id,
            effective.policy,
            effective.sources.join(", ")
        ),
        STEP_POLICY => format!(
            "this launch comes from inside a run step, and playbook `{}` is `{}` for agents ({}); a run step cannot confirm a launch. A person starts it from the dashboard or from a terminal",
            subject.id,
            effective.policy,
            effective.sources.join(", ")
        ),
        _ => format!(
            "the engine does not resume a run of playbook `{}` that is `{}` for agents ({}) and was never authorized; a person resumes it",
            subject.id,
            effective.policy,
            effective.sources.join(", ")
        ),
    };
    let mut v = json!({
        "policy": policy,
        "id": subject.id,
        "launch_policy": effective.policy,
        "launch_sources": effective.sources,
        "launch_version": subject.version,
        "launch_choice": subject.choice.as_str(),
        "launch_kind": ctx.kind.label(),
        "detail": detail,
    });
    if let Some(n) = nonce {
        v["launch_nonce"] = json!(n);
    } else {
        v["launch_by_person"] = json!(true);
    }
    if let Some(run) = ctx.kind.run_id() {
        v["run_id"] = json!(run);
    }
    v
}

/// The launch fields of a refusal `from`, copied onto `into`: a trust
/// refusal of a tree that also needs a launch confirmation names both, so
/// one question to the person covers both answers.
pub(crate) fn merge_hint(into: &mut Value, from: &Value) {
    for k in [
        "billing",
        "launch_policy",
        "launch_sources",
        "launch_version",
        "launch_choice",
        "launch_kind",
        "launch_nonce",
        "launch_by_person",
    ] {
        if let Some(v) = from.get(k) {
            into[k] = v.clone();
        }
    }
}

/// The authorization run `run_dir` holds: its manifest's record, or the
/// latest a resume journaled; `None` for a run never authorized (started by
/// an older apb).
pub fn run_authorization(run_dir: &Path) -> Option<LaunchRecord> {
    let resumed = crate::event::read_all(run_dir).ok().and_then(|events| {
        events.iter().rev().find_map(|e| match &e.payload {
            crate::event::EventPayload::RunResumed {
                launch: Some(r), ..
            } => Some(r.clone()),
            _ => None,
        })
    });
    resumed.or_else(|| {
        crate::manifest::read(run_dir)
            .ok()
            .flatten()
            .and_then(|m| m.launch)
    })
}

/// A refusal as one line: `<policy>: <detail>`.
pub fn refusal_line(v: &Value) -> String {
    format!(
        "{}: {}",
        v["policy"].as_str().unwrap_or("launch_refused"),
        v["detail"].as_str().unwrap_or_default()
    )
}

/// Whether the child run `child_run` may be continued under its parent: it
/// holds its own authorization, its parent holds none (an unchecked run,
/// left as it always was), or its snapshot is exactly the parent's pin for
/// the node. A parent's record alone never covers an unverified child.
pub fn child_covered(
    root: &Path,
    parent_dir: &Path,
    pin: Option<&ChildExpectation>,
    child_run: &str,
) -> Result<(), String> {
    let child_dir = root.join(".apb/runs").join(child_run);
    if run_authorization(&child_dir).is_some() || run_authorization(parent_dir).is_none() {
        return Ok(());
    }
    let digest = std::fs::read_to_string(child_dir.join("playbook.yaml"))
        .ok()
        .and_then(|y| apb_core::scope::definition_digest(&y, &child_dir).ok());
    match (pin, digest) {
        (Some(p), Some(d)) if p.playbook_digest == d => Ok(()),
        _ => Err(format!(
            "{CHILD_NOT_COVERED}: child run `{child_run}` was not started from its verified pin, so its parent's authorization does not cover it; start the playbook again"
        )),
    }
}
