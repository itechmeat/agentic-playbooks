//! Next actions on the command line: `apb runs <id>` and `apb wait` print
//! each action with its runnable command, `apb wait --follow-auto` returns
//! on the aggregated verdict, and `--request` names the request an answer
//! or a decision is for.

use std::fs;
use std::path::Path;

const GATED: &str = "schema: 2\nid: g\nname: g\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: gate, type: human_review, options: [approved, rejected] }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: gate }\n  - { from: gate, to: f }\n";

fn apb_in(root: &Path, args: &[&str]) -> std::process::Output {
    crate::common::apb_std()
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

/// A project with run `r` of the gated playbook: the review requested at
/// seq 1, then the journal lines given.
fn seed(root: &Path, after: &[serde_json::Value]) {
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let dir = root.join(".apb/runs/r");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), GATED).unwrap();
    let mut lines = vec![
        serde_json::json!({"seq": 0, "ts": 1, "type": "run_started", "playbook": "g", "version": "1.0.0"}),
        serde_json::json!({"seq": 1, "ts": 2, "type": "review_requested", "node": "gate", "options": ["approved", "rejected"]}),
    ];
    for (i, v) in after.iter().enumerate() {
        let mut v = v.clone();
        v["seq"] = serde_json::json!(2 + i);
        v["ts"] = serde_json::json!(3 + i);
        lines.push(v);
    }
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

fn paused_until_in_an_hour() -> serde_json::Value {
    let until = apb_core::clock::now_ms_u64() + 3_600_000;
    serde_json::json!({"type": "run_paused", "reason": "later", "by": "operator", "until": until})
}

/// A run paused with `until` while its review stays open: the default wait
/// says stopped (4), as before; following automatic progress it needs input
/// (3) at once, with the review as the primary action.
#[test]
fn follow_auto_on_a_paused_run_with_an_open_review_needs_input() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), &[paused_until_in_an_hour()]);

    let out = apb_in(dir.path(), &["wait", "r", "--timeout", "20"]);
    assert_eq!(out.status.code(), Some(4), "default: {out:?}");

    let out = apb_in(
        dir.path(),
        &["wait", "r", "--follow-auto", "--timeout", "20"],
    );
    assert_eq!(out.status.code(), Some(3), "follow-auto: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("apb review '--decision=<approved|rejected>' --request=review:1 r gate"),
        "{stdout}"
    );
}

/// `apb runs <id>` lists the next actions with their commands, and the
/// printed review command, its decision filled in, decides the review; the
/// same request again is refused.
#[test]
fn the_printed_review_command_decides_and_a_second_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), &[]);
    let out = apb_in(dir.path(), &["runs", "r"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("next: node `gate` waits for a human review decision"),
        "{stdout}"
    );
    // The printed line pasted as it is, its placeholder not filled in, is
    // refused and spends nothing.
    let unfilled = [
        "review",
        "--decision=<approved|rejected>",
        "--request=review:1",
        "r",
        "gate",
    ];
    let out = apb_in(dir.path(), &unfilled);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("placeholder"),
        "{out:?}"
    );
    let decide = [
        "review",
        "--decision=approved",
        "--request=review:1",
        "r",
        "gate",
    ];
    let out = apb_in(dir.path(), &decide);
    assert!(out.status.success(), "{out:?}");
    let out = apb_in(dir.path(), &decide);
    assert!(!out.status.success(), "a second decision: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already"),
        "{out:?}"
    );
    // A request of another kind is refused before anything is posted.
    let out = apb_in(dir.path(), &["answer", "--request=review:1", "r", "x"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("question"));
}

/// `apb runs <id> --json` carries the actions as the MCP run_status object
/// does, the review's request among its arguments.
#[test]
fn runs_json_carries_the_next_actions() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), &[]);
    let out = apb_in(dir.path(), &["runs", "r", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["next_actions"][0]["code"], "decide_review", "{v}");
    assert_eq!(v["next_actions"][0]["arguments"]["request"], 1);
    assert_eq!(v["stop"], "needs_input");
}

/// `apb run` of a playbook whose param has no default and no value is
/// refused before anything is written, with the start action that answers
/// it, the same refusal the dashboard and MCP give.
#[test]
fn a_start_without_a_needed_param_is_refused_with_its_action() {
    let dir = tempfile::tempdir().unwrap();
    let out = apb_in(dir.path(), &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let vdir = dir.path().join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - { name: target, type: text }\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: f }\n",
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();

    let out = apb_in(dir.path(), &["run", "p"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("run refused (param_needs_value)"), "{err}");
    assert!(err.contains("pass `--param target=<value>`"), "{err}");
    let runs = dir.path().join(".apb/runs");
    assert!(!runs.exists() || fs::read_dir(&runs).unwrap().next().is_none());

    let out = apb_in(dir.path(), &["run", "p", "--param", "target=x"]);
    assert!(out.status.success(), "{out:?}");
}

const ASKING: &str = "schema: 2\nid: q\nname: q\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: hi, interactive: true }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: f }\n";

/// A free-text question's printed answer placeholder, pasted as it is, is
/// refused and spends nothing; an answer that merely looks bracketed
/// (`<ok>`) is an answer like any other.
#[test]
fn only_the_printed_placeholder_is_refused_as_an_answer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    assert!(apb_in(root, &["init"]).status.success());
    let run = root.join(".apb/runs/qr");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("playbook.yaml"), ASKING).unwrap();
    apb_engine::question::post_question(&run, "ask", 1, "anything else?", vec![]).unwrap();
    let lines = [
        serde_json::json!({"seq": 0, "ts": 1, "type": "run_started", "playbook": "q", "version": "1.0.0"}),
        serde_json::json!({"seq": 1, "ts": 2, "type": "question_asked", "node": "ask", "attempt": 1, "question": "anything else?", "options": []}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(run.join("events.jsonl"), text).unwrap();

    let out = apb_in(root, &["answer", "qr", "<your answer>"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("placeholder of the printed command"),
        "{out:?}"
    );
    assert!(
        apb_engine::question::read_answers_after(&run, None)
            .unwrap()
            .is_empty(),
        "nothing was posted"
    );
    let out = apb_in(root, &["answer", "qr", "<ok>"]);
    assert!(out.status.success(), "{out:?}");
    let answers = apb_engine::question::read_answers_after(&run, None).unwrap();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].answer, "<ok>");
}
