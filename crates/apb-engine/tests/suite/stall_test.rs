//! The stall exit (spec node-contracts D5) end to end: with `stall_after`
//! set, a node whose check keeps rejecting with the same reason stops
//! retrying, ends `failed` with a `stalled:` output, and routes on a
//! `stalled` edge before a `failure` edge. A recording stub stands in for
//! the agent; the rejections come from a `success_check`.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::{recording_stub, run_with_stub, seed_playbook};

const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";

/// The single run directory under `root`.
fn run_dir(root: &Path) -> std::path::PathBuf {
    fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
        .expect("one run")
}

/// A playbook whose `w` node's check always fails: `stall_after` on the
/// node, a `stalled` edge routed before the failure fallback.
fn playbook(extra: &str, check: &str) -> String {
    format!(
        r#"schema: 2
id: st
name: ST
version: 1.0.0
defaults: {{ profile: main }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: w, type: agent_task, prompt: "do", max_retries: 5, success_check: {check}{extra} }}
  - {{ id: repair, type: script, script: scripts/route.sh, runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
  - {{ id: bad, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: w }}
  - {{ from: w, to: repair, condition: {{ type: node_status, node: w, equals: stalled }} }}
  - {{ from: w, to: done, condition: {{ type: node_status, node: w, equals: success }} }}
  - {{ from: w, to: bad, fallback: true }}
  - {{ from: repair, to: done }}
"#
    )
}

fn seed(root: &Path, extra: &str) {
    seed_with_check(root, extra, "\"scripts/check.sh\"");
}

fn seed_with_check(root: &Path, extra: &str, check: &str) {
    init_project(root).unwrap();
    seed_playbook(root, "st", &playbook(extra, check));
    let vdir = root.join(".apb/playbooks/st/1.0.0/scripts");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("check.sh"), "#!/bin/sh\nexit 1\n").unwrap();
    fs::write(vdir.join("route.sh"), "echo routed\n").unwrap();
    common::seed_main(root);
}

/// Every `attempt_finished` of `w`, in journal order.
fn attempts(root: &Path) -> Vec<(String, Option<String>, bool)> {
    read_all(&run_dir(root))
        .unwrap()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node,
                rejection_fingerprint,
                stalled,
                ..
            } if node == "w" => Some((status_of(e), rejection_fingerprint.clone(), *stalled)),
            _ => None,
        })
        .collect()
}

fn status_of(e: &apb_engine::event::Event) -> String {
    match &e.payload {
        EventPayload::AttemptFinished { status, .. } => status.clone(),
        _ => String::new(),
    }
}

/// The failed node's terminal output.
fn node_output(root: &Path, node: &str) -> String {
    read_all(&run_dir(root))
        .unwrap()
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n,
                output,
                status,
                ..
            } if n == node && status == "failed" => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Whether an edge `from -> to` was traversed.
fn traversed(root: &Path, from: &str, to: &str) -> bool {
    read_all(&run_dir(root))
        .unwrap()
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::EdgeTraversed { from: f, to: t, .. } if f == from && t == to))
}

/// B2.3.1: three identical rejections under `stall_after: 3` end the node
/// after exactly three attempts, the last one `stalled`, with the
/// `stalled:` output and the `stalled` edge taken before the fallback.
#[test]
fn three_identical_rejections_stall_at_three_and_route_stalled() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), ", stall_after: 3");
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "st", &stub, None),
        RunStatus::Succeeded
    );
    let all = attempts(dir.path());
    assert_eq!(all.len(), 3, "{all:?}");
    assert!(all.iter().all(|(s, fp, _)| s == "failed" && fp.is_some()));
    assert!(all[2].2, "the last attempt carries stalled: true");
    assert!(!all[0].2 && !all[1].2);
    let out = node_output(dir.path(), "w");
    assert!(out.starts_with("stalled:"), "{out}");
    assert!(
        out.contains("success_check `scripts/check.sh` failed"),
        "{out}"
    );
    assert!(traversed(dir.path(), "w", "repair"), "the stalled edge ran");
    assert!(!traversed(dir.path(), "w", "bad"));
}

/// A stalled node is `failed` too: with no `stalled` edge, a `failure`
/// edge takes it (D5 wrong implementation 6).
#[test]
fn a_failure_edge_still_takes_a_stalled_node() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), ", stall_after: 3");
    let pb = dir.path().join(".apb/playbooks/st/1.0.0/playbook.yaml");
    let yaml = fs::read_to_string(&pb)
        .unwrap()
        .replace("equals: stalled", "equals: failure");
    fs::write(&pb, yaml).unwrap();
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "st", &stub, None),
        RunStatus::Succeeded
    );
    let all = attempts(dir.path());
    assert_eq!(all.len(), 3, "{all:?}");
    assert!(all[2].2, "the third identical rejection stalls");
    assert!(traversed(dir.path(), "w", "repair"), "the failure edge ran");
}

