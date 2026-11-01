//! Edge addresses in the journal, `route_evaluated` diagnostics and the
//! rule that diagnostics never steer a run.
//!
//! - Two identical bounded edges: one decision record per target naming
//!   both addresses, the counted record unchanged in number and naming the
//!   same set, the pair's budget spent once per push.
//! - `route_evaluated` is written for one-of decisions, dead ends (in a fork
//!   branch too) and `max_loops` exits, from the evaluation that decided.
//! - Every text a record carries is scrubbed whole before it is clipped:
//!   a long declared secret returned in an `output_field`, and a secret the
//!   run inherits from its parent, never reach the sidecar.
//! - The rule test: the same playbooks run with diagnostics on and with the
//!   writer off give equal journals and equal final run states.

use std::fs;
use std::path::{Path, PathBuf};

use apb_core::registry::init_project;
use apb_engine::diagnostics::{self, Diagnostic};
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};

use crate::common;

fn seed(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    init_project(root).unwrap();
    let vdir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
}

fn run_dir(root: &Path, run_id: &str) -> PathBuf {
    root.join(".apb/runs").join(run_id)
}

fn hops(events: &[Event]) -> Vec<(String, String, bool, Vec<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::EdgeTraversed {
                from,
                to,
                uncounted,
                edges,
                ..
            } => Some((from.clone(), to.clone(), *uncounted, edges.clone())),
            _ => None,
        })
        .collect()
}

fn routes(run_dir: &Path) -> Vec<apb_engine::diagnostics::route::RouteEvaluated> {
    diagnostics::read(run_dir)
        .into_iter()
        .filter_map(|d| match d {
            Diagnostic::Route(r) => Some(r),
            _ => None,
        })
        .collect()
}

const DUPLICATES: &str = r#"
schema: 2
id: dup
name: dup
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: b, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: b, max_traversals: 1 }
  - { from: a, to: b, max_traversals: 1 }
  - { from: b, to: done }
"#;

#[test]
fn identical_bounded_edges_record_both_addresses_once_per_target() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "dup", DUPLICATES, &[("ok.sh", "echo ok\n")]);
    let res = run(dir.path(), "dup", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(dir.path(), &res.run_id)).unwrap();
    let both = vec!["e:a>b#1".to_string(), "e:a>b#2".to_string()];
    let a_to_b: Vec<_> = hops(&events)
        .into_iter()
        .filter(|(f, t, ..)| f == "a" && t == "b")
        .collect();
    // One decision record for the target, one counted record per push.
    assert_eq!(
        a_to_b,
        vec![
            ("a".into(), "b".into(), true, both.clone()),
            ("a".into(), "b".into(), false, both),
        ]
    );
    let state = RunState::fold(&events);
    assert_eq!(state.edge_counts.get(&("a".into(), "b".into())), Some(&1));
    // A fan-out writes no route record.
    assert!(routes(&run_dir(dir.path(), &res.run_id)).is_empty());
}

const ONE_OF: &str = r#"
schema: 2
id: oneof
name: oneof
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: script, script: "scripts/verdict.sh", runner: sh }
  - { id: keep, type: finish, outcome: success }
  - { id: drop, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: drop, condition: { type: output_field, node: gate, field: verdict, equals: drop } }
  - { id: kept, from: gate, to: keep, condition: { type: output_field, node: gate, field: verdict, equals: keep } }
  - { from: gate, to: drop, fallback: true }
"#;

#[test]
fn a_one_of_decision_records_every_check_and_the_edge_taken() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "oneof",
        ONE_OF,
        &[("verdict.sh", "echo '{\"verdict\":\"keep\"}'\n")],
    );
    let res = run(dir.path(), "oneof", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let rd = run_dir(dir.path(), &res.run_id);
    let events = read_all(&rd).unwrap();
    let gate_hops: Vec<_> = hops(&events)
        .into_iter()
        .filter(|(f, ..)| f == "gate")
        .collect();
    assert_eq!(
        gate_hops,
        vec![("gate".into(), "keep".into(), true, vec!["id:kept".into()])]
    );
    let records = routes(&rd);
    let gate: Vec<_> = records.iter().filter(|r| r.node == "gate").collect();
    assert_eq!(gate.len(), 1, "{records:?}");
    let r = gate[0];
    assert_eq!((r.mode.as_str(), r.execution), ("one_of", 1));
    assert_eq!(r.chosen, vec!["id:kept".to_string()]);
    let results: Vec<(&str, &str)> = r
        .checks
        .iter()
        .map(|c| (c.edge.as_str(), c.result.as_str()))
        .collect();
    assert_eq!(
        results,
        vec![
            ("e:gate>drop#1", "not_matched"),
            ("id:kept", "matched"),
            ("e:gate>drop#2", "not_reached"),
        ]
    );
    assert_eq!(r.checks[0].observed, Some(serde_json::json!("keep")));
    // The record follows the journal it explains.
    let finished = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == "gate"))
        .unwrap();
    assert!(r.after_seq > finished.seq);
}

