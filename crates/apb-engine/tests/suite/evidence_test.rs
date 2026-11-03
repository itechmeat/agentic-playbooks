//! Evidence sessions and the claim check (`apb_engine::evidence`).
//!
//! The sources are real: a run of script nodes captures a records package,
//! a plain note and a package its collector calls incomplete. Sessions are
//! opened over that run as the engine opens them for an attempt, read
//! through the shared reader, and the claims a status file holds are
//! checked. The last tests drive whole runs with a stand-in agent.

use std::fs;
use std::path::{Path, PathBuf};

use apb_core::registry::init_project;
use apb_core::schema::{ClaimsRequirement, Evidence, OnInvalidClaims};
use apb_core::step_env::StepCaller;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::evidence::check::at_success;
use apb_engine::evidence::session::{self, Opening};
use apb_engine::evidence::{Flag, Verdict};
use apb_engine::grade::reader::{ReadRequest, ReadResponse};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use serde_json::{Value, json};

use crate::common;
use crate::token_economy_test::{invocations, recording_stub, run_with_stub, seed_playbook};

const MAIL: &str = r#"{"apb_records": 1, "complete": true, "source": "inbox"}
{"id": "m1", "kind": "mail", "text": "The invoice for March was paid on time by Acme."}
{"id": "m:2", "kind": "mail", "text": "Acme asked to move the meeting to Friday afternoon."}
{"id": "t1", "kind": "ticket", "text": "Printer code PRN77aaSecretX is jammed again on floor two."}
"#;
const PARTIAL: &str = r#"{"apb_records": 1, "complete": false, "source": "first page of the inbox"}
{"id": "p1", "kind": "mail", "text": "Only the first page was read."}
"#;
const NOTE: &str = "Plain note about the quarterly budget review.";

/// `collect` keeps a package and a note, `partial` a package its collector
/// calls incomplete, `broken` a file declared a package that is not one.
fn sources_playbook() -> String {
    r#"schema: 2
id: src
name: src
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: collect, type: script, script: scripts/collect.sh, runner: sh, results: [ { path: mail.jsonl, format: records }, { path: note.txt } ] }
  - { id: partial, type: script, script: scripts/partial.sh, runner: sh, results: [ { path: partial.jsonl, format: records } ] }
  - { id: broken, type: script, script: scripts/broken.sh, runner: sh, results: [ { path: broken.jsonl, format: records } ] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: collect }
  - { from: collect, to: partial }
  - { from: partial, to: broken }
  - { from: broken, to: done }
"#
    .to_string()
}

fn write_scripts(vdir: &Path) {
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    let put = |name: &str, file: &str, body: &str| {
        common::write_sync(
            &vdir.join("scripts").join(name),
            &format!("cat > {file} <<'APB_EOF'\n{body}APB_EOF\necho ok\n"),
        );
    };
    common::write_sync(
        &vdir.join("scripts/collect.sh"),
        &format!(
            "cat > mail.jsonl <<'APB_EOF'\n{MAIL}APB_EOF\nprintf '%s' '{NOTE}' > note.txt\necho ok\n"
        ),
    );
    put("partial.sh", "partial.jsonl", PARTIAL);
    put("broken.sh", "broken.jsonl", "not a package\n");
}

/// A finished run of the sources playbook; its directory.
fn sources_run(root: &Path) -> PathBuf {
    init_project(root).unwrap();
    seed_playbook(root, "src", &sources_playbook());
    write_scripts(&root.join(".apb/playbooks/src/1.0.0"));
    let r = run(root, "src", None, RunOptions::default()).unwrap();
    assert_eq!(r.outcome, RunStatus::Succeeded);
    root.join(".apb/runs").join(&r.run_id)
}

fn spec(from: &[&str], kinds: Option<&[&str]>) -> Evidence {
    Evidence {
        from: from.iter().map(|s| s.to_string()).collect(),
        kinds: kinds.map(|k| k.iter().map(|s| s.to_string()).collect()),
        claims: ClaimsRequirement::Optional,
        on_invalid: OnInvalidClaims::Mark,
        budget_chars: None,
    }
}

/// Opens a session for attempt `n` of `node` over `rd`.
fn open(rd: &Path, node: &str, n: u32, spec: &Evidence) -> String {
    let run_id = rd.file_name().unwrap().to_str().unwrap().to_string();
    session::open(
        rd,
        &Opening {
            run_id: &run_id,
            node,
            item: None,
            attempt_number: n,
            stem: format!("{node}-{n}"),
            spec,
        },
    )
    .unwrap()
}

fn read(rd: &Path, s: &str, req: Value) -> ReadResponse {
    let req: ReadRequest = serde_json::from_value(req).unwrap();
    session::read(rd, s, &req, &StepCaller::default()).unwrap()
}