/// The recovery path (a written success verdict on a non-zero exit)
/// stalls the same way as the report path (D5 wrong implementation 8).
#[test]
fn the_recovery_path_stalls_too() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), ", stall_after: 3");
    let part =
        format!("printf '%s' '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"; {OK}\nexit 1");
    let stub = recording_stub(dir.path(), &part);
    assert_eq!(
        run_with_stub(dir.path(), "st", &stub, None),
        RunStatus::Succeeded
    );
    let all = attempts(dir.path());
    assert_eq!(all.len(), 3, "{all:?}");
    assert!(all[2].2, "the last attempt carries stalled: true");
    let out = node_output(dir.path(), "w");
    assert!(out.starts_with("stalled:"), "{out}");
    assert!(traversed(dir.path(), "w", "repair"), "the stalled edge ran");
}

/// B2.3.2: without `stall_after` every retry runs, each with a fingerprint.
#[test]
fn without_stall_after_all_retries_run_with_fingerprints() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "");
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "st", &stub, None),
        RunStatus::Failed
    );
    let all = attempts(dir.path());
    assert_eq!(all.len(), 6, "max_retries: 5 means six attempts");
    assert!(
        all.iter()
            .all(|(s, fp, stalled)| s == "failed" && fp.is_some() && !stalled)
    );
    let out = node_output(dir.path(), "w");
    assert!(out.contains("success_check"), "{out}");
}

/// B2.3.7: a transient failure between two identical rejections neither
/// joins nor breaks the streak: the stall still lands on the third
/// identical rejection.
#[test]
fn a_transient_failure_between_identical_rejections_does_not_break_the_streak() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), ", stall_after: 3");
    let part =
        format!("if [ \"$N\" = 2 ]; then echo 'connection reset by peer' 1>&2; exit 1; fi; {OK}");
    let stub = recording_stub(dir.path(), &part);
    assert_eq!(
        run_with_stub(dir.path(), "st", &stub, None),
        RunStatus::Succeeded
    );
    // Attempt 1 rejected, attempt 2 transient (its one infrastructure retry
    // reports success and is rejected), attempt 3 rejected: the third
    // identical rejection stalls. The transient attempt is journaled
    // `failed` too, so count the rejections by their fingerprint.
    let rejected: Vec<_> = attempts(dir.path())
        .into_iter()
        .filter(|(_, fp, _)| fp.is_some())
        .collect();
    assert_eq!(rejected.len(), 3, "{rejected:?}");
    assert!(rejected.last().unwrap().2, "the last rejection stalls");
    assert!(!rejected[0].2 && !rejected[1].2);
}

/// B2.3.5: with a `retry_delay` the stall happens at the same attempt: the
/// deferred retry counts like any other.
#[test]
fn the_stall_survives_a_deferred_retry() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), ", stall_after: 3, retry_delay: { initial: 1s }");
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "st", &stub, None),
        RunStatus::Succeeded
    );
    let all = attempts(dir.path());
    assert_eq!(all.len(), 3, "{all:?}");
    assert!(all[2].2, "the third identical rejection stalls");
}

/// B2.3.6: a profile with a fallback step ends the node at the stall
/// without advancing the chain: no `fallback_triggered`.
#[test]
fn a_stall_ends_the_node_without_a_fallback_step() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    seed(dir.path(), ", stall_after: 3");
    // The node's profile carries a fallback step the stall must never reach.
    let pdir = dir.path().join(".apb/profiles/fb");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: fb\ndescription: d\nexecutor:\n  agent: claude\n  model: haiku\n  fallbacks:\n    - { agent: codex, model: haiku }\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "role").unwrap();
    let yaml = fs::read_to_string(dir.path().join(".apb/playbooks/st/1.0.0/playbook.yaml"))
        .unwrap()
        .replace("defaults: { profile: main }", "defaults: { profile: fb }");
    fs::write(
        dir.path().join(".apb/playbooks/st/1.0.0/playbook.yaml"),
        yaml,
    )
    .unwrap();
    // The fallback's executor needs its own CLI; the stub answers for both.
    let stub = recording_stub(dir.path(), OK);
    let _home = common::EnvVar::set("HOME", home.path());
    assert_eq!(
        run_with_stub(dir.path(), "st", &stub, None),
        RunStatus::Succeeded
    );
    let all = attempts(dir.path());
    assert_eq!(all.len(), 3, "{all:?}");
    assert!(all[2].2);
    let events = read_all(&run_dir(dir.path())).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::FallbackTriggered { .. })),
        "the stall must end the node before the chain advances"
    );
}

