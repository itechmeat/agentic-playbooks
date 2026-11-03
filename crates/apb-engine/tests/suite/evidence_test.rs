//! Citations for node outputs: an `agent_task` with `evidence:` reads the
//! stored snapshots of its sources through its attempt's session, and its
//! claims are checked against what that attempt read.
//!
//! A stub agent writes its session handle and prompt, then waits while the
//! test reads through the session the way an agent would (`apb evidence
//! read` and MCP `run_evidence_read` call the same function), then writes
//! the status file the test composed from what it read.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::evidence::{self, CitationFlag, ClaimVerdict, session};
use apb_engine::grade::reader::{ReadRequest, ReadResponse};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use serde_json::{Value, json};

use crate::common;

const MAIL: &str = r#"{"apb_records": 1, "complete": true, "source": "desk inbox"}
{"id": "m-1", "kind": "email", "text": "The customer reports that the invoice for October is late."}
{"id": "m-2", "kind": "email", "text": "A refund was promised on the phone."}
"#;
const TASKS: &str = r#"{"apb_records": 1, "complete": false, "source": "tracker, first page only"}
{"id": "t-1", "kind": "task", "text": "Call the customer back."}
"#;
const CALLS: &str = r#"{"apb_records": 1, "complete": true, "source": "phone log"}
{"id": "k-1", "kind": "call", "text": "Outgoing call, no answer."}
"#;

fn playbook(evidence: &str, retries: u32) -> String {
    format!(
        r#"schema: 2
id: ev
name: Evidence
version: 1.0.0
defaults:
  profile: main
nodes:
  - {{ id: start, type: start }}
  - id: collect
    type: script
    script: scripts/collect.sh
    runner: sh
    results:
      - {{ path: mail.jsonl, format: records }}
      - {{ path: tasks.jsonl, format: records }}
      - {{ path: calls.jsonl, format: records }}
      - {{ path: notes.md }}
      - {{ path: broken.jsonl, format: records }}
  - {{ id: use, type: agent_task, prompt: "answer from the sources", max_retries: {retries}, evidence: {evidence} }}
  - {{ id: ok, type: finish, outcome: success }}
  - {{ id: no, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: collect }}
  - {{ from: collect, to: use }}
  - {{ from: use, to: ok, condition: {{ type: node_status, node: use, equals: success }} }}
  - {{ from: use, to: no, fallback: true }}
"#
    )
}