/// The reference of the record whose text holds `needle`, from a page of
/// every view (which reads them whole).
fn refs(rd: &Path, s: &str) -> Vec<(String, String)> {
    let file = session::load(rd, s).unwrap();
    let mut out = Vec::new();
    for v in &file.views {
        let r = read(rd, s, json!({"op": "page", "view": v.id}));
        out.extend(r.items.into_iter().map(|i| (i.r#ref, i.text)));
    }
    out
}

fn ref_of(all: &[(String, String)], needle: &str) -> String {
    all.iter()
        .find(|(_, t)| t.contains(needle))
        .unwrap_or_else(|| panic!("{needle}: {all:?}"))
        .0
        .clone()
}

/// Writes `claims` as the status file of attempt `n` of `node`.
fn claims(rd: &Path, node: &str, n: u32, claims: Value) {
    fs::create_dir_all(rd.join("agent-status")).unwrap();
    fs::write(
        rd.join(format!("agent-status/{node}-{n}.json")),
        json!({"status": "success", "outputs": {"claims": claims}}).to_string(),
    )
    .unwrap();
}

fn verdicts(rd: &Path, s: &str) -> Vec<(String, Verdict, Vec<String>, Vec<Flag>)> {
    session::load(rd, s)
        .unwrap()
        .claims
        .unwrap()
        .claims
        .into_iter()
        .map(|c| (c.id, c.verdict, c.problems, c.flags))
        .collect()
}

fn one<'a>(
    v: &'a [(String, Verdict, Vec<String>, Vec<Flag>)],
    id: &str,
) -> &'a (String, Verdict, Vec<String>, Vec<Flag>) {
    v.iter().find(|c| c.0 == id).unwrap()
}

/// The routine of the design: a claim citing a record read whole is
/// structurally valid; one only found by search, one never read (its record
/// is only seen in a listing), a span past the record and a reference
/// outside the session are unsupported, each with its problem.
#[test]
fn claims_are_checked_against_what_the_session_returned_whole() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let s = open(&rd, "read", 1, &spec(&["collect"], None));
    let outline = read(&rd, &s, json!({"op": "outline"}));
    assert!(outline.items.iter().all(|i| !i.citable));
    // `m1` is read whole; `t1` is only found by a search.
    let file = session::load(&rd, &s).unwrap();
    let package = file.views.iter().find(|v| v.kind == "records").unwrap();
    assert_eq!(package.collector_complete, Some(true));
    let m1 = package.records[0].r#ref.clone();
    let m2 = package.records[1].r#ref.clone();
    let t1 = package.records[2].r#ref.clone();
    assert!(m2.ends_with("@m%3A2"), "{m2}");
    let whole = read(&rd, &s, json!({"op": "read", "ref": m1}));
    assert!(whole.items[0].citable);
    assert_eq!(
        whole.items[0].text,
        "The invoice for March was paid on time by Acme."
    );
    let found = read(&rd, &s, json!({"op": "search", "query": "printer"}));
    assert_eq!(found.items[0].r#ref, t1);
    assert!(!found.items[0].citable);
    let outside = format!("r:{}/output", "a".repeat(64));
    claims(
        &rd,
        "read",
        1,
        json!([
            {"id": "c1", "text": "The March invoice was paid on time.", "cites": [{"ref": m1, "span": [4, 30]}]},
            {"id": "c2", "text": "The invoice was settled.", "cites": [{"ref": m1, "span": [4, 11]}]},
            {"id": "c3", "text": "The printer is jammed.", "cites": [{"ref": t1}]},
            {"id": "c4", "text": "The meeting moved to Friday.", "cites": [{"ref": m2}]},
            {"id": "c5", "text": "The invoice was paid.", "cites": [{"ref": m1, "span": [10, 400]}]},
            {"id": "c6", "text": "Something happened.", "cites": [{"ref": outside}]},
        ]),
    );
    assert_eq!(
        at_success(&rd, &s, &spec(&["collect"], None)).unwrap(),
        None
    );
    let v = verdicts(&rd, &s);
    assert_eq!(one(&v, "c1").1, Verdict::StructurallyValid);
    assert!(one(&v, "c1").3.is_empty(), "{v:?}");
    assert_eq!(one(&v, "c2").1, Verdict::StructurallyValid);
    assert_eq!(one(&v, "c2").3, vec![Flag::ShortSpan]);
    assert_eq!(one(&v, "c3").1, Verdict::Unsupported);
    assert!(
        one(&v, "c3").2[0].contains("only seen in a listing or a search"),
        "{v:?}"
    );
    assert_eq!(one(&v, "c4").1, Verdict::Unsupported);
    assert!(one(&v, "c4").2[0].contains("was not read in full"), "{v:?}");
    assert!(one(&v, "c5").2[0].contains("outside the record"), "{v:?}");
    assert!(
        one(&v, "c6").2[0].contains("names no record of this session"),
        "{v:?}"
    );
    // The accepted citation is kept in the byte form of the stored text.
    let stored = session::load(&rd, &s).unwrap().claims.unwrap();
    let c1 = stored.claims.iter().find(|c| c.id == "c1").unwrap();
    assert_eq!(c1.cites, vec![format!("{m1}#[4,30)")]);
    // Surfaces show the fragment next to the claim and the fixed wording.
    let rows = apb_engine::evidence::rows(&rd);
    let shown = rows[0].claims.iter().find(|c| c.id == "c1").unwrap();
    assert_eq!(shown.cites[0].fragment, "invoice for March was paid");
    assert_eq!(shown.text, "The March invoice was paid on time.");
    assert!(
        shown
            .wording
            .starts_with("Structurally valid: the cited record exists")
    );
    assert!(
        shown
            .wording
            .contains("Whether it supports the claim was not checked.")
    );
    let refused = rows[0].claims.iter().find(|c| c.id == "c3").unwrap();
    assert_eq!(
        refused.wording,
        "Not supported by a cited record (not shown as a fact)."
    );
}

/// An absence rests on every view of its kinds: over a package its
/// collector calls incomplete it is `not_enough_data`; over a complete
/// package read to its end, with no record of the kind, `absence_ok`; not
/// read to its end, unsupported.
#[test]
fn an_absence_needs_complete_views_read_to_their_end() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let kinds: &[&str] = &["mail", "complaint"];
    let absence =
        json!([{"id": "a1", "text": "No complaint arrived.", "absence": {"kinds": ["complaint"]}}]);

    let s = open(&rd, "read", 1, &spec(&["partial"], Some(kinds)));
    refs(&rd, &s);
    claims(&rd, "read", 1, absence.clone());
    at_success(&rd, &s, &spec(&["partial"], Some(kinds))).unwrap();
    let v = verdicts(&rd, &s);
    assert_eq!(v[0].1, Verdict::NotEnoughData, "{v:?}");
    assert!(v[0].2[0].contains("incomplete_view"));

    let s = open(&rd, "read", 2, &spec(&["collect"], Some(kinds)));
    claims(&rd, "read", 2, absence.clone());
    at_success(&rd, &s, &spec(&["collect"], Some(kinds))).unwrap();
    assert_eq!(verdicts(&rd, &s)[0].1, Verdict::AbsenceOk);
    // A `kinds` list keeps the session to its kinds: no note, no ticket.
    let file = session::load(&rd, &s).unwrap();
    let kinds_seen: Vec<&str> = file.views.iter().map(|v| v.kind.as_str()).collect();
    assert_eq!(kinds_seen, ["mail", "complaint"]);

    let s = open(&rd, "read", 3, &spec(&["collect"], None));
    claims(
        &rd,
        "read",
        3,
        json!([{"id": "a2", "text": "No record at all.", "absence": {"kinds": ["records"]}}]),
    );
    at_success(&rd, &s, &spec(&["collect"], None)).unwrap();
    let v = verdicts(&rd, &s);
    assert_eq!(v[0].1, Verdict::Unsupported, "{v:?}");
    assert!(v[0].2[0].contains("read to its end"));
}

