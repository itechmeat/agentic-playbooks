//! Join readiness and liveness across executions of the same nodes: loop
//! rounds, a join interrupted while it ran, a re-run into a join, a
//! supervisor retry. Every state is folded from a journal written the way
//! the drive writes it, so the journal positions routing reads are real.

use super::*;
use crate::event::{Event, EventPayload};

/// One journal record of a fixture.
enum Ev {
    Start(&'static str),
    Finish(&'static str, &'static str, &'static str),
    Hop(&'static str, &'static str),
    Revived(&'static str, Vec<String>),
}
use Ev::*;

fn fold(evs: &[Ev]) -> RunState {
    let events: Vec<Event> = evs
        .iter()
        .enumerate()
        .map(|(i, e)| Event {
            seq: i as u64,
            ts: 0,
            payload: match e {
                Start(n) => EventPayload::NodeStarted {
                    node: (*n).into(),
                    attempt: 1,
                },
                Finish(n, status, output) => EventPayload::NodeFinished {
                    node: (*n).into(),
                    status: (*status).into(),
                    attempt: 1,
                    output: (*output).into(),
                    artifacts: Vec::new(),
                },
                Hop(from, to) => EventPayload::EdgeTraversed {
                    from: (*from).into(),
                    to: (*to).into(),
                    via_policy: false,
                    uncounted: true,
                },
                Revived(from, nodes) => EventPayload::NodesRevived {
                    from_node: (*from).into(),
                    nodes: nodes.clone(),
                    superseded: Vec::new(),
                },
            },
        })
        .collect();
    RunState::fold(&events)
}

/// One execution of `node` with `output` and `status`, then the hops the
/// drive journaled out of it.
fn exec(
    node: &'static str,
    status: &'static str,
    output: &'static str,
    hops: &[&'static str],
) -> Vec<Ev> {
    let mut v = vec![Start(node), Finish(node, status, output)];
    v.extend(hops.iter().map(|to| Hop(node, to)));
    v
}

fn ok(node: &'static str, output: &'static str, hops: &[&'static str]) -> Vec<Ev> {
    exec(node, "succeeded", output, hops)
}

fn journal(steps: Vec<Vec<Ev>>) -> RunState {
    fold(&steps.into_iter().flatten().collect::<Vec<_>>())
}

fn active(nodes: &[&str]) -> Vec<String> {
    nodes.iter().map(|n| n.to_string()).collect()
}

/// `gate -> {a, b}` (either-or), both into `m`; `m -> gate` loops.
const LOOP: &str = r#"
schema: 1
id: l
name: L
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: prompt, prompt: "g" }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: m, type: prompt, prompt: "m" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: a, condition: { type: output_match, node: gate, pattern: A } }
  - { from: gate, to: b, fallback: true }
  - { from: a, to: m, join: all }
  - { from: b, to: m, join: all }
  - { from: m, to: gate, condition: { type: output_match, node: m, pattern: again } }
  - { from: m, to: done, fallback: true }
"#;

/// The live set before `m` runs, walked from `gate`.
fn live_before_m(s: &RunState) -> BTreeSet<String> {
    let pb = Playbook::from_yaml(LOOP).unwrap();
    live_nodes(&pb, s, &active(&["gate"]), "m")
}

/// Round 2 sends the gate to `b`: `a`, taken in round 1, is not live.
#[test]
fn a_branch_of_an_earlier_round_is_not_live() {
    let s = journal(vec![
        ok("start", "", &["gate"]),
        ok("gate", "A", &["a"]),
        ok("a", "", &["m"]),
        ok("m", "again", &["gate"]),
        ok("gate", "B", &["b"]),
    ]);
    let live = live_before_m(&s);
    assert!(live.contains("b"));
    assert!(!live.contains("a"));
}

/// The gate ran in round 1 and `m` routed back into it: it runs again,
/// so both of its branches are live although it chose `a` last time.
#[test]
fn a_node_delivered_into_again_follows_every_edge() {
    let s = journal(vec![
        ok("start", "", &["gate"]),
        ok("gate", "A", &["a"]),
        ok("a", "", &["m"]),
        ok("m", "again", &["gate"]),
    ]);
    let live = live_before_m(&s);
    assert!(live.contains("a") && live.contains("b"));
}

/// The walk stops at the join being weighed: `done` lies only behind it.
#[test]
fn the_walk_never_passes_through_the_join() {
    let s = journal(vec![ok("start", "", &["gate"])]);
    let live = live_before_m(&s);
    assert!(live.contains("m"));
    assert!(!live.contains("done"));
}

/// A loop that changed its route must not deadlock its merge point: round
/// 1 took `a`, round 2 takes `b`, and `m` runs on `b` alone.
#[test]
fn a_loop_fork_that_changed_branches_still_resolves_its_downstream_join() {
    let pb = Playbook::from_yaml(LOOP).unwrap();
    let s = journal(vec![
        ok("start", "", &["gate"]),
        ok("gate", "A", &["a"]),
        ok("a", "", &["m"]),
        ok("m", "again", &["gate"]),
        ok("gate", "B", &["b"]),
        ok("b", "", &[]),
    ]);
    assert_eq!(
        join_readiness(&pb, "m", &s, &active(&["b"])),
        JoinReadiness::ReadySuccess
    );
}

/// Round 2 of the gate takes `a` again and the drive journals the hop
/// again; then a patch removes `gate -> a` while `a` runs. The hop of
/// the latest round is still where the gate went.
#[test]
fn a_hop_of_the_latest_round_outlives_a_patch_that_removes_its_edge() {
    let s = journal(vec![
        ok("start", "", &["gate"]),
        ok("gate", "A", &["a"]),
        ok("a", "", &["m"]),
        ok("m", "again", &["gate"]),
        ok("gate", "A", &["a"]),
        vec![Start("a")],
    ]);
    let patched = Playbook::from_yaml(&LOOP.replace(
        "  - { from: gate, to: a, condition: { type: output_match, node: gate, pattern: A } }\n",
        "",
    ))
    .unwrap();
    assert!(patched.edges.iter().all(|e| e.to != "a"));
    assert!(routed_targets(&patched, "gate", &s).contains("a"));
}

/// A fork whose branches meet in a `require: all_succeeded` join.
const FORK: &str = r#"
schema: 1
id: f
name: F
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "a" }
  - { id: b, type: prompt, prompt: "b" }
  - { id: m, type: prompt, prompt: "m" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: m, join: all, require: all_succeeded }
  - { from: b, to: m, join: all, require: all_succeeded }
  - { from: m, to: done }
"#;

/// The join started, then the driver died (a crash or a `--now` pause):
/// the resume weighs it again on the deliveries it already had.
#[test]
fn a_require_join_interrupted_while_it_ran_is_ready_on_resume() {
    let pb = Playbook::from_yaml(FORK).unwrap();
    let mut evs = vec![
        ok("start", "", &["a", "b"]),
        ok("a", "", &["m"]),
        ok("b", "", &["m"]),
    ];
    evs.push(vec![Start("m")]);
    let s = journal(evs);
    assert_eq!(
        join_readiness(&pb, "m", &s, &active(&["m"])),
        JoinReadiness::ReadySuccess
    );
}

/// The join failed on its own and a supervisor retries it: its inputs
/// still stand.
#[test]
fn a_require_join_a_supervisor_retries_is_ready() {
    let pb = Playbook::from_yaml(FORK).unwrap();
    let s = journal(vec![
        ok("start", "", &["a", "b"]),
        ok("a", "", &["m"]),
        ok("b", "", &["m"]),
        exec("m", "failed", "", &[]),
        vec![Revived("m", vec!["m".into()])],
    ]);
    assert_eq!(
        join_readiness(&pb, "m", &s, &active(&["m"])),
        JoinReadiness::ReadySuccess
    );
}

/// A join refused because `c` failed; the operator re-runs from `c`
/// (`--from-node`), and `b`, which succeeded and is not revived, still
/// counts once `c` succeeds.
fn rerun_into_join(join: &str) -> JoinReadiness {
    let yaml = format!(
        r#"
schema: 1
id: r
name: R
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - {{ id: b, type: prompt, prompt: "b" }}
  - {{ id: c, type: prompt, prompt: "c" }}
  - {{ id: j, type: prompt, prompt: "j" }}
  - {{ id: done, type: finish, outcome: success }}
  - {{ id: failed, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: b }}
  - {{ from: start, to: c }}
  - {{ from: b, to: j, {join} }}
  - {{ from: c, to: j, {join} }}
  - {{ from: j, to: done, condition: {{ type: node_status, node: j, equals: success }} }}
  - {{ from: j, to: failed, condition: {{ type: node_status, node: j, equals: failure }} }}
"#
    );
    let pb = Playbook::from_yaml(&yaml).unwrap();
    let first: Vec<Vec<Ev>> = vec![
        ok("start", "", &["b", "c"]),
        ok("b", "", &["j"]),
        exec("c", "failed", "", &["j"]),
        exec("j", "failed", "refused", &["failed"]),
    ];
    let before = journal(first);
    let revived: Vec<String> = Revival::new(&pb, &before, &active(&["c"]))
        .nodes()
        .iter()
        .cloned()
        .collect();
    assert!(revived.contains(&"j".to_string()), "the re-run reaches j");
    let s = journal(vec![
        ok("start", "", &["b", "c"]),
        ok("b", "", &["j"]),
        exec("c", "failed", "", &["j"]),
        exec("j", "failed", "refused", &["failed"]),
        vec![Revived("c", revived)],
        ok("c", "", &[]),
    ]);
    join_readiness(&pb, "j", &s, &active(&["c"]))
}

#[test]
fn a_re_run_of_the_failed_branch_into_a_require_join_is_ready() {
    assert_eq!(
        rerun_into_join("join: all, require: all_succeeded"),
        JoinReadiness::ReadySuccess
    );
}

#[test]
fn a_re_run_of_the_failed_branch_into_an_any_join_is_ready() {
    assert_eq!(rerun_into_join("join: any"), JoinReadiness::ReadySuccess);
}

/// A loop whose join also takes an input from outside the loop: `x`
/// delivers once, and counts for every round.
#[test]
fn a_loop_join_counts_an_input_from_outside_the_loop_in_round_two() {
    let pb = Playbook::from_yaml(
        r#"
schema: 1
id: o
name: O
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: x, type: prompt, prompt: "x" }
  - { id: gate, type: prompt, prompt: "g" }
  - { id: a, type: prompt, prompt: "a" }
  - { id: m, type: prompt, prompt: "m" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: x }
  - { from: start, to: gate }
  - { from: gate, to: a }
  - { from: x, to: m, join: all, require: all_succeeded }
  - { from: a, to: m, join: all, require: all_succeeded }
  - { from: m, to: gate, condition: { type: output_match, node: m, pattern: again } }
  - { from: m, to: done, fallback: true }
"#,
    )
    .unwrap();
    let s = journal(vec![
        ok("start", "", &["x", "gate"]),
        ok("x", "", &["m"]),
        ok("gate", "", &["a"]),
        ok("a", "", &["m"]),
        ok("m", "again", &["gate"]),
        ok("gate", "", &["a"]),
        ok("a", "", &[]),
    ]);
    assert_eq!(
        join_readiness(&pb, "m", &s, &active(&["a"])),
        JoinReadiness::ReadySuccess
    );
}

/// Inside the loop the round before's delivery is not this round's: in
/// round 2 `m` waits for `a`, which runs again.
#[test]
fn a_loop_join_waits_for_its_in_loop_input_in_round_two() {
    let pb = Playbook::from_yaml(LOOP).unwrap();
    let s = journal(vec![
        ok("start", "", &["gate"]),
        ok("gate", "A", &["a"]),
        ok("a", "", &["m"]),
        ok("m", "again", &["gate"]),
        ok("gate", "A", &["a"]),
    ]);
    assert_eq!(
        join_readiness(&pb, "m", &s, &active(&["a"])),
        JoinReadiness::NotReady
    );
}
