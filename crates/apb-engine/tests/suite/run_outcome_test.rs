//! The human run outcome (`OutcomeSummary`) and the run listing's "needs a
//! look" marker, read from a journal written the way the engine writes it.

use std::path::Path;

use apb_engine::run_outcome::summary::{EvidenceSource, GapKind, NeedsLookReason};
use apb_engine::run_outcome::{NeedsLook, OutcomeSummary};
use apb_engine::run_view::RunView;
use serde_json::{Value, json};

const PLAYBOOK: &str = r#"
schema: 2
id: crm
name: crm
version: 1.0.0
defaults: { profile: x }
goal:
  statement: Every open deal has an owner
  criteria:
    - { description: the fix script passes, check: { type: script, path: scripts/t.sh } }
    - { description: a person spot-checks five deals }
baseline_check:
  checks:
    - id: unowned
      description: Open deals without an owner
      connector: { name: twenty, function: list_opportunities }
      observe:
        - { id: count, type: number, path: totalCount, expect: { equals: 0 } }
nodes:
  - { id: start, type: start }
  - { id: fix, type: agent_task, prompt: assign owners }
  - { id: verify, type: agent_task, prompt: check, success_check: { marker: CHECKED } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: fix }
  - { from: fix, to: verify }
  - { from: verify, to: done }
"#;

const CONNECTOR: &str = r#"
name: twenty
version: 0.1.0
auth: { kind: header, header: Authorization, value_template: "Bearer {{secret.token}}" }
account_fields:
  - { name: base_url, required: true }
  - { name: token, required: true, secret: true }
functions:
  - { name: list_opportunities, description: list, read_only: true, method: GET, url: "{{account.base_url}}/o" }
  - { name: update_opportunity, description: update, method: PATCH, url: "{{account.base_url}}/o" }
"#;

fn number(n: f64) -> Value {
    json!({ "state": "value", "value": { "type": "number", "value": n } })
}

fn finding(location: &str) -> Value {
    json!({ "kind": "data_quality", "location": location, "evidence": "two deals share one id", "why_not_fixed": "needs a person" })
}

fn call(function: &str, outcome: &str, status: Option<u16>, complete: Option<bool>) -> Value {
    json!({ "type": "connector_call", "node_id": "fix", "connector": "twenty", "function": function,
            "account": "default", "outcome": outcome, "http_status": status, "response_complete": complete })
}

fn node(kind: &str, id: &str) -> Value {
    match kind {
        "started" => json!({ "type": "node_started", "node": id, "attempt": 1 }),
        _ => {
            json!({ "type": "node_finished", "node": id, "status": "succeeded", "attempt": 1, "output": "" })
        }
    }
}

fn compared(observation: &str, verdict: &str, regressed: bool) -> Value {
    json!({ "type": "baseline_compared", "check": "unowned", "observation": observation, "rule": "equals 0",
            "before": number(37.0), "after": number(20.0), "verdict": verdict, "regressed": regressed })
}

/// Writes the run's playbook snapshot, connector snapshot and journal.
fn seed(dir: &Path, playbook: &str, events: &[Value]) {
    std::fs::create_dir_all(dir.join("connectors")).unwrap();
    std::fs::write(dir.join("playbook.yaml"), playbook).unwrap();
    std::fs::write(dir.join("connectors/twenty.yaml"), CONNECTOR).unwrap();
    let mut journal = String::new();
    for (i, e) in events.iter().enumerate() {
        let mut e = e.clone();
        e["seq"] = json!(i as u64 + 1);
        e["ts"] = json!(1000 + i as u64);
        journal.push_str(&format!("{e}\n"));
    }
    std::fs::write(dir.join("events.jsonl"), journal).unwrap();
}

fn outcome_of(playbook: &str, events: &[Value]) -> OutcomeSummary {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("r1");
    seed(&dir, playbook, events);
    let view = RunView::load(&dir, "r1").unwrap();
    OutcomeSummary::build(&view, &dir)
}