/// A project with the playbook, the collector and the stub agent.
fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/ev/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/ev/current"), "1.0.0").unwrap();
    common::seed_main(root);
    let mut collect = String::new();
    for (name, body) in [
        ("mail.jsonl", MAIL),
        ("tasks.jsonl", TASKS),
        ("calls.jsonl", CALLS),
        ("notes.md", "plain notes about the October invoice\n"),
        ("broken.jsonl", "not a header\n"),
    ] {
        collect.push_str(&format!("cat > {name} <<'APB_EOF'\n{body}APB_EOF\n"));
    }
    collect.push_str("echo collected\n");
    common::write_sync(&vdir.join("scripts/collect.sh"), &collect);
    let r = root.display();
    let agent = root.join("agent.sh");
    common::write_sync(
        &agent,
        &format!(
            "#!/bin/sh\nn=\"$APB_ATTEMPT\"\nprintf '%s\\n' \"$*\" > \"{r}/prompt.$n\"\nprintf '%s' \"$APB_EVIDENCE_SESSION\" > \"{r}/session.$n.tmp\"\nmv \"{r}/session.$n.tmp\" \"{r}/session.$n\"\nwhile [ ! -f \"{r}/go.$n\" ]; do sleep 0.05; done\ncat \"{r}/status.$n\" > \"$APB_STATUS_FILE\"\necho done\n"
        ),
    );
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The attempt `n`'s session handle, once its agent started.
fn handle_of(root: &Path, n: u32) -> String {
    let at = Instant::now();
    let path = root.join(format!("session.{n}"));
    loop {
        if let Ok(h) = fs::read_to_string(&path) {
            return h;
        }
        assert!(
            at.elapsed() < Duration::from_secs(60),
            "attempt {n} never started"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One reading step through the session, as an agent makes it.
fn op(rd: &Path, h: &str, req: ReadRequest) -> ReadResponse {
    session::read(rd, h, &req, None).unwrap_or_else(|e| panic!("{req:?}: {e}"))
}

fn run_dir_of(root: &Path, h: &str) -> PathBuf {
    let (run_id, _) = session::parse_handle(h).unwrap();
    root.join(".apb/runs").join(run_id)
}

/// The references of `view` (a listing routes, it never reads).
fn refs(rd: &Path, h: &str, view: &str) -> Vec<String> {
    op(
        rd,
        h,
        ReadRequest::Children {
            view: view.into(),
            from: 0,
            count: None,
        },
    )
    .items
    .into_iter()
    .map(|i| i.r#ref)
    .collect()
}

/// The view ids of the session, from its outline.
fn views(rd: &Path, h: &str) -> Vec<String> {
    op(rd, h, ReadRequest::Outline)
        .items
        .into_iter()
        .map(|i| i.r#ref.trim_start_matches("n:").to_string())
        .collect()
}

fn view_ending(all: &[String], suffix: &str) -> String {
    all.iter()
        .find(|v| v.ends_with(suffix))
        .unwrap_or_else(|| panic!("no view ending {suffix} in {all:?}"))
        .clone()
}

/// Lets attempt `n` finish with `claims` in its status file.
fn finish(root: &Path, n: u32, claims: Value) {
    let status =
        json!({ "status": "success", "outputs": { "answer": "see claims", "claims": claims } });
    fs::write(root.join(format!("status.{n}")), status.to_string()).unwrap();
    fs::write(root.join(format!("go.{n}")), "").unwrap();
}

fn start(root: &Path) -> std::thread::JoinHandle<(RunStatus, String)> {
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let r = run(&root, "ev", None, RunOptions::default()).unwrap();
        (r.outcome, r.run_id)
    })
}

const EVIDENCE_ALL: &str = "{ from: [collect], kinds: [email, task, call, file, output] }";

#[test]
fn claims_rest_only_on_records_this_attempt_read_whole() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, &playbook(EVIDENCE_ALL, 0));
    let _agent = common::EnvVar::set("APB_AGENT_CMD", root.join("agent.sh").as_os_str());
    let driver = start(root);
    let h = handle_of(root, 1);
    let rd = run_dir_of(root, &h);
    let all = views(&rd, &h);
    let email = view_ending(&all, "mail.jsonl~email");
    let task = view_ending(&all, "tasks.jsonl~task");
    let call = view_ending(&all, "calls.jsonl~call");
    let notes = view_ending(&all, "notes.md");
    let mail_refs = refs(&rd, &h, &email);
    let (m1, m2) = (mail_refs[0].clone(), mail_refs[1].clone());
    assert!(m1.ends_with("@m-1"), "{m1}");
    // m-2 is seen only through a search hit: routing, never a read.
    let hits = op(
        &rd,
        &h,
        ReadRequest::Search {
            query: "refund".into(),
            view: None,
        },
    );
    assert_eq!(hits.items.len(), 1);
    assert!(!hits.items[0].citable);
    let read = op(
        &rd,
        &h,
        ReadRequest::Read {
            r#ref: m1.clone(),
            span: None,
        },
    );
    assert!(read.items[0].citable);
    assert!(read.items[0].text.contains("invoice for October"));
    op(
        &rd,
        &h,
        ReadRequest::Page {
            view: call.clone(),
            from: 0,
            count: None,
        },
    );
    // The notes file, read with `cat` past the reader: its reference is
    // spelled from the snapshot id, never listed or read here.
    let snapshot = m1.split('/').next().unwrap().trim_start_matches("r:");
    let notes_ref = format!("r:{snapshot}/file:workspace:notes.md");
    assert!(notes.ends_with("notes.md"));
    // A citation of a record of a snapshot outside the session.
    let foreign = format!("r:{}/output", "a".repeat(64));
    let at = |text: &str, needle: &str| -> [u64; 2] {
        let s = text.find(needle).unwrap();
        let a = text[..s].chars().count() as u64;
        [a, a + needle.chars().count() as u64]
    };
    let m1_text = &read.items[0].text;
    let c1_span = at(m1_text, "the invoice for October is late");
    finish(
        root,
        1,
        json!([
            { "id": "c1", "text": "The October invoice is late",
              "cites": [{ "ref": m1, "span": c1_span }] },
            { "id": "c2", "text": "The customer wrote", "cites": [{ "ref": m1, "span": [4, 12] }] },
            { "id": "c3", "text": "A refund was promised", "cites": [{ "ref": m2 }] },
            { "id": "c4", "text": "late", "cites": [{ "ref": m1, "span": [0, 4000] }] },
            { "id": "c5", "text": "elsewhere", "cites": [{ "ref": foreign }] },
            { "id": "c6", "text": "nothing is open", "absence": { "kinds": ["task"] } },
            { "id": "c7", "text": "nobody answered", "absence": { "kinds": ["call"] } },
            { "id": "c8", "text": "the notes say so", "cites": [{ "ref": notes_ref }] },
            { "id": "c9", "text": "zzz", "cites": [{ "ref": m1 }] }
        ]),
    );
    let (outcome, run_id) = driver.join().unwrap();
    // `on_invalid: mark` keeps the node's status.
    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(rd.file_name().unwrap().to_string_lossy(), run_id);
    let shown = evidence::view(&rd, Some("use")).unwrap();
    assert_eq!(shown.len(), 1);
    let s = &shown[0];
    assert_eq!(s.state, "checked");
    let claim = |id: &str| s.claims.iter().find(|c| c.id == id).unwrap().clone();
    let c1 = claim("c1");
    assert_eq!(c1.verdict, ClaimVerdict::StructurallyValid);
    assert_eq!(c1.wording, evidence::STRUCTURALLY_VALID);
    assert!(c1.flags.is_empty(), "{:?}", c1.flags);
    assert_eq!(c1.text, "The October invoice is late");
    assert_eq!(
        c1.cites[0].fragment.as_deref(),
        Some("the invoice for October is late")
    );
    // The accepted citation is kept in byte form.
    // The text is ASCII, so the byte range equals the character span.
    let [a, b] = c1_span;
    assert!(
        c1.cites[0]
            .stored
            .as_deref()
            .unwrap()
            .ends_with(&format!("@m-1#[{a},{b})"))
    );
    assert_eq!(claim("c2").flags, vec![CitationFlag::ShortSpan]);
    let c3 = claim("c3");
    assert_eq!(c3.verdict, ClaimVerdict::Unsupported);
    assert_eq!(c3.wording, evidence::UNSUPPORTED);
    assert!(
        c3.problems[0].contains("only seen in a listing or a search"),
        "{:?}",
        c3.problems
    );
    assert!(claim("c4").problems[0].contains("outside the record"));
    assert!(claim("c5").problems[0].contains("names no record"));
    let c6 = claim("c6");
    assert_eq!(c6.verdict, ClaimVerdict::NotEnoughData);
    assert!(
        c6.problems[0].starts_with("incomplete_view"),
        "{:?}",
        c6.problems
    );
    assert_eq!(claim("c7").verdict, ClaimVerdict::AbsenceOk);
    // The notes file was never read through the reader.
    assert!(claim("c8").problems[0].contains("was not read in full"));
    let c9 = claim("c9");
    assert_eq!(c9.verdict, ClaimVerdict::StructurallyValid);
    assert_eq!(
        c9.flags,
        vec![CitationFlag::WholeRecord, CitationFlag::NoSharedWords]
    );
    assert!(
        c9.cites[0]
            .fragment
            .as_deref()
            .unwrap()
            .starts_with("The customer")
    );
    // The collector's own statements, shown as such.
    let tasks = s
        .packages
        .iter()
        .find(|p| p.view == task)
        .expect("the tasks package");
    assert!(!tasks.complete);
    assert_eq!(tasks.collector, evidence::COLLECTOR_INCOMPLETE);
    // A records entry that is no package is stored and read as plain text,
    // with its first problem.
    assert!(view_ending(&all, "broken.jsonl").ends_with("/file:workspace:broken.jsonl"));
    let manifest: Value = serde_json::from_slice(
        &fs::read(rd.join(format!("results/snapshots/{snapshot}.json"))).unwrap(),
    )
    .unwrap();
    let broken = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == "broken.jsonl")
        .unwrap();
    assert_eq!(broken["format"], "records");
    assert_eq!(
        broken["records_invalid"],
        "line 1: header not a JSON object"
    );
    assert!(
        manifest["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["path"] != "broken.jsonl")
            .all(|e| e.get("records_invalid").is_none())
    );
    // The trace stores references and counts, never a text.
    let trace = s.trace.as_ref().unwrap();
    assert!(trace.ops.iter().any(|o| o.op == "search"));
    for entry in fs::read_dir(rd.join("evidence")).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for needle in [
            "invoice for October",
            "refund",
            "Outgoing call",
            "plain notes",
            "answer from the sources",
            "The October invoice is late",
        ] {
            assert!(
                !text.contains(needle),
                "{needle} stored in the session file"
            );
        }
    }
    // The session closed with its attempt: a later read is refused.
    let late = session::read(&rd, &h, &ReadRequest::Outline, None).unwrap_err();
    assert!(late.to_string().contains("is closed"), "{late}");
    // Another run's step is refused.
    let other = session::read(&rd, &h, &ReadRequest::Outline, Some("other-1")).unwrap_err();
    assert!(other.to_string().contains("run_step_other_run"), "{other}");
    // The prompt carried the session and the reading instructions.
    let prompt = fs::read_to_string(root.join("prompt.1")).unwrap();
    assert!(
        prompt.contains("## evidence") && prompt.contains(&h),
        "{prompt}"
    );
    // The run guard names the feature.
    let events = read_all(&rd).unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::RunGuard { requires } if requires.iter().any(|r| r == "evidence")
    )));
}