/// The slower branch of the fork, so the branches finish in one order.
const SLOW: &str = "sleep 1\necho ok\n";

const FORK_DEAD_END: &str = r#"
schema: 2
id: forkdead
name: forkdead
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: b, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: done, condition: { type: node_status, node: a, equals: failure } }
  - { from: b, to: done }
"#;

#[test]
fn a_dead_end_inside_a_fork_branch_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "forkdead",
        FORK_DEAD_END,
        &[("ok.sh", "echo ok\n"), ("slow.sh", SLOW)],
    );
    let res = run(dir.path(), "forkdead", None, RunOptions::default()).unwrap();
    let rd = run_dir(dir.path(), &res.run_id);
    let records = routes(&rd);
    let a = records
        .iter()
        .find(|r| r.node == "a")
        .unwrap_or_else(|| panic!("no record for a: {records:?}"));
    assert_eq!(a.mode, "dead_end");
    assert!(a.chosen.is_empty());
    assert_eq!(a.checks[0].result, "not_matched");
    assert_eq!(a.checks[0].observed, Some(serde_json::json!("succeeded")));
}

const MAX_LOOPS: &str = r#"
schema: 2
id: loops
name: loops
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: check, type: condition, max_loops: 2 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: check }
  - { from: check, to: work, condition: { type: node_status, node: work, equals: success } }
  - { from: check, to: done, fallback: true }
"#;

#[test]
fn a_max_loops_exit_records_the_fallback_it_took() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "loops", MAX_LOOPS, &[("ok.sh", "echo ok\n")]);
    let res = run(dir.path(), "loops", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let rd = run_dir(dir.path(), &res.run_id);
    let events = read_all(&rd).unwrap();
    let exit: Vec<_> = hops(&events)
        .into_iter()
        .filter(|(f, t, ..)| f == "check" && t == "done")
        .collect();
    assert_eq!(
        exit,
        vec![(
            "check".into(),
            "done".into(),
            true,
            vec!["e:check>done#1".into()]
        )]
    );
    let records = routes(&rd);
    let modes: Vec<(&str, u32)> = records
        .iter()
        .filter(|r| r.node == "check")
        .map(|r| (r.mode.as_str(), r.execution))
        .collect();
    assert_eq!(
        modes,
        vec![("one_of", 1), ("one_of", 2), ("loop_exhausted", 3)]
    );
}

// --- scrub before clip ------------------------------------------------------

/// A long value no token-shape rule recognizes: only the run's declared
/// secret makes it a secret.
fn long_secret() -> String {
    "quietharbor".repeat(12)
}

fn secret_playbook(id: &str) -> String {
    format!(
        r#"
schema: 2
id: {id}
name: {id}
version: 1.0.0
params:
  - {{ name: token, type: text, secret: true }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: gate, type: script, script: "scripts/leak.sh", runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: gate }}
  - {{ from: gate, to: done, condition: {{ type: output_field, node: gate, field: verdict, equals: none }} }}
  - {{ from: gate, to: done, fallback: true }}
"#
    )
}

fn leak_script() -> String {
    format!("echo '{{\"verdict\":\"{}\"}}'\n", long_secret())
}

/// No fragment of the secret long enough to identify it is in `text`.
fn assert_no_fragment(text: &str) {
    let secret = long_secret();
    for start in 0..secret.len() - 16 {
        let piece = &secret[start..start + 16];
        assert!(!text.contains(piece), "a fragment of the secret leaked");
    }
}

#[test]
fn a_long_declared_secret_in_an_output_field_is_scrubbed_before_it_is_clipped() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "leak",
        &secret_playbook("leak"),
        &[("leak.sh", &leak_script())],
    );
    let opts = RunOptions {
        params: [("token".to_string(), long_secret())].into(),
        ..RunOptions::default()
    };
    let res = run(dir.path(), "leak", None, opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let rd = run_dir(dir.path(), &res.run_id);
    let text = fs::read_to_string(rd.join(diagnostics::FILE)).unwrap();
    assert!(text.contains("route_evaluated"), "the record was written");
    assert!(text.contains("[redacted]"), "the value was replaced");
    assert_no_fragment(&text);
}

const SECRET_CONNECTOR: &str = r#"
name: crm
version: 0.1.0
account_fields:
  - name: base_url
    required: true
  - name: token
    secret: true