// --- B2.3.3 and B2.3.4: the evidence check's rejection note names the
// claims, so the stand-in agent varies the claim ids: two alternating texts
// never stall, and a timestamp plus counter in the text still stalls.

const MAIL: &str = r#"{"apb_records": 1, "complete": true, "source": "inbox"}
{"id": "m1", "kind": "mail", "text": "The invoice for March was paid on time by Acme."}
"#;

fn evidence_playbook(stall: &str) -> String {
    format!(
        r#"schema: 2
id: ev
name: EV
version: 1.0.0
defaults: {{ profile: main, max_retries: 5 }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: collect, type: script, script: scripts/collect.sh, runner: sh, results: [ {{ path: mail.jsonl, format: records }} ] }}
  - {{ id: read, type: agent_task, prompt: "Summarize the mail.", evidence: {{ from: [collect], on_invalid: fail }}{stall} }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: collect }}
  - {{ from: collect, to: read }}
  - {{ from: read, to: done }}
"#
    )
}

fn seed_evidence(root: &Path, stall: &str) {
    init_project(root).unwrap();
    seed_playbook(root, "ev", &evidence_playbook(stall));
    let vdir = root.join(".apb/playbooks/ev/1.0.0/scripts");
    fs::create_dir_all(&vdir).unwrap();
    common::write_sync(
        &vdir.join("collect.sh"),
        &format!("cat > mail.jsonl <<'APB_EOF'\n{MAIL}APB_EOF\necho ok\n"),
    );
    common::seed_main(root);
}

fn claims_stub(dir: &Path, id_expr: &str) -> String {
    // The claim cites a record outside the session, so the check rejects it;
    // the claim id is what varies. `id_expr` is shell expanded at run time;
    // the JSON is one double-quoted argument, so $ID expands.
    recording_stub(
        dir,
        &format!(
            "ID={id_expr}; printf '%s' \"{{\\\"status\\\":\\\"success\\\",\\\"outputs\\\":{{\\\"claims\\\":[{{\\\"id\\\":\\\"$ID\\\",\\\"text\\\":\\\"x\\\",\\\"cites\\\":[{{\\\"ref\\\":\\\"r:{}/output\\\"}}]}}]}}}}\" > \"$APB_STATUS_FILE\"; {OK}",
            "a".repeat(64)
        ),
    )
}

fn evidence_attempts(root: &Path) -> Vec<(String, bool)> {
    read_all(&run_dir(root))
        .unwrap()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node,
                stalled,
                status,
                ..
            } if node == "read" => Some((status.clone(), *stalled)),
            _ => None,
        })
        .collect()
}

/// B2.3.3: two alternating rejection texts are two fingerprints: no stall,
/// every retry runs.
#[test]
fn alternating_rejection_texts_never_stall() {
    let dir = tempfile::tempdir().unwrap();
    seed_evidence(dir.path(), ", stall_after: 2");
    let stub = claims_stub(
        dir.path(),
        "$(if [ $((N % 2)) = 1 ]; then echo alt-a; else echo alt-b; fi)",
    );
    assert_eq!(
        run_with_stub(dir.path(), "ev", &stub, None),
        RunStatus::Failed
    );
    let all = evidence_attempts(dir.path());
    assert_eq!(all.len(), 6, "{all:?}");
    assert!(all.iter().all(|(_, stalled)| !stalled));
    let out = node_output(dir.path(), "read");
    assert!(out.contains("evidence check rejected"), "{out}");
}

/// B2.3.4: a rejection text whose volatile part is a timestamp and a
/// counter still stalls: the normalization strips both.
#[test]
fn a_rejection_with_a_changing_timestamp_and_counter_stalls() {
    let dir = tempfile::tempdir().unwrap();
    seed_evidence(dir.path(), ", stall_after: 2");
    // Both the timestamp and the counter change with the attempt.
    let stub = claims_stub(dir.path(), "c-2026-11-0${N}T10:0${N}:00Z-$N");
    assert_eq!(
        run_with_stub(dir.path(), "ev", &stub, None),
        RunStatus::Failed
    );
    let all = evidence_attempts(dir.path());
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(all[1].1, "the second identical rejection stalls");
}