#[test]
fn on_invalid_fail_reruns_the_attempt_with_the_problems_in_its_prompt() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook("{ from: [collect], claims: required, on_invalid: fail }", 2),
    );
    let _agent = common::EnvVar::set("APB_AGENT_CMD", root.join("agent.sh").as_os_str());
    let driver = start(root);
    // Attempt 1 reads a record whole and reports no claim at all.
    let h1 = handle_of(root, 1);
    let rd = run_dir_of(root, &h1);
    let all = views(&rd, &h1);
    let mail = view_ending(&all, "mail.jsonl");
    let m1 = refs(&rd, &h1, &mail)[0].clone();
    op(
        &rd,
        &h1,
        ReadRequest::Read {
            r#ref: m1.clone(),
            span: None,
        },
    );
    finish(root, 1, json!([]));
    // Attempt 2 cites the record attempt 1 read: another attempt's read
    // does not count.
    let h2 = handle_of(root, 2);
    assert_ne!(h1, h2, "one session per attempt");
    let prompt2 = fs::read_to_string(root.join("prompt.2")).unwrap();
    assert!(
        prompt2.contains("The previous attempt's claims were refused")
            && prompt2.contains("claims are required"),
        "{prompt2}"
    );
    finish(
        root,
        2,
        json!([{ "id": "c1", "text": "late invoice", "cites": [{ "ref": m1 }] }]),
    );
    // Attempt 3 reads it itself and the claim stands.
    let h3 = handle_of(root, 3);
    let prompt3 = fs::read_to_string(root.join("prompt.3")).unwrap();
    assert!(prompt3.contains("was not read in full"), "{prompt3}");
    op(
        &rd,
        &h3,
        ReadRequest::Read {
            r#ref: m1.clone(),
            span: None,
        },
    );
    finish(
        root,
        3,
        json!([{ "id": "c1", "text": "late invoice", "cites": [{ "ref": m1 }] }]),
    );
    let (outcome, _) = driver.join().unwrap();
    assert_eq!(outcome, RunStatus::Succeeded);
    let events = read_all(&rd).unwrap();
    let kinds: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node, failure_kind, ..
            } if node == "use" => Some(failure_kind.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            Some(evidence::CHECK_REJECTED.to_string()),
            Some(evidence::CHECK_REJECTED.to_string()),
            None
        ]
    );
    let shown = evidence::view(&rd, None).unwrap();
    assert_eq!(shown.len(), 3);
    assert!(shown[0].problems[0].contains("claims are required"));
    assert_eq!(shown[1].claims[0].verdict, ClaimVerdict::Unsupported);
    assert_eq!(shown[2].claims[0].verdict, ClaimVerdict::StructurallyValid);
}

