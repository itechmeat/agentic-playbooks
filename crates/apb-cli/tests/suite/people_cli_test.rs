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

/// The seq of the run's gate's open request, as `--request review:<seq>`
/// names it.
fn open_request_arg(root: &Path, run_id: &str) -> String {
    open_request_arg_of(root, run_id, "gate")
}

/// [`open_request_arg`] of gate `node`.
fn open_request_arg_of(root: &Path, run_id: &str, node: &str) -> String {
    let events =
        fs::read_to_string(root.join(".apb/runs").join(run_id).join("events.jsonl")).unwrap();
    let seq = events
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .rfind(|v| v["type"] == "review_requested" && v["node"] == node)
        .map(|v| v["seq"].as_u64().expect("the request's seq"))
        .expect("the gate's open request");
    format!("review:{seq}")
}

fn apb_in(root: &Path, args: &[&str]) -> std::process::Output {
    crate::common::apb_std()
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}

/// [`apb_in`] with extra environment variables.
fn apb_in_env(root: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut c = crate::common::apb_std();
    c.args(args).current_dir(root);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
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

    let request = open_request_arg(dir.path(), &run_id);
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
            "--request",
            &request,
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
        // The decide command names the open request: a form gate takes a
        // decision only for the request it answers.
        let request = open_request_arg(dir.path(), &run_id);
        assert!(
            text.contains(&format!(
                "    decide with: apb review {run_id} gate --decision <option> --request {request} --field <name>=<value>"
            )),
            "{text}"
        );
    }
    // The printed command is followed as printed, with the option and no
    // field filled in, and it is accepted.
    let text = String::from_utf8_lossy(&waited.stdout).into_owned();
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("    decide with: apb "))
        .unwrap_or_else(|| panic!("a decide line: {text}"))
        .replace(" --field <name>=<value>", "")
        .replace("<option>", "reject");
    let argv: Vec<&str> = line.split_whitespace().collect();
    let out = apb_in(dir.path(), &argv);
    assert!(out.status.success(), "the printed command failed: {out:?}");
}

/// A script list with a pilot of two over five inline items.
const PILOT_LIST: &str = r#"schema: 2
id: cpilot
name: CLI pilot
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: work
    type: script
    script: "scripts/item.sh"
    runner: sh
    list:
      items: { inline: [ { id: i1 }, { id: i2 }, { id: i3 }, { id: i4 }, { id: i5 } ] }
      batch: { size: 2, parallel: 1, retries: 0 }
      pilot: { items: 2 }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: ok }
"#;

