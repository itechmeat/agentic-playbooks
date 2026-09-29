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
}

impl RepUsage {
    pub fn from_events(events: &[Event]) -> Self {
        match RunUsage::from_events(events) {
            Some(u) => RepUsage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cost_usd: u.cost_usd,
            },
            None => RepUsage::default(),
        }
    }

    pub fn tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
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

/// [`apb_core::eval::validate_suite`] with the journal's event types.
pub fn validate_suite(playbook_dir: &Path, playbook: &Playbook) -> Vec<apb_core::validate::Issue> {
    apb_core::eval::validate_suite_with(playbook_dir, playbook, &is_event_type)
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
pub fn visited(events: &[Event]) -> Vec<&str> {
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
    script_checks(input, &mut out);
    (out, goal)
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
pub fn failing_kinds<'a>(reps: impl Iterator<Item = &'a [CheckResult]>) -> Vec<(String, usize)> {
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
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into() }),
            ev(1, serde_json::from_value(serde_json::json!({"type":"node_finished","node":"s","status":"succeeded","attempt":1,"output":"","artifacts":[]})).unwrap()),
            ev(2, EventPayload::NodeStarted { node: "w".into(), attempt: 1 }),
            ev(3, serde_json::from_value(serde_json::json!({"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"{\"verdict\":\"ok\",\"n\":3}","artifacts":[]})).unwrap()),
            ev(4, EventPayload::NodeStarted { node: "w".into(), attempt: 1 }),
            ev(5, serde_json::from_value(serde_json::json!({"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"{\"verdict\":\"ok\",\"n\":3}","artifacts":[]})).unwrap()),
            ev(6, serde_json::from_value(serde_json::json!({"type":"node_finished","node":"f","status":"succeeded","attempt":1,"output":"","artifacts":[]})).unwrap()),
            ev(7, EventPayload::RunFinished { outcome: "succeeded".into() }),
        ]
    }

    fn case(yaml: &str) -> EvalCase {
        apb_core::eval::parse_case(yaml, "c").unwrap()
    }

    const PB: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: w, type: prompt, prompt: hi }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: w }\n  - { from: w, to: f }\n";

    fn run(case_yaml: &str) -> Vec<CheckResult> {
        let c = case(case_yaml);
        let pb: Playbook = serde_yaml_ng::from_str(PB).unwrap();
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
    fn visited_counts_executions_and_finish_nodes_once() {
        assert_eq!(visited(&journal()), ["s", "w", "w", "f"]);
    }

    #[test]
    fn route_output_and_event_checks_read_the_journal() {
        let checks = run(
            "schema: 1\nid: c\nfixture: { dir: fixtures/x }\nchecks:\n  route: { visits: [s, w, f], in_order: true, not_visits: [f], max_visits: { w: 1 } }\n  outputs:\n    - { node: w, field: verdict, equals: ok }\n    - { node: w, field: n, equals: \"3\" }\n    - { node: w, not_matches: verdict }\n    - { node: w, field: missing, non_empty: true }\n  events: { absent: [run_error], max: { retry_started: 1 } }\n",
        );
        assert_eq!(status(&checks, "run.outcome"), CheckStatus::Passed);
        assert_eq!(status(&checks, "route.visits"), CheckStatus::Passed);
        assert_eq!(status(&checks, "route.not_visits"), CheckStatus::Failed);
        assert_eq!(status(&checks, "route.max_visits[w]"), CheckStatus::Failed);
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
        assert!(is_event_type("run_error"));
        assert!(is_event_type("run_finished"));
        assert!(!is_event_type("run_eror"));
        let dir = tempfile::tempdir().unwrap();
        let evals = dir.path().join("evals");
        std::fs::create_dir_all(evals.join("fixtures/x")).unwrap();
        std::fs::write(
            evals.join("c.yaml"),
            "schema: 1\nid: c\nfixture: { dir: fixtures/x }\nchecks:\n  events: { absent: [run_eror, run_error], max: { retry_startd: 1 } }\n",
        )
        .unwrap();
        let pb: Playbook = serde_yaml_ng::from_str(PB).unwrap();
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
        let pb: Playbook = serde_yaml_ng::from_str(PB).unwrap();
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
}