/// Every flag of a weak but valid citation, on one fixture.
#[test]
fn a_weak_but_valid_citation_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    // The consumer reads its own earlier execution: `collect` itself.
    let s = open(&rd, "collect", 2, &spec(&["collect"], None));
    let all = refs(&rd, &s);
    let m1 = ref_of(&all, "invoice for March");
    let note = ref_of(&all, "quarterly budget");
    let mut list = vec![
        json!({"id": "whole", "text": "The quarterly budget review note.", "cites": [{"ref": note}]}),
        json!({"id": "short", "text": "Acme paid the invoice.", "cites": [{"ref": m1, "span": [4, 11]}]}),
        json!({"id": "words", "text": "Weather tomorrow looks sunny.", "cites": [{"ref": m1, "span": [0, 30]}]}),
    ];
    for i in 0..5 {
        list.push(json!({"id": format!("s{i}"), "text": "The invoice for March was paid.", "cites": [{"ref": m1, "span": [4, 30]}]}));
    }
    claims(&rd, "collect", 2, Value::Array(list));
    at_success(&rd, &s, &spec(&["collect"], None)).unwrap();
    let v = verdicts(&rd, &s);
    assert!(v.iter().all(|c| c.1 == Verdict::StructurallyValid), "{v:?}");
    assert!(one(&v, "whole").3.contains(&Flag::WholeRecord));
    assert!(one(&v, "short").3.contains(&Flag::ShortSpan));
    assert!(one(&v, "words").3.contains(&Flag::NoSharedWords));
    assert!(!one(&v, "s0").3.contains(&Flag::NoSharedWords));
    for c in &v {
        assert!(c.3.contains(&Flag::SelfCitation), "{c:?}");
    }
    // Seven claims cite `m1`: more than five.
    assert!(one(&v, "s0").3.contains(&Flag::SharedRecord));
    assert!(!one(&v, "whole").3.contains(&Flag::SharedRecord));
}

/// A secret known only after capture: the record holding it is served
/// redacted and is not citable, and a claim citing it is unsupported.
#[test]
fn a_record_holding_a_late_known_secret_is_served_redacted_and_not_citable() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let secret = "PRN77aaSecretX";
    fs::write(
        rd.join("run.yaml"),
        format!("params:\n  token: \"{secret}\"\n"),
    )
    .unwrap();
    let pb = fs::read_to_string(rd.join("playbook.yaml")).unwrap();
    fs::write(
        rd.join("playbook.yaml"),
        pb.replace(
            "nodes:\n",
            "params:\n  - { name: token, type: text, secret: true }\nnodes:\n",
        ),
    )
    .unwrap();
    let s = open(&rd, "read", 1, &spec(&["collect"], None));
    let all = refs(&rd, &s);
    let t1 = ref_of(&all, "is jammed again");
    let served = read(&rd, &s, json!({"op": "read", "ref": t1}));
    assert!(!served.items[0].text.contains(secret), "{served:?}");
    assert!(!served.items[0].citable);
    let file = session::load(&rd, &s).unwrap();
    let rec = file
        .views
        .iter()
        .flat_map(|v| &v.records)
        .find(|r| r.r#ref == t1)
        .unwrap();
    assert!(!rec.citable);
    claims(
        &rd,
        "read",
        1,
        json!([{"id": "c1", "text": "The printer is jammed again.", "cites": [{"ref": t1}]}]),
    );
    at_success(&rd, &s, &spec(&["collect"], None)).unwrap();
    let v = verdicts(&rd, &s);
    assert_eq!(v[0].1, Verdict::Unsupported);
    assert!(v[0].2[0].contains("served redacted"), "{v:?}");
}

