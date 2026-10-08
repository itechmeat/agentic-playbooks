//! Deserializable argument DTOs for the MCP tools (schemas via JsonSchema).
//! These are pure input types; the handler logic lives in the `server` module
//! and delegates to `crate::tools` / `profile_tools` / `advisory_tools`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookIdArgs {
    pub id: String,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookGetArgs {
    pub id: String,
    /// Playbook version (defaults to the latest).
    pub version: Option<String>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
    /// "summary" (default) returns the compact interface without prompt
    /// bodies; "full" returns the complete authoring payload (yaml + full
    /// playbook + layout). Any other value falls back to summary.
    #[serde(default)]
    pub detail: Option<String>,
}

/// An argument with only an optional workspace - for read tools without any
/// other parameters (playbook_list, runs_list).
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WorkspaceArg {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookRunArgs {
    /// Playbook identifier.
    pub id: String,
    /// Playbook version (defaults to the latest).
    pub version: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Free-form instruction for nodes that expect one.
    pub instruction: Option<String>,
    /// supervise: "self" - start it in the background under the calling
    /// session's supervision (without waiting for completion) and return a
    /// token for the supervisor tools.
    pub supervise: Option<String>,
    /// background: true - start the run in the background (autonomous) and
    /// return the run_id immediately, without waiting for completion. For
    /// clients with a short tool-call timeout; poll status via
    /// run_status/run_events.
    #[serde(default)]
    pub background: Option<bool>,
    /// acknowledge_untrusted: true - the user's confirmation to run a
    /// playbook with an unapproved digest (spec 9). Trust only; consent to
    /// irreversible effects is `confirm_irreversible`. Pass it only after
    /// asking the person. Without it an untrusted playbook is refused.
    #[serde(default)]
    pub acknowledge_untrusted: Option<bool>,
    /// confirm_irreversible - the person's consent to a playbook whose
    /// effects include irreversible (push, merge, deploy, publish; 0.24.0,
    /// recorded in the run manifest). Pass the `consent_nonce` of the refusal
    /// you showed the person; a playbook that changed since then is refused
    /// again with a new nonce. A bare `true` is accepted for one release with
    /// a deprecation note.
    #[serde(default)]
    pub confirm_irreversible: Option<ConfirmIrreversibleArg>,
    /// Definition scope: "project" (default) or "global". A global playbook
    /// runs in the current project (spec 5.1).
    #[serde(default)]
    pub scope: Option<String>,
    /// Run id to continue as a fresh run (issue #42 finding 10). Links the new
    /// run to the predecessor in runs_list/run_status.
    #[serde(default)]
    pub continued_from: Option<String>,
    /// The run's working tree: a directory (absolute, or relative to the
    /// project) that agent and script nodes run in and whose busy lock the
    /// run takes, so runs over different git worktrees do not wait on each
    /// other. Overrides the playbook's `worktree`. Omit to let the playbook
    /// decide (the project root when it declares none).
    #[serde(default)]
    pub worktree: Option<String>,
    // --- host execution mode (0.23.0) ---
    /// "host" or "cli" (default "cli"). Pass "host" ONLY when the person
    /// asked for mono, host or single-agent mode, or for the run to use your
    /// own subagents: apb then spawns no agent CLI, and every agent step
    /// becomes a host task YOU execute in this session with your own
    /// subagent tool, never through an agent CLI (see run_wait and
    /// run_task_submit).
    /// A host-mode run always starts in the background.
    #[serde(default)]
    pub execution: Option<String>,
    // --- end host execution mode ---
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookWriteArgs {
    pub id: String,
    pub yaml: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrustListArgs {
    /// Only approvals of this kind: `playbook`, `profile_bundle`,
    /// `connector` or `connector_account`.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrustRevokeArgs {
    /// A digest (`sha256:...`, exactly that approval) or an id (every
    /// approval recorded under it, e.g. every version of a playbook).
    pub target: String,
    /// With an id: only approvals of this kind (`playbook`, `profile_bundle`,
    /// `connector`, `connector_account`).
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrashRestoreArgs {
    /// A trash entry name (`<id>-<deleted_at_ms>`, from playbook_trash_list)
    /// or a playbook id, which restores that id's latest deletion.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunRefArgs {
    pub run_id: String,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunWaitArgs {
    pub run_id: String,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
    /// How many milliseconds to block at most (default 50000, max 1800000).
    /// Pass the largest value your host's tool-call timeout allows: each
    /// return costs a model turn.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProgressReportArgs {
    pub run_id: String,
    /// Iterations completed in the current cycle group.
    pub done: u64,
    /// Total iterations planned for the current cycle group.
    pub total: u64,
    /// Optional human label shown next to the bar, e.g. "chapter 3 of 14".
    #[serde(default)]
    pub label: Option<String>,
    /// The node id this report belongs to. An executing agent should pass its
    /// own, which it has in APB_NODE_ID; omitting it keeps the old
    /// best-effort attribution to whatever node the drive is currently
    /// holding.
    #[serde(default)]
    pub node: Option<String>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunEventsArgs {
    pub run_id: String,
    /// Return events starting from this seq (inclusive).
    pub from_seq: Option<u64>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunRetroContextArgs {
    pub run_id: String,
    /// Compare with the median of this many earlier finished runs of the same
    /// playbook version (default 10, at most 100, 0 for no comparison).
    #[serde(default)]
    pub compare_last: Option<usize>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunResumeArgs {
    pub run_id: String,
    /// Node to resume from (determined automatically by default).
    pub from_node: Option<String>,
    /// Continue despite environment drift: an agent executable recorded in the
    /// run manifest changed on disk since the run started. By default resume
    /// refuses this and returns the drift error inline; set true to override
    /// (the accepted drift is recorded as an event in the run log).
    #[serde(default)]
    pub allow_environment_drift: bool,
    /// Resume a run whose playbook snapshot (YAML plus scripts) is not an
    /// approved digest. Pass true only after confirming with the user, as for
    /// `playbook_run`.
    #[serde(default)]
    pub acknowledge_untrusted: Option<bool>,
    /// The person's consent to the run's irreversible effects, needed once
    /// for a run that has no valid consent recorded (one started by an
    /// older apb): the refusal's `consent_nonce`, or (deprecated) `true`.
    #[serde(default)]
    pub confirm_irreversible: Option<ConfirmIrreversibleArg>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewDecideArgs {
    pub run_id: String,
    /// Identifier of the human_review node.
    pub node: String,
    pub decision: String,
    #[serde(default)]
    pub note: String,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Answers a pending interactive question on a run (spec
/// 2026-07-20-interactive-nodes). Exactly one of `run_id`/`token` identifies
/// the run: `run_id` is the plain/operator path (`answered_by: "human"`),
/// `token` is the supervisor-session path (`answered_by: "supervisor"`) -
/// `crate::server::capability_for_tool` maps this tool to the `observe`
/// capability. `node` should normally be omitted (there is usually exactly
/// one pending question) or copied verbatim from `run_status`'s
/// `pending_question.node`: an explicit `node` is not checked against the
/// pending channel, so a typo'd name silently appends an orphaned answer
/// instead of erroring.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunAnswerArgs {
    /// Run to answer (human/operator path). Provide this OR `token`.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Supervisor session token (supervisor path). Provide this OR `run_id`.
    #[serde(default)]
    pub token: Option<String>,
    /// The interactive node; omit when exactly one question is pending.
    #[serde(default)]
    pub node: Option<String>,
    pub answer: String,
    /// workspace_id of another workspace (spec 7). Only meaningful with
    /// `run_id`; the supervisor-token path resolves its own run and ignores
    /// this. None - the current workspace.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorWaitArgs {
    /// Supervisor session token, issued on start with supervise: "self".
    pub token: String,
    /// Return wakes starting from this seq (excluding ones already seen):
    /// pass the previous answer's `next_after_seq`.
    pub after_seq: Option<u64>,
    /// How many milliseconds to block for the next wake
    /// (default 50000, max 1800000). Longer is cheaper: each return is a
    /// model turn.
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorInspectArgs {
    pub token: String,
    /// Keep the long texts inside `events` verbatim. Off by default: they are
    /// the node outputs and wake details already present in `outputs`,
    /// `context` and `wakes`.
    #[serde(default)]
    pub full_events: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorRunRefArgs {
    pub token: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorRetryArgs {
    pub token: String,
    pub node: String,
    /// Substitute prompt for the retry attempt (defaults to the original).
    pub prompt_override: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorContinueArgs {
    pub token: String,
    /// Node to resume the run from, skipping the failed node.
    pub node: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorContextArgs {
    pub token: String,
    pub note: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorInterruptArgs {
    pub token: String,
    /// Why the running attempt is being interrupted, recorded verbatim in the
    /// journaled `attempt_interrupted` event. Defaults to a generic reason.
    #[serde(default)]
    pub reason: Option<String>,
    /// The node whose running attempt to interrupt. Omit to interrupt EVERY
    /// attempt currently running in the run (the historical broadcast).
    #[serde(default)]
    pub node: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorReportArgs {
    pub token: String,
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorPatchArgs {
    pub token: String,
    /// Full YAML of the patched playbook (will become the patch version).
    pub yaml: String,
    /// Classification of the fix: `improvement` or `workaround` (see 10.5).
    pub classification: String,
    /// Node the run will resume from after the migration.
    pub continue_from: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorRebindArgs {
    pub token: String,
    /// Node whose executor profile should be rebound. Must be a profile-bound
    /// node (agent_task or finish-with-prompt).
    pub node: String,
    /// Name of the new profile to bind the node to.
    pub profile: String,
    /// Scope of the new profile: "project", "global", or "auto" (default). Auto
    /// resolves it the same way a node profile reference does - the run's origin
    /// first, then global.
    #[serde(default)]
    pub scope: Option<String>,
    /// Acknowledge an untrusted profile bundle, mirroring run start. Required
    /// (after user confirmation) when the new bundle is not approved; otherwise
    /// the rebind is refused with `untrusted_profile_requires_acknowledge`.
    #[serde(default)]
    pub acknowledge_untrusted: bool,
    /// Optional note recorded verbatim in the journaled `profile_rebound` event.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookCatalogArgs {
    /// Catalog revision known to the client. If it matches the current one,
    /// the body is not returned (response `{ unchanged: true }`).
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
    /// The task in one sentence (optional). Only when the machine enabled
    /// decision-model catalog ranking: adds an advisory `ranked` list,
    /// `needs_playbook_p` and `covered_by`; otherwise ignored.
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookCaptureArgs {
    /// Synopsis of the action: title, steps, params, trigger. Free-form
    /// structure; secret values must not be put here (spec 8.3).
    pub synopsis: serde_json::Value,
    /// Scope chosen by the user: "project" or "global" (not a
    /// recommendation).
    pub selected_scope: String,
    /// YAML of the new playbook (v1 - the agent writes it itself via
    /// playbook_howto).
    pub yaml: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SuggestionDismissArgs {
    /// English kebab-slug identifying the suggestion, lowercase [a-z0-9-]
    /// starting with a letter or digit, at most 64 chars (anything else is
    /// rejected: the record has to stay addressable by CLI and dashboard). A
    /// stable record id, not the matching key: matching is done by the
    /// synopsis below.
    pub pattern: String,
    /// Legacy hard-TTL override in days; defaults to 90. Applies to a hard
    /// dismissal only.
    #[serde(default)]
    pub ttl_days: Option<u64>,
    /// "soft" for "not now" (the snooze escalates with every repeat) or
    /// "hard" for an explicit never-again. Absent means hard, so an old-style
    /// call keeps its old meaning.
    #[serde(default)]
    pub kind: Option<String>,
    /// One English sentence describing the action that was offered. Strongly
    /// recommended: this is what a future session compares a candidate action
    /// against, by meaning. Never put secret values here.
    #[serde(default)]
    pub synopsis: String,
    /// "project" (default) or "global". Use global only when the user's own
    /// wording says everywhere.
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookTrialArgs {
    pub id: String,
    pub version: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Free-form instruction for nodes that expect one, exactly like
    /// `playbook_run`'s `instruction` (rendered as `{{run.instruction}}`).
    #[serde(default)]
    pub instruction: Option<String>,
    /// Definition scope: "project" (default) or "global".
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookApproveArgs {
    pub id: String,
    pub version: Option<String>,
    /// Definition scope: "project" (default) or "global".
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProfileListArgs {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProfileGetArgs {
    pub name: String,
    /// Scope: "project" (default) or "global".
    #[serde(default = "default_project_scope")]
    pub scope: String,
}

fn default_project_scope() -> String {
    "project".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentsDetectArgs {
    /// Ignore the cache and probe agents again.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AdoptReportArgs {
    /// A specific playbook; None - all playbooks of the project.
    #[serde(default)]
    pub id: Option<String>,
}

/// One declared subscription (for subscriptions_set).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SubscriptionArg {
    pub agent: String,
    #[serde(default)]
    pub plan: Option<String>,
    /// "full" | "partial" | "unknown" (default unknown).
    #[serde(default)]
    pub coverage: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SubscriptionsSetArgs {
    #[serde(default)]
    pub subscriptions: Vec<SubscriptionArg>,
    /// The user declined the survey - do not offer it again.
    #[serde(default)]
    pub declined: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProfileFallbackInput {
    pub agent: String,
    pub model: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProfileWriteArgs {
    pub name: String,
    #[serde(default = "default_project_scope")]
    pub scope: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub soul_md: String,
    #[serde(default)]
    pub skills: Vec<String>,
    pub agent: String,
    pub model: String,
    #[serde(default)]
    pub fallbacks: Vec<ProfileFallbackInput>,
    /// SOUL requirement: "any" (default) or "native_required".
    #[serde(default)]
    pub soul: Option<String>,
    /// For updating an existing profile - its current profile_digest
    /// (optimistic concurrency). Absence means creating a new one.
    #[serde(default)]
    pub expected_digest: Option<String>,
    /// Agent environment: "minimal" (the default: apb's own settings, no user
    /// plugins, MCP servers, skills or CLAUDE.md; the project's own ones and
    /// the profile's skills still load) or "full" (the operator's whole
    /// personal setup, for a profile that depends on a user-scope plugin,
    /// skill or MCP server). Absent: an update keeps the stored value.
    #[serde(default)]
    pub environment: Option<String>,
    /// Deprecated spelling of `environment`: true = minimal, false = full.
    #[serde(default)]
    pub hermetic: Option<bool>,
    /// ZCode permission mode for the profile's zcode steps in a run that grants
    /// autonomy: "yolo" (default: files, shell, network) or "edit" (file edits
    /// only, no shell commands). Absent: an update keeps the stored value.
    #[serde(default)]
    pub zcode_mode: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProfileMoveArgs {
    pub name: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProfileDeleteArgs {
    pub name: String,
    #[serde(default = "default_project_scope")]
    pub scope: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookPrepareRunArgs {
    pub id: String,
    pub version: Option<String>,
    /// workspace_id of the target workspace (required - a contract for
    /// another workspace).
    pub workspace: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookExecutePlanArgs {
    /// Signed plan_token from playbook_prepare_run.
    pub plan_token: String,
    /// The user's confirmation to run an unapproved (untrusted) playbook in
    /// another workspace (spec 9). Trust only. Without it such a plan is
    /// refused.
    #[serde(default)]
    pub acknowledge_untrusted: Option<bool>,
    /// The person's consent to irreversible effects in the plan (0.24.0):
    /// the `consent_nonce` of the refusal you showed them, or (deprecated
    /// for one release) `true`.
    #[serde(default)]
    pub confirm_irreversible: Option<ConfirmIrreversibleArg>,
}

/// `confirm_irreversible`: the refusal's `consent_nonce` (a string), or a
/// bare boolean (`true` is deprecated for one release).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ConfirmIrreversibleArg {
    Flag(bool),
    Nonce(String),
}

impl From<ConfirmIrreversibleArg> for apb_engine::consent::Confirmation {
    fn from(a: ConfirmIrreversibleArg) -> Self {
        match a {
            ConfirmIrreversibleArg::Flag(b) => Self::Flag(b),
            ConfirmIrreversibleArg::Nonce(n) => Self::Nonce(n),
        }
    }
}

// --- host execution mode (0.23.0) ---
/// Token usage a host reports for a task (all optional counts).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskUsageArg {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
    #[serde(default)]
    pub cache_write_tokens: Option<u64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunTaskSubmitArgs {
    /// The run the task belongs to.
    pub run_id: String,
    /// The task id from `pending_tasks`, verbatim.
    pub task_id: String,
    /// "succeeded" (the subagent did the task), "failed" (it could not; the
    /// normal retry policy applies) or "blocked" (it needs the person: put
    /// the question in `output`; the run then waits for run_answer).
    pub status: String,
    /// The subagent's final reply, verbatim, including its closing yaml
    /// status block. For "blocked": the question for the person.
    pub output: String,
    /// Token usage the subagent consumed, when your host reports it.
    #[serde(default)]
    pub usage: Option<TaskUsageArg>,
    /// A short note for the journal (optional).
    #[serde(default)]
    pub note: Option<String>,
    /// The model the subagent actually ran on, when you know it (optional).
    /// Journaled as reported; `model_hint` is only the profile's declaration.
    #[serde(default)]
    pub model: Option<String>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}
// --- end host execution mode ---
