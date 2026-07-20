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
    /// playbook with an unapproved digest (spec 9). Without it an untrusted
    /// playbook is refused by policy.
    #[serde(default)]
    pub acknowledge_untrusted: Option<bool>,
    /// Definition scope: "project" (default) or "global". A global playbook
    /// runs in the current project (spec 5.1).
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaybookWriteArgs {
    pub id: String,
    pub yaml: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunRefArgs {
    pub run_id: String,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
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
pub struct RunResumeArgs {
    pub run_id: String,
    /// Node to resume from (determined automatically by default).
    pub from_node: Option<String>,
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupervisorWaitArgs {
    /// Supervisor session token, issued on start with supervise: "self".
    pub token: String,
    /// Return wakes starting from this seq (excluding ones already seen).
    pub after_seq: Option<u64>,
    /// How many milliseconds to block waiting for the next wake.
    pub timeout_ms: Option<u64>,
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
    /// English kebab-slug of the suggestion pattern that should not be
    /// repeated.
    pub pattern: String,
    /// TTL in days; defaults to 90.
    #[serde(default)]
    pub ttl_days: Option<u64>,
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
    /// another workspace (spec 9). Without it an untrusted plan is refused.
    #[serde(default)]
    pub acknowledge_untrusted: Option<bool>,
}