/// The session file keeps references and counts only: no record text, no
/// claim text and no search phrase, byte for byte.
#[test]
fn the_read_trace_stores_no_text() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let s = open(&rd, "read", 1, &spec(&["collect"], None));
    let all = refs(&rd, &s);
    // A phrase that is in no source text: the trace names where it
    // searched, never the phrase.
    read(&rd, &s, json!({"op": "search", "query": "zzqsearchword"}));
    let m1 = ref_of(&all, "invoice for March");
    claims(
        &rd,
        "read",
        1,
        json!([
            {"id": "c1", "text": "Unique claim wording zebra.", "cites": [{"ref": m1, "span": [4, 30]}]},
            {"id": "c2", "text": "No complaint arrived.", "absence": {"kinds": ["complaint", "Secretneedle"]}}
        ]),
    );
    at_success(&rd, &s, &spec(&["collect"], None)).unwrap();
    session::close(&rd, &s).unwrap();
    let bytes = fs::read(rd.join(format!("evidence/{s}.json"))).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    for needle in [
        "invoice for March",
        "Friday afternoon",
        "quarterly",
        "budget review",
        "zebra",
        "zzqsearchword",
        "Secretneedle",
    ] {
        assert!(!text.contains(needle), "{needle} in {text}");
    }
    let file = session::load(&rd, &s).unwrap();
    let trace = file.trace.unwrap();
    assert!(trace.read_chars > 0);
    assert!(trace.views.iter().all(|v| v.ended), "{:?}", trace.views);
}

/// A read after the attempt ended is refused, and so is a read from a step
/// of another run.
#[test]
fn a_closed_session_and_another_runs_step_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let s = open(&rd, "read", 1, &spec(&["collect"], None));
    let outline: ReadRequest = serde_json::from_value(json!({"op": "outline"})).unwrap();
    let other = StepCaller {
        run_id: Some("another-run".into()),
        host_task: None,
    };
    let e = session::read(&rd, &s, &outline, &other).unwrap_err();
    assert!(e.to_string().contains("belongs to run"), "{e}");
    let e = session::find(dir.path(), &s, &other).unwrap_err();
    assert!(e.to_string().contains("belongs to another run"), "{e}");
    assert_eq!(
        session::find(dir.path(), &s, &StepCaller::default()).unwrap(),
        rd
    );
    session::close(&rd, &s).unwrap();
    let e = session::read(&rd, &s, &outline, &StepCaller::default()).unwrap_err();
    assert!(e.to_string().contains("is closed"), "{e}");
}

/// A file declared a package that does not parse is stored as plain text
/// with its first problem, and a session reads it as one plain record.
#[test]
fn an_invalid_package_is_kept_as_plain_text_with_its_problem() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let view = apb_engine::result_view::ResultsView::load(&rd).unwrap();
    let row = view
        .rows
        .iter()
        .find(|r| r.execution.node == "broken")
        .unwrap();
    let m: apb_core::result_store::SnapshotManifest = serde_json::from_slice(
        &fs::read(rd.join(format!("results/snapshots/{}.json", row.snapshot_id))).unwrap(),
    )
    .unwrap();
    let file = m
        .entries
        .iter()
        .find(|e| e.path.as_deref() == Some("broken.jsonl"))
        .unwrap();
    assert_eq!(file.format, apb_core::result_store::ResultFormat::Text);
    assert!(
        file.records_invalid
            .as_deref()
            .is_some_and(|p| p.contains("line 1 is not the header")),
        "{file:?}"
    );
    let s = open(&rd, "read", 1, &spec(&["broken"], None));
    let kinds: Vec<String> = session::load(&rd, &s)
        .unwrap()
        .views
        .into_iter()
        .map(|v| v.kind)
        .collect();
    assert!(kinds.contains(&"file".to_string()), "{kinds:?}");
}

// --- duplicate ids, failed sources, caps and session lifetimes -----------

/// Two claims with the same id (or ids equal in their first 64 characters)
/// each get their own verdict: the repeated id is a problem, and a surface
/// never shows one claim's text under another claim's verdict.
#[test]
fn duplicate_claim_ids_are_a_problem_and_rows_keep_the_claims_apart() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let s = open(&rd, "read", 1, &spec(&["collect"], None));
    let all = refs(&rd, &s);
    let m1 = ref_of(&all, "invoice for March");
    let long_id = "k".repeat(70);
    claims(
        &rd,
        "read",
        1,
        json!([
            {"id": "c1", "text": "The March invoice was paid on time.", "cites": [{"ref": m1, "span": [4, 30]}]},
            {"id": "c1", "text": "Something else entirely happened here.", "cites": []},
            {"id": long_id, "text": "The first long id.", "cites": [{"ref": m1, "span": [4, 30]}]},
            {"id": format!("{long_id}xyz"), "text": "The second long id.", "cites": [{"ref": m1, "span": [4, 30]}]}
        ]),
    );
    at_success(&rd, &s, &spec(&["collect"], None)).unwrap();
    let v = verdicts(&rd, &s);
    assert_eq!(one(&v, "c1").1, Verdict::StructurallyValid);
    let dupes: Vec<_> = v.iter().filter(|c| c.0 == "c1").collect();
    assert_eq!(dupes.len(), 2, "{v:?}");
    assert_eq!(dupes[1].1, Verdict::Unsupported);
    assert!(dupes[1].2[0].contains("duplicate"), "{v:?}");
    let longs: Vec<_> = v.iter().filter(|c| c.0.starts_with("kkkk")).collect();
    assert_eq!(
        longs.len(),
        2,
        "ids clipped to 64 characters collide: {v:?}"
    );
    assert_eq!(longs[1].1, Verdict::Unsupported);
    // The rows show each verdict with its own text, never another claim's.
    let rows = apb_engine::evidence::rows(&rd);
    let shown: Vec<_> = rows[0].claims.iter().filter(|c| c.id == "c1").collect();
    assert_eq!(shown[0].text, "The March invoice was paid on time.");
    assert_eq!(shown[1].text, "Something else entirely happened here.");
}