functions:
  - name: noop
    description: A read that is never called
    read_only: true
    mock: { status: 200, body: {} }
"#;

#[test]
fn a_workspace_connector_secret_is_scrubbed_too() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let _token = common::EnvVar::set("APB_TEST_EDGE_ROUTE_TOKEN", long_secret());
    let cdir = cfg.path().join("connectors/crm");
    fs::create_dir_all(&cdir).unwrap();
    fs::write(cdir.join("connector.yaml"), SECRET_CONNECTOR).unwrap();
    // No param declares anything: the value is only the workspace's.
    let pb = secret_playbook("ws").replace(
        "params:\n  - { name: token, type: text, secret: true }\n",
        "",
    );
    seed(dir.path(), "ws", &pb, &[("leak.sh", &leak_script())]);
    let acct = apb_core::connector::config::project_config_path(dir.path(), "crm");
    fs::create_dir_all(acct.parent().unwrap()).unwrap();
    fs::write(
        &acct,
        "accounts:\n  - name: main\n    base_url: http://127.0.0.1:9\n    token: \"{{env.APB_TEST_EDGE_ROUTE_TOKEN}}\"\n",
    )
    .unwrap();
    let res = run(dir.path(), "ws", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let text =
        fs::read_to_string(run_dir(dir.path(), &res.run_id).join(diagnostics::FILE)).unwrap();
    assert!(text.contains("route_evaluated"), "the record was written");
    assert!(text.contains("[redacted]"), "the value was replaced");
    assert_no_fragment(&text);
}

const PARENT: &str = r#"
schema: 2
id: parent
name: parent
version: 1.0.0
params:
  - { name: token, type: text, secret: true }
nodes:
  - { id: start, type: start }
  - { id: sub, type: playbook, playbook: child }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: sub }
  - { from: sub, to: done }
"#;

#[test]
fn a_secret_the_run_inherits_from_its_parent_is_scrubbed_too() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "parent", PARENT, &[]);
    let child = secret_playbook("child").replace(
        "params:\n  - { name: token, type: text, secret: true }\n",
        "",
    );
    let vdir = dir.path().join(".apb/playbooks/child/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), child).unwrap();
    fs::write(vdir.join("scripts/leak.sh"), leak_script()).unwrap();
    fs::write(dir.path().join(".apb/playbooks/child/current"), "1.0.0").unwrap();
    let opts = RunOptions {
        params: [("token".to_string(), long_secret())].into(),
        ..RunOptions::default()
    };
    let res = run(dir.path(), "parent", None, opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let runs = dir.path().join(".apb/runs");
    let mut child_sidecars = 0;
    for entry in fs::read_dir(&runs).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap().to_string_lossy() == res.run_id {
            continue;
        }
        if let Ok(text) = fs::read_to_string(path.join(diagnostics::FILE)) {
            child_sidecars += 1;
            assert!(text.contains("[redacted]"), "the value was replaced");
            assert_no_fragment(&text);
        }
    }
    assert_eq!(child_sidecars, 1, "the child wrote its route record");
}

// --- the rule: diagnostics never steer a run --------------------------------

const LIST: &str = r#"
schema: 2
id: lst
name: lst
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: work
    type: script
    script: "scripts/ok.sh"
    runner: sh
    list:
      items: { inline: [ { id: i1 }, { id: i2 }, { id: i3 } ] }
      batch: { size: 2, parallel: 1, retries: 0 }
  - { id: done, type: finish, outcome: success }
  - { id: odd, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: output_field, node: work, field: passed, equals: "3" } }
  - { from: work, to: odd, fallback: true }
"#;

const BOUNDED_LOOP: &str = r#"
schema: 2
id: bl
name: bl
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/fail.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: gave_up, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: work, condition: { type: node_status, node: work, equals: failure }, max_traversals: 2 }
  - { from: work, to: gave_up, fallback: true }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
"#;

const JUDGE_EDGES: &str = r#"
schema: 2
id: je
name: je
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: review, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: fix, type: agent_task, prompt: "Fix it.", profile: main }
  - { id: ship, type: finish, outcome: success }
  - { id: fixed, type: finish, outcome: success }
edges:
  - { from: start, to: review }
  - { from: fix, to: fixed }
  - { from: review, to: fix, condition: { type: judge, question: "Does the output report a defect?", min_p: 0.7, on_unavailable: false } }
  - { from: review, to: ship, condition: { type: judge, question: "Is the output ready?", min_p: 0.7, on_unavailable: true } }
"#;

const JUDGE_DECISIONS: &str = "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      edge_0: { type: noul, noul: 0.2 }\n      edge_1: { type: noul, noul: 0.9 }\nuses:\n  judge_edge: { mode: enforce }\n";

