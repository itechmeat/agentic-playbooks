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
use apb_core::profile::QualifiedProfileRef;
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::registry::Registry;
use apb_core::trust::TrustStore;
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

/// An adoption-readiness report for a playbook (spec 5.2): for each profile
/// checks resolvability, skills, bundle trust, and the availability of the
/// executor chain's models per detection.
pub fn playbook_adopt_report(root: &Path, id: Option<&str>) -> Result<Value, ToolError> {
    let reg = Registry::open(root).map_err(|e| ToolError::Engine(e.to_string()))?;
    // all-mode: enumerate by directory (playbook_ids does not fail because of one
    // broken playbook - otherwise one unparseable YAML would hide the report for all
    // the rest; loading each one happens below, with a diagnostic for the unloadable one).
    let ids: Vec<String> = match id {
        Some(one) => vec![one.to_string()],
        None => reg.playbook_ids(),
    };
    let cx = model_check::ModelContext::load();
    let store = TrustStore::load();
    let mut reports = Vec::new();
    for wid in ids {
        // An unloadable playbook - an explicit diagnostic on its id (not a silent
        // skip, which for an explicit id would return an empty report).
        match reg.load(&wid, None) {
            Ok(loaded) => {
                let mut findings = Vec::new();
                // The readiness report is thorough: we also include the supervisor profile
                // (supervised: true) to surface its problems, even though
                // supervision is only needed with --supervise.
                for r in crate::policy::collect_profile_refs(&loaded.playbook, true) {
                    adopt_check_profile(root, &r, &cx, &store, &mut findings);
                }
                // A profile several nodes bind (or that is also the default
                // and the supervisor) is one thing to fix: report each finding
                // once, in first-seen order.
                let mut seen = std::collections::HashSet::new();
                findings.retain(|f| seen.insert(f.to_string()));
                reports.push(json!({ "id": wid, "findings": findings }));
            }
            Err(e) => {
                reports.push(json!({
                    "id": wid,
                    "findings": [json!({ "code": "playbook_unloadable", "detail": e.to_string() })],
                }));
            }
        }
    }
    Ok(json!({ "playbooks": reports }))
}

/// Checks one playbook profile and appends findings with spec 5.2 codes.
fn adopt_check_profile(
    root: &Path,
    r: &QualifiedProfileRef,
    cx: &model_check::ModelContext,
    store: &TrustStore,
    findings: &mut Vec<Value>,
) {
    let origin = PlaybookOrigin::Project;
    // Resolve + bundle in one pass (skills are handled inside compute_bundle).
    match profile_store::compute_bundle(root, origin, r) {
        Ok((loaded, _pairs, bundle)) => {
            let key = format!("{}/{}", profile_store::scope_str(loaded.scope), loaded.name);
            if !store.is_approved(&bundle) {
                findings.push(json!({ "code": "untrusted", "ref": key }));
            }
            let ex = &loaded.doc.executor;
            let chain = std::iter::once((ex.agent.as_str(), ex.model.as_str())).chain(
                ex.fallbacks
                    .iter()
                    .map(|f| (f.agent.as_str(), f.model.as_str())),
            );
            for (agent, model) in chain {
                adopt_check_model(agent, model, &key, cx, findings);
            }
        }
        Err(e) => {
            let (code, msg) = classify_profile_error(&e);
            findings.push(json!({ "code": code, "ref": r.name, "detail": msg }));
        }
    }
}

/// One `(agent, model)` of a profile's executor chain, judged by the shared
/// [`model_check`] (spec 5.2 environment part): availability is asserted only
/// when detection authority is Full; zcode's allowlist and the config's
/// `model_policy` block; an id outside apb's closed list for the agent is
/// `model_unknown`.
fn adopt_check_model(
    agent: &str,
    model: &str,
    profile_key: &str,
    cx: &model_check::ModelContext,
    findings: &mut Vec<Value>,
) {
    let Some(issue) = model_check::check(agent, model, cx) else {
        return;
    };
    let mut f = json!({ "code": issue.code(), "ref": profile_key, "agent": agent });
    match &issue {
        model_check::ModelIssue::AgentNotInstalled => {}
        model_check::ModelIssue::NotAllowed(detail)
        | model_check::ModelIssue::PolicyViolation(detail) => {
            f["model"] = json!(model);
            f["detail"] = json!(detail);
        }
        _ => f["model"] = json!(model),
    }
    findings.push(f);
}

fn classify_profile_error(e: &profile_store::ProfileError) -> (&'static str, String) {
    use profile_store::ProfileError;
    match e {
        ProfileError::SkillMissing(_) => ("skill_missing", e.to_string()),
        ProfileError::NotFound(_) => ("profile_missing", e.to_string()),
        other => ("profile_unresolved", other.to_string()),
    }
}

const PROFILE_FORMAT: &str = "\
A profile is the single executor binding for a node. profile.yaml carries name, description, executor (agent + model + ordered fallbacks), soul (any | native_required), and skills (names or {name, scope}). SOUL.md holds the role system prompt. A node references a profile by name (scope auto) or {name, scope}. Skill content is never embedded into prompts. For isolation none (default) skills are delivered advisory by name and the agent reads the live .agents/skills; for isolation full or best_effort the run materializes skill copies from the run snapshot into an isolated per-node workdir and the agent reads only that snapshot (attempts record skills_mode: materialized vs advisory). Skills semantics: an empty skills list means the profile grants NO skills (empty is none, not all). When the user authors a profile through the dashboard the skills default to all-off and the user picks what to grant; when YOU author a profile on the user's behalf, pre-select exactly the skills the role needs from the outset. environment: minimal (default, omit the key) keeps the operator's own plugins, user MCP servers, user skills and user CLAUDE.md out of a claude executor (the project's own CLAUDE.md and .claude/skills and the profile's skills still load); set environment: full only when the role depends on something only the operator's personal setup provides, such as a plugin skill or a user-scope MCP server.";

const SELECTION_RULES: &str = "\
Pick agent and model from the task's purpose using the models table as a hint only. Match the purpose (coding, review, planning, writing, cheap-glue, vision-tasks, and so on) to a high-scoring model, then confirm the user has access (subscription or key). Prefer a fallback chain that degrades gracefully. The table is advisory: never hard-bind a node to a table entry, and never claim a model works without detection evidence.";

const COVERAGE_SEMANTICS: &str = "\
Model availability is only asserted when detection authority is Full: then a missing model is model_not_available. For Partial, Display, Static, or no list, treat availability as model_unverifiable and do not block on it. Where apb keeps a closed model list (claude, codex, zcode), an id outside it (a typo, a retired model) is model_unknown: check it, it rarely runs. zcode is stricter: apb allows only GLM-5.3 and GLM-5.3-Flash (bare ids, optional @low|high|max effort, low when omitted) and reports any other zcode model as model_not_allowed, which does block. So does model_policy_violation: the user's global config has a model_policy rule the model breaks (for example an allowlist that keeps routine work on a smaller model); pick an allowed model instead.";

const AUTHORIZATION_BOUNDS: &str = "\
Create a project profile a directly requested playbook needs without extra questions. Ask the user before an unexpected global mutation or an initiative-driven change to a profile other playbooks already use. Cross-workspace profile mutations are not allowed.";
