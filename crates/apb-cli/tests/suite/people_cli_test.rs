//! `apb review --field` on the command line (people-in-the-loop D3): the
//! values a decision carries reach the gate - the journaled `review_fields`
//! event carries them and the run proceeds - and a `--field` value without
//! `=` is refused before anything is posted.

use std::fs;
use std::path::Path;

/// A fielded gate: `amount` is required for `approve`, and the approve
/// decision finishes the run.
const FIELDS_GATE: &str = r#"
schema: 2
id: rform
name: RForm
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approve, reject], fields: [{ name: amount, type: number, required: [approve] }] }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approve } }
"#;

fn apb_in(root: &Path, args: &[&str]) -> std::process::Output {
    crate::common::apb_std()
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

/// A project with the fielded-gate playbook installed as its current
/// version (the same seeding `run_wait_cli_test` uses).
fn seed(root: &Path) {
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let vdir = root.join(".apb/playbooks/rform/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), FIELDS_GATE).unwrap();
    fs::write(root.join(".apb/playbooks/rform/current"), "1.0.0").unwrap();
}

/// `apb review --field amount=1180.5` decides the gate: the run proceeds to
/// success and the journaled `review_fields` event carries the value.
#[test]
fn review_field_flags_reach_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let out = apb_in(dir.path(), &["run", "rform", "--detach"]);
    assert!(out.status.success(), "run --detach failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let run_id = stdout
        .trim()
        .strip_prefix("run started: ")
        .unwrap_or_else(|| panic!("unexpected output: {stdout}"))
        .to_string();
    let _guard = crate::common::RunGuard::new(dir.path(), &run_id);

    let out = apb_in(dir.path(), &["wait", &run_id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(3), "needs input: {out:?}");

    let out = apb_in(
        dir.path(),
        &[
            "review",
            &run_id,
            "gate",
            "--decision",
            "approve",
            "--field",
            "amount=1180.5",
        ],
    );
    assert!(out.status.success(), "review failed: {out:?}");

    let out = apb_in(dir.path(), &["wait", &run_id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(0), "succeeded: {out:?}");

    let events = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&run_id)
            .join("events.jsonl"),
    )
    .unwrap();
    let fields_event = events
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["type"] == "review_fields" && v["node"] == "gate")
        .expect("a review_fields event in the journal");
    assert_eq!(fields_event["fields"]["amount"], serde_json::json!(1180.5));
}

/// A `--field` value without `=` is refused with exit 2 before anything is
/// posted, and the refusal does not repeat what was typed (it may be a
/// value pasted without its name).
#[test]
fn a_field_without_equals_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = apb_in(
        dir.path(),
        &[
            "review",
            "r1",
            "gate",
            "--decision",
            "approve",
            "--field",
            "s3cret-pasted-value",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--field needs NAME=VALUE"), "{stderr}");
    assert!(!stderr.contains("s3cret-pasted-value"), "{stderr}");
}

/// A gate that shows a param's text beside its field: the text a run
/// renders is printed through the terminal sanitizer.
const SHOWN_GATE: &str = r#"
schema: 2
id: rform
name: RForm
version: 1.0.0
params:
  - { name: note, type: text }
nodes:
  - { id: start, type: start }
  - id: gate
    type: human_review
    options: [approve, reject]
    show: [{ label: Note, text: "{{params.note}}" }]
    fields: [{ name: amount, type: number, required: [approve] }]
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approve } }
"#;

/// `apb wait` and `apb runs <id>` on a run waiting at a fielded gate print
/// what the gate shows beside its options, not only the decide command:
/// the scope, the shown text (its control characters replaced) and the
/// field with its requirement.
#[test]
fn a_waiting_gate_prints_its_scope_show_and_fields() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    fs::write(
        dir.path().join(".apb/playbooks/rform/1.0.0/playbook.yaml"),
        SHOWN_GATE,
    )
    .unwrap();
    let out = apb_in(
        dir.path(),
        &[
            "run",
            "rform",
            "--detach",
            "--param",
            "note=see\u{1b}[2Jthis",
        ],
    );
    assert!(out.status.success(), "run --detach failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let run_id = stdout
        .trim()
        .strip_prefix("run started: ")
        .unwrap_or_else(|| panic!("unexpected output: {stdout}"))
        .to_string();
    let _guard = crate::common::RunGuard::new(dir.path(), &run_id);

    let waited = apb_in(dir.path(), &["wait", &run_id, "--timeout", "30"]);
    assert_eq!(waited.status.code(), Some(3), "needs input: {waited:?}");
    let shown = apb_in(dir.path(), &["runs", &run_id]);
    for out in [&waited, &shown] {
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("    scope: nothing changed since the last decision"),
            "{text}"
        );
        assert!(text.contains("    Note: see [2Jthis"), "{text}");
        assert!(
            !text.contains('\u{1b}'),
            "an escape reached the terminal: {text:?}"
        );
        assert!(
            text.contains("    field amount (number, required for approve): default none"),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "    decide with: apb review {run_id} gate --decision <option> --field <name>=<value>"
            )),
            "{text}"
        );
    }
    let out = apb_in(
        dir.path(),
        &["review", &run_id, "gate", "--decision", "reject"],
    );
    assert!(out.status.success(), "review failed: {out:?}");
}

/// A run id that is no single path segment of an existing run is refused
/// before anything is posted, as before decisions carried values.
#[test]
fn review_refuses_an_unsafe_or_missing_run_id() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    // A run directory the escape would reach.
    let elsewhere = dir.path().join("other/.apb/runs/r1");
    fs::create_dir_all(&elsewhere).unwrap();
    for id in ["../../other/.apb/runs/r1", "r-missing"] {
        let out = apb_in(dir.path(), &["review", id, "gate", "--decision", "approve"]);
        assert_eq!(out.status.code(), Some(2), "{id}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("not found"), "{id}: {stderr}");
    }
    assert!(
        !elsewhere.join("reviews.jsonl").exists(),
        "nothing was posted outside the project"
    );
}
