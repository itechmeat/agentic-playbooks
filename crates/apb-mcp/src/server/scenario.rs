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
use crate::tools::scenario::{ScenarioCaller, ScenarioRunRequest};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaybookScenariosArgs {
    /// The playbook id (a project playbook).
    pub id: String,
    /// The version; `current` when absent.
    #[serde(default)]
    pub version: Option<String>,
    /// workspace_id of another workspace (spec 7). None - the current one.
    #[serde(default)]
    pub workspace: Option<String>,
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
    #[serde(default)]
    pub workspace: Option<String>,
}

#[tool_router(router = scenario_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "List a playbook's scenarios (evals/scenarios/<name>.yaml: scripted answers for every point a run reaches outside apb, with expectations), each with its newest quick and smoke result and whether that result is fresh (same version digest and scenario file), whether the scenario is stale for the version, and one issue per file that does not load.",
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
        off_runtime(move || {
            tools::scenario::scenarios(&root, &args.id, args.version.as_deref())
                .and_then(tools::grade::to_value)
        })
        .await
    }

    #[tool(
        description = "Play one scenario of a playbook through the real engine, with every point that would reach outside apb answered from the scenario, and return its report (verdict passed, failed, invalid, stale or incomplete, the failed checks first, the trace, unused and missing answers) and the run graph. level quick sends nothing, starts no program and writes only below .apb/scenario-runs/ (at most 60 s); nothing it does counts as run history. level smoke asks this machine's decision providers instead of the scripted judge answers: only when the user asks for it, with max_decision_requests (1 to 200), for an approved version, never from a run step (at most 300 s).",
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
        let req = ScenarioRunRequest {
            scenario: args.scenario,
            level,
            version: args.version,
            max_decision_requests: args.max_decision_requests,
        };
        off_runtime(move || {
            let exe = apb_core::fsutil::reexec_exe()
                .map_err(|e| tools::ToolError::Engine(format!("the apb executable: {e}")))?;
            let step = StepCaller::from_env();
            let caller = ScenarioCaller {
                step: &step,
                keyless_dashboard: false,
            };
            tools::scenario::run_scenario(&root, &args.id, &req, caller, &exe)
                .and_then(tools::grade::to_value)
        })
        .await
    }
}
