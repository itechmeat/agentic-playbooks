//! Quick scenario runs through the shipped entry point, `apb eval --level
//! quick`, on the fix-loop fixture (`tests/fixtures/scenario/fixloop`): an
//! agent with a retry delay, a script gate, a judge loop, a person's
//! review, a publish step, a fork with a dead join input, a child playbook
//! and a read-only agent. Every test plays in its own project and config
//! directory with nothing on `PATH` but shims that record any start.

use std::fs;

use serde_json::Value;

use crate::http_service::Service;
use crate::scenario_fixture::{Project, check, failed_checks, journal, run_dir, snapshot};

/// A machine whose judges ask a provider at `base_url` at enforce: a quick
/// run must never call it.
fn machine_provider(p: &Project, base_url: &str) {
    fs::write(
        p.cfg.path().join("decisions.yaml"),
        format!(
            "version: 1\nmode: enforce\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{base_url}\", model: jev-1.13.0 }}\nuses:\n  judge_node: {{ mode: enforce }}\n  judge_edge: {{ mode: enforce }}\n"
        ),
    )
    .unwrap();
}

fn event_types(events: &[Value]) -> Vec<&str> {
    events.iter().filter_map(|e| e["type"].as_str()).collect()
}

/// The visits of `node` in the trace, each as its attempts'
/// `(n, status, failure_kind, answer)`.
fn attempts(report: &Value, node: &str) -> Vec<Vec<(u64, String, String, String)>> {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    report["executions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["node"] == node)
        .map(|x| {
            x["attempts"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|a| {
                            (
                                a["n"].as_u64().unwrap(),
                                s(&a["status"]),
                                s(&a["failure_kind"]),
                                s(&a["answer"]),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// A1, A2 and the A13 rows D1, D3, R1 and U6: the green scenario plays the
/// whole fix loop on the real drive in a few seconds, starts no program,
/// sends no request to the machine's decision provider, and its report
/// carries the trace: every execution with its visit and attempts, each
/// edge taken by address counted once per hop, the dead join input, and no
/// empty template. The infrastructure backoff and the delayed retry are
/// due at once (the delay journaled, no waker), the read-only node takes no tree state, and the child playbook
/// runs as a nested scenario run next to its parent.
#[test]
fn quick_green_plays_the_fix_loop_with_no_side_effect_and_traces_it() {
    let p = Project::new(&["fixloop", "kid"]);
    let stub = Service::spawn(Default::default());
    machine_provider(&p, &stub.base_url);

    let (code, r) = p.play("fixloop", "quick", "green", &[]);

    assert_eq!(p.started(), "", "a quick run started a program");
    assert_eq!(
        (code, r["verdict"].as_str()),
        (0, Some("passed")),
        "{:?}\n{r:#}",
        failed_checks(&r)
    );
    assert!(
        stub.bodies("/v1/systemone").is_empty(),
        "a quick run asked the machine's provider"
    );

    // The trace: visits and attempts per execution.
    assert_eq!(
        attempts(&r, "work"),
        vec![
            vec![
                (1, "failed".into(), "transient".into(), "used".into()),
                (2, "failed".into(), String::new(), "used".into()),
                (3, "succeeded".into(), String::new(), "used".into()),
            ],
            vec![(1, "succeeded".into(), String::new(), "used".into())],
        ]
    );
    let visits = |node: &str| {
        r["executions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["node"] == node)
            .map(|x| x["visit"].as_u64().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        (visits("gate"), visits("triage"), visits("summary")),
        (vec![1, 2], vec![1, 2], vec![1])
    );
    // Edges by address, one count per hop.
    let taken: Vec<(String, u64)> = r["edges_taken"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["edge"].as_str().unwrap().to_string(),
                e["count"].as_u64().unwrap(),
            )
        })
        .collect();
    for (edge, count) in [
        ("id:again", 1),
        ("e:work>gate#1", 2),
        ("e:triage>approve#1", 1),
        ("e:lint>lintfix#1", 1),
    ] {
        assert!(
            taken.contains(&(edge.to_string(), count)),
            "{edge} x{count} not in {taken:?}"
        );
    }
    assert!(
        !taken
            .iter()
            .any(|(e, _)| e == "id:lint-clean" || e == "id:rejected")
    );
    assert_eq!(
        r["joins_dead"],
        serde_json::json!([{ "join": "summary", "inputs": ["lint"] }])
    );
    assert_eq!(r["templates_empty"], serde_json::json!([]));
    let real: u64 = r["boundaries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["real"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(real, 0, "a quick run acted for real: {:#}", r["boundaries"]);

    let dir = run_dir(&p, &r);
    let events = journal(&dir);
    let types = event_types(&events);
    // D1: the delayed retry is journaled as in a real run, then attempt 2
    // starts at once, with no waker left behind.
    // The journal's own clock says it was not waited out (a wall-clock
    // bound would flake on a loaded machine): the next attempt starts
    // well before the journaled delay would have passed.
    let deferred = types
        .iter()
        .position(|t| *t == "retry_deferred")
        .unwrap_or_else(|| panic!("{types:?}"));
    let next = events[deferred..]
        .iter()
        .find(|e| e["type"] == "attempt_started")
        .unwrap_or_else(|| panic!("{types:?}"));
    let delay = events[deferred]["delay_ms"].as_u64().unwrap();
    let gap = next["ts"].as_u64().unwrap() - events[deferred]["ts"].as_u64().unwrap();
    assert!(
        delay > 0 && gap < delay,
        "waited {gap} ms of a {delay} ms delay"
    );
    assert!(!dir.join("wakers").exists(), "a waker was set up");
    // R1: the read-only node took no tree state.
    assert!(!types.contains(&"read_only_checked"), "{types:?}");
    // U6: the child ran as a nested scenario run next to its parent.
    let child = events
        .iter()
        .find(|e| e["type"] == "child_run_started")
        .and_then(|e| e["run_id"].as_str())
        .expect("the docs node started a child run");
    assert!(
        p.root()
            .join(".apb/scenario-runs")
            .join(child)
            .join("events.jsonl")
            .is_file()
    );
    assert!(!p.root().join(".apb/runs").join(child).exists());
}

/// A7 and A12 at quick: the project tree outside `.apb/scenario-runs` is
/// the same before and after (paths, sizes, modification times), no busy
/// lock is taken, and no run surface finds the scenario run.
#[test]
fn a_quick_run_leaves_the_project_as_it_was_and_no_surface_finds_it() {
    let p = Project::new(&["fixloop", "kid"]);
    let before = snapshot(p.root());
    let (code, r) = p.play("fixloop", "quick", "green", &[]);
    assert_eq!(before, snapshot(p.root()), "the project tree changed");
    assert_eq!(code, 0, "{r:#}");
    let lock = walkdir_has(p.root(), "workdir.lock");
    assert!(!lock, "a busy lock file appeared");

    let id = r["run_id"].as_str().unwrap();
    let text = |o: std::process::Output| {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    assert_eq!(text(p.apb(&["runs"])).trim(), "no runs yet");
    let stats: Value = serde_json::from_str(&text(p.apb(&["stats", "--json"]))).unwrap();
    assert_eq!(stats["runs"], 0, "{stats:#}");
    for args in [vec!["resume", id], vec!["runs", id]] {
        let out = p.apb(&args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && err.contains("not found"),
            "apb {args:?} found the scenario run: {err}"
        );
    }
}

fn walkdir_has(dir: &std::path::Path, name: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|e| e.file_name() == name || (e.path().is_dir() && walkdir_has(&e.path(), name)))
}

/// The reject scenario with one more answer than the run asks for.
const EXTRA: &str = "schema: 1\nid: extra\ninstruction: the parser\nanswers:\n  work:\n    - visit: 1\n      attempts:\n        - { status: succeeded, output: fixed }\n        - { status: succeeded, output: never asked }\n  gate:\n    - { visit: 1, script: { exit: 0, stdout: QA green } }\n  triage:\n    - { visit: 1, judge: { verdict: { value: green, p: 0.95 } } }\n  approve:\n    - { visit: 1, review: { decision: reject } }\nexpect:\n  run: { outcome: [failed] }\n  finish: failed\n";

/// A4: an answer for attempt 2 of a node whose attempt 1 succeeds fails the
/// scenario by `unused_answers`, naming node, visit and attempt; with
/// `on_extra: warn` the scenario passes and still lists it.
#[test]
fn an_answer_nobody_used_fails_the_scenario_unless_warned() {
    let p = Project::new(&["fixloop"]);
    p.scenario("fixloop", "extra", EXTRA);
    let (code, r) = p.play("fixloop", "quick", "extra", &[]);
    assert_eq!(code, 1, "{r:#}");
    assert_eq!(failed_checks(&r), vec!["unused_answers".to_string()]);
    let (_, observed) = check(&r, "unused_answers").unwrap();
    assert!(
        observed.contains("work visit 1 attempt 2"),
        "unused_answers observed {observed:?}"
    );

    p.scenario(
        "fixloop",
        "extra",
        &EXTRA.replace("id: extra\n", "id: extra\non_extra: warn\n"),
    );
    let (code, r) = p.play("fixloop", "quick", "extra", &[]);
    assert_eq!((code, r["verdict"].as_str()), (0, Some("passed")), "{r:#}");
    assert!(check(&r, "unused_answers").is_none());
    let extra = &r["answers"]["extra"];
    assert_eq!(
        (&extra[0]["node"], &extra[0]["visit"], &extra[0]["attempt"]),
        (&Value::from("work"), &Value::from(1), &Value::from(2)),
        "{extra:#}"
    );
}

/// A5: a scenario with no answer for the agent stops `invalid` under
/// `on_missing: error`, naming the boundary and its key; under `fail` each
/// attempt fails as an agent would, the run takes the node's failure route
/// and the report lists the missing answers.
#[test]
fn a_missing_answer_is_invalid_or_takes_the_failure_route() {
    let p = Project::new(&["fixloop"]);
    let body =
        "schema: 1\nid: gap\ninstruction: the parser\nexpect:\n  run: { outcome: [failed] }\n";
    p.scenario("fixloop", "gap", body);
    let (code, r) = p.play("fixloop", "quick", "gap", &[]);
    assert_eq!((code, r["verdict"].as_str()), (3, Some("invalid")), "{r:#}");
    let reason = r["reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("agent_attempt") && reason.contains("`work` visit 1 attempt 1"),
        "{reason}"
    );

    p.scenario(
        "fixloop",
        "gap",
        &body.replace("id: gap\n", "id: gap\non_missing: fail\n"),
    );
    let (code, r) = p.play("fixloop", "quick", "gap", &[]);
    assert_eq!((code, r["verdict"].as_str()), (0, Some("passed")), "{r:#}");
    let missing = &r["answers"]["missing"];
    assert_eq!(
        (
            &missing[0]["kind"],
            &missing[0]["node"],
            &missing[1]["attempt"]
        ),
        (
            &Value::from("agent_attempt"),
            &Value::from("work"),
            &Value::from(2)
        ),
        "{missing:#}"
    );
    assert_eq!(r["finish"], "failed", "{r:#}");
}

/// The A13 rows R2, U2 and U5: a scripted change on a read-only node fails
/// it on the live path with no retry; an unattended run stops at a review
/// that has no unattended outcome before the scenario's decision is read,
/// and at a publish step even after the review decided on its own.
#[test]
fn read_only_and_unattended_take_their_live_paths() {
    let p = Project::new(&["fixloop", "kid"]);
    let green =
        fs::read_to_string(p.playbook("fixloop").join("evals/scenarios/green.yaml")).unwrap();
    let expect_at = green.find("expect:").unwrap();
    let answers = &green[..expect_at];

    // R2: a dirty tree on the read-only summary.
    p.scenario(
        "fixloop",
        "dirty",
        &answers
            .replace("id: green\n", "id: dirty\non_extra: warn\n")
            .replace(
                "- { visit: 1, attempts: [ { status: succeeded, output: \"all done\" } ] }",
                "- { visit: 1, attempts: [ { status: succeeded, output: \"all done\", read_only: { dirty: [src/a.rs] } }, { status: succeeded } ] }",
            ),
    );
    let (_, r) = p.play("fixloop", "quick", "dirty", &[]);
    let events = journal(&run_dir(&p, &r));
    assert!(
        event_types(&events).contains(&"read_only_violated"),
        "{:?}",
        event_types(&events)
    );
    assert_eq!(attempts(&r, "summary").first().map(Vec::len), Some(1));
    assert_eq!(r["outcome"], "failed", "{r:#}");

    // U2: unattended, the review has no unattended outcome.
    p.scenario(
        "fixloop",
        "alone",
        &answers.replace(
            "id: green\n",
            "id: alone\nunattended: true\non_extra: warn\n",
        ),
    );
    let (_, r) = p.play("fixloop", "quick", "alone", &[]);
    let events = journal(&run_dir(&p, &r));
    let error = events
        .iter()
        .find(|e| e["type"] == "run_error")
        .and_then(|e| e["reason"].as_str())
        .unwrap_or("");
    assert!(error.contains("hitl_required"), "{error:?}");
    let extra = r["answers"]["extra"].to_string();
    assert!(
        extra.contains("\"review\""),
        "the review answer was read: {extra}"
    );

    // U5: the review decides on its own, the publish step still stops.
    p.edit_playbook(
        "fixloop",
        "{ id: approve, type: human_review }",
        "{ id: approve, type: human_review, unattended: { decide: approve } }",
    );
    let (_, r) = p.play("fixloop", "quick", "alone", &[]);
    let events = journal(&run_dir(&p, &r));
    let error = events
        .iter()
        .find(|e| e["type"] == "run_error")
        .and_then(|e| e["reason"].as_str())
        .unwrap_or("")
        .to_string();
    assert!(
        error.contains("hitl_required") && error.contains("ship"),
        "{error:?}"
    );
    assert_eq!(p.started(), "");
}

/// The A13 row R5: a version the validator rejects (here V07, a node no
/// route reaches) is refused before any scenario runs, with the code, and
/// leaves no scenario run behind.
#[test]
fn an_invalid_version_is_refused_before_any_run() {
    let p = Project::new(&["fixloop", "kid"]);
    p.edit_playbook(
        "fixloop",
        "{ from: gate, to: triage }",
        "{ from: gate, to: approve }",
    );
    let out = p.apb(&["eval", "fixloop", "--level", "quick", "--scenario", "green"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains("V07"), "{err}");
    let runs = p.root().join(".apb/scenario-runs");
    let played = fs::read_dir(&runs)
        .map(|d| d.flatten().any(|e| e.file_name() != "index"))
        .unwrap_or(false);
    assert!(!played, "a scenario ran on an invalid version");
}

/// A project that carries `.apb/scenario-runs` as a link (a cloned
/// repository can commit one) never moves a scenario's writes: the child
/// refuses to enter scenario mode through it, and nothing lands where the
/// link points.
#[cfg(unix)]
#[test]
fn a_linked_scenario_runs_directory_is_refused_before_anything_is_written() {
    let p = Project::new(&["fixloop", "kid"]);
    let elsewhere = tempfile::tempdir().unwrap();
    let runs = p.root().join(".apb/scenario-runs");
    let _ = fs::remove_dir_all(&runs);
    std::os::unix::fs::symlink(elsewhere.path(), &runs).unwrap();
    let out = p.apb(&[
        "eval",
        "fixloop",
        "--level",
        "quick",
        "--scenario",
        "green",
        "--json",
    ]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(out.status.code(), Some(0), "{text}");
    assert_eq!(
        fs::read_dir(elsewhere.path()).unwrap().count(),
        0,
        "a scenario wrote through the link: {text}"
    );
    assert!(p.started().is_empty(), "{}", p.started());
}

/// `--scenarios` lists and runs nothing, so a flag that would shape a run
/// is refused with it, never ignored; `--version` and `--json` go with it.
#[test]
fn listing_scenarios_refuses_a_run_flag() {
    let p = Project::new(&["fixloop", "kid"]);
    let listed = p.apb(&[
        "eval",
        "fixloop",
        "--scenarios",
        "--version",
        "1.0.0",
        "--json",
    ]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    for flag in [
        vec!["--level", "smoke"],
        vec!["--scenario", "green"],
        vec!["--yes"],
        vec!["--max-decision-requests", "3"],
        vec!["--case", "x"],
    ] {
        let mut args = vec!["eval", "fixloop", "--scenarios"];
        args.extend(flag.iter().copied());
        let out = p.apb(&args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && err.contains("cannot be used with"),
            "{flag:?}: {err}"
        );
    }
    assert!(p.started().is_empty(), "{}", p.started());
}
