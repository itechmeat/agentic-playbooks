//! The checks of one eval repetition (design C2 section 6). All of them read
//! the run journal, the tree after the run and the case's own scripts; none
//! asks a model. The playbook's goal criteria are read from their
//! `goal_checked` events, never re-run.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use apb_core::eval::{EvalCase, GoalMode};
use apb_core::schema::{GoalCheck, Playbook};
use serde::{Deserialize, Serialize};

use super::doubles::{ArgsKept, CaptureError, CapturedCall};
use crate::event::{Event, EventPayload};
use crate::run_view::RunUsage;
use crate::state::{NodeStatus, RunState};

/// How long one case script may run (the goal-script cap).
pub const CASE_SCRIPT_TIMEOUT: Duration = Duration::from_secs(600);
/// The tail of a script's output kept as the detail.
const DETAIL_TAIL: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    /// The check could not run; counts as not passed and is reported apart.
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    /// `run.outcome`, `goal`, `route.visits`, `outputs[post_pr]`, `files[a]`,
    /// `script[scripts/x.sh]`, ...
    pub kind: String,
    pub status: CheckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CheckResult {
    fn new(kind: impl Into<String>, ok: bool, detail: impl FnOnce() -> String) -> Self {
        CheckResult {
            kind: kind.into(),
            status: if ok {
                CheckStatus::Passed
            } else {
                CheckStatus::Failed
            },
            detail: (!ok).then(detail),
        }
    }

    fn error(kind: impl Into<String>, detail: String) -> Self {
        CheckResult {
            kind: kind.into(),
            status: CheckStatus::Error,
            detail: Some(detail),
        }
    }
}

/// One `goal_checked` result as the eval reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalResult {
    pub index: usize,
    pub description: String,
    pub check: String,
    pub status: String,
}

/// Tokens and cost of one repetition, from `attempt_finished.usage`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// `cost_usd` split by basis; empty in a result an older apb stored.
    #[serde(
        default,
        skip_serializing_if = "crate::run_view::CostByBasis::is_empty"
    )]
    pub cost_by_basis: crate::run_view::CostByBasis,
}

impl RepUsage {
    pub fn from_events(events: &[Event]) -> Self {
        match RunUsage::from_events(events) {
            Some(u) => RepUsage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cost_usd: u.cost_usd,
                cost_by_basis: u.cost_by_basis,
            },
            None => RepUsage::default(),
        }
    }

    pub fn tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// The cost split by basis; a cost an older apb stored without a split
    /// is of unknown basis. Empty without a cost.
    pub fn cost_split(&self) -> crate::run_view::CostByBasis {
        match self.cost_usd {
            Some(c) if self.cost_by_basis.is_empty() => crate::run_view::CostByBasis {
                unknown_basis_usd: Some(c),
                ..Default::default()
            },
            _ => self.cost_by_basis,
        }
    }
}

/// Whether `t` is an event type this apb's journal can hold. Asked of the
/// event type itself (serde names every variant it does not know), so the
/// list can never drift from the journal.
pub fn is_event_type(t: &str) -> bool {
    match serde_json::from_value::<EventPayload>(serde_json::json!({ "type": t })) {
        Ok(_) => true,
        Err(e) => !e.to_string().contains("unknown variant"),
    }
}

/// [`apb_core::eval::validate_suite`] with the journal's event types and
/// the installed connectors' facts (V280 checks doubles against them).
pub fn validate_suite(playbook_dir: &Path, playbook: &Playbook) -> Vec<apb_core::validate::Issue> {
    apb_core::eval::validate_suite_with(
        playbook_dir,
        playbook,
        &is_event_type,
        &apb_core::connector::resolve::validation_facts(),
    )
}

/// What the checks read.
pub struct CheckInput<'a> {
    pub case: &'a EvalCase,
    /// The version the run executed.
    pub playbook: &'a Playbook,
    /// The run's journal; empty when the run never started.
    pub events: &'a [Event],
    /// The run's `events.jsonl` lines as JSON, for event-type checks.
    pub raw_types: &'a [String],
    pub tree: &'a Path,
    /// The commit the fixture materialized (the tree's tip before the run).
    pub fixture_commit: &'a str,
    /// The suite directory the case scripts run from.
    pub suite_dir: &'a Path,
    /// Set when the runner stopped the run (a gate, the timeout, a budget).
    pub stopped: Option<&'a str>,
    /// The case env overlay, then `APB_EVAL_*` and the run ids, for the
    /// case scripts.
    pub script_env: Vec<(String, String)>,
    /// An empty directory: the hooks path of every git call the checks and
    /// the case scripts make in the tree (see [`super::git`]).
    pub hooks_dir: &'a Path,
    /// The calls the case's doubles answered
    /// ([`super::doubles::read_calls`]), or why the capture does not
    /// read; `Ok(&[])` for a run without doubles.
    pub calls: Result<&'a [CapturedCall], &'a CaptureError>,
}

/// The run's terminal outcome as the checks see it: `stopped` when the
/// runner stopped it, else the journal's `run_finished`, else `incomplete`.
pub fn outcome(events: &[Event], stopped: Option<&str>) -> String {
    if stopped.is_some() {
        return "stopped".into();
    }
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunFinished { outcome } => Some(outcome.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "incomplete".into())
}