/// A `from` source whose latest settlement failed (its run continued over a
/// fallback edge) declares a records package the snapshot does not hold: an
/// absence over a kind that only that package could carry is
/// `not_enough_data`, never `absence_ok`.
#[test]
fn an_absence_over_a_failed_source_is_not_enough_data() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    let yaml = r#"schema: 2
id: fl
name: fl
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: inbox_a, type: script, script: scripts/a.sh, runner: sh, results: [ { path: a.jsonl, format: records } ] }
  - { id: inbox_b, type: script, script: scripts/b.sh, runner: sh, results: [ { path: b.jsonl, format: records } ] }
  - { id: read, type: script, script: scripts/read.sh, runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: inbox_a }
  - { from: start, to: inbox_b }
  - { from: inbox_a, to: read, fallback: true }
  - { from: inbox_b, to: read }
  - { from: read, to: done }
"#
    .to_string();
    seed_playbook(root, "fl", &yaml);
    let vdir = root.join(".apb/playbooks/fl/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    let put = |name: &str, body: &str| {
        common::write_sync(&vdir.join("scripts").join(name), body);
    };
    put("a.sh", "exit 3\n");
    put(
        "b.sh",
        "cat > b.jsonl <<'APB_EOF'\n{\"apb_records\": 1, \"complete\": true, \"source\": \"inbox b\"}\n{\"id\": \"x1\", \"kind\": \"complaint\", \"text\": \"The printer is late again.\"}\nAPB_EOF\necho ok\n",
    );
    put("read.sh", "echo done\n");
    let r = run(root, "fl", None, RunOptions::default()).unwrap();
    let rd = root.join(".apb/runs").join(&r.run_id);
    let outcomes: Vec<String> = read_all(&rd)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RunFinished { outcome } => Some(outcome.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        r.outcome,
        RunStatus::Succeeded,
        "the fallback edge carries the run: {outcomes:?}"
    );
    let kinds: &[&str] = &["complaint"];
    let s = open(&rd, "read", 1, &spec(&["inbox_a", "inbox_b"], Some(kinds)));
    refs(&rd, &s);
    claims(
        &rd,
        "read",
        1,
        json!([{"id": "a1", "text": "No complaint arrived.", "absence": {"kinds": ["complaint"]}}]),
    );
    at_success(&rd, &s, &spec(&["inbox_a", "inbox_b"], Some(kinds))).unwrap();
    let v = verdicts(&rd, &s);
    assert_eq!(v[0].1, Verdict::NotEnoughData, "{v:?}");
    assert!(
        v[0].2.iter().any(|p| p.contains("incomplete_view")),
        "{v:?}"
    );
}

/// `absence.kinds` accepts only the record kind grammar, capped: anything
/// else is a claim problem, and the raw text is never stored.
#[test]
fn absence_kinds_outside_the_grammar_are_a_problem() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let s = open(&rd, "read", 1, &spec(&["collect"], None));
    refs(&rd, &s);
    claims(
        &rd,
        "read",
        1,
        json!([
            {"id": "a1", "text": "No complaint.", "absence": {"kinds": ["complaint", "Not A Slug"]}},
            {"id": "a2", "text": "Still none.", "absence": {"kinds": [["nested"]]}},
            {"id": "a3", "text": "Many kinds.", "absence": {"kinds": (0..20).map(|i| format!("k{i}")).collect::<Vec<_>>()}}
        ]),
    );
    at_success(&rd, &s, &spec(&["collect"], None)).unwrap();
    let v = verdicts(&rd, &s);
    assert_eq!(one(&v, "a1").1, Verdict::Unsupported);
    assert!(one(&v, "a1").2[0].contains("not a slug"), "{v:?}");
    assert_eq!(one(&v, "a2").1, Verdict::Unsupported);
    assert_eq!(one(&v, "a3").1, Verdict::Unsupported);
    assert!(one(&v, "a3").2[0].contains("absence kinds"), "{v:?}");
}