/// A run paused at a list pilot gate: the hint `apb wait` prints for the
/// open gate names the request the pilot's decision must carry, and the
/// command, followed as printed, is accepted.
#[test]
fn a_paused_pilot_gate_hint_names_its_request() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let vdir = root.join(".apb/playbooks/cpilot/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), PILOT_LIST).unwrap();
    fs::write(vdir.join("scripts/item.sh"), "echo 'item done'\n").unwrap();
    fs::write(root.join(".apb/playbooks/cpilot/current"), "1.0.0").unwrap();
    let out = apb_in(root, &["run", "cpilot", "--detach"]);
    assert!(out.status.success(), "run --detach failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let run_id = stdout
        .trim()
        .strip_prefix("run started: ")
        .unwrap_or_else(|| panic!("unexpected output: {stdout}"))
        .to_string();
    let _guard = crate::common::RunGuard::new(root, &run_id);
    let out = apb_in(root, &["wait", &run_id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(3), "needs input: {out:?}");
    let out = apb_in(root, &["pause", &run_id, "--reason", "later"]);
    assert!(out.status.success(), "pause failed: {out:?}");
    let out = apb_in(root, &["wait", &run_id, "--timeout", "30"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let line = text
        .lines()
        .find(|l| l.contains("open: human review (node `work`)"))
        .unwrap_or_else(|| panic!("an open gate line: {text}"));
    let request = open_request_arg_of(root, &run_id, "work");
    assert!(line.contains(&format!("--request {request}")), "{line}");
    let command = line
        .split('`')
        .find(|part| part.starts_with("apb review "))
        .unwrap_or_else(|| panic!("a review command: {line}"))
        .strip_prefix("apb ")
        .unwrap()
        .replace("<run-id>", &run_id)
        .replace("<option>", "continue");
    let argv: Vec<&str> = command.split_whitespace().collect();
    let out = apb_in(root, &argv);
    assert!(out.status.success(), "the printed command failed: {out:?}");
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

/// A form gate's decision names the request it answers: `apb review`
/// without `--request` exits 2 printing `decide_again`, and the same
/// decision naming the open request succeeds.
#[test]
fn review_without_request_on_a_form_gate_is_refused() {
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
    // The request a person names is the one the printed command carries.
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let printed_request = printed
        .split_whitespace()
        .find_map(|w| w.strip_prefix("--request="))
        .unwrap_or_else(|| panic!("the printed command names its request: {printed}"))
        .to_string();

    let out = apb_in(
        dir.path(),
        &["review", &run_id, "gate", "--decision", "reject"],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("decide_again"), "{stderr}");
    assert!(
        !dir.path()
            .join(".apb/runs")
            .join(&run_id)
            .join("reviews.jsonl")
            .exists(),
        "a refused decision is not queued"
    );

    assert_eq!(
        printed_request,
        open_request_arg(dir.path(), &run_id),
        "the printed command names the open request"
    );
    let request = printed_request;
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
            "--request",
            &request,
        ],
    );
    assert!(out.status.success(), "the named review failed: {out:?}");
    let out = apb_in(dir.path(), &["wait", &run_id, "--timeout", "30"]);
    assert_eq!(out.status.code(), Some(0), "succeeded: {out:?}");
}

/// The `--require-ready` refusal comes from preparation: a playbook
/// without a budget, started unattended, is refused `not_ready` with exit
/// 2 and no run directory.
#[test]
fn require_ready_refuses_a_playbook_without_budget() {
    let dir = tempfile::tempdir().unwrap();
    seed_ready(dir.path(), false);
    let out = apb_in(
        dir.path(),
        &["run", "rready", "--unattended", "--require-ready"],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not_ready"), "{stderr}");
    assert!(
        !dir.path().join(".apb/runs").exists()
            || fs::read_dir(dir.path().join(".apb/runs")).unwrap().count() == 0,
        "no run directory"
    );
}

/// An unattended start of a playbook that is not ready warns, naming what
/// is missing and the version the gate selected, and still starts.
#[test]
fn an_unready_unattended_start_warns() {
    let dir = tempfile::tempdir().unwrap();
    seed_ready(dir.path(), false);
    let out = apb_in(dir.path(), &["run", "rready", "--unattended", "--detach"]);
    assert!(out.status.success(), "the run starts: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: not ready to run alone:"),
        "{stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let run_id = stdout
        .trim()
        .strip_prefix("run started: ")
        .unwrap_or_default()
        .to_string();
    assert!(!run_id.is_empty());
    let _guard = crate::common::RunGuard::new(dir.path(), &run_id);
    // The run directory's snapshot names the version 1.0.0 the gate chose.
    let snapshot = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&run_id)
            .join("playbook.yaml"),
    )
    .unwrap();
    assert!(snapshot.contains("version: 1.0.0"), "the gate's version");
}

/// `--waive-probation` always asks its typed confirmation: without a
/// terminal it exits 2 (also for a playbook that asks for no probation),
/// and inside a run step it exits 2 with the run-step text; no run is
/// created in any case.
#[test]
fn waive_probation_needs_a_typed_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    // A playbook that asks for no probation: the question is asked all
    // the same.
    seed_ready(dir.path(), false);
    let no_runs = |dir: &Path| {
        !dir.join(".apb/runs").exists() || fs::read_dir(dir.join(".apb/runs")).unwrap().count() == 0
    };
    // Without a terminal.
    let out = apb_in(
        dir.path(),
        &["run", "rready", "--unattended", "--waive-probation"],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "--waive-probation needs a person at an interactive terminal to type the playbook id"
        ),
        "{stderr}"
    );
    assert!(no_runs(dir.path()), "no run directory");
    // Inside a run step: the run-step refusal, before any question.
    let out = apb_in_env(
        dir.path(),
        &["run", "rready", "--unattended", "--waive-probation"],
        &[("APB_RUN_ID", "r-step")],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("a run step does not waive probation"),
        "{stderr}"
    );
    assert!(no_runs(dir.path()), "no run directory");
}

/// `apb doctor` names a probation ledger that does not verify.
#[test]
fn doctor_names_an_unverifiable_probation_ledger() {
    let dir = tempfile::tempdir().unwrap();
    seed_ready(dir.path(), true);
    let ledger = dir.path().join(".apb/probation/project/rready.json");
    fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    fs::write(&ledger, "{not json").unwrap();
    let out = apb_in(dir.path(), &["doctor"]);
    assert!(out.status.success(), "doctor runs: {out:?}");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("probation") && text.contains("do not verify"),
        "{text}"
    );
}

/// Seeds the project with playbook `rready` (1.0.0), a plain script
/// playbook, with (or without) the top-level `unattended:` block.
fn seed_ready(root: &Path, with_unattended: bool) {
    let out = apb_in(root, &["init"]);
    assert!(out.status.success(), "apb init failed: {out:?}");
    let block = if with_unattended {
        "unattended:\n  after_attended_successes: 2\n"
    } else {
        ""
    };
    let yaml = format!(
        "schema: 2\nid: rready\nname: rready\nversion: 1.0.0\n{block}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: scripts/w.sh, runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    );
    let vdir = root.join(".apb/playbooks/rready/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(vdir.join("scripts/w.sh"), "echo ok\n").unwrap();
    fs::write(root.join(".apb/playbooks/rready/current"), "1.0.0").unwrap();
}