/// A finished crm run: a partial fix, a write with an unknown effect, a
/// read cut short, an inbox gap, a cut output, a manual criterion.
fn crm_run() -> Vec<Value> {
    vec![
        json!({ "type": "baseline_observed", "check": "unowned", "phase": "before", "status": "observed",
                "observations": { "count": number(37.0) }, "input_digest": "d" }),
        node("started", "fix"),
        call("update_opportunity", "ok", Some(200), Some(true)),
        call("update_opportunity", "timeout", None, None),
        call(
            "list_opportunities",
            "incomplete_response",
            Some(200),
            Some(false),
        ),
        json!({ "type": "attempt_notes", "node": "fix", "attempt": 1,
                "findings": [finding("deals:17")], "warnings": [{ "subject": "deal 9", "reason": "owner left the company" }] }),
        json!({ "type": "attempt_finished", "node": "fix", "attempt": 1, "status": "succeeded",
                "output_truncated": [{ "stream": "stdout", "kept_bytes": 10, "dropped_bytes_at_least": 5 }] }),
        json!({ "type": "attempt_notes", "node": "fix", "attempt": 2, "findings": [finding("deals:17")] }),
        node("finished", "fix"),
        node("started", "verify"),
        node("finished", "verify"),
        json!({ "type": "inbox_gap", "node_id": "fix", "connector": "twenty", "account": "default", "from": 3, "through": 4 }),
        node("started", "done"),
        json!({ "type": "baseline_observed", "check": "unowned", "phase": "after", "status": "observed",
                "observations": { "count": number(20.0) }, "input_digest": "d" }),
        compared("count", "not_met", false),
        json!({ "type": "goal_checked", "index": 0, "status": "passed" }),
        json!({ "type": "goal_checked", "index": 1, "status": "manual" }),
        node("finished", "done"),
        json!({ "type": "run_finished", "outcome": "succeeded" }),
    ]
}

#[test]
fn what_nobody_checked_is_listed_as_not_verified_never_as_a_pass() {
    let o = outcome_of(PLAYBOOK, &crm_run());
    let gaps: Vec<(GapKind, &str)> = o
        .unverified
        .iter()
        .map(|g| (g.kind, g.subject.as_str()))
        .collect();
    assert_eq!(
        gaps,
        [
            (GapKind::ManualCriterion, "a person spot-checks five deals"),
            (GapKind::SelfReportedNode, "fix"),
            (GapKind::UnknownCall, "twenty.update_opportunity"),
            (GapKind::IncompleteResponse, "twenty.list_opportunities"),
            (
                GapKind::InboxGap,
                "twenty/default: unread events 3-4 were dropped by inbox retention before they were read (found by fix)"
            ),
            (GapKind::OutputTruncated, "fix attempt 1"),
        ]
    );
    let verified: Vec<(EvidenceSource, &str, &str)> = o
        .verified
        .iter()
        .map(|e| (e.source, e.subject.as_str(), e.result.as_str()))
        .collect();
    assert_eq!(
        verified,
        [
            (EvidenceSource::Goal, "the fix script passes", "passed"),
            (EvidenceSource::SuccessCheck, "verify", "passed"),
            (EvidenceSource::Baseline, "unowned/count", "not_met"),
        ]
    );
    assert_eq!(
        o.done,
        [
            "succeeded: fix, verify",
            "twenty.update_opportunity: 1 write went through"
        ]
    );
    assert!(
        o.headline
            .starts_with("Run succeeded; 2 of 3 checks passed; 6 not verified")
    );
}

#[test]
fn the_baseline_tells_problem_and_change_but_never_whether_the_goal_was_reached() {
    let o = outcome_of(PLAYBOOK, &crm_run());
    let b = o.baseline.expect("the playbook has a baseline check");
    assert_eq!(b.problem, ["Open deals without an owner: count 37"]);
    assert_eq!(b.change, ["unowned/count: 37 -> 20, equals 0: not met"]);
    // A manual criterion still waits: not known, whatever the baseline says.
    assert_eq!(b.goal_reached, None);

    // The same run with only the passing script criterion: the goal is
    // reached although the baseline rule is not met.
    let script_only = PLAYBOOK.replace(
        "    - { description: a person spot-checks five deals }\n",
        "",
    );
    let events: Vec<Value> = crm_run()
        .into_iter()
        .filter(|e| e["index"] != json!(1))
        .collect();
    let o = outcome_of(&script_only, &events);
    assert_eq!(o.baseline.unwrap().goal_reached, Some(true));
}

#[test]
fn findings_are_kept_once_per_node_with_their_latest_attempt() {
    let o = outcome_of(PLAYBOOK, &crm_run());
    assert_eq!(o.findings.len(), 1);
    assert_eq!(
        (o.findings[0].node.as_str(), o.findings[0].attempt),
        ("fix", 2)
    );
    assert_eq!(o.warnings.len(), 1);
    let lines = o.lines();
    for section in [
        "Done:",
        "Verified:",
        "Not verified:",
        "Findings:",
        "Warnings:",
        "Baseline:",
    ] {
        assert!(lines.iter().any(|l| l == section), "missing {section}");
    }
}