/// One node declaring two records files: a `complaint` absence is proven
/// only when every declared records file the node has produced a package
/// view. When the second declaration is a glob that matched nothing (the
/// snapshot is whole: a glob matching nothing is no omission), the kinds
/// only that package could carry get an empty incomplete view, so the
/// absence is `not_enough_data`, never `absence_ok` proven from the first
/// package alone. When both packages were captured and neither holds a
/// complaint, `absence_ok`.
#[test]
fn an_absence_over_one_of_two_declared_packages_is_not_enough_data() {
    let kinds: &[&str] = &["mail", "complaint"];
    let absence =
        json!([{"id": "a1", "text": "No complaint arrived.", "absence": {"kinds": ["complaint"]}}]);
    let yaml = |id: &str, second: &str| {
        format!(
            r#"schema: 2
id: {id}
name: {id}
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - {{ id: pull, type: script, script: scripts/pull.sh, runner: sh, results: [ {{ path: mail.jsonl, format: records }}, {{ path: '{second}', format: records }} ] }}
  - {{ id: read, type: script, script: scripts/read.sh, runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: pull }}
  - {{ from: pull, to: read }}
  - {{ from: read, to: done }}
"#
        )
    };
    let pull = |tickets: bool| {
        let mut s = String::from(
            "cat > mail.jsonl <<'APB_EOF'\n{\"apb_records\": 1, \"complete\": true, \"source\": \"inbox\"}\n{\"id\": \"m1\", \"kind\": \"mail\", \"text\": \"The March invoice was paid on time.\"}\nAPB_EOF\n",
        );
        if tickets {
            s.push_str("cat > tickets.jsonl <<'APB_EOF'\n{\"apb_records\": 1, \"complete\": true, \"source\": \"helpdesk\"}\nAPB_EOF\n");
        }
        s.push_str("echo ok\n");
        s
    };
    // Both packages captured: neither holds a complaint.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    seed_playbook(root, "both", &yaml("both", "tickets.jsonl"));
    let vdir = root.join(".apb/playbooks/both/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    common::write_sync(&vdir.join("scripts/pull.sh"), &pull(true));
    common::write_sync(&vdir.join("scripts/read.sh"), "echo done\n");
    let r = run(root, "both", None, RunOptions::default()).unwrap();
    assert_eq!(r.outcome, RunStatus::Succeeded);
    let rd = root.join(".apb/runs").join(&r.run_id);
    let s = open(&rd, "read", 1, &spec(&["pull"], Some(kinds)));
    refs(&rd, &s);
    claims(&rd, "read", 1, absence.clone());
    at_success(&rd, &s, &spec(&["pull"], Some(kinds))).unwrap();
    assert_eq!(verdicts(&rd, &s)[0].1, Verdict::AbsenceOk);

    // The second declaration is a glob that matched nothing: the snapshot
    // is whole and holds one package, so a complaint absence cannot be
    // proven from the mail package alone.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    seed_playbook(root, "one", &yaml("one", "tickets-*.jsonl"));
    let vdir = root.join(".apb/playbooks/one/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    common::write_sync(&vdir.join("scripts/pull.sh"), &pull(false));
    common::write_sync(&vdir.join("scripts/read.sh"), "echo done\n");
    let r = run(root, "one", None, RunOptions::default()).unwrap();
    assert_eq!(r.outcome, RunStatus::Succeeded);
    let rd = root.join(".apb/runs").join(&r.run_id);
    let s = open(&rd, "read", 1, &spec(&["pull"], Some(kinds)));
    refs(&rd, &s);
    claims(&rd, "read", 1, absence.clone());
    at_success(&rd, &s, &spec(&["pull"], Some(kinds))).unwrap();
    let v = verdicts(&rd, &s);
    assert_eq!(v[0].1, Verdict::NotEnoughData, "{v:?}");
    assert!(
        v[0].2.iter().any(|p| p.contains("incomplete_view")),
        "{v:?}"
    );
    let file = session::load(&rd, &s).unwrap();
    assert!(
        file.views
            .iter()
            .any(|v| !v.complete && v.kind == "complaint"),
        "{:?}",
        file.views
            .iter()
            .map(|v| (&v.id, v.kind.clone(), v.complete))
            .collect::<Vec<_>>()
    );
}

/// The stored verdict keeps only kinds that are valid slugs, capped and
/// scrubbed: a claim whose kinds fail the grammar stores no kinds at all,
/// so raw agent text never lands in the session file.
#[test]
fn invalid_absence_kinds_are_not_stored() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let kinds: &[&str] = &["mail", "complaint"];
    let s = open(&rd, "read", 1, &spec(&["collect"], Some(kinds)));
    refs(&rd, &s);
    claims(
        &rd,
        "read",
        1,
        json!([
            {"id": "ok", "text": "No complaint.", "absence": {"kinds": ["complaint"]}},
            {"id": "bad", "text": "None.", "absence": {"kinds": ["complaint", "Not A Slug"]}},
            {"id": "many", "text": "None.", "absence": {"kinds": (0..20).map(|i| format!("k{i}")).collect::<Vec<_>>()}}
        ]),
    );
    at_success(&rd, &s, &spec(&["collect"], Some(kinds))).unwrap();
    let file = session::load(&rd, &s).unwrap();
    let outcome = file.claims.unwrap();
    let stored: Vec<(String, Option<&Vec<String>>)> = outcome
        .claims
        .iter()
        .map(|c| (c.id.clone(), c.absence.as_ref()))
        .collect();
    assert_eq!(
        stored.iter().find(|(id, _)| id == "ok").unwrap().1,
        Some(&vec!["complaint".to_string()])
    );
    assert_eq!(stored.iter().find(|(id, _)| id == "bad").unwrap().1, None);
    assert_eq!(stored.iter().find(|(id, _)| id == "many").unwrap().1, None);
    let bytes = fs::read(rd.join(format!("evidence/{s}.json"))).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("Not A Slug"), "{text}");
    assert!(!text.contains("\"k0\""), "{text}");
}

/// The check accepts a bounded number of cites per claim, and the read
/// trace keeps a bounded number of ops, so a looping reader cannot grow a
/// session file without end.
#[test]
fn cites_are_capped_and_the_trace_keeps_bounded_ops() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let s = open(&rd, "read", 1, &spec(&["collect"], None));
    let all = refs(&rd, &s);
    let m1 = ref_of(&all, "invoice for March");
    let cites: Vec<Value> = (0..40)
        .map(|i| json!({"ref": m1, "span": [4, 20 + i]}))
        .collect();
    claims(
        &rd,
        "read",
        1,
        json!([{"id": "c1", "text": "The invoice was paid on time by Acme.", "cites": cites}]),
    );
    at_success(&rd, &s, &spec(&["collect"], None)).unwrap();
    let v = verdicts(&rd, &s);
    assert_eq!(one(&v, "c1").1, Verdict::Unsupported);
    assert!(one(&v, "c1").2[0].contains("cites"), "{v:?}");
    // A reader that repeats an op that returns nothing cannot grow the
    // trace without end: the ops list is capped, oldest dropped.
    let s2 = open(&rd, "read", 2, &spec(&["collect"], None));
    for _ in 0..522 {
        read(&rd, &s2, json!({"op": "outline"}));
    }
    let file = session::load(&rd, &s2).unwrap();
    assert_eq!(file.state.ops.len(), 512);
    assert_eq!(file.state.ops[0].op, "outline");
}

