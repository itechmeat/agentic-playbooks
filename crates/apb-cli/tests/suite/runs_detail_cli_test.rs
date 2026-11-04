//! `apb runs` over a journal with token usage and an event a newer apb
//! wrote (issue #167): the table flags the skipped event, `apb runs <id>`
//! shows the run's totals and the same note.

use predicates::prelude::*;
use std::fs;

use crate::common::{apb, apb_std};

const JOURNAL: &str = concat!(
    r#"{"seq":0,"ts":1790000000000,"type":"run_started","playbook":"demo","version":"1.0.0"}"#,
    "\n",
    r#"{"seq":1,"ts":1790000000001,"type":"node_started","node":"w","attempt":1}"#,
    "\n",
    r#"{"seq":2,"ts":1790000000002,"type":"attempt_finished","node":"w","attempt":1,"status":"succeeded","duration_ms":10,"session":null,"summary":"done","usage":{"input_tokens":1200,"output_tokens":300,"cache_read_tokens":5000,"cache_write_tokens":0,"cost_usd":0.0123,"source":"reported"}}"#,
    "\n",
    r#"{"seq":3,"ts":1790000000003,"type":"future_event","use":"completion_check","node":"w"}"#,
    "\n",
    r#"{"seq":4,"ts":1790000000004,"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok","artifacts":[]}"#,
    "\n",
    r#"{"seq":5,"ts":1790000000005,"type":"run_finished","outcome":"succeeded"}"#,
    "\n",
);

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run = dir.path().join(".apb/runs/demo-1");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("events.jsonl"), JOURNAL).unwrap();
    dir
}

#[test]
fn the_runs_table_flags_events_a_newer_apb_wrote() {
    let dir = project();
    apb()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "demo-1\tdemo\tsucceeded\t1 unknown event (newer apb?)",
        ));
}

