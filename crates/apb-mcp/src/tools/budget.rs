//! The run budget and estimate on the MCP surface: turning the budget
//! arguments into engine requests, the pre-start decision (the
//! `budget_needs_decision` refusal), the `estimate` and `budget` blocks every
//! run answer carries, and the structured `budget_paused` refusal of a plain
//! resume. The HTTP API reuses the same refusals, so both surfaces answer
//! alike.

use std::path::Path;

use apb_core::budget::{BudgetRequest, OnNoEstimate};
use apb_core::store::ResolvedPlaybook;
use apb_engine::budget::{BUDGET_PAUSED, BudgetResume, StartCheck};
use apb_engine::pause::PauseCause;
use apb_engine::run_estimate::{RunEstimate, render_lines};
use apb_engine::run_view::RunView;
use serde_json::{Value, json};

use super::ToolError;

/// The refusal of a start whose budget needs the person's decision first.
pub const BUDGET_NEEDS_DECISION: &str = "budget_needs_decision";

/// What a caller does when a run waits on its budget.
pub const BUDGET_PAUSE_NEXT: &str = "the run is paused by its budget (see budget.paused): ask the person whether to raise the budget, set a new one, or let the paused node run once over it, then call run_resume with budget (raise_tokens, raise_usd, tokens, usd, on_no_estimate or once). A plain run_resume is refused";

fn invalid(detail: &str) -> Value {
    json!({ "error": "invalid_budget", "detail": detail })
}

/// Parses `on_no_estimate` (`ask`, `pause` or `continue`).
fn policy(s: Option<&str>) -> Result<Option<OnNoEstimate>, Value> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(v) => OnNoEstimate::parse(v)
            .map(Some)
            .ok_or_else(|| invalid("on_no_estimate must be ask, pause or continue")),
    }
}

fn tokens(v: Option<u64>) -> Result<Option<u64>, Value> {
    match v {
        Some(0) => Err(invalid("a token amount must be greater than 0")),
        v => Ok(v),
    }
}

fn usd(v: Option<f64>) -> Result<Option<f64>, Value> {
    match v {
        Some(x) if !x.is_finite() || x <= 0.0 => Err(invalid(
            "a dollar amount must be a finite number greater than 0",
        )),
        v => Ok(v),
    }
}

/// The run's budget argument; `None` when it sets nothing. A refusal
/// (`invalid_budget`) for a zero or negative amount or an unknown policy.
pub fn start_request(
    tokens_arg: Option<u64>,
    usd_arg: Option<f64>,
    policy_arg: Option<&str>,
) -> Result<Option<BudgetRequest>, Value> {
    let req = BudgetRequest {
        tokens: tokens(tokens_arg)?,
        usd: usd(usd_arg)?,
        on_no_estimate: policy(policy_arg)?,
    };
    Ok((req != BudgetRequest::default()).then_some(req))
}

/// A resume's budget change; `None` when it asks for none.
pub struct ResumeArgs<'a> {
    pub raise_tokens: Option<u64>,
    pub raise_usd: Option<f64>,
    pub tokens: Option<u64>,
    pub usd: Option<f64>,
    pub on_no_estimate: Option<&'a str>,
    pub once: bool,
}

/// The engine request for a resume's budget change; `None` when it asks for
/// none.
pub fn resume_request(a: ResumeArgs<'_>) -> Result<Option<BudgetResume>, Value> {
    if (a.tokens.is_some() && a.raise_tokens.is_some())
        || (a.usd.is_some() && a.raise_usd.is_some())
    {
        return Err(invalid(
            "set a new limit or raise it, not both, for the same dimension",
        ));
    }
    let req = BudgetResume {
        raise_tokens: tokens(a.raise_tokens)?,
        raise_usd: usd(a.raise_usd)?,
        tokens: tokens(a.tokens)?,
        usd: usd(a.usd)?,
        on_no_estimate: policy(a.on_no_estimate)?,
        once: a.once,
    };
    Ok((!req.is_empty()).then_some(req))
}

/// The estimate as run answers carry it: the run figures without the
/// per-node detail, plus a one-line-per-figure `summary`.
pub fn estimate_block(e: &RunEstimate) -> Value {
    let mut v = full_estimate(e);
    if let Some(obj) = v.as_object_mut() {
        obj.remove("nodes");
    }
    v
}

/// The whole estimate, per-node figures included, plus its `summary`.
pub fn full_estimate(e: &RunEstimate) -> Value {
    let mut v = json!(e);
    v["summary"] = json!(render_lines(e).join("\n"));
    v
}

/// The estimate of playbook `id` (`version`, else current) of the project
/// at `root`, before a start, in the project's configured execution mode.
pub fn playbook_estimate(root: &Path, id: &str, version: Option<&str>) -> Result<Value, ToolError> {
    let mode = apb_core::execution::resolve_for(root, &Default::default())
        .map(|r| r.mode)
        .unwrap_or_default();
    let e = apb_engine::run_estimate::for_playbook(root, id, version, mode.as_str())?;
    Ok(full_estimate(&e))
}