fn needs_look(events: &[Value]) -> Option<NeedsLook> {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), PLAYBOOK, events);
    let view = RunView::load(tmp.path(), "r1").unwrap();
    NeedsLook::for_run(tmp.path(), &view.events)
}

fn after(count: f64) -> Value {
    json!({ "type": "baseline_observed", "check": "unowned", "phase": "after", "status": "observed",
            "observations": { "count": number(count) }, "input_digest": "d" })
}

#[test]
fn only_a_regression_findings_or_warnings_call_for_a_look() {
    // Failed calls, cut outputs and an unmet rule alone do not.
    let quiet = [
        node("started", "fix"),
        call("update_opportunity", "timeout", None, None),
        after(20.0),
        compared("count", "not_met", false),
        json!({ "type": "run_finished", "outcome": "failed" }),
    ];
    assert_eq!(needs_look(&quiet), None);

    let regressed = [after(40.0), compared("count", "not_met", true)];
    let n = needs_look(&regressed).expect("a regression");
    assert_eq!(n.reasons, [NeedsLookReason::Regression]);
    assert_eq!(n.line(), "1 regression");

    // A node that can act started after the comparison: it is stale.
    let stale = [
        after(40.0),
        compared("count", "not_met", true),
        node("started", "fix"),
    ];
    assert_eq!(needs_look(&stale), None);

    let n = needs_look(&crm_run()).expect("findings and warnings");
    assert_eq!(
        n.reasons,
        [NeedsLookReason::Findings, NeedsLookReason::Warnings]
    );
    assert_eq!(n.line(), "1 finding, 1 warning");
}

/// One journal, three readers: the run's outcome, the run listing's needs
/// a look and `apb stats` agree on which comparisons are current. A node
/// that cannot act (the finish) starting after a comparison leaves it
/// current; an acting one makes it stale; a comparison without an after
/// snapshot before it is not current at all.
#[test]
fn the_outcome_the_listing_and_stats_read_the_same_current_comparisons() {
    let started = json!({ "type": "run_started", "playbook": "crm", "version": "1.0.0" });
    let cases: [(Vec<Value>, usize); 3] = [
        (
            vec![
                started.clone(),
                after(40.0),
                compared("count", "not_met", true),
                node("started", "done"),
            ],
            1,
        ),
        (
            vec![
                started.clone(),
                after(40.0),
                compared("count", "not_met", true),
                node("started", "fix"),
            ],
            0,
        ),
        (vec![started, compared("count", "not_met", true)], 0),
    ];
    for (events, regressions) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("r1");
        seed(&dir, PLAYBOOK, &events);
        let view = RunView::load(&dir, "r1").unwrap();
        let summary = OutcomeSummary::build(&view, &dir);
        let listing = NeedsLook::for_run(&dir, &view.events);
        let run = apb_engine::run_stats::StatsRun::load(&dir).unwrap();
        let stats = apb_engine::run_stats::build(&[run], &Default::default());
        let stats_regressed: usize = stats.versions[0].baseline.iter().map(|b| b.regressed).sum();
        let summary_regressed = summary.needs_look.as_ref().map_or(0, |n| n.regressions);
        let listing_regressed = listing.as_ref().map_or(0, |n| n.regressions);
        assert_eq!(
            (
                summary_regressed as usize,
                listing_regressed as usize,
                stats_regressed
            ),
            (regressions, regressions, regressions),
            "{events:?}"
        );
        // The Baseline section of the same summary says so too.
        let worse = summary.baseline.map_or(0, |b| {
            b.change
                .iter()
                .filter(|c| c.contains("worse than before"))
                .count()
        });
        assert_eq!(worse, regressions, "{events:?}");
    }
}

/// A run interrupted while its goal criteria were being checked does not
/// read as having reached its goal.
#[test]
fn a_goal_with_an_unchecked_criterion_is_not_known_to_be_reached() {
    let script_only = PLAYBOOK.replace(
        "    - { description: a person spot-checks five deals }\n",
        "    - { description: the second script passes, check: { type: script, path: scripts/u.sh } }\n",
    );
    let events: Vec<Value> = crm_run()
        .into_iter()
        .filter(|e| e["index"] != json!(1))
        .collect();
    let o = outcome_of(&script_only, &events);
    assert_eq!(o.baseline.unwrap().goal_reached, None);
}

