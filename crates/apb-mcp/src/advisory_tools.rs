//! The advisory layer of the MCP (spec 2026-07-12, sections 7-8): free agent
//! detection, profile howto (format + models table + subscriptions + selection
//! rules), recording declared subscriptions, and an adoption-readiness report.
//!
//! Everything is read-only except subscriptions_set (writes the overlay and the onboarding state).
//! The models table is a hint, not a hard binding: adoption codes for
//! models strictly account for detection authority (spec 8.4).

use std::path::Path;

use apb_core::agent_catalog;
use apb_core::model_check;
use apb_core::models_table::{self, OnboardingState, Subscription};
use apb_core::profile_store::PlaybookOrigin;
use apb_core::registry::Registry;
use apb_core::scope::{Origin, PlaybookRef, Scope};
use apb_core::trust::TrustStore;
use apb_engine::fit::profile_findings;
use serde_json::{Value, json};

use crate::tools::ToolError;

/// Detects installed agents (spec 7.6) and the model options apb offers for
/// each: the same `agent_catalog` snapshot the dashboard's `/api/models`
/// serves. `refresh` re-probes instead of using the detection memo.
pub fn agents_detect(refresh: bool) -> Result<Value, ToolError> {
    let c = agent_catalog::load(refresh).map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(json!({ "agents": c.agents, "options_by_agent": c.options_by_agent }))
}

/// The profile howto bundle: format, selection rules, models table, purposes,
/// subscriptions, detection, and hints. When onboarding is `Uninitialized` it carries the
/// `subscriptions_uninitialized` flag so the agent offers the survey.
pub fn profile_howto() -> Result<Value, ToolError> {
    let catalog = agent_catalog::load(false).map_err(|e| ToolError::Engine(e.to_string()))?;
    let table = &catalog.table;
    let state = models_table::onboarding::read().map_err(|e| ToolError::Engine(e.to_string()))?;
    let mut out = json!({
        "format": PROFILE_FORMAT,
        "selection_rules": SELECTION_RULES,
        "coverage_semantics": COVERAGE_SEMANTICS,
        "authorization_bounds": AUTHORIZATION_BOUNDS,
        "models_table": {
            "as_of": table.as_of,
            "models": table.models,
            "purposes": table.purposes,
        },
        "subscriptions": table.subscriptions,
        "agents": catalog.agents,
        "options_by_agent": catalog.options_by_agent,
    });
    if state == OnboardingState::Uninitialized {
        out["subscriptions_uninitialized"] = json!(true);
    }
    Ok(out)
}

/// Declares subscriptions or declines the survey. `declined: true` writes
/// onboarding state `Declined` and leaves subscriptions untouched; otherwise it writes the
/// subscriptions section into the overlay and state `Configured`.
pub fn subscriptions_set(
    subscriptions: Vec<Subscription>,
    declined: bool,
) -> Result<Value, ToolError> {
    if declined {
        models_table::onboarding::write(OnboardingState::Declined)
            .map_err(|e| ToolError::Engine(e.to_string()))?;
        return Ok(json!({ "declined": true }));
    }
    models_table::write_subscriptions(&subscriptions)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    models_table::onboarding::write(OnboardingState::Configured)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(json!({ "configured": true, "count": subscriptions.len() }))
}