/// The journal as it compares across two runs: run ids, the project path,
/// timestamps, durations and process ids normalized away.
fn normalized(root: &Path, run_id: &str, events: &[Event]) -> Vec<serde_json::Value> {
    fn scrub(v: &mut serde_json::Value, root: &str, run_id: &str) {
        match v {
            serde_json::Value::Object(map) => {
                for key in [
                    "ts",
                    "duration_ms",
                    "latency_ms",
                    "pid",
                    "started_at",
                    "token",
                    "nonce",
                ] {
                    map.remove(key);
                }
                map.values_mut().for_each(|x| scrub(x, root, run_id));
            }
            serde_json::Value::Array(items) => {
                items.iter_mut().for_each(|x| scrub(x, root, run_id))
            }
            serde_json::Value::String(s) => {
                *s = s.replace(root, "<root>").replace(run_id, "<run>");
            }
            _ => {}
        }
    }
    let root = root.to_string_lossy().to_string();
    events
        .iter()
        .map(|e| {
            let mut v = serde_json::to_value(e).unwrap();
            scrub(&mut v, &root, run_id);
            v
        })
        .collect()
}

fn state_text(root: &Path, run_id: &str, events: &[Event]) -> String {
    format!("{:?}", RunState::fold(events))
        .replace(&root.to_string_lossy().to_string(), "<root>")
        .replace(run_id, "<run>")
}

struct Case {
    id: &'static str,
    yaml: &'static str,
    scripts: &'static [(&'static str, &'static str)],
    decisions: Option<&'static str>,
}

/// One run of `case` in a fresh project; with `diagnostics` off, the writer
/// is disabled for the project and the sidecar is deleted after the run.
fn run_case(case: &Case, diagnostics_on: bool) -> (Vec<serde_json::Value>, String, bool) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    seed(dir.path(), case.id, case.yaml, case.scripts);
    common::seed_main(dir.path());
    if let Some(d) = case.decisions {
        fs::write(cfg.path().join("decisions.yaml"), d).unwrap();
    }
    if !diagnostics_on {
        diagnostics::writer::disable_under(dir.path());
    }
    let _env = common::env_lock();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let _fake = common::EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1");
    let res = run(dir.path(), case.id, None, RunOptions::default()).unwrap();
    let rd = run_dir(dir.path(), &res.run_id);
    let had_diagnostics = rd.join(diagnostics::FILE).exists();
    if case.decisions.is_some() {
        let events = read_all(&rd).unwrap();
        assert!(
            events.iter().any(
                |e| matches!(&e.payload, EventPayload::DecisionMade { use_site, edge_addresses, .. }
                if use_site == "judge_edge" && edge_addresses.len() == 2)
            ),
            "{}: the judge edges were asked",
            case.id
        );
    }
    if !diagnostics_on {
        let _ = fs::remove_file(rd.join(diagnostics::FILE));
    }
    let events = read_all(&rd).unwrap();
    (
        normalized(dir.path(), &res.run_id, &events),
        state_text(dir.path(), &res.run_id, &events),
        had_diagnostics,
    )
}

#[test]
fn diagnostics_on_and_off_give_equal_journals_and_run_states() {
    let cases = [
        Case {
            id: "bl",
            yaml: BOUNDED_LOOP,
            scripts: &[("fail.sh", "echo no\nexit 1\n")],
            decisions: None,
        },
        Case {
            id: "loops",
            yaml: MAX_LOOPS,
            scripts: &[("ok.sh", "echo ok\n")],
            decisions: None,
        },
        Case {
            id: "forkdead",
            yaml: FORK_DEAD_END,
            scripts: &[("ok.sh", "echo ok\n"), ("slow.sh", SLOW)],
            decisions: None,
        },
        Case {
            id: "je",
            yaml: JUDGE_EDGES,
            scripts: &[("ok.sh", "echo looks fine\n")],
            decisions: Some(JUDGE_DECISIONS),
        },
        Case {
            id: "lst",
            yaml: LIST,
            scripts: &[("ok.sh", "echo ok\n")],
            decisions: None,
        },
    ];
    for case in &cases {
        let (on, on_state, wrote) = run_case(case, true);
        let (off, off_state, wrote_off) = run_case(case, false);
        assert!(wrote, "{}: diagnostics on wrote a sidecar", case.id);
        assert!(!wrote_off, "{}: the disabled writer wrote nothing", case.id);
        assert_eq!(on.len(), off.len(), "{}: journal lengths", case.id);
        for (i, (a, b)) in on.iter().zip(&off).enumerate() {
            assert_eq!(a, b, "{}: event {i}", case.id);
        }
        assert_eq!(on_state, off_state, "{}: final run state", case.id);
    }
}