#[test]
fn runs_with_an_id_shows_the_usage_totals_and_the_note() {
    let dir = project();
    apb()
        .args(["runs", "demo-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("demo-1\tdemo 1.0.0\tsucceeded"))
        .stdout(predicate::str::contains("  w\tsucceeded"))
        .stdout(predicate::str::contains(
            "  usage: 1200 input, 300 output, 5000 cache read, 0 cache write tokens over 1 attempt, cost $0.0123 basis unknown",
        ))
        .stdout(predicate::str::contains("  1 unknown event (newer apb?)"));
}

/// A limit the run journaled at start is one line of `apb runs <id>`; a
/// run without one has no such line.
#[test]
fn runs_with_an_id_shows_the_run_limits() {
    let dir = project();
    let run = dir.path().join(".apb/runs/limits-1");
    fs::create_dir_all(&run).unwrap();
    let journal = JOURNAL.replacen(
        "\n",
        &format!(
            "\n{}\n",
            r#"{"seq":100,"ts":1790000000000,"type":"run_provenance","limits":[{"code":"same_agent_reviewer","detail":"the reviewer runs on the same agent as the main role, in a fresh session; reviews are less independent"}]}"#
        ),
        1,
    );
    fs::write(run.join("events.jsonl"), journal).unwrap();
    apb()
        .args(["runs", "limits-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  limit: the reviewer runs on the same agent as the main role",
        ));
    apb()
        .args(["runs", "demo-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("limit:").not());
}

#[test]
fn runs_with_an_unknown_id_fails() {
    let dir = project();
    apb()
        .args(["runs", "nope"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("run `nope` not found"));
}

// --- 0.24.0: the execution mode in `apb runs` ---

/// A run whose manifest carries an execution block, next to the plain one.
fn project_with_host_run(execution: &str, extra_events: &str) -> tempfile::TempDir {
    let dir = project();
    let run = dir.path().join(".apb/runs/host-1");
    fs::create_dir_all(&run).unwrap();
    let fallback = if extra_events.is_empty() {
        String::new()
    } else {
        format!("{extra_events}\n")
    };
    let journal = format!(
        concat!(
            r#"{{"seq":0,"ts":1790000001000,"type":"run_started","playbook":"demo","version":"1.0.0"}}"#,
            "\n",
            r#"{{"seq":1,"ts":1790000001001,"type":"node_started","node":"w","attempt":1}}"#,
            "\n{}",
            r#"{{"seq":4,"ts":1790000001004,"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok","artifacts":[]}}"#,
            "\n",
            r#"{{"seq":5,"ts":1790000001005,"type":"run_finished","outcome":"succeeded"}}"#,
            "\n",
        ),
        fallback
    );
    fs::write(run.join("events.jsonl"), journal).unwrap();
    fs::write(
        run.join("manifest.yaml"),
        format!("profiles: []\nnode_bindings: {{}}\nexecution:\n{execution}"),
    )
    .unwrap();
    // A run with a manifest always has its config: one without it withholds.
    fs::write(run.join("run.yaml"), "params: {}\n").unwrap();
    dir
}

#[test]
fn runs_with_an_id_names_the_host_execution_mode() {
    let dir = project_with_host_run(
        "  mode: host\n  source: argument\n  client: claude-code\n",
        "",
    );
    apb()
        .args(["runs", "host-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  execution: host (argument, client claude-code)\n",
        ));
    apb()
        .args(["runs", "demo-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("  execution: cli\n"));
}

#[test]
fn runs_with_an_id_names_the_nodes_that_fell_back_to_the_host() {
    let dir = project_with_host_run(
        "  mode: cli\n  source: default\n  client: claude-code\n  fallback_to_host: true\n",
        r#"{"seq":3,"ts":1790000001003,"type":"execution_fallback","node":"w","attempt":1,"reason":"spawn failed"}"#,
    );
    apb()
        .args(["runs", "host-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  execution: cli (default, client claude-code, host fallback allowed); host fallback: w\n",
        ));
}

#[test]
fn the_runs_table_gets_a_mode_column_only_when_a_run_has_an_execution_block() {
    let plain = project();
    apb()
        .arg("runs")
        .current_dir(plain.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "demo-1\tdemo\tsucceeded\t1 unknown event (newer apb?)\n",
        ));
    let dir = project_with_host_run("  mode: host\n  source: argument\n", "");
    apb()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("host-1\tdemo\tsucceeded\thost\n"))
        .stdout(predicate::str::contains("demo-1\tdemo\tsucceeded\tcli\t"));
}

const FALLBACK_EXECUTION: &str =
    "  mode: cli\n  source: default\n  client: claude-code\n  fallback_to_host: true\n";
const FALLBACK_EVENT: &str = r#"{"seq":3,"ts":1790000001003,"type":"execution_fallback","node":"w","attempt":1,"reason":"spawn failed"}"#;

/// A `cli` run with the host fallback shows as `cli+host-fallback` in the
/// table (R2).
#[test]
fn the_runs_table_marks_a_run_with_the_host_fallback() {
    let dir = project_with_host_run(FALLBACK_EXECUTION, FALLBACK_EVENT);
    apb()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "host-1\tdemo\tsucceeded\tcli+host-fallback\n",
        ));
}

/// `--json` of a fallback run carries the nodes that fell back, the list
/// the text line names (R1).
#[test]
fn runs_json_names_the_nodes_that_fell_back() {
    let dir = project_with_host_run(FALLBACK_EXECUTION, FALLBACK_EVENT);
    let out = apb()
        .args(["runs", "host-1", "--json"])
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["execution"]["fallback_to_host"], true);
    assert_eq!(v["execution"]["fell_back"], serde_json::json!(["w"]));
}

/// `--json` needs a run id (R3).
#[test]
fn runs_json_without_an_id_is_a_usage_error() {
    let dir = project();
    apb()
        .args(["runs", "--json"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("<RUN_ID>"));
}

#[test]
fn runs_json_is_the_run_status_object() {
    let dir = project_with_host_run("  mode: host\n  source: argument\n", "");
    let out = apb()
        .args(["runs", "host-1", "--json"])
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let expected = apb_mcp::tools::run::run_status(dir.path(), "host-1").unwrap();
    assert!(
        v["execution"].get("fell_back").is_none(),
        "nothing fell back"
    );
    // `driver_alive` reads the process table at request time; the rest is
    // the same fold of the same journal.
    let strip = |mut v: serde_json::Value| {
        v.as_object_mut().unwrap().remove("driver_alive");
        v
    };
    assert_eq!(strip(v.clone()), strip(expected));
    assert_eq!(v["execution"]["mode"], "host");
    assert_eq!(v["run_status"], "succeeded");
}

/// Issue #195: `apb runs <id>` lists what a fork's branch-failure policy did
/// and which joins refused.
#[test]
fn runs_with_an_id_lists_branch_failures() {
    let dir = project();
    let run = dir.path().join(".apb/runs/fork-1");
    fs::create_dir_all(&run).unwrap();
    let journal = [
        r#"{"seq":0,"ts":1790000002000,"type":"run_started","playbook":"demo","version":"1.0.0"}"#,
        r#"{"seq":1,"ts":1790000002001,"type":"node_finished","node":"a","status":"failed","attempt":1,"output":"boom","artifacts":[]}"#,
        r#"{"seq":2,"ts":1790000002002,"type":"branch_failed","fork":"start","node":"a","policy":"fail_fast","target":"rejected"}"#,
        r#"{"seq":3,"ts":1790000002003,"type":"node_finished","node":"b","status":"cancelled","attempt":1,"output":"cancelled","artifacts":[]}"#,
        r#"{"seq":4,"ts":1790000002004,"type":"branch_cancelled","fork":"start","node":"b","failed_node":"a"}"#,
        r#"{"seq":5,"ts":1790000002005,"type":"run_finished","outcome":"failed"}"#,
    ];
    fs::write(run.join("events.jsonl"), journal.join("\n") + "\n").unwrap();
    apb()
        .args(["runs", "fork-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  branch_failed a: fork `start` fail_fast: failed, the run goes to `rejected`\n",
        ))
        .stdout(predicate::str::contains(
            "  branch_cancelled b: fork `start`: cancelled after `a` failed\n",
        ));
}

// --- the human run outcome ---

/// A run whose attempt reported a finding.
fn project_with_a_finding() -> tempfile::TempDir {
    let dir = project();
    let run = dir.path().join(".apb/runs/noted-1");
    fs::create_dir_all(&run).unwrap();
    fs::write(
        run.join("events.jsonl"),
        concat!(
            r#"{"seq":0,"ts":1790000000100,"type":"run_started","playbook":"noted","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":1790000000101,"type":"node_started","node":"w","attempt":1}"#,
            "\n",
            r#"{"seq":2,"ts":1790000000102,"type":"attempt_notes","node":"w","attempt":1,"findings":[{"kind":"security","location":"src/auth.rs:12","evidence":"token compared with ==","why_not_fixed":"needs a design decision"}]}"#,
            "\n",
            r#"{"seq":3,"ts":1790000000103,"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":"ok","artifacts":[]}"#,
            "\n",
            r#"{"seq":4,"ts":1790000000104,"type":"run_finished","outcome":"succeeded"}"#,
            "\n",
        ),
    )
    .unwrap();
    dir
}

#[test]
fn the_runs_table_marks_a_run_that_needs_a_look() {
    let dir = project_with_a_finding();
    apb()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "noted-1\tnoted\tsucceeded\tneeds a look: 1 finding",
        ))
        .stdout(
            predicate::str::contains("demo-1\tdemo\tsucceeded\t1 unknown")
                .and(predicate::str::contains("demo-1\tdemo\tsucceeded\tneeds a look").not()),
        );
}

#[test]
fn runs_with_an_id_prints_the_outcome_with_its_findings() {
    let dir = project_with_a_finding();
    apb()
        .args(["runs", "noted-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  outcome: Run succeeded; nothing checked by the engine; needs a look (1 finding)",
        ))
        .stdout(predicate::str::contains("    Findings:"))
        .stdout(predicate::str::contains(
            "      security at src/auth.rs:12: token compared with == (not fixed: needs a design decision; reported by w attempt 1)",
        ));
}

const LISTED: &str = "schema: 2\nid: listed\nname: listed\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: start, type: start }\n  - id: rows\n    type: agent_task\n    prompt: Check the row in the item section.\n    list:\n      items: { inline: [ { id: a }, { id: b } ] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: rows }\n  - { from: rows, to: done }\n";

const LIST_JOURNAL: &str = concat!(
    r#"{"seq":0,"ts":1,"type":"run_started","playbook":"listed","version":"1.0.0"}"#,
    "\n",
    r#"{"seq":1,"ts":2,"type":"list_started","node":"rows","total":2,"items_digest":"d","batch_size":2,"parallel":1}"#,
    "\n",
    r#"{"seq":2,"ts":3,"type":"list_item_settled","node":"rows","item":"a","state":"passed","verdict":"accept","round":0}"#,
    "\n",
    r#"{"seq":3,"ts":4,"type":"list_item_settled","node":"rows","item":"b","state":"needs_look","reason":"failed","round":1}"#,
    "\n",
);

#[test]
fn runs_items_prints_one_line_per_item_in_the_state_asked() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run = dir.path().join(".apb/runs/listed-1");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("playbook.yaml"), LISTED).unwrap();
    fs::write(run.join("events.jsonl"), LIST_JOURNAL).unwrap();
    apb()
        .args(["runs", "listed-1", "--items", "--state", "needs_look"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "rows: 2 of 2 processed, 1 passed, 1 need a look\n  b\tneeds_look\tround 1\tfailed\n",
        ))
        .stdout(predicate::str::contains("  a\t").not());
    apb()
        .args(["runs", "listed-1", "--items", "--node", "other"])
        .current_dir(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no list node `other`"));
    apb()
        .args(["runs", "listed-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "List rows: 2 processed, 1 passed, 1 needs a look",
        ));
}