/// An adoption-readiness report ("will it fit here?") for a run in the
/// project at `root`: one playbook (`id`, in definition scope `scope`,
/// project when absent), or every playbook of `scope` (both scopes when
/// absent). Each report carries the four-section [`apb_engine::fit`] report
/// and the profile and model `findings` with their codes (a profile's
/// resolvability, skills, bundle trust and the executor chain's models per
/// detection). A store that does not exist (no project here, a scope asked
/// for that this machine lacks) is a `playbook_store_missing` diagnostic.
pub fn playbook_adopt_report(
    root: &Path,
    id: Option<&str>,
    scope: Option<&str>,
) -> Result<Value, ToolError> {
    let explicit_scope = scope.is_some();
    let scopes: Vec<Scope> = match (scope, id) {
        (Some(s), _) => {
            vec![Scope::parse(s).ok_or_else(|| ToolError::Engine(format!("unknown scope `{s}`")))?]
        }
        (None, Some(_)) => vec![Scope::Project],
        (None, None) => vec![Scope::Project, Scope::Global],
    };
    let cx = model_check::ModelContext::load();
    let store = TrustStore::load();
    let mut reports = Vec::new();
    let mut diagnostics = Vec::new();
    for scope in scopes {
        let Some(reg) = scope_registry(root, scope) else {
            let detail = format!("no {} playbook store", scope.as_str());
            match id {
                Some(one) => reports.push(json!({
                    "id": one,
                    "scope": scope.as_str(),
                    "findings": [json!({ "code": "playbook_unloadable", "detail": detail })],
                })),
                // A missing global store on a machine without one is normal;
                // a missing project store, or a scope asked for, is said.
                None if scope == Scope::Project || explicit_scope => diagnostics.push(json!({
                    "code": "playbook_store_missing",
                    "scope": scope.as_str(),
                    "detail": detail,
                })),
                None => {}
            }
            continue;
        };
        // all-mode: enumerate by directory (playbook_ids does not fail because of one
        // broken playbook - otherwise one unparseable YAML would hide the report for all
        // the rest; loading each one happens below, with a diagnostic for the unloadable one).
        let ids: Vec<String> = match id {
            Some(one) => vec![one.to_string()],
            None => reg.playbook_ids(),
        };
        for wid in ids {
            reports.push(adopt_one(root, &reg, scope, &wid, &cx, &store));
        }
    }
    // Host execution mode (0.23.0): how this machine executes agent steps.
    // The findings above judge the profile CLIs, which a run started with
    // execution: host ignores.
    let (_, execution) = apb_core::execution::doctor_line(root);
    Ok(json!({
        "playbooks": reports,
        "diagnostics": diagnostics,
        "execution": {
            "default": "cli",
            "detail": execution,
            "host_mode": "a run started with execution: host spawns no agent CLI; agent, model, fallback agents and command of profiles are ignored, so agent and model findings above do not block such a run",
        },
    }))
}

/// The registry of `scope` for a run in `root`; `None` when that store does
/// not exist.
fn scope_registry(root: &Path, scope: Scope) -> Option<Registry> {
    match scope {
        Scope::Project => Registry::open(root).ok(),
        Scope::Global => {
            apb_core::store::global_playbooks_parent().and_then(|p| Registry::open_dir(&p).ok())
        }
    }
}

/// The report of one playbook: its fit and the profile findings. An
/// unloadable playbook is an explicit diagnostic on its id, not a silent
/// skip (which for an explicit id would return an empty report).
fn adopt_one(
    root: &Path,
    reg: &Registry,
    scope: Scope,
    id: &str,
    cx: &model_check::ModelContext,
    store: &TrustStore,
) -> Value {
    let loaded = match reg.load(id, None) {
        Ok(l) => l,
        Err(e) => {
            return json!({
                "id": id,
                "scope": scope.as_str(),
                "findings": [json!({ "code": "playbook_unloadable", "detail": e.to_string() })],
            });
        }
    };
    let origin = match scope {
        Scope::Project => PlaybookOrigin::Project,
        Scope::Global => PlaybookOrigin::Global,
    };
    // The readiness report is thorough: the supervisor profile counts too
    // (supervised: true), even though supervision is only needed with
    // --supervise. A profile several nodes bind (or that is also the
    // default and the supervisor) is one thing to fix: each finding once,
    // in first-seen order.
    let mut seen = std::collections::HashSet::new();
    let findings: Vec<Value> = crate::policy::collect_profile_refs(&loaded.playbook, true)
        .iter()
        .flat_map(|r| profile_findings(root, origin, r, cx, store))
        .filter(|f| seen.insert(f.clone()))
        .map(|f| json!(f))
        .collect();
    let wref = PlaybookRef {
        origin: match scope {
            Scope::Project => Origin::Project { workspace_id: None },
            Scope::Global => Origin::Global,
        },
        id: id.to_string(),
        version: None,
    };
    let mut out = json!({ "id": id, "scope": scope.as_str(), "findings": findings });
    match apb_engine::fit::assess(root, &wref) {
        Ok(fit) => out["fit"] = json!(fit),
        Err(e) => out["fit_error"] = json!(e.to_string()),
    }
    out
}