/// The refusal of a start the budget needs a decision for, `None` when the
/// start may proceed.
pub fn start_decision(check: &StartCheck) -> Option<Value> {
    if !check.needs_decision {
        return None;
    }
    Some(json!({
        "error": BUDGET_NEEDS_DECISION,
        "unestimated": check.undecided,
        "estimate": estimate_block(&check.estimate),
        "options": ["pause", "continue"],
        "instruction": "these gated nodes have no spend estimate in a dimension the budget limits, and the budget's on_no_estimate is ask: ask the person whether the run should pause before such a node (pause) or run it anyway (continue), then start it again with budget.on_no_estimate set to their answer",
    }))
}

/// What a start takes from its pre-start budget check: the warnings to show
/// with the answer, and the estimate the person was shown, which becomes the
/// run's snapshot (so the start does not read the history twice).
#[derive(Debug, Default)]
pub struct Preflight {
    pub warnings: Vec<String>,
    pub estimate: Option<RunEstimate>,
}

/// The budget check before a start of `resolved` in `execution` mode with
/// the budget argument `req`: the warnings and the estimate, or the refusal
/// to return instead of starting. A playbook whose estimate cannot be read
/// starts without warnings or estimate; the start itself reports why.
pub fn preflight(
    resolved: &ResolvedPlaybook,
    execution: &str,
    req: Option<&BudgetRequest>,
) -> Result<Preflight, Value> {
    let Ok(check) = apb_engine::budget::check_resolved(resolved, execution, req) else {
        return Ok(Preflight::default());
    };
    if req.is_some() && check.budget.is_none() {
        return Err(invalid(
            "a budget needs a token or a dollar limit; on_no_estimate alone sets none",
        ));
    }
    match start_decision(&check) {
        Some(refusal) => Err(refusal),
        None => Ok(Preflight {
            warnings: check.warnings,
            estimate: Some(check.estimate),
        }),
    }
}

/// Adds the run's `estimate` (from its start snapshot) and `budget` blocks
/// to a run answer; each is absent when the run has none.
pub fn add_run_blocks(out: &mut Value, run_dir: &Path, view: &RunView) {
    if let Some(e) = apb_engine::run_estimate::snapshot::read(run_dir) {
        out["estimate"] = estimate_block(&e);
    }
    if let Some(b) = view.budget(run_dir) {
        out["budget"] = json!(b);
    }
}

/// [`add_run_blocks`] for the run `run_id` of the project at `root`, for a
/// start answer that only carries the id.
pub fn with_start_blocks(root: &Path, mut out: Value) -> Value {
    let Some(run_id) = out["run_id"].as_str().map(str::to_string) else {
        return out;
    };
    let dir = root.join(".apb/runs").join(&run_id);
    if let Ok(view) = RunView::load(&dir, &run_id) {
        add_run_blocks(&mut out, &dir, &view);
    }
    out
}

/// Copies the `estimate` and `budget` blocks of a start answer into
/// another answer for the same run.
pub fn carry_blocks(from: &Value, to: &mut Value) {
    for key in ["estimate", "budget"] {
        if let Some(v) = from.get(key) {
            to[key] = v.clone();
        }
    }
}

/// The refusal of an MCP budget change no person confirmed yet.
pub const BUDGET_CHANGE_NEEDS_CONFIRMATION: &str = "budget_change_requires_confirmation";

/// The nonce a person's confirmation of budget change `req` on run `run_id`
/// echoes. It binds the change to the run and to the lifecycle event the
/// run is at (its pause), so another change, or the same one asked for a
/// later pause, needs a new confirmation. Like the irreversible-consent
/// nonce it is not a secret: it makes the caller stop and show the person
/// the change, it cannot prove a person answered.
pub fn budget_nonce(run_id: &str, lifecycle: Option<u64>, req: &BudgetResume) -> String {
    let input = format!(
        "{run_id}\n{}\n{}",
        lifecycle.map_or_else(String::new, |s| s.to_string()),
        serde_json::to_string(req).unwrap_or_default()
    );
    let hex = apb_core::content::sha256_hex(input.as_bytes());
    let hex = hex.strip_prefix("sha256:").unwrap_or(&hex);
    format!("budget-{}", &hex[..32])
}