/// A finding a later attempt reports again carries that attempt's number
/// and its own reason, never the first attempt's reason.
#[test]
fn a_finding_reported_again_reads_in_the_latest_attempts_words() {
    let mut second = finding("deals:17");
    second["why_not_fixed"] = json!("still needs a person");
    let events = [
        json!({ "type": "attempt_notes", "node": "fix", "attempt": 1, "findings": [finding("deals:17")] }),
        json!({ "type": "attempt_notes", "node": "fix", "attempt": 2, "findings": [second] }),
    ];
    let o = outcome_of(PLAYBOOK, &events);
    assert_eq!(o.findings.len(), 1);
    assert_eq!(o.findings[0].attempt, 2);
    assert_eq!(o.findings[0].finding.why_not_fixed, "still needs a person");
}

fn intent(id: &str, label: &str) -> Value {
    json!({ "type": "send_intent_recorded", "intent": id, "nonce": "n", "node": "fix", "attempt": 1,
            "connector": "twenty", "function": "update_opportunity", "account": "default",
            "mode": "none", "label": label, "pid": 0, "deadline_ms": 0 })
}

fn settled(id: &str, outcome: &str, source: &str) -> Value {
    json!({ "type": "send_intent_settled", "intent": id, "outcome": outcome, "source": source,
            "detail": if outcome == "unknown" { json!("timeout") } else { Value::Null } })
}

fn intent_call(id: &str, outcome: &str) -> Value {
    let mut c = call("update_opportunity", outcome, None, None);
    c["intent"] = json!(id);
    c
}

fn subjects(o: &OutcomeSummary, kind: GapKind) -> Vec<(String, String)> {
    o.unverified
        .iter()
        .filter(|g| g.kind == kind)
        .map(|g| (g.subject.clone(), g.detail.clone().unwrap_or_default()))
        .collect()
}

/// The send ledger in the outcome: a send nobody can tell went out is a gap
/// that names the command resolving it and calls for a look, also when its
/// sender vanished before any call was journaled; a send a lookup later
/// found is done, not a gap.
#[test]
fn an_unknown_send_is_a_gap_and_a_send_settled_later_is_not() {
    let events = [
        node("started", "fix"),
        intent("si-a", "announce"),
        intent_call("si-a", "timeout"),
        settled("si-a", "unknown", "response"),
        intent("si-b", "digest"),
        intent_call("si-b", "timeout"),
        settled("si-b", "unknown", "response"),
        settled("si-b", "confirmed", "reconcile"),
        // Recorded, then the sender died before its call was journaled.
        intent("si-c", "summary"),
        node("finished", "fix"),
        json!({ "type": "run_finished", "outcome": "succeeded" }),
    ];
    let o = outcome_of(PLAYBOOK, &events);
    let gaps = subjects(&o, GapKind::UnknownCall);
    assert_eq!(
        gaps.len(),
        2,
        "one gap per open intent, none per call: {gaps:?}"
    );
    // The wording of a gap and a done line is `run_outcome::intents`'s;
    // here only which intent lands where.
    assert_eq!(gaps[0].0, "twenty.update_opportunity");
    assert!(gaps[0].1.contains("(si-a)"));
    assert!(gaps[1].1.contains("(si-c)"));
    assert!(o.done.iter().any(|d| d.contains("(si-b)")), "{:?}", o.done);
    let look = o.needs_look.expect("unknown sends call for a look");
    assert_eq!(look.unknown_sends, 2);
    assert!(look.reasons.contains(&NeedsLookReason::UnknownSends));
    assert!(look.line().ends_with("2 unknown sends"));
    // The listing reads the same.
    assert_eq!(needs_look(&events).map(|n| n.unknown_sends), Some(2));
}

/// The commits' `Apb-Run` trailers in the done line: one that names this
/// run is fine, one without it or naming another run is counted.
#[test]
fn commits_without_this_runs_trailer_are_counted_in_the_done_line() {
    let commit = |sha: &str, ids: Value| json!({ "sha": sha, "subject": "s", "apb_run": ids });
    let events = [
        node("started", "fix"),
        json!({ "type": "artifacts_committed", "node": "fix", "before": "b", "after": "c1",
                "commits": [commit("c1", json!(["r1"])), commit("c2", json!([])), commit("c3", json!(["r0"]))] }),
        json!({ "type": "artifacts_committed", "node": "fix", "before": "c1", "after": "c4",
                "commits": [{ "sha": "c4", "subject": "old event" }] }),
        node("finished", "fix"),
    ];
    let o = outcome_of(PLAYBOOK, &events);
    assert!(
        o.done.contains(
            &"fix: 3 commits (1 without an Apb-Run trailer, 1 naming another run)".to_string()
        ),
        "{:?}",
        o.done
    );
    assert!(
        o.done.contains(&"fix: 1 commit".to_string()),
        "{:?}",
        o.done
    );
}