const PROFILE_FORMAT: &str = "\
A profile is the single executor binding for a node. profile.yaml carries name, description, executor (agent + model + ordered fallbacks), soul (any | native_required), and skills (names or {name, scope}). SOUL.md holds the role system prompt. A node references a profile by name (scope auto) or {name, scope}. Skill content is never embedded into prompts. For isolation none (default) skills are delivered advisory by name and the agent reads the live .agents/skills; for isolation full or best_effort the run materializes skill copies from the run snapshot and the agent reads only that snapshot (attempts record skills_mode: materialized vs advisory). An isolated attempt that gets a sandbox (Linux with bubblewrap) runs in the node's own tree with the copies mounted read-only over .agents/skills and .claude/skills, its home hidden except the agent's own login and declared paths; a best_effort attempt without a sandbox runs in a fresh scratch directory that holds the copies and no project tree. isolation full refuses to run (isolation_unavailable) where no sandbox is possible, and only claude and codex declare their sandbox needs so far. See docs/ISOLATION.md. Skills semantics: an empty skills list means the profile grants NO skills (empty is none, not all). When the user authors a profile through the dashboard the skills default to all-off and the user picks what to grant; when YOU author a profile on the user's behalf, pre-select exactly the skills the role needs from the outset. environment: minimal (default, omit the key) keeps the operator's own plugins, user MCP servers, user skills and user CLAUDE.md out of a claude executor (the project's own CLAUDE.md and .claude/skills and the profile's skills still load); set environment: full only when the role depends on something only the operator's personal setup provides, such as a plugin skill or a user-scope MCP server. process_env: which of the operator's environment variables the agent process inherits. policy inherit (default, omit the key) passes everything; policy allowlist passes only a base set (PATH, HOME, locale, temp and XDG dirs, proxies, certificate bundles), the variables the profile's agent CLI reads (for claude ANTHROPIC_* and CLAUDE_CODE_*, for codex CODEX_* and OPENAI_*, and so on) and the names in allow: exact names or a prefix with one trailing * (NPM_CONFIG_*), at most 64, names only, never values, for example process_env: { policy: allowlist, allow: [GH_TOKEN, SSH_AUTH_SOCK] }. Connector-referenced variables are withheld under either policy. A node with isolation full or best_effort always gets the allowlist, even when the profile says inherit (agent_env_applied records forced_by: isolation). Each allowlisted attempt journals agent_env_applied with the withheld names; apb doctor --run names them when an attempt fails. A cmd: secret helper of a connector runs with the agent's allowlisted environment, so add what it needs (GNUPGHOME, PASSWORD_STORE_DIR) to allow.";

const SELECTION_RULES: &str = "\
Pick agent and model from the task's purpose using the models table as a hint only. Match the purpose (coding, review, planning, writing, cheap-glue, vision-tasks, and so on) to a high-scoring model, then confirm the user has access (subscription or key). Prefer a fallback chain that degrades gracefully. The table is advisory: never hard-bind a node to a table entry, and never claim a model works without detection evidence.";

const COVERAGE_SEMANTICS: &str = "\
Model availability is only asserted when detection authority is Full: then a missing model is model_not_available. For Partial, Display, Static, or no list, treat availability as model_unverifiable and do not block on it. Where apb keeps a closed model list (claude, codex, zcode), an id outside it (a typo, a retired model) is model_unknown: check it, it rarely runs. The exception is model_new_in_family, a note only: an unlisted claude id that follows a known family's pattern (claude-<opus|sonnet|haiku|fable>-<major>-<minor>, optionally -YYYYMMDD) is most likely a model newer than this apb and is accepted. zcode is stricter: apb allows only GLM-5.3 and GLM-5.3-Flash (bare ids, optional @low|high|max effort, low when omitted) and reports any other zcode model as model_not_allowed, which does block. So does model_policy_violation: the user's global config has a model_policy rule the model breaks (for example an allowlist that keeps routine work on a smaller model); pick an allowed model instead.";

const AUTHORIZATION_BOUNDS: &str = "\
Create a project profile a directly requested playbook needs without extra questions. Ask the user before an unexpected global mutation or an initiative-driven change to a profile other playbooks already use. Cross-workspace profile mutations are not allowed.";

#[cfg(test)]
mod tests {
    use super::PROFILE_FORMAT;

    /// The howto describes isolation as enforced: the sandbox in the node's
    /// tree, the scratch directory only as the fallback without one, the
    /// forced allowlist, and the refusal of `full`.
    #[test]
    fn the_profile_format_describes_the_sandbox() {
        for phrase in [
            "runs in the node's own tree",
            "without a sandbox runs in a fresh scratch directory",
            "isolation_unavailable",
            "forced_by: isolation",
            "docs/ISOLATION.md",
        ] {
            assert!(PROFILE_FORMAT.contains(phrase), "missing `{phrase}`");
        }
        assert!(!PROFILE_FORMAT.contains("isolated per-node workdir"));
    }
}