/// The MCP rule for a budget change at resume (`run_resume` with `budget`):
/// a person must confirm it. `Ok(())` once `confirm` echoes the nonce of the
/// refusal the person was shown; else the refusal to return, with the nonce
/// and the change as text. A run a supervisor agent watches is refused
/// outright: its budget is changed by a person at `apb resume` or the
/// dashboard, never through the agent channel. A run whose config or
/// journal does not read is refused too (fail closed).
pub fn confirm_budget_change(
    root: &Path,
    run_id: &str,
    req: &BudgetResume,
    confirm: Option<&str>,
) -> Result<(), Value> {
    let dir = root.join(".apb/runs").join(run_id);
    let refused = |detail: &str| json!({ "error": "budget_change_refused", "detail": detail });
    // Fail closed: a config that does not read could hide a supervised run.
    let Ok(config) = apb_engine::run_config::read_run_config(&dir) else {
        return Err(refused(
            "the run's configuration cannot be read, so whether a supervisor agent watches it is unknown",
        ));
    };
    if config.mode.expects_supervisor_agent() {
        return Err(refused(
            "a supervisor agent watches this run: a person changes its budget with `apb resume` or the dashboard",
        ));
    }
    // The nonce binds the change to the run's pause: without the journal
    // there is no pause to bind it to.
    let Ok(events) = apb_engine::event::read_all(&dir) else {
        return Err(refused(
            "the run's journal cannot be read, so the change cannot be bound to its pause",
        ));
    };
    let lifecycle = apb_engine::scheduler::lifecycle_seq(&events);
    let nonce = budget_nonce(run_id, lifecycle, req);
    if confirm == Some(nonce.as_str()) {
        return Ok(());
    }
    Err(json!({
        "error": BUDGET_CHANGE_NEEDS_CONFIRMATION,
        "change": change_text(req),
        "budget_nonce": nonce,
        "instruction": "a budget change needs the person's confirmation: show them this change, and if they agree call run_resume again with the same budget and confirm_budget set to budget_nonce",
    }))
}

/// `raise tokens by 100000, run the paused node once`: what a budget change
/// asks for, for the person.
fn change_text(r: &BudgetResume) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = r.raise_tokens {
        parts.push(format!("raise the token limit by {t}"));
    }
    if let Some(u) = r.raise_usd {
        parts.push(format!("raise the dollar limit by ${u:.2}"));
    }
    if let Some(t) = r.tokens {
        parts.push(format!("set the token limit to {t}"));
    }
    if let Some(u) = r.usd {
        parts.push(format!("set the dollar limit to ${u:.2}"));
    }
    if let Some(p) = r.on_no_estimate {
        parts.push(format!("on_no_estimate {}", p.as_str()));
    }
    if r.once {
        parts.push("let the paused node run once over the budget".into());
    }
    parts.join(", ")
}

/// Whether the run's pause is its budget's.
pub fn budget_paused(view: &RunView) -> bool {
    view.pause()
        .is_some_and(|p| p.cause == Some(PauseCause::Budget))
}

/// The structured refusal of a resume the budget does not allow, when the
/// engine's conflict is one; `None` for any other conflict.
pub fn resume_refusal(run_dir: &Path, run_id: &str, detail: &str) -> Option<Value> {
    if !detail.starts_with(BUDGET_PAUSED) {
        return None;
    }
    let mut out = json!({
        "error": BUDGET_PAUSED,
        "detail": detail,
        "next": BUDGET_PAUSE_NEXT,
    });
    if let Ok(view) = RunView::load(run_dir, run_id)
        && let Some(b) = view.budget(run_dir)
    {
        out["budget"] = json!(b);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_refuse_zero_amounts_and_unknown_policies_and_empty_is_none() {
        assert_eq!(start_request(None, None, None).unwrap(), None);
        assert_eq!(
            start_request(Some(0), None, None).unwrap_err()["error"],
            "invalid_budget"
        );
        assert!(start_request(None, Some(-1.0), None).is_err());
        assert!(start_request(None, None, Some("maybe")).is_err());
        let r = start_request(Some(5), None, Some("Continue"))
            .unwrap()
            .unwrap();
        assert_eq!(r.on_no_estimate, Some(OnNoEstimate::Continue));
        let none = ResumeArgs {
            raise_tokens: None,
            raise_usd: None,
            tokens: None,
            usd: None,
            on_no_estimate: None,
            once: false,
        };
        assert!(resume_request(none).unwrap().is_none());
        let both = ResumeArgs {
            raise_tokens: Some(5),
            raise_usd: None,
            tokens: Some(10),
            usd: None,
            on_no_estimate: None,
            once: false,
        };
        assert_eq!(resume_request(both).unwrap_err()["error"], "invalid_budget");
    }

    #[test]
    fn a_budget_nonce_binds_the_change_and_the_pause() {
        let raise = BudgetResume {
            raise_tokens: Some(1000),
            ..BudgetResume::default()
        };
        let once = BudgetResume {
            once: true,
            ..BudgetResume::default()
        };
        let n = budget_nonce("r1", Some(7), &raise);
        assert_eq!(n, budget_nonce("r1", Some(7), &raise));
        assert_ne!(n, budget_nonce("r1", Some(9), &raise));
        assert_ne!(n, budget_nonce("r1", Some(7), &once));
        assert_ne!(n, budget_nonce("r2", Some(7), &raise));
    }
}