#[test]
fn a_record_whose_secret_became_known_after_capture_is_served_redacted_and_not_citable() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, &playbook("{ from: [collect] }", 0));
    let _agent = common::EnvVar::set("APB_AGENT_CMD", root.join("agent.sh").as_os_str());
    let driver = start(root);
    let h = handle_of(root, 1);
    let rd = run_dir_of(root, &h);
    // The phrase becomes a secret value of the run after capture.
    let secret = "promised on the phone";
    fs::write(
        rd.join("run.yaml"),
        format!("params:\n  token: \"{secret}\"\n"),
    )
    .unwrap();
    let pb = fs::read_to_string(rd.join("playbook.yaml")).unwrap();
    fs::write(
        rd.join("playbook.yaml"),
        format!("{pb}params:\n  - {{ name: token, type: text, secret: true }}\n"),
    )
    .unwrap();
    let all = views(&rd, &h);
    let mail = view_ending(&all, "mail.jsonl");
    let m2 = refs(&rd, &h, &mail)[1].clone();
    let read = op(
        &rd,
        &h,
        ReadRequest::Read {
            r#ref: m2.clone(),
            span: None,
        },
    );
    assert!(
        !read.items[0].text.contains(secret),
        "{}",
        read.items[0].text
    );
    assert!(!read.items[0].citable);
    finish(
        root,
        1,
        json!([{ "id": "c1", "text": "a refund", "cites": [{ "ref": m2 }] }]),
    );
    let _ = driver.join().unwrap();
    let shown = evidence::view(&rd, None).unwrap();
    let c1 = &shown[0].claims[0];
    assert_eq!(c1.verdict, ClaimVerdict::Unsupported);
    assert!(
        c1.problems[0].contains("served redacted"),
        "{:?}",
        c1.problems
    );
    assert!(!serde_json::to_string(&shown).unwrap().contains(secret));
}