// --- isolation: one line per isolated attempt ---

#[test]
fn runs_with_an_id_prints_one_isolation_line_per_isolated_attempt() {
    let dir = project();
    let run = dir.path().join(".apb/runs/iso-1");
    fs::create_dir_all(&run).unwrap();
    fs::write(
        run.join("events.jsonl"),
        concat!(
            r#"{"seq":0,"ts":1790000000000,"type":"run_started","playbook":"demo","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":1790000000001,"type":"attempt_started","node":"w","attempt":1,"agent":"claude","isolation":{"level":"full","backend":"bwrap","network":"none","limits":["user.host_uid","kernel.escapes"]}}"#,
            "\n",
            r#"{"seq":2,"ts":1790000000002,"type":"attempt_started","node":"b","attempt":1,"agent":"claude","isolation":{"level":"best_effort","backend":"none","network":"open","limits":["no_sandbox"],"fallbacks":["disabled (APB_SANDBOX=off)"]}}"#,
            "\n",
            r#"{"seq":3,"ts":1790000000003,"type":"attempt_started","node":"plain","attempt":1,"agent":"claude"}"#,
            "\n",
        ),
    )
    .unwrap();
    let out = apb()
        .args(["runs", "iso-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  isolation node `w` attempt 1: full on bwrap, network none; limits: user.host_uid, kernel.escapes",
        ))
        .stdout(predicate::str::contains(
            "  isolation node `b` attempt 1: best_effort on none, network open; limits: no_sandbox; no sandbox: disabled (APB_SANDBOX=off)",
        ))
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.matches("  isolation ").count(), 2, "{text}");
}