/// Wall clock from `run_started` to the last event.
pub fn duration_ms(events: &[Event]) -> Option<u64> {
    let start = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunStarted { .. } => Some(e.ts),
        _ => None,
    })?;
    let end = events.last()?.ts;
    u64::try_from(end.saturating_sub(start)).ok()
}

/// The `type` of every journal line, in order (unknown types included).
pub fn raw_event_types(run_dir: &Path) -> Vec<String> {
    std::fs::read_to_string(run_dir.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
        .collect()
}

fn tail(text: &str) -> String {
    let text = text.trim();
    let n = text.chars().count();
    text.chars().skip(n.saturating_sub(DETAIL_TAIL)).collect()
}

fn goal_results(events: &[Event]) -> Vec<GoalResult> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::GoalChecked {
                index,
                description,
                check,
                status,
                ..
            } => Some(GoalResult {
                index: *index,
                description: description.clone(),
                check: check.clone(),
                status: status.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn goal_check(input: &CheckInput, goal: &[GoalResult], accepted: &[String]) -> Option<CheckResult> {
    if input.case.checks.goal != GoalMode::Required {
        return None;
    }
    let criteria = input.playbook.goal.as_ref().map(|g| {
        g.criteria
            .iter()
            .filter(|c| !matches!(c.check, GoalCheck::Manual))
            .count()
    });
    if criteria.unwrap_or(0) == 0 {
        return None;
    }
    let checked: Vec<&GoalResult> = goal.iter().filter(|g| g.check != "manual").collect();
    if checked.is_empty() {
        let stopped_ok = input.stopped.is_some() && accepted.iter().any(|o| o == "stopped");
        return Some(if stopped_ok {
            CheckResult {
                kind: "goal".into(),
                status: CheckStatus::Passed,
                detail: Some("not_reached: the run was stopped before a finish node".into()),
            }
        } else {
            CheckResult::new("goal", false, || {
                "the run ended before a finish node, no criterion was checked".into()
            })
        });
    }
    let bad: Vec<String> = checked
        .iter()
        .filter(|g| g.status != "passed")
        .map(|g| format!("{} ({}) {}", g.index + 1, g.description, g.status))
        .collect();
    Some(CheckResult::new("goal", bad.is_empty(), || {
        format!("criteria not passed: {}", bad.join("; "))
    }))
}

/// The nodes a run visited, in order, once per execution: every
/// `node_started`, every `review_requested` (a gate the run reached), plus
/// a `node_finished` with no open start (a finish or start node, which the
/// engine journals without `node_started`).
fn visited(events: &[Event]) -> Vec<&str> {
    let mut open: BTreeMap<&str, usize> = BTreeMap::new();
    let mut out = Vec::new();
    for e in events {
        match &e.payload {
            EventPayload::NodeStarted { node, .. } => {
                *open.entry(node.as_str()).or_default() += 1;
                out.push(node.as_str());
            }
            EventPayload::ReviewRequested { node, .. } => {
                *open.entry(node.as_str()).or_default() += 1;
                out.push(node.as_str());
            }
            EventPayload::NodeFinished { node, .. } => match open.get_mut(node.as_str()) {
                Some(n) if *n > 0 => *n -= 1,
                _ => out.push(node.as_str()),
            },
            _ => {}
        }
    }
    out
}

fn route_checks(input: &CheckInput, out: &mut Vec<CheckResult>) {
    let Some(route) = &input.case.checks.route else {
        return;
    };
    let started = visited(input.events);
    if !route.visits.is_empty() {
        let missing: Vec<&String> = route
            .visits
            .iter()
            .filter(|v| !started.contains(&v.as_str()))
            .collect();
        let in_order = !route.in_order || {
            let mut it = started.iter();
            route.visits.iter().all(|v| it.any(|s| s == v))
        };
        out.push(CheckResult::new(
            "route.visits",
            missing.is_empty() && in_order,
            || {
                if missing.is_empty() {
                    format!("visited out of order: {}", started.join(" -> "))
                } else {
                    format!(
                        "not visited: {}; route: {}",
                        missing
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        started.join(" -> ")
                    )
                }
            },
        ));
    }
    if !route.not_visits.is_empty() {
        let hit: Vec<&String> = route
            .not_visits
            .iter()
            .filter(|v| started.contains(&v.as_str()))
            .collect();
        out.push(CheckResult::new("route.not_visits", hit.is_empty(), || {
            format!(
                "visited: {}",
                hit.iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }));
    }
    for (node, max) in &route.max_visits {
        let n = started.iter().filter(|s| *s == node).count();
        out.push(CheckResult::new(
            format!("route.max_visits[{node}]"),
            n <= *max as usize,
            || format!("{n} visits, at most {max}"),
        ));
    }
}

fn regex_is_match(pattern: &str, text: &str) -> Result<bool, String> {
    regex::Regex::new(pattern)
        .map(|r| r.is_match(text))
        .map_err(|e| e.to_string())
}

fn output_checks(input: &CheckInput, state: &RunState, out: &mut Vec<CheckResult>) {
    for o in &input.case.checks.outputs {
        let kind = match &o.field {
            Some(f) => format!("outputs[{}.{f}]", o.node),
            None => format!("outputs[{}]", o.node),
        };
        let Some(text) = state.outputs.get(&o.node) else {
            out.push(CheckResult::new(kind, false, || {
                format!("node `{}` recorded no output", o.node)
            }));
            continue;
        };
        let value = match &o.field {
            None => Some(text.clone()),
            Some(f) => serde_json::from_str::<serde_json::Value>(text.trim())
                .ok()
                .and_then(|v| v.get(f).cloned())
                .map(|v| match v {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                }),
        };
        let Some(value) = value else {
            out.push(CheckResult::new(kind, false, || {
                "the output has no such field".into()
            }));
            continue;
        };
        let mut problems = Vec::new();
        if let Some(eq) = &o.equals
            && value.trim() != eq.trim()
        {
            problems.push(format!("does not equal `{eq}`"));
        }
        if o.non_empty == Some(true) && value.trim().is_empty() {
            problems.push("is empty".to_string());
        }
        for (pattern, want) in [(&o.matches, true), (&o.not_matches, false)] {
            let Some(p) = pattern else { continue };
            match regex_is_match(p, &value) {
                Ok(m) if m == want => {}
                Ok(_) if want => problems.push(format!("does not match `{p}`")),
                Ok(_) => problems.push(format!("matches `{p}`")),
                Err(e) => {
                    out.push(CheckResult::error(kind.clone(), e));
                    problems.clear();
                    break;
                }
            }
        }
        if out
            .last()
            .is_some_and(|c| c.kind == kind && c.status == CheckStatus::Error)
        {
            continue;
        }
        out.push(CheckResult::new(kind, problems.is_empty(), || {
            format!("the output {}", problems.join(", "))
        }));
    }
}

/// The file at `rel` in the fixture commit, `None` when it has none.
/// The tree's `.git` is the agent's to edit, so the read is hardened.
fn fixture_file(
    hooks: &Path,
    tree: &Path,
    commit: &str,
    rel: &str,
) -> Result<Option<Vec<u8>>, String> {
    let out = super::git::command(hooks)
        .arg("-C")
        .arg(tree)
        .args(["cat-file", "blob", &format!("{commit}:{rel}")])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(out.status.success().then_some(out.stdout))
}

/// The checked file's bytes, `None` when absent. A symlink at the path, or
/// on any directory below the tree, is never followed: the agent could
/// point it anywhere on the operator's filesystem.
fn read_checked(tree: &Path, rel: &str) -> Result<Option<Vec<u8>>, String> {
    let path = tree.join(rel);
    apb_core::fsutil::ensure_no_symlink_below(tree, &path)
        .map_err(|_| format!("`{rel}` is a symlink or lies below one, which is not followed"))?;
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() => std::fs::read(&path).map(Some).map_err(|e| e.to_string()),
        Ok(_) => Err(format!("`{rel}` is not a regular file")),
        Err(_) => Ok(None),
    }
}

fn file_checks(input: &CheckInput, out: &mut Vec<CheckResult>) {
    for f in &input.case.checks.files {
        let kind = format!("files[{}]", f.path);
        let now = match read_checked(input.tree, &f.path) {
            Ok(n) => n,
            Err(why) => {
                out.push(CheckResult::new(kind, false, || why));
                continue;
            }
        };
        let mut problems = Vec::new();
        if let Some(want) = f.exists
            && want != now.is_some()
        {
            problems.push(if want { "does not exist" } else { "exists" }.to_string());
        }
        if let Some(p) = &f.matches {
            let Some(bytes) = now.as_deref() else {
                problems.push(format!("does not exist, so it cannot match `{p}`"));
                out.push(CheckResult::new(kind, false, || problems.join(", ")));
                continue;
            };
            let text = String::from_utf8_lossy(bytes);
            match regex_is_match(p, &text) {
                Ok(true) => {}
                Ok(false) => problems.push(format!("does not match `{p}`")),
                Err(e) => {
                    out.push(CheckResult::error(kind, e));
                    continue;
                }
            }
        }
        if f.unchanged_from_fixture == Some(true) {
            match fixture_file(input.hooks_dir, input.tree, input.fixture_commit, &f.path) {
                Ok(before) if before == now => {}
                Ok(_) => problems.push("changed from the fixture".to_string()),
                Err(e) => {
                    out.push(CheckResult::error(kind, e));
                    continue;
                }
            }
        }
        out.push(CheckResult::new(kind, problems.is_empty(), || {
            problems.join(", ")
        }));
    }
}

fn event_checks(input: &CheckInput, out: &mut Vec<CheckResult>) {
    let count = |t: &str| input.raw_types.iter().filter(|x| *x == t).count();
    if let Some(ev) = &input.case.checks.events {
        for t in &ev.absent {
            let n = count(t);
            out.push(CheckResult::new(
                format!("events.absent[{t}]"),
                n == 0,
                || format!("{n} `{t}` event(s)"),
            ));
        }
        for (t, max) in &ev.max {
            let n = count(t);
            out.push(CheckResult::new(
                format!("events.max[{t}]"),
                n <= *max as usize,
                || format!("{n} `{t}` event(s), at most {max}"),
            ));
        }
    }
    if input.case.checks.deliverables.is_some() {
        let n = count("deliverable_missing") + count("output_fields_missing");
        out.push(CheckResult::new("deliverables", n == 0, || {
            format!("{n} missing deliverable or output field event(s)")
        }));
    }
}

fn script_checks(input: &CheckInput, out: &mut Vec<CheckResult>) {
    for s in &input.case.checks.scripts {
        let kind = format!("script[{s}]");
        // The scripts run after untrusted agent activity in the tree: their
        // own git calls get the same hardening as apb's.
        let mut env: Vec<(&str, String)> = input
            .script_env
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        env.push((
            "GIT_CONFIG_PARAMETERS",
            super::git::config_parameters(input.hooks_dir),
        ));
        env.push(("GIT_CONFIG_NOSYSTEM", "1".into()));
        match crate::script::run_script_with_env_removed(
            input.suite_dir,
            input.tree,
            s,
            "sh",
            Some(CASE_SCRIPT_TIMEOUT),
            None,
            &env,
            &super::git::LOCATION_VARS,
        ) {
            Ok(r) if r.status == NodeStatus::Succeeded => {
                out.push(CheckResult::new(kind, true, String::new))
            }
            Ok(r) if r.status == NodeStatus::Failed => {
                let t = tail(&r.stdout);
                out.push(CheckResult::new(kind, false, || {
                    if t.is_empty() {
                        "exited non-zero".into()
                    } else {
                        format!("exited non-zero: {t}")
                    }
                }))
            }
            Ok(r) => out.push(CheckResult::error(
                kind,
                format!("ended {}", r.status.as_str()),
            )),
            Err(e) => out.push(CheckResult::error(kind, e.to_string())),
        }
    }
}

/// Every check of the case over one finished (or stopped) repetition, in a
/// fixed order: outcome, goal, route, outputs, files, events, scripts.
pub fn evaluate(input: &CheckInput) -> (Vec<CheckResult>, Vec<GoalResult>) {
    let mut out = Vec::new();
    let accepted = input
        .case
        .checks
        .run
        .as_ref()
        .map(|r| r.outcome.clone())
        .unwrap_or_else(|| vec!["succeeded".to_string()]);
    let outcome = outcome(input.events, input.stopped);
    out.push(CheckResult::new(
        "run.outcome",
        accepted.contains(&outcome),
        || {
            let why = input.stopped.map(|s| format!(" ({s})")).unwrap_or_default();
            format!(
                "outcome `{outcome}`{why}, expected one of {}",
                accepted.join(", ")
            )
        },
    ));
    let goal = goal_results(input.events);
    if let Some(c) = goal_check(input, &goal, &accepted) {
        out.push(c);
    }
    let state = RunState::fold(input.events);
    route_checks(input, &mut out);
    output_checks(input, &state, &mut out);
    file_checks(input, &mut out);
    event_checks(input, &mut out);
    connector_checks(input, &mut out);
    script_checks(input, &mut out);
    (out, goal)
}

/// `connector.function at site` of the first five of `calls`.
fn first_five(calls: &[&CapturedCall]) -> String {
    calls
        .iter()
        .take(5)
        .map(|c| format!("{}.{} at {}", c.connector, c.function, c.site))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The checks of a case with doubles over the calls they answered: every
/// dispatched call answered and delivered (`connector_doubles`), and each
/// `checks.connector_calls` entry. A capture that does not read, a
/// reservation never delivered and args not kept whole never pass a check
/// over the calls they hide: such a check fails as indeterminate.
fn connector_checks(input: &CheckInput, out: &mut Vec<CheckResult>) {
    if input.case.doubles.is_empty() {
        return;
    }
    let calls = match input.calls {
        Ok(calls) => calls,
        Err(e) => {
            out.push(CheckResult::new("connector_doubles", false, || {
                if e.line == 0 {
                    e.reason.clone()
                } else {
                    format!(
                        "the call capture does not read (line {}): {}",
                        e.line, e.reason
                    )
                }
            }));
            for cc in &input.case.checks.connector_calls {
                out.push(CheckResult::new(
                    format!("connector_calls[{}]", cc.function),
                    false,
                    || "indeterminate: the call capture does not read".to_string(),
                ));
            }
            return;
        }
    };
    // The journal's own record of the doubled calls: every call a doubled
    // run dispatches is journaled `connector_call` (`connector::call`
    // `append_event`, after its answer and before its delivery). A call the
    // journal holds and the capture does not means the capture was removed
    // or cut short, never that no call was made.
    if let Some(why) = capture_gap(input.events, calls) {
        out.push(CheckResult::new("connector_doubles", false, || why.clone()));
        for cc in &input.case.checks.connector_calls {
            out.push(CheckResult::new(
                format!("connector_calls[{}]", cc.function),
                false,
                || format!("indeterminate: {why}"),
            ));
        }
        return;
    }
    let unmatched: Vec<&CapturedCall> = calls
        .iter()
        .filter(|c| c.delivered && c.answer.is_none())
        .collect();
    let undelivered: Vec<&CapturedCall> = calls.iter().filter(|c| !c.delivered).collect();
    out.push(CheckResult::new(
        "connector_doubles",
        unmatched.is_empty() && undelivered.is_empty(),
        || {
            let mut why = Vec::new();
            if !unmatched.is_empty() {
                why.push(format!(
                    "{} call(s) no double answered: {}",
                    unmatched.len(),
                    first_five(&unmatched)
                ));
            }
            if !undelivered.is_empty() {
                why.push(format!(
                    "{} call(s) reserved and never delivered: {}",
                    undelivered.len(),
                    first_five(&undelivered)
                ));
            }
            why.join("; ")
        },
    ));
    for cc in &input.case.checks.connector_calls {
        out.push(call_check(cc, calls));
    }
}

/// Why the capture does not account for the doubled calls the run journal
/// records (its `connector_call` events, one per dispatched call of a
/// doubled run): `capture_missing` when the capture holds no call while
/// the journal holds some, `capture_incomplete` when a function has fewer
/// dispatches (reservations and their replays) than journaled calls. `None` when every journaled call is
/// in the capture (the capture may hold more: a call whose process died
/// before its journal line, which fails as never delivered).
fn capture_gap(events: &[Event], calls: &[CapturedCall]) -> Option<String> {
    let mut journaled: std::collections::BTreeMap<(String, String), usize> =
        std::collections::BTreeMap::new();
    for e in events {
        if let EventPayload::ConnectorCall {
            connector,
            function,
            ..
        } = &e.payload
        {
            *journaled
                .entry((connector.clone(), function.clone()))
                .or_insert(0) += 1;
        }
    }
    let total: usize = journaled.values().sum();
    if total == 0 {
        return None;
    }
    if calls.is_empty() {
        return Some(format!(
            "capture_missing: the run journal records {total} doubled call(s) and the call capture holds none"
        ));
    }
    let short: Vec<String> = journaled
        .iter()
        .filter_map(|((c, f), n)| {
            // Each reservation is one dispatch, and each replay of it one
            // more, each journaled on its own.
            let held: usize = calls
                .iter()
                .filter(|x| &x.connector == c && &x.function == f)
                .map(|x| 1 + x.replays as usize)
                .sum();
            (held < *n).then(|| format!("{c}.{f} ({n} journaled, {held} captured)"))
        })
        .collect();
    (!short.is_empty()).then(|| {
        format!(
            "capture_incomplete: the call capture holds fewer calls than the run journal records: {}",
            short.join(", ")
        )
    })
}

/// One `connector_calls[<function>]` check over the captured calls.
fn call_check(cc: &apb_core::eval_doubles::CallCheck, calls: &[CapturedCall]) -> CheckResult {
    let kind = format!("connector_calls[{}]", cc.function);
    let (connector, function) = cc.function.split_once('.').unwrap_or((&cc.function, ""));
    let selects = |c: &&CapturedCall| {
        c.connector == connector
            && c.function == function
            && cc.site.as_deref().is_none_or(|s| s == c.site)
    };
    let undelivered = calls
        .iter()
        .filter(selects)
        .filter(|c| !c.delivered)
        .count();
    if undelivered > 0 {
        return CheckResult::new(kind, false, || {
            format!(
                "indeterminate: {undelivered} call(s) of {} reserved and never delivered",
                cc.function
            )
        });
    }
    let selected: Vec<&CapturedCall> = calls.iter().filter(selects).collect();
    if cc.args.is_some() || cc.contains.is_some() {
        let partial = selected
            .iter()
            .filter(|c| c.args_kept != ArgsKept::Whole)
            .count();
        if partial > 0 {
            return CheckResult::new(kind, false, || {
                format!(
                    "indeterminate: {partial} call(s) of {} kept no whole args (clipped or withheld)",
                    cc.function
                )
            });
        }
    }
    let k = selected
        .iter()
        .filter(|c| {
            cc.args
                .as_ref()
                .is_none_or(|p| apb_core::eval_doubles::args_hold(p, &c.args))
                && cc
                    .contains
                    .as_deref()
                    .is_none_or(|t| apb_core::eval_doubles::args_contain(&c.args, t))
        })
        .count() as u32;
    let ok = match (cc.count, cc.min, cc.max) {
        (None, None, None) => k > 0,
        (count, min, max) => {
            count.is_none_or(|n| k == n) && min.is_none_or(|n| k >= n) && max.is_none_or(|n| k <= n)
        }
    };
    CheckResult::new(kind, ok, || {
        let rule = match (cc.count, cc.min, cc.max) {
            (Some(n), ..) => format!("exactly {n}"),
            (None, Some(lo), Some(hi)) => format!("{lo} to {hi}"),
            (None, Some(lo), None) => format!("at least {lo}"),
            (None, None, Some(hi)) => format!("at most {hi}"),
            (None, None, None) => "at least 1".to_string(),
        };
        format!("{k} matching call(s), expected {rule}")
    })
}

/// `passed` when every check passed, `error` when none failed but one could
/// not run, else `failed`.
pub fn verdict(checks: &[CheckResult]) -> &'static str {
    if checks.iter().all(|c| c.status == CheckStatus::Passed) {
        "passed"
    } else if checks.iter().any(|c| c.status == CheckStatus::Failed) {
        "failed"
    } else {
        "error"
    }
}

/// How often each check kind did not pass across repetitions, most first.
pub(crate) fn failing_kinds<'a>(
    reps: impl Iterator<Item = &'a [CheckResult]>,
) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for checks in reps {
        for c in checks.iter().filter(|c| c.status != CheckStatus::Passed) {
            *counts.entry(c.kind.clone()).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: 1000 + seq as u128 * 10,
            payload,
        }
    }

    fn journal() -> Vec<Event> {
        vec![
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into(), definition: None , external_data: false,}),
            ev(1, serde_json::from_value(serde_json::json!({"type":"node_finished","node":"s","status":"succeeded","attempt":1,"output":"","artifacts":[]})).unwrap()),
            ev(2, EventPayload::NodeStarted { node: "w".into(), attempt: 1 }),
            ev(3, serde_json::from_value(serde_json::json!({"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"{\"verdict\":\"ok\",\"n\":3}","artifacts":[]})).unwrap()),
            ev(4, EventPayload::NodeStarted { node: "w".into(), attempt: 1 }),
            ev(5, serde_json::from_value(serde_json::json!({"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"{\"verdict\":\"ok\",\"n\":3}","artifacts":[]})).unwrap()),
            ev(6, serde_json::from_value(serde_json::json!({"type":"review_requested","node":"g","options":["approve"]})).unwrap()),
            ev(7, serde_json::from_value(serde_json::json!({"type":"node_finished","node":"f","status":"succeeded","attempt":1,"output":"","artifacts":[]})).unwrap()),
            ev(8, EventPayload::RunFinished { outcome: "succeeded".into() }),
        ]
    }

    fn case(yaml: &str) -> EvalCase {
        yaml_serde::from_str(yaml).unwrap()
    }

    const PB: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: w, type: prompt, prompt: hi }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: w }\n  - { from: w, to: f }\n";

    fn run(case_yaml: &str) -> Vec<CheckResult> {
        let c = case(case_yaml);
        let pb: Playbook = yaml_serde::from_str(PB).unwrap();
        let events = journal();
        let tree = tempfile::tempdir().unwrap();
        let input = CheckInput {
            case: &c,
            playbook: &pb,
            events: &events,
            raw_types: &[
                "run_started".into(),
                "retry_started".into(),
                "retry_started".into(),
            ],
            tree: tree.path(),
            fixture_commit: "HEAD",
            suite_dir: tree.path(),
            stopped: None,
            script_env: Vec::new(),
            hooks_dir: tree.path(),
            calls: Ok(&[]),
        };
        evaluate(&input).0
    }

    fn status(checks: &[CheckResult], kind: &str) -> CheckStatus {
        checks
            .iter()
            .find(|c| c.kind == kind)
            .unwrap_or_else(|| panic!("{kind} in {checks:?}"))
            .status
    }

    #[test]
    fn route_output_and_event_checks_read_the_journal() {
        let checks = run(
            "schema: 1\nid: c\nfixture: { dir: fixtures/x }\nchecks:\n  route: { visits: [s, w, g, f], in_order: true, not_visits: [f], max_visits: { w: 1, f: 1, g: 1 } }\n  outputs:\n    - { node: w, field: verdict, equals: ok }\n    - { node: w, field: n, equals: \"3\" }\n    - { node: w, not_matches: verdict }\n    - { node: w, field: missing, non_empty: true }\n  events: { absent: [run_error], max: { retry_started: 1 } }\n",
        );
        assert_eq!(status(&checks, "run.outcome"), CheckStatus::Passed);
        assert_eq!(status(&checks, "route.visits"), CheckStatus::Passed);
        assert_eq!(status(&checks, "route.not_visits"), CheckStatus::Failed);
        // Two executions of `w`; the finish node (no `node_started`) and the
        // gate (`review_requested`) count once each.
        assert_eq!(status(&checks, "route.max_visits[w]"), CheckStatus::Failed);
        assert_eq!(status(&checks, "route.max_visits[f]"), CheckStatus::Passed);
        assert_eq!(status(&checks, "route.max_visits[g]"), CheckStatus::Passed);
        assert_eq!(status(&checks, "outputs[w.verdict]"), CheckStatus::Passed);
        assert_eq!(status(&checks, "outputs[w.n]"), CheckStatus::Passed);
        assert_eq!(status(&checks, "outputs[w]"), CheckStatus::Failed);
        assert_eq!(status(&checks, "outputs[w.missing]"), CheckStatus::Failed);
        assert_eq!(
            status(&checks, "events.absent[run_error]"),
            CheckStatus::Passed
        );
        assert_eq!(
            status(&checks, "events.max[retry_started]"),
            CheckStatus::Failed
        );
        assert_eq!(verdict(&checks), "failed");
        let order = run(
            "schema: 1\nid: c\nfixture: { dir: fixtures/x }\nchecks:\n  route: { visits: [f, w], in_order: true }\n",
        );
        assert_eq!(status(&order, "route.visits"), CheckStatus::Failed);
    }

    #[test]
    fn a_missing_script_is_an_error_not_a_failure() {
        let checks = run(
            "schema: 1\nid: c\nfixture: { dir: fixtures/x }\nchecks:\n  scripts: [scripts/none.sh]\n",
        );
        assert_eq!(
            status(&checks, "script[scripts/none.sh]"),
            CheckStatus::Error
        );
        assert_eq!(verdict(&checks), "error");
    }

    /// K2: an event type this apb never writes is a V80, a real one is not.
    #[test]
    fn v80_names_an_event_type_the_journal_does_not_have() {
        for t in [
            "run_error",
            "run_finished",
            "list_started",
            "list_batch_started",
            "list_item_settled",
        ] {
            assert!(is_event_type(t), "{t}");
        }
        assert!(!is_event_type("run_eror"));
        let dir = tempfile::tempdir().unwrap();
        let evals = dir.path().join("evals");
        std::fs::create_dir_all(evals.join("fixtures/x")).unwrap();
        std::fs::write(
            evals.join("c.yaml"),
            "schema: 1\nid: c\nfixture: { dir: fixtures/x }\nchecks:\n  events: { absent: [run_eror, run_error], max: { retry_startd: 1 } }\n",
        )
        .unwrap();
        let pb: Playbook = yaml_serde::from_str(PB).unwrap();
        let got: Vec<String> = validate_suite(dir.path(), &pb)
            .into_iter()
            .map(|i| format!("{} {}", i.code, i.message))
            .collect();
        assert_eq!(
            got,
            [
                "V80 eval case `c`: events: `retry_startd` is not an event type of this apb",
                "V80 eval case `c`: events: `run_eror` is not an event type of this apb",
            ]
        );
    }

    /// K3: the `files` checks over a real tree with a fixture commit: a
    /// changed file, a file that should not exist, a pattern over a missing
    /// file and a symlinked path all fail with their reason.
    #[cfg(unix)]
    #[test]
    fn file_checks_fail_with_their_reason() {
        let tree = tempfile::tempdir().unwrap();
        let t = tree.path();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(t)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(t.join("kept.txt"), "same\n").unwrap();
        std::fs::write(t.join("edited.txt"), "before\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "fixture"]);
        std::fs::write(t.join("edited.txt"), "after\n").unwrap();
        std::fs::write(t.join("present.txt"), "x\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "secret\n").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), t.join("link.txt")).unwrap();
        let c = case(
            "schema: 1\nid: c\nfixture: { dir: fixtures/x }\nchecks:\n  files:\n    - { path: kept.txt, unchanged_from_fixture: true }\n    - { path: edited.txt, unchanged_from_fixture: true }\n    - { path: present.txt, exists: false }\n    - { path: missing.txt, matches: \"^\" }\n    - { path: link.txt, matches: secret }\n",
        );
        let pb: Playbook = yaml_serde::from_str(PB).unwrap();
        let hooks = tempfile::tempdir().unwrap();
        let input = CheckInput {
            case: &c,
            playbook: &pb,
            events: &journal(),
            raw_types: &[],
            tree: t,
            fixture_commit: "HEAD",
            suite_dir: t,
            stopped: None,
            script_env: Vec::new(),
            hooks_dir: hooks.path(),
            calls: Ok(&[]),
        };
        let mut out = Vec::new();
        file_checks(&input, &mut out);
        let got: Vec<(String, CheckStatus, Option<String>)> = out
            .into_iter()
            .map(|c| (c.kind, c.status, c.detail))
            .collect();
        let row = |k: &str| got.iter().find(|(kind, ..)| kind == k).unwrap().clone();
        assert_eq!(row("files[kept.txt]").1, CheckStatus::Passed);
        assert_eq!(
            row("files[edited.txt]").2.as_deref(),
            Some("changed from the fixture")
        );
        assert_eq!(row("files[present.txt]").2.as_deref(), Some("exists"));
        let missing = row("files[missing.txt]");
        assert_eq!(missing.1, CheckStatus::Failed);
        assert!(missing.2.unwrap().starts_with("does not exist"));
        let link = row("files[link.txt]");
        assert_eq!(link.1, CheckStatus::Failed);
        assert!(link.2.unwrap().contains("symlink"), "the link was followed");
    }

    const DOUBLED: &str = "schema: 1\nid: c\nfixture: { dir: fixtures/x }\nconnectors:\n  crm:\n    find:\n      - answer: { status: 200, body: {} }\n    update:\n      - answer: { status: 200, body: {} }\nchecks:\n  connector_calls:\n";

    fn captured(function: &str, args: serde_json::Value, answered: bool) -> CapturedCall {
        CapturedCall {
            seq: 0,
            site: "w".into(),
            connector: "crm".into(),
            function: function.into(),
            account: "main".into(),
            item: None,
            call_key: "k".into(),
            args,
            args_kept: ArgsKept::Whole,
            answer: answered.then_some(0),
            delivered: true,
            replays: 0,
        }
    }

    /// The connector checks of a doubled case whose `connector_calls`
    /// entries are `entries` (YAML list lines), over `calls`.
    fn doubled(
        entries: &str,
        calls: Result<&[CapturedCall], &CaptureError>,
    ) -> Vec<(String, CheckStatus, Option<String>)> {
        let c = case(&format!("{DOUBLED}{entries}"));
        let pb: Playbook = yaml_serde::from_str(PB).unwrap();
        let tree = tempfile::tempdir().unwrap();
        let input = CheckInput {
            case: &c,
            playbook: &pb,
            events: &[],
            raw_types: &[],
            tree: tree.path(),
            fixture_commit: "HEAD",
            suite_dir: tree.path(),
            stopped: None,
            script_env: Vec::new(),
            hooks_dir: tree.path(),
            calls,
        };
        let mut out = Vec::new();
        connector_checks(&input, &mut out);
        out.into_iter()
            .map(|c| (c.kind, c.status, c.detail))
            .collect()
    }

    fn row<'a>(
        rows: &'a [(String, CheckStatus, Option<String>)],
        kind: &str,
    ) -> &'a (String, CheckStatus, Option<String>) {
        rows.iter()
            .find(|r| r.0 == kind)
            .unwrap_or_else(|| panic!("{kind} in {rows:?}"))
    }

    #[test]
    fn connector_calls_count_min_max_args_contains() {
        let calls = [
            captured("find", serde_json::json!({ "q": "acme corp" }), true),
            captured("find", serde_json::json!({ "q": "other" }), true),
            captured(
                "update",
                serde_json::json!({ "id": 1, "set": { "a": 2 } }),
                true,
            ),
        ];
        let rows = doubled(
            "    - { function: crm.find, count: 2 }\n    - { function: crm.find, contains: acme, min: 1, max: 1 }\n    - { function: crm.update, args: { set: { a: 2 } } }\n    - { function: crm.update, args: { id: 2 } }\n    - { function: crm.find, site: elsewhere, max: 0 }\n    - { function: crm.find, min: 3 }\n",
            Ok(&calls),
        );
        assert_eq!(row(&rows, "connector_doubles").1, CheckStatus::Passed);
        let statuses: Vec<CheckStatus> = rows[1..].iter().map(|r| r.1).collect();
        assert_eq!(
            statuses,
            [
                CheckStatus::Passed,
                CheckStatus::Passed,
                CheckStatus::Passed,
                CheckStatus::Failed,
                CheckStatus::Passed,
                CheckStatus::Failed,
            ]
        );
        assert_eq!(
            rows[4].2.as_deref(),
            Some("0 matching call(s), expected at least 1")
        );
        assert_eq!(
            rows[6].2.as_deref(),
            Some("2 matching call(s), expected at least 3")
        );
    }

    #[test]
    fn an_unmatched_capture_fails_connector_doubles() {
        let calls = [
            captured("find", serde_json::json!({}), true),
            captured("update", serde_json::json!({}), false),
        ];
        let rows = doubled("    - { function: crm.update }\n", Ok(&calls));
        let d = row(&rows, "connector_doubles");
        assert_eq!(d.1, CheckStatus::Failed);
        assert_eq!(
            d.2.as_deref(),
            Some("1 call(s) no double answered: crm.update at w")
        );
        // The unmatched call was still made: it counts.
        assert_eq!(
            row(&rows, "connector_calls[crm.update]").1,
            CheckStatus::Passed
        );
    }

    #[test]
    fn a_clipped_capture_makes_an_args_check_indeterminate() {
        for kept in [ArgsKept::Clipped, ArgsKept::Withheld] {
            let mut clipped = captured("find", serde_json::json!({ "clipped": true }), true);
            clipped.args_kept = kept;
            let calls = [clipped];
            let rows = doubled(
                "    - { function: crm.find, contains: acme, count: 0 }\n",
                Ok(&calls),
            );
            let r = row(&rows, "connector_calls[crm.find]");
            assert_eq!(r.1, CheckStatus::Failed, "{kept:?}");
            assert_eq!(
                r.2.as_deref(),
                Some(
                    "indeterminate: 1 call(s) of crm.find kept no whole args (clipped or withheld)"
                )
            );
            // Without args or contains every call counts, whole or not.
            let rows = doubled("    - { function: crm.find, count: 1 }\n", Ok(&calls));
            assert_eq!(
                row(&rows, "connector_calls[crm.find]").1,
                CheckStatus::Passed
            );
        }
    }

    #[test]
    fn a_corrupt_capture_line_fails_the_repetition() {
        let e = CaptureError {
            line: 3,
            reason: "the line does not parse".into(),
        };
        let rows = doubled("    - { function: crm.find, max: 5 }\n", Err(&e));
        let d = row(&rows, "connector_doubles");
        assert_eq!(d.1, CheckStatus::Failed);
        assert_eq!(
            d.2.as_deref(),
            Some("the call capture does not read (line 3): the line does not parse")
        );
        let r = row(&rows, "connector_calls[crm.find]");
        assert_eq!(
            (r.1, r.2.as_deref()),
            (
                CheckStatus::Failed,
                Some("indeterminate: the call capture does not read")
            )
        );
    }

    #[test]
    fn an_undelivered_reservation_fails_the_repetition() {
        let mut pending = captured("update", serde_json::json!({ "id": 1 }), true);
        pending.delivered = false;
        let calls = [captured("find", serde_json::json!({}), true), pending];
        let rows = doubled(
            "    - { function: crm.update, count: 0 }\n    - { function: crm.find, count: 1 }\n",
            Ok(&calls),
        );
        let d = row(&rows, "connector_doubles");
        assert_eq!(d.1, CheckStatus::Failed);
        assert_eq!(
            d.2.as_deref(),
            Some("1 call(s) reserved and never delivered: crm.update at w")
        );
        // Never read as "no call": `count: 0` is indeterminate, not passed.
        let r = row(&rows, "connector_calls[crm.update]");
        assert_eq!(
            (r.1, r.2.as_deref()),
            (
                CheckStatus::Failed,
                Some("indeterminate: 1 call(s) of crm.update reserved and never delivered")
            )
        );
        assert_eq!(
            row(&rows, "connector_calls[crm.find]").1,
            CheckStatus::Passed
        );
    }
}