/// A session never bound to an attempt is refused as soon as a newer
/// session exists for the same attempt — even one opened in the same
/// millisecond: `opened_at` alone cannot order them, so any other
/// unbound-or-not session of the same attempt at the same time supersedes.
#[test]
fn an_unbound_session_opened_the_same_millisecond_as_a_newer_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let stale = open(&rd, "read", 1, &spec(&["collect"], None));
    let fresh = open(&rd, "read", 1, &spec(&["collect"], None));
    // Pin the two opens to the same millisecond: without the fix the tie
    // is not an ordering and the orphaned session stays readable.
    let fresh_at = session::load(&rd, &fresh).unwrap().opened_at;
    let mut file = session::load(&rd, &stale).unwrap();
    file.opened_at = fresh_at;
    let at = rd.join(format!("evidence/{stale}.json"));
    let json = serde_json::to_vec_pretty(&file).unwrap();
    fs::write(&at, json).unwrap();
    let req: ReadRequest = serde_json::from_value(json!({"op": "outline"})).unwrap();
    let err = session::read(&rd, &stale, &req, &StepCaller::default()).unwrap_err();
    assert!(
        err.to_string().contains("never bound to an attempt"),
        "{err}"
    );
    // The newer one, bound, still reads.
    session::bind_attempt(&rd, &fresh, 7).unwrap();
    let _ = session::read(&rd, &fresh, &req, &StepCaller::default()).unwrap();
}

/// A session that was bound but whose attempt is written off in the
/// journal refuses reads even though it was never closed (crash safety).
#[test]
fn an_open_session_of_a_written_off_attempt_refuses_reads() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let s = open(&rd, "read", 1, &spec(&["collect"], None));
    let started = {
        use apb_engine::event::EventLog;
        let mut log = EventLog::open(&rd).unwrap();
        let started = log
            .append(EventPayload::AttemptStarted {
                item: None,
                node: "read".into(),
                attempt: 1,
                agent: "test-agent".into(),
                soul_delivery: None,
                skills_mode: None,
                pid: None,
                spawn_ms: None,
                model: None,
                workdir: None,
                transcript: None,
                token: None,
                isolation: None,
                exec: None,
            })
            .unwrap()
            .seq;
        session::bind_attempt(&rd, &s, started).unwrap();
        log.append(EventPayload::AttemptFinished {
            item: None,
            output_truncated: Vec::new(),
            transcript_incomplete: false,
            node: "read".into(),
            attempt: 1,
            status: "failed".into(),
            duration_ms: None,
            session: None,
            summary: None,
            rejected_output: None,
            partial_output: None,
            failure_kind: None,
            usage: None,
        })
        .unwrap();
        started
    };
    let file = session::load(&rd, &s).unwrap();
    assert_eq!(file.attempt, Some(started));
    let req: ReadRequest = serde_json::from_value(json!({"op": "outline"})).unwrap();
    let e = session::read(&rd, &s, &req, &StepCaller::default()).unwrap_err();
    assert!(e.to_string().contains("closed"), "{e}");
}

/// A record read whole in one attempt's session is not citable from a
/// later attempt's session: the refs name the same records, the read state
/// does not carry over.
#[test]
fn a_citation_does_not_carry_across_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let s1 = open(&rd, "read", 1, &spec(&["collect"], None));
    let all = refs(&rd, &s1);
    let m1 = ref_of(&all, "invoice for March");
    session::close(&rd, &s1).unwrap();
    let s2 = open(&rd, "read", 2, &spec(&["collect"], None));
    claims(
        &rd,
        "read",
        2,
        json!([{"id": "c1", "text": "The March invoice was paid.", "cites": [{"ref": m1, "span": [4, 30]}]}]),
    );
    at_success(&rd, &s2, &spec(&["collect"], None)).unwrap();
    let v = verdicts(&rd, &s2);
    assert_eq!(v[0].1, Verdict::Unsupported, "{v:?}");
    assert!(v[0].2[0].contains("was not read in full"), "{v:?}");
}

/// A session that was never bound to an attempt is refused once a newer
/// session exists for the same attempt: its attempt never started.
#[test]
fn an_unbound_session_is_refused_once_a_newer_session_exists() {
    let dir = tempfile::tempdir().unwrap();
    let rd = sources_run(dir.path());
    let s1 = open(&rd, "read", 1, &spec(&["collect"], None));
    let s2 = open(&rd, "read", 1, &spec(&["collect"], None));
    let req: ReadRequest = serde_json::from_value(json!({"op": "outline"})).unwrap();
    let e = session::read(&rd, &s1, &req, &StepCaller::default()).unwrap_err();
    assert!(e.to_string().contains("never bound"), "{e}");
    // The newer session itself still serves.
    read(&rd, &s2, json!({"op": "outline"}));
}

// --- whole runs with a stand-in agent ------------------------------------

const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";

fn reading_playbook(on_invalid: &str) -> String {
    format!(
        r#"schema: 2
id: rd
name: rd
version: 1.0.0
defaults: {{ profile: main, max_retries: 1 }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: collect, type: script, script: scripts/collect.sh, runner: sh, results: [ {{ path: mail.jsonl, format: records }} ] }}
  - {{ id: read, type: agent_task, prompt: "Summarize the mail.", evidence: {{ from: [collect], on_invalid: {on_invalid} }} }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: collect }}
  - {{ from: collect, to: read }}
  - {{ from: read, to: done }}
"#
    )
}

