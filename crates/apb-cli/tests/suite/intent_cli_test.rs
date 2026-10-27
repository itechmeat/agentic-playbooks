//! `apb intent` through the real binary: the send intents of a run listed,
//! and a person's decision on an unknown one recorded once.

use std::path::Path;

use crate::common::apb;

/// A run whose one send went out with no answer recorded.
fn seed(root: &Path) {
    let dir = root.join(".apb/runs/r1");
    std::fs::create_dir_all(&dir).unwrap();
    let lines = [
        r#"{"seq":0,"ts":1,"type":"send_intent_recorded","intent":"si-a","nonce":"n","node":"announce","attempt":2,"connector":"telegram","function":"send_message","account":"main","mode":"none","label":"release-1.4","pid":0,"deadline_ms":0}"#,
        r#"{"seq":1,"ts":2,"type":"send_intent_settled","intent":"si-a","outcome":"unknown","source":"response","detail":"timeout"}"#,
    ];
    std::fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn intent_list_names_the_unknown_send_and_resolve_settles_it_once() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run = |args: &[&str]| apb().current_dir(dir.path()).args(args).output().unwrap();

    let listed = run(&["intent", "list", "r1"]);
    assert!(listed.status.success());
    let text = stdout(&listed);
    assert!(text.contains(
        "si-a telegram.send_message intent \"release-1.4\" by announce attempt 2 [unknown via response]: timeout"
    ));
    assert!(text.contains("1 send is not known"));

    // A decision is required, and only one of the two.
    assert!(!run(&["intent", "resolve", "r1", "si-a"]).status.success());
    assert!(
        !run(&["intent", "resolve", "r1", "si-a", "--sent", "--not-sent"])
            .status
            .success()
    );

    // From inside a run (an agent step or a host task's subagent) a
    // decision is refused: only a person decides what a send did.
    for (var, value) in [("APB_RUN_ID", "r9"), ("APB_HOST_TASK", "t-1")] {
        let inside = apb()
            .current_dir(dir.path())
            .env(var, value)
            .args(["intent", "resolve", "r1", "si-a", "--sent"])
            .output()
            .unwrap();
        assert_eq!(inside.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&inside.stderr).contains("report the send as a finding"));
    }

    let resolved = run(&[
        "intent",
        "resolve",
        "r1",
        "si-a",
        "--sent",
        "--note",
        "seen in the channel",
    ]);
    assert!(resolved.status.success());
    assert!(stdout(&resolved).contains("settled as sent"));

    let again = run(&["intent", "resolve", "r1", "si-a", "--not-sent"]);
    assert_eq!(again.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&again.stderr).contains("already confirmed as sent"));

    let json = run(&["intent", "list", "r1", "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout(&json).trim()).unwrap();
    assert_eq!(v[0]["state"], "confirmed");
    assert_eq!(v[0]["source"], "person");
    assert_eq!(v[0]["settled_by"], "a person, through apb intent resolve");

    let missing = run(&["intent", "list", "nope"]);
    assert_eq!(missing.status.code(), Some(2));
}