/// A finished run whose drive claim names a process that is gone (a
/// sub-playbook child its parent drove in-process) is not "driver dead",
/// neither in the table nor in the run's own view.
#[test]
fn a_finished_run_is_never_listed_as_driver_dead() {
    let dir = project();
    // A pid no live process holds: far above any pid_max.
    fs::write(dir.path().join(".apb/runs/demo-1/driver.pid"), "2147483646").unwrap();
    for args in [vec!["runs"], vec!["runs", "demo-1"]] {
        apb()
            .args(&args)
            .current_dir(dir.path())
            .assert()
            .success()
            .stdout(predicate::str::contains("succeeded"))
            .stdout(predicate::str::contains("driver dead").not());
    }
    // The control: the same claim on a run that has not finished is
    // listed as driver dead, so the check above can see the words.
    let open = dir.path().join(".apb/runs/open-1");
    fs::create_dir_all(&open).unwrap();
    let unfinished: String = JOURNAL
        .lines()
        .filter(|l| !l.contains("run_finished"))
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(open.join("events.jsonl"), unfinished).unwrap();
    fs::write(open.join("driver.pid"), "2147483646").unwrap();
    for args in [vec!["runs"], vec!["runs", "open-1"]] {
        apb()
            .args(&args)
            .current_dir(dir.path())
            .assert()
            .success()
            .stdout(predicate::str::contains("driver dead"));
    }
}

