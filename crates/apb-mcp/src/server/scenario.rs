//! MCP tool handlers for scenario runs: `playbook_scenarios` and
//! `playbook_scenario_run`. The logic lives in `crate::tools::scenario`,
//! shared with `apb eval --level quick|smoke` and the HTTP API. Neither is
//! routed by a read-only server: a run writes its scenario run directory,
//! and a smoke run spends decision requests.

use apb_core::scenario::ScenarioLevel;
use apb_core::step_env::StepCaller;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::quality::{invalid, off_runtime};
use super::{WfMcp, to_call_tool_result};
use crate::tools;
use crate::tools::scenario::{ScenarioCaller, ScenarioRunRequest, Scope};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaybookScenariosArgs {
    /// The playbook id.
    pub id: String,
    /// The version; `current` when absent.
    #[serde(default)]
    pub version: Option<String>,
    /// Where the playbook is read: "project" (default) or "global" (the
    /// global store, where the shipped defaults keep their scenarios).
    #[serde(default)]
    pub scope: Option<String>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunScenarioCaptureArgs {
    /// The finished run's id.
    pub run_id: String,
    /// The scenario's name: [a-z0-9][a-z0-9-]*, at most 64 characters.
    pub name: String,
    /// Only assert these nodes' statuses and visits; default: every node
    /// that ran.
    #[serde(default)]
    pub nodes: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaybookScenarioRunArgs {
    pub id: String,
    /// The scenario's name (its file stem under `evals/scenarios/`).
    pub scenario: String,
    /// `quick` (scripted, sends nothing) or `smoke` (asks this machine's
    /// decision providers for the judge and engine decisions, at their cost).
    pub level: String,
    #[serde(default)]
    pub version: Option<String>,
    /// The decision request cap of a smoke run, 1 to 200: required there,
    /// refused at quick.
    #[serde(default)]
    pub max_decision_requests: Option<u32>,
    /// Where the playbook is read: "project" (default) or "global". The
    /// scenario run is written under the current project's
    /// `.apb/scenario-runs/` either way.
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    /// external_data - the scenario run keeps nothing for learning (see
    /// playbook_run): its index line holds only what retention needs.
    #[serde(default)]
    pub external_data: Option<bool>,
}

/// The `scope` argument: project when absent.
fn scope_of(arg: Option<&str>) -> Result<Scope, String> {
    match arg {
        None => Ok(Scope::Project),
        Some(label) => {
            Scope::parse(label).ok_or_else(|| format!("scope `{label}`: project or global"))
        }
    }
}

#[tool_router(router = scenario_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "List a playbook's scenarios (evals/scenarios/<name>.yaml: scripted answers for every point a run reaches outside apb, with expectations), each with its newest quick and smoke result in the current project and whether that result is fresh (same version digest and scenario file), whether the scenario is stale for the version, and one issue per file that does not load. scope global lists a playbook of the global store, where the shipped defaults keep their scenarios.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_scenarios(
        &self,
        Parameters(args): Parameters<PlaybookScenariosArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(args.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let scope = match scope_of(args.scope.as_deref()) {
            Ok(s) => s,
            Err(e) => return invalid(e),
        };
        off_runtime(move || {
            tools::scenario::scenarios(&root, scope, &args.id, args.version.as_deref())
                .and_then(tools::grade::to_value)
        })
        .await
    }

    #[tool(
        description = "Play one scenario of a playbook through the real engine, with every point that would reach outside apb answered from the scenario, and return its report (verdict passed, failed, invalid, stale or incomplete, the failed checks first, the trace, unused and missing answers) and the run graph. level quick sends nothing, starts no program and writes only below .apb/scenario-runs/ (at most 60 s); nothing it does counts as run history. level smoke asks this machine's decision providers instead of the scripted judge answers: only when the user asks for it, with max_decision_requests (1 to 200), for an approved version, never from a run step (at most 300 s). scope global plays a playbook of the global store; the scenario run is still written in the current project.",
        annotations(destructive_hint = false)
    )]
    pub(crate) async fn playbook_scenario_run(
        &self,
        Parameters(args): Parameters<PlaybookScenarioRunArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(args.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let level = match args.level.as_str() {
            "quick" => ScenarioLevel::Quick,
            "smoke" => ScenarioLevel::Smoke,
            other => return invalid(format!("level `{other}`: quick or smoke")),
        };
        let scope = match scope_of(args.scope.as_deref()) {
            Ok(s) => s,
            Err(e) => return invalid(e),
        };
        let req = ScenarioRunRequest {
            scenario: args.scenario,
            level,
            version: args.version,
            max_decision_requests: args.max_decision_requests,
            external_data: args.external_data == Some(true),
        };
        off_runtime(move || {
            let exe = apb_core::fsutil::reexec_exe()
                .map_err(|e| tools::ToolError::Engine(format!("the apb executable: {e}")))?;
            let step = StepCaller::from_env();
            let caller = ScenarioCaller {
                step: &step,
                keyless_dashboard: false,
            };
            tools::scenario::run_scenario(&root, scope, &args.id, &req, caller, &exe)
                .and_then(tools::grade::to_value)
        })
        .await
    }

    #[tool(
        description = "Show a finished production run as a scenario: the YAML with one scripted answer per node execution and the run's expectations, scrubbed of the run's secrets, with the warnings and the digest. PREVIEW ONLY: it writes nothing. Show the YAML to the person; they save it with `apb scenario capture --save` or on the dashboard. Refused from a run step.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_scenario_capture(
        &self,
        Parameters(args): Parameters<RunScenarioCaptureArgs>,
    ) -> CallToolResult {
        let root = self.root.clone();
        off_runtime(move || {
            let step = StepCaller::from_env();
            let a = tools::scenario_capture::CaptureArgs {
                name: args.name,
                nodes: args.nodes,
                save: false,
                digest: None,
            };
            tools::scenario_capture::capture(
                &root,
                &args.run_id,
                &a,
                apb_core::launch::floor::SaveChannel::Agent,
                &step,
            )
            .and_then(tools::grade::to_value)
        })
        .await
    }
}