/// The stub's `read` attempts: the first cites a record outside the
/// session, the later ones write no claims. Each records its session.
fn reading_stub(root: &Path) -> String {
    let outside = format!("r:{}/output", "a".repeat(64));
    recording_stub(
        root,
        &format!(
            "if [ \"$NODE\" = read ]; then echo \"$APB_EVIDENCE_SESSION\" >> \"$LOG/sessions\"; \
             if [ \"$N\" = 1 ]; then printf '%s' '{{\"status\":\"success\",\"outputs\":{{\"claims\":[{{\"id\":\"c1\",\"text\":\"x\",\"cites\":[{{\"ref\":\"{outside}\"}}]}}]}}}}' > \"$APB_STATUS_FILE\"; \
             else printf '%s' '{{\"status\":\"success\",\"outputs\":{{}}}}' > \"$APB_STATUS_FILE\"; fi; fi; {OK}"
        ),
    )
}

fn seed_reading(root: &Path, on_invalid: &str) {
    seed_reading_max(root, on_invalid, 1);
}

fn seed_reading_max(root: &Path, on_invalid: &str, max_retries: u32) {
    init_project(root).unwrap();
    seed_playbook(
        root,
        "rd",
        &reading_playbook(on_invalid)
            .replace("max_retries: 1", &format!("max_retries: {max_retries}")),
    );
    let vdir = root.join(".apb/playbooks/rd/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    common::write_sync(
        &vdir.join("scripts/collect.sh"),
        &format!("cat > mail.jsonl <<'APB_EOF'\n{MAIL}APB_EOF\necho ok\n"),
    );
    common::seed_main(root);
}

fn only_run(root: &Path) -> PathBuf {
    fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
        .unwrap()
}

/// `on_invalid: fail` fails the attempt as `check_rejected` and the retry
/// is told the problems; each attempt reads through its own session, named
/// in its prompt and its environment, and each session is closed.
#[test]
fn on_invalid_fail_reruns_the_attempt_with_the_problems_in_its_prompt() {
    let dir = tempfile::tempdir().unwrap();
    seed_reading(dir.path(), "fail");
    let stub = reading_stub(dir.path());
    assert_eq!(
        run_with_stub(dir.path(), "rd", &stub, None),
        RunStatus::Succeeded
    );
    let rd = only_run(dir.path());
    let sessions: Vec<String> = fs::read_to_string(dir.path().join("inv/sessions"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(sessions.len(), 2);
    assert_ne!(sessions[0], sessions[1]);
    let inv = invocations(dir.path());
    let prompts: Vec<&str> = inv
        .iter()
        .filter(|i| i.node == "read")
        .map(|i| i.prompt())
        .collect();
    assert!(prompts[0].contains("## evidence"), "{}", prompts[0]);
    assert!(prompts[0].contains(&sessions[0]));
    assert!(!prompts[0].contains("claims were refused"));
    assert!(prompts[1].contains(&sessions[1]));
    assert!(prompts[1].contains("claims were refused"), "{}", prompts[1]);
    assert!(prompts[1].contains("names no record of this session"));
    let events = read_all(&rd).unwrap();
    let kinds: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node, failure_kind, ..
            } if node == "read" => Some(failure_kind.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec![Some("check_rejected".to_string()), None]);
    for s in &sessions {
        let f = session::load(&rd, s).unwrap();
        assert!(f.closed_at.is_some());
        assert!(f.attempt.is_some(), "bound to its attempt_started");
    }
    // The run guard names the feature.
    assert!(events.iter().any(|e| matches!(&e.payload,
        EventPayload::RunGuard { requires, .. } if requires.iter().any(|r| r == "evidence"))));
}

/// The refusal note names only the attempt it belongs to: a later attempt
/// that fails some other way clears it, so the next prompt does not carry a
/// stale "claims were refused" section.
#[test]
fn the_refusal_note_is_cleared_when_a_later_attempt_fails_differently() {
    let dir = tempfile::tempdir().unwrap();
    seed_reading_max(dir.path(), "fail", 2);
    let outside = format!("r:{}/output", "a".repeat(64));
    let stub = recording_stub(
        dir.path(),
        &format!(
            "if [ \"$NODE\" = read ]; then \
             if [ \"$N\" = 1 ]; then printf '%s' '{{\"status\":\"success\",\"outputs\":{{\"claims\":[{{\"id\":\"c1\",\"text\":\"x\",\"cites\":[{{\"ref\":\"{outside}\"}}]}}]}}}}' > \"$APB_STATUS_FILE\"; \
             elif [ \"$N\" = 2 ]; then echo the agent failed >&2; exit 1; \
             else printf '%s' '{{\"status\":\"success\",\"outputs\":{{}}}}' > \"$APB_STATUS_FILE\"; fi; fi; {OK}"
        ),
    );
    assert_eq!(
        run_with_stub(dir.path(), "rd", &stub, None),
        RunStatus::Succeeded
    );
    let inv = invocations(dir.path());
    let prompts: Vec<&str> = inv
        .iter()
        .filter(|i| i.node == "read")
        .map(|i| i.prompt())
        .collect();
    assert_eq!(prompts.len(), 3, "{}", inv.len());
    assert!(prompts[1].contains("claims were refused"), "{}", prompts[1]);
    assert!(
        !prompts[2].contains("claims were refused"),
        "{}",
        prompts[2]
    );
}

/// `on_invalid: mark` keeps the node's status and marks the claims.
#[test]
fn on_invalid_mark_keeps_the_status_and_marks_the_claims() {
    let dir = tempfile::tempdir().unwrap();
    seed_reading(dir.path(), "mark");
    let stub = reading_stub(dir.path());
    assert_eq!(
        run_with_stub(dir.path(), "rd", &stub, None),
        RunStatus::Succeeded
    );
    let rd = only_run(dir.path());
    let rows = apb_engine::evidence::rows(&rd);
    assert_eq!(rows.len(), 1, "one attempt");
    assert!(rows[0].closed && rows[0].checked);
    assert_eq!(rows[0].claims[0].verdict, Verdict::Unsupported);
}