/// `apb runs | head` closes the pipe early: the CLI ends quietly with the
/// status of a process `SIGPIPE` ended, never a panic on stderr.
#[test]
fn a_closed_stdout_ends_the_cli_quietly() {
    let dir = project();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let out = apb_std()
        .arg("runs")
        .current_dir(dir.path())
        .stdout(writer)
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(out.status.code(), Some(141), "{stderr}");
}

/// A judge output in the journal cannot forge terminal lines in the route
/// block: control characters and line breaks are made harmless.
#[test]
fn the_route_block_neutralizes_control_characters_from_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let run = dir.path().join(".apb/runs/judge-1");
    fs::create_dir_all(&run).unwrap();
    let yaml = "schema: 2\nid: j\nname: j\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - id: triage\n    type: judge\n    questions:\n      ok: { type: noul, instructions: i }\n    on_unavailable: { route: done }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: triage }\n  - { from: triage, to: done }\n";
    let digest = apb_engine::run_definitions::store(&run, yaml.as_bytes()).unwrap();
    fs::write(run.join("playbook.yaml"), yaml).unwrap();
    fs::write(run.join("run.yaml"), "params: {}\n").unwrap();
    let output = serde_json::json!({"decided_by": "x\u{1b}[2J\nforged: line"}).to_string();
    let lines = [
        serde_json::json!({"seq":1,"ts":1,"type":"run_started","playbook":"j","version":"1.0.0","definition":digest}),
        serde_json::json!({"seq":2,"ts":2,"type":"node_started","node":"triage","attempt":1}),
        serde_json::json!({"seq":3,"ts":3,"type":"node_finished","node":"triage","status":"succeeded","attempt":1,"output":output}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(run.join("events.jsonl"), text).unwrap();
    let out = apb_std()
        .args(["runs", "judge-1"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("judge triage #1: decided by x"), "{stdout}");
    assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
    assert!(!stdout.lines().any(|l| l.starts_with("forged")), "{stdout}");
}

/// Every dollar figure says what it is: the run's cost is split by basis in
/// plain words, so a subscription equivalent never reads as money spent.
#[test]
fn runs_with_an_id_splits_the_cost_by_basis() {
    let dir = project();
    let attempt = |seq: u64, cost: f64, basis: &str| {
        format!(
            r#"{{"seq":{seq},"ts":1790000000002,"type":"attempt_finished","node":"w","attempt":{seq},"status":"succeeded","duration_ms":10,"usage":{{"input_tokens":10,"output_tokens":5,"cost_usd":{cost},"source":"reported","cost_basis":"{basis}"}}}}"#
        )
    };
    let journal = [
        r#"{"seq":0,"ts":1790000000000,"type":"run_started","playbook":"demo","version":"1.0.0"}"#
            .to_string(),
        attempt(1, 0.01, "billed"),
        attempt(2, 0.0125, "subscription_equivalent"),
        r#"{"seq":3,"ts":1790000000005,"type":"run_finished","outcome":"succeeded"}"#.to_string(),
    ]
    .join("\n");
    let run = dir.path().join(".apb/runs/demo-2");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("events.jsonl"), journal + "\n").unwrap();
    apb()
        .args(["runs", "demo-2"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "over 2 attempts, cost $0.0100 billed; $0.0125 subscription equivalent, not billed",
        ));
}

// --- read-only node: one line per checked attempt ---

/// `apb runs <id>` prints one line per read-only check: an agent attempt's
/// tier from its start with the violation that failed it, a partial
/// before-state with its limit, and a script's violation (no start of its
/// own); `--json` carries the same rows.
#[test]
fn runs_with_an_id_prints_one_line_per_read_only_check() {
    let dir = project();
    let run = dir.path().join(".apb/runs/ro-1");
    fs::create_dir_all(&run).unwrap();
    fs::write(
        run.join("events.jsonl"),
        concat!(
            r#"{"seq":0,"ts":1790000000000,"type":"run_started","playbook":"demo","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":1,"ts":1790000000001,"type":"read_only_checked","node":"look","attempt":1,"check":{"tier":"enforced"}}"#,
            "\n",
            r#"{"seq":2,"ts":1790000000002,"type":"read_only_checked","node":"scan","attempt":1,"check":{"tier":"detected_partial","entries":20000,"bytes":4096,"partial":"entries"}}"#,
            "\n",
            r#"{"seq":3,"ts":1790000000003,"type":"read_only_violated","node":"scan","attempt":1,"tier":"detected_partial","changes":[{"path":"notes.md","change":"modified"}],"evidence":"read_only/scan/1.json"}"#,
            "\n",
            r#"{"seq":4,"ts":1790000000004,"type":"node_finished","node":"scan","status":"failed","attempt":1,"output":"","artifacts":[]}"#,
            "\n",
            r#"{"seq":5,"ts":1790000000005,"type":"read_only_violated","node":"sh","attempt":1,"changes":[{"path":".env","change":"added"}]}"#,
            "\n",
        ),
    )
    .unwrap();
    let out = apb()
        .args(["runs", "ro-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    for line in [
        "  read-only node `look` attempt 1: enforced (the sandbox binds the tree read-only)",
        "  read-only node `scan` attempt 1: detected_partial (checked after the attempt over 20000 entries, 4096 bytes; the entries limit stopped it short, so a change past it goes unseen); changed the tree (read_only_dirty), 1 path: notes.md modified; evidence read_only/scan/1.json",
        "  read-only node `sh` attempt 1: detected (checked after the attempt); changed the tree (read_only_dirty), 1 path: .env added",
    ] {
        assert!(
            text.contains(&format!("{line}\n")),
            "missing {line:?} in\n{text}"
        );
    }
    let json = apb()
        .args(["runs", "ro-1", "--json"])
        .current_dir(dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let rows = v["read_only"].as_array().expect("read_only rows");
    assert_eq!(rows.len(), 3, "{v}");
    assert_eq!(rows[0]["tier"], "enforced");
    assert_eq!(rows[1]["partial"], "entries");
    assert_eq!(rows[1]["violated"]["evidence"], "read_only/scan/1.json");
    assert_eq!(rows[2]["violated"]["changes"][0]["path"], ".env");
}

// --- PR 21a: the attempt identity in `apb runs <id>` ---

/// One journal for the attempt identity surfaces (PR 21a, card A5): a CLI
/// primary that fails and a fallback entered after `check_rejected`; a
/// host attempt whose blocked task reported no model and whose follow-up
/// reported `glm-5.3-flash` (partial); one whose two tasks reported
/// different models (mixed); one that reported the hinted model.
const IDENTITY_JOURNAL: &[&str] = &[
    r#"{"seq":0,"ts":1790000002000,"type":"run_started","playbook":"demo","version":"1.0.0"}"#,
    r#"{"seq":1,"ts":1790000002001,"type":"attempt_started","node":"a","attempt":1,"agent":"claude","model":"sonnet","exec":{"binding":"sha256:aaaaaaaaaaaaaaaaaaaaaaaa","step":0,"source":{"kind":"primary"},"executed_by":"cli"}}"#,
    r#"{"seq":2,"ts":1790000002002,"type":"attempt_finished","node":"a","attempt":1,"status":"failed"}"#,
    r#"{"seq":3,"ts":1790000002003,"type":"attempt_started","node":"a","attempt":2,"agent":"claude","model":"haiku","exec":{"binding":"sha256:aaaaaaaaaaaaaaaaaaaaaaaa","step":1,"source":{"kind":"fallback","index":1,"of":1},"executed_by":"cli","entered":{"from_step":0,"closed_attempt":1,"reason":"check_rejected"}}}"#,
    r#"{"seq":4,"ts":1790000002004,"type":"attempt_finished","node":"a","attempt":2,"status":"succeeded"}"#,
    r#"{"seq":5,"ts":1790000002005,"type":"host_task_requested","task_id":"b-1","node":"b","attempt":1,"model_hint":"opus"}"#,
    r#"{"seq":6,"ts":1790000002006,"type":"attempt_started","node":"b","attempt":1,"agent":"host","model":"opus"}"#,
    r#"{"seq":7,"ts":1790000002007,"type":"host_task_submitted","task_id":"b-1","status":"blocked","submitted_by":"host"}"#,
    r#"{"seq":8,"ts":1790000002008,"type":"host_task_requested","task_id":"b-2","node":"b","attempt":1}"#,
    r#"{"seq":9,"ts":1790000002009,"type":"host_task_submitted","task_id":"b-2","status":"succeeded","submitted_by":"host","model":"glm-5.3-flash"}"#,
    r#"{"seq":10,"ts":1790000002010,"type":"attempt_finished","node":"b","attempt":1,"status":"succeeded"}"#,
    r#"{"seq":11,"ts":1790000002011,"type":"host_task_requested","task_id":"c-1","node":"c","attempt":1,"model_hint":"opus"}"#,
    r#"{"seq":12,"ts":1790000002012,"type":"attempt_started","node":"c","attempt":1,"agent":"host","model":"opus"}"#,
    r#"{"seq":13,"ts":1790000002013,"type":"host_task_submitted","task_id":"c-1","status":"blocked","submitted_by":"host","model":"opus"}"#,
    r#"{"seq":14,"ts":1790000002014,"type":"host_task_requested","task_id":"c-2","node":"c","attempt":1}"#,
    r#"{"seq":15,"ts":1790000002015,"type":"host_task_submitted","task_id":"c-2","status":"succeeded","submitted_by":"host","model":"sonnet"}"#,
    r#"{"seq":16,"ts":1790000002016,"type":"attempt_finished","node":"c","attempt":1,"status":"succeeded"}"#,
    r#"{"seq":17,"ts":1790000002017,"type":"host_task_requested","task_id":"d-1","node":"d","attempt":1,"model_hint":"opus"}"#,
    r#"{"seq":18,"ts":1790000002018,"type":"attempt_started","node":"d","attempt":1,"agent":"host","model":"opus"}"#,
    r#"{"seq":19,"ts":1790000002019,"type":"host_task_submitted","task_id":"d-1","status":"succeeded","submitted_by":"host","model":"claude-opus-4-1"}"#,
    r#"{"seq":20,"ts":1790000002020,"type":"attempt_finished","node":"d","attempt":1,"status":"succeeded"}"#,
    r#"{"seq":21,"ts":1790000002021,"type":"host_task_requested","task_id":"e-2","node":"summarize-the-findings","attempt":2,"model_hint":"sonnet"}"#,
    r#"{"seq":22,"ts":1790000002022,"type":"attempt_started","node":"summarize-the-findings","attempt":2,"agent":"host","model":"sonnet","exec":{"binding":"sha256:bbbbbbbbbbbbbbbbbbbbbbbb","step":1,"source":{"kind":"fallback","index":1,"of":1},"executed_by":"host","entered":{"from_step":0,"closed_attempt":17,"reason":"check_rejected"}}}"#,
    r#"{"seq":23,"ts":1790000002023,"type":"host_task_submitted","task_id":"e-2","status":"succeeded","submitted_by":"host","model":"glm-5.3-flash"}"#,
    r#"{"seq":24,"ts":1790000002024,"type":"attempt_finished","node":"summarize-the-findings","attempt":2,"status":"succeeded"}"#,
    r#"{"seq":25,"ts":1790000002025,"type":"run_finished","outcome":"succeeded"}"#,
];

/// A CLI attempt names the model apb requested and its chain position, a
/// fallback the reason its step was entered; a host attempt names what the
/// host reported (partial, mixed or one model), never the hint as the
/// model, and whether the hint was followed; the run's hint follow line
/// counts them.
#[test]
fn runs_with_an_id_shows_requested_and_reported_models_and_why_a_step_was_entered() {
    let dir = project();
    let run = dir.path().join(".apb/runs/ident-1");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("events.jsonl"), IDENTITY_JOURNAL.join("\n") + "\n").unwrap();
    let out = apb_std()
        .args(["runs", "ident-1"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for line in [
        "  model a attempt 1 (cli): sonnet requested by apb, step 0 primary\n",
        "  model a attempt 2 (cli): haiku requested by apb, step 1 fallback 1 of 1, entered from step 0: check_rejected\n",
        "  model b attempt 1 (host): partial, glm-5.3-flash reported by the host, 1 task not reported, hint opus not followed, position not recorded\n",
        "  model c attempt 1 (host): mixed, opus, sonnet reported by the host, hint opus not followed, position not recorded\n",
        "  model d attempt 1 (host): claude-opus-4-1 reported by the host, hint opus followed, position not recorded\n",
        // A long host line keeps its reason: the line is not cut at the
        // width of a question.
        "  model summarize-the-findings attempt 2 (host): glm-5.3-flash reported by the host, hint sonnet not followed, step 1 fallback 1 of 1, entered from step 0: check_rejected\n",
        "  hints: 4 hinted, 1 followed, 3 not followed, 0 not reported\n",
    ] {
        assert!(text.contains(line), "missing {line:?} in:\n{text}");
    }
    assert!(
        !text.contains("(host): opus"),
        "a hint shown as the model:\n{text}"
    );
}

/// A run whose `run.yaml` does not parse shows its own clear config
/// error in the listing and in its detail, keeps every other run fully
/// readable, and never prints a secret value: its own text withholds as
/// `[secrets_unresolved]` until the file reads again.
#[test]
fn a_run_with_an_unreadable_config_names_the_error_hides_no_run_and_no_secret() {
    let dir = project();
    let bad = dir.path().join(".apb/runs/bad-1");
    fs::create_dir_all(&bad).unwrap();
    fs::write(bad.join("events.jsonl"), JOURNAL).unwrap();
    // `guarded` is no run mode: the file does not parse. The value the
    // parser quotes must never reach a surface.
    fs::write(bad.join("run.yaml"), "mode: guarded\n").unwrap();
    apb()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("demo-1\tdemo\tsucceeded"))
        // The bad run keeps its journal facts and names the error.
        .stdout(predicate::str::contains("bad-1\tdemo\tsucceeded"))
        .stdout(predicate::str::contains("config error:"))
        .stdout(predicate::str::contains("unknown variant").not());
    // The detail shows the error and the withheld text, never the
    // parser's quote.
    let out = apb()
        .args(["runs", "bad-1"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("config error:") && text.contains("run.yaml does not parse for this apb"),
        "the detail names the config error:\n{text}"
    );
    assert!(
        !text.contains("unknown variant") && !text.contains("guarded"),
        "no parser quote of the file's value:\n{text}"
    );
    assert!(
        text.contains("demo 1.0.0\tsucceeded") && text.contains("[secrets_unresolved]"),
        "the run shows with its text withheld:\n{text}"
    );
    // The JSON list keeps the good run readable and names the error on
    // the bad one.
    let out = apb()
        .args(["runs", "--playbook", "demo", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("\"config_error\"")
            && text.contains("run.yaml does not parse for this apb")
            && !text.contains("unknown variant"),
        "the JSON names the error:\n{text}"
    );
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let rows = v["runs"].as_array().expect("listing rows");
    let good = rows.iter().find(|r| r["run_id"] == "demo-1").unwrap();
    assert!(
        good.get("secrets_unresolved").is_none(),
        "the good run withholds nothing: {good}"
    );
    let bad_row = rows.iter().find(|r| r["run_id"] == "bad-1").unwrap();
    assert_eq!(bad_row["secrets_unresolved"], true);
    assert_eq!(bad_row["status"], "succeeded");
}

/// A guarded run's manifest carries the internal `+guard` suffix; no
/// surface shows it: the detail line reads `cli (guarded)`, the table
/// column reads `cli`, and `run_status` reports `mode: cli` with
/// `guarded: true`.
#[test]
fn a_guarded_runs_mode_reads_as_its_base_mode_with_a_guard_attribute() {
    let dir = project_with_host_run("  mode: cli+guard\n  source: default\n", "");
    apb()
        .args(["runs", "host-1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  execution: cli (default, guarded)\n",
        ))
        .stdout(predicate::str::contains("+guard").not());
    apb()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("host-1\tdemo\tsucceeded\tcli\n"))
        .stdout(predicate::str::contains("+guard").not());
    let out = apb_mcp::tools::run::run_status(dir.path(), "host-1").unwrap();
    assert_eq!(out["execution"]["mode"], "cli");
    assert_eq!(out["execution"]["guarded"], true);
}
