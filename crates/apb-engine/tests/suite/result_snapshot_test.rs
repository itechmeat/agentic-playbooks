//! Result snapshot capture at settlement: what is stored, what is omitted,
//! the limits, the secret scrub and the journal stamps.
//!
//! Every test drives a real run of script nodes (no agent, no model) and
//! reads the stored manifests and objects back from the run directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use apb_core::registry::init_project;
use apb_core::result_store::{
    EntryPart, IncompleteReason, OmissionReason, OutputOmitted, ResultScope, ResultStamp,
    SnapshotManifest, SnapshotOmission,
};
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

fn seed(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    init_project(root).unwrap();
    let vdir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
    for (name, body) in scripts {
        crate::common::write_sync(&vdir.join("scripts").join(name), body);
    }
}

fn run_dir(root: &Path, run_id: &str) -> PathBuf {
    root.join(".apb/runs").join(run_id)
}

/// Runs `id` to its end, at most `secs` seconds (a capture that blocks on
/// a FIFO fails the test instead of hanging it).
fn run_within(root: &Path, id: &str, opts: RunOptions, secs: u64) -> (RunStatus, PathBuf) {
    let (tx, rx) = std::sync::mpsc::channel();
    let (root_owned, id_owned) = (root.to_path_buf(), id.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(run(&root_owned, &id_owned, None, opts).map(|r| (r.outcome, r.run_id)));
    });
    let (outcome, run_id) = rx
        .recv_timeout(Duration::from_secs(secs))
        .expect("the run ended in time")
        .unwrap();
    (outcome, run_dir(root, &run_id))
}

fn run_ok(root: &Path, id: &str, opts: RunOptions) -> PathBuf {
    let (outcome, rd) = run_within(root, id, opts, 120);
    assert_eq!(outcome, RunStatus::Succeeded);
    rd
}

/// The stamp on each `node_finished` of `node`, in journal order.
fn node_stamps(events: &[Event], node: &str) -> Vec<Option<ResultStamp>> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n, result, ..
            } if n == node => Some(result.clone()),
            _ => None,
        })
        .collect()
}

fn manifest(rd: &Path, id: &str) -> SnapshotManifest {
    let bytes = fs::read(rd.join(format!("results/snapshots/{id}.json"))).unwrap();
    // The id is the digest of the stored bytes.
    assert_eq!(apb_core::result_store::snapshot_id(&bytes), id);
    serde_json::from_slice(&bytes).unwrap()
}

/// The stored text of the object `digest` names.
fn object(rd: &Path, digest: &str) -> String {
    let hex = digest.strip_prefix("sha256:").unwrap();
    fs::read_to_string(rd.join(format!("results/objects/{}/{hex}", &hex[..2]))).unwrap()
}

fn only_stamp(events: &[Event], node: &str) -> ResultStamp {
    let stamps = node_stamps(events, node);
    assert_eq!(stamps.len(), 1, "{stamps:?}");
    stamps[0].clone().expect("a stamp")
}

fn file_text(rd: &Path, m: &SnapshotManifest, path: &str) -> Option<String> {
    m.entries
        .iter()
        .find(|e| e.part == EntryPart::File && e.path.as_deref() == Some(path))
        .map(|e| object(rd, &e.digest))
}

fn omission(m: &SnapshotManifest, path: &str) -> Option<OmissionReason> {
    m.omitted.iter().find_map(|o| match o {
        SnapshotOmission::Item(i) if i.path.as_deref() == Some(path) => Some(i.reason),
        _ => None,
    })
}

fn one_node(id: &str, node_extra: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: scripts/work.sh, runner: sh{node_extra} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    )
}

/// Three executions of a looping node that rewrites one file keep three
/// snapshots, each with the bytes of its own execution, readable after the
/// file is gone.
#[test]
fn a_loop_keeps_one_snapshot_per_execution() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: loop
name: loop
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: scripts/work.sh, runner: sh, results: [ { path: report.md } ] }
  - { id: again, type: script, script: scripts/again.sh, runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: again, condition: { type: output_field, node: work, field: more, equals: "yes" }, max_traversals: 5 }
  - { from: again, to: work }
  - { from: work, to: done, fallback: true }
"#;
    let work = "n=$(cat .count 2>/dev/null || echo 0); n=$((n+1)); echo $n > .count\n\
                printf 'execution %s\\n' \"$n\" > report.md\n\
                if [ \"$n\" -lt 3 ]; then echo '{\"more\":\"yes\"}'; else echo '{\"more\":\"no\"}'; fi\n";
    seed(
        root,
        "loop",
        yaml,
        &[("work.sh", work), ("again.sh", "echo ok\n")],
    );
    let rd = run_ok(root, "loop", RunOptions::default());
    let events = read_all(&rd).unwrap();
    let stamps: Vec<ResultStamp> = node_stamps(&events, "work")
        .into_iter()
        .map(|s| s.expect("every execution is stamped"))
        .collect();
    assert_eq!(stamps.len(), 3);
    fs::remove_file(root.join("report.md")).unwrap();
    for (i, stamp) in stamps.iter().enumerate() {
        let m = manifest(&rd, &stamp.snapshot_id);
        assert_eq!(m.subject.node, "work");
        assert_eq!(m.subject.execution, i as u32 + 1);
        assert_eq!(
            file_text(&rd, &m, "report.md").as_deref(),
            Some(format!("execution {}\n", i + 1).as_str())
        );
        assert_eq!(stamp.entries as usize, m.entries.len());
    }
    let ids: std::collections::BTreeSet<_> = stamps.iter().map(|s| &s.snapshot_id).collect();
    assert_eq!(ids.len(), 3, "three distinct snapshots");
    // Every transaction ended: nothing is left in staging.
    let staging = rd.join("results/staging");
    let left: Vec<_> = fs::read_dir(&staging)
        .map(|d| d.filter_map(Result::ok).map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

/// A run with no declaration writes no `results/` and no stamp.
#[test]
fn a_run_without_a_declaration_writes_nothing_new() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "plain",
        &one_node("plain", ""),
        &[("work.sh", "echo hi > report.md\necho ok\n")],
    );
    let rd = run_ok(root, "plain", RunOptions::default());
    assert!(!rd.join("results").exists());
    let journal = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    for key in ["\"result\"", "\"attempts\""] {
        assert!(!journal.contains(key), "{key} in the journal");
    }
}

/// Output, fields and files are stored as text; what cannot be stored
/// safely is listed with its reason, and nothing blocks on a FIFO.
#[test]
fn files_are_stored_as_text_and_unsafe_ones_omitted_unread() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let decls = [
        "report.md",
        "unchanged.md",
        "data.bin",
        "nul.txt",
        "missing.md",
        ".env.local",
        "link.md",
        "fifo.md",
        "hard.md",
        "out/*.txt",
        "out/*.none",
    ]
    .iter()
    .map(|p| format!("{{ path: \"{p}\" }}"))
    .collect::<Vec<_>>()
    .join(", ");
    let yaml = one_node(
        "files",
        &format!(", outputs: {{ fields: [verdict] }}, results: [ {decls} ]"),
    );
    let work = "printf 'the report\\n' > report.md\n\
                printf '\\377\\376' > data.bin\n\
                printf 'a\\000b' > nul.txt\n\
                mkfifo .env.local fifo.md\n\
                ln -s report.md link.md\n\
                printf 'elsewhere\\n' > other.txt; ln other.txt hard.md\n\
                mkdir -p out; printf 'one\\n' > out/a.txt; printf 'two\\n' > out/b.txt\n\
                echo '{\"verdict\":\"good\",\"extra\":\"x\"}'\n";
    fs::write(root.join("unchanged.md"), "as it was\n").unwrap();
    seed(root, "files", &yaml, &[("work.sh", work)]);
    let (outcome, rd) = run_within(root, "files", RunOptions::default(), 60);
    assert_eq!(outcome, RunStatus::Succeeded);
    let events = read_all(&rd).unwrap();
    let stamp = only_stamp(&events, "work");
    let m = manifest(&rd, &stamp.snapshot_id);
    let output = m
        .entries
        .iter()
        .find(|e| e.part == EntryPart::Output)
        .unwrap();
    assert!(object(&rd, &output.digest).contains("\"verdict\":\"good\""));
    let fields = m
        .entries
        .iter()
        .find(|e| e.part == EntryPart::Fields)
        .unwrap();
    assert_eq!(object(&rd, &fields.digest), r#"{"verdict":"good"}"#);
    assert_eq!(
        file_text(&rd, &m, "report.md").as_deref(),
        Some("the report\n")
    );
    assert_eq!(
        file_text(&rd, &m, "unchanged.md").as_deref(),
        Some("as it was\n")
    );
    assert_eq!(file_text(&rd, &m, "out/a.txt").as_deref(), Some("one\n"));
    assert_eq!(file_text(&rd, &m, "out/b.txt").as_deref(), Some("two\n"));
    let reasons = [
        ("data.bin", OmissionReason::BinaryNotSupported),
        ("nul.txt", OmissionReason::BinaryNotSupported),
        ("missing.md", OmissionReason::NotFound),
        (".env.local", OmissionReason::SecretShaped),
        ("link.md", OmissionReason::Link),
        ("fifo.md", OmissionReason::NotRegularFile),
        ("hard.md", OmissionReason::HardLink),
    ];
    for (path, reason) in reasons {
        assert_eq!(omission(&m, path), Some(reason), "{path}");
    }
    assert_eq!(stamp.omitted as usize, reasons.len());
    assert_eq!(stamp.entries, 6);
    assert!(!stamp.incomplete);
    assert!(
        m.entries
            .iter()
            .all(|e| e.scope.is_none() || e.scope == Some(ResultScope::Workspace))
    );
    // The attempt-free script settled with the tree's provenance.
    assert!(!m.provenance.cache_hit);
}

fn long_secret() -> String {
    "quietharbor".repeat(12)
}

/// A declared secret param value and a token-shaped value are scrubbed
/// before storage, in the output, in the declared fields (a secret in a
/// number too), in file contents and in paths, and the original bytes are
/// nowhere under `results/`. A token found only in a file is nowhere in the
/// journal either.
#[test]
fn secrets_are_scrubbed_in_contents_and_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let secret = long_secret();
    let pin = "48291736";
    let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
    let yaml = one_node(
        "sec",
        ", outputs: { fields: [note, pin] }, results: [ { path: report.md }, { path: \"named/*.txt\", scope: run } ]",
    )
    .replace(
        "nodes:",
        "params:\n  - { name: token, type: text, secret: true }\n  - { name: pin, type: text, secret: true }\nnodes:",
    );
    let work = format!(
        "printf 'key {secret} and {token} end\\n' > report.md\n\
         mkdir -p \"$APB_RUN_DIR/named\"; printf 'plain\\n' > \"$APB_RUN_DIR/named/{secret}.txt\"\n\
         printf '{{\"note\":\"from {secret}\",\"pin\":{pin},\"ok\":true}}\\n'\n"
    );
    seed(root, "sec", &yaml, &[("work.sh", &work)]);
    let opts = RunOptions {
        params: [
            ("token".to_string(), secret.clone()),
            ("pin".to_string(), pin.to_string()),
        ]
        .into(),
        ..RunOptions::default()
    };
    let rd = run_ok(root, "sec", opts);
    let events = read_all(&rd).unwrap();
    let m = manifest(&rd, &only_stamp(&events, "work").snapshot_id);
    let report = m
        .entries
        .iter()
        .find(|e| e.path.as_deref() == Some("report.md"))
        .unwrap();
    assert!(report.redacted);
    let text = object(&rd, &report.digest);
    assert!(
        text.starts_with("key ") && text.ends_with(" end\n"),
        "{text}"
    );
    assert!(!text.contains(&secret) && !text.contains(&token), "{text}");
    let named = m
        .entries
        .iter()
        .find(|e| e.scope == Some(ResultScope::Run))
        .unwrap();
    assert_eq!(named.path.as_deref(), Some("<redacted path 1>"));
    assert!(!named.redacted, "the contents held no secret");
    for part in [EntryPart::Output, EntryPart::Fields] {
        let e = m.entries.iter().find(|e| e.part == part).unwrap();
        assert!(e.redacted, "{part:?}");
        let text = object(&rd, &e.digest);
        assert!(
            !text.contains(&secret) && !text.contains(&token) && !text.contains(pin),
            "{part:?}: {text}"
        );
    }
    // A byte search of the store and the journal.
    let mut stack = vec![rd.join("results")];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let bytes = fs::read(&p).unwrap();
                let s = String::from_utf8_lossy(&bytes);
                assert!(
                    !s.contains(&secret) && !s.contains(&token) && !s.contains(pin),
                    "{}",
                    p.display()
                );
            }
        }
    }
    let journal = fs::read_to_string(rd.join("events.jsonl")).unwrap();
    // The token was only in a file: nothing of a capture reaches the
    // journal. (The journal keeps the node's raw output as it always did;
    // that is not the store.)
    assert!(!journal.contains(&token), "token in journal");
}

/// A glob over 10000 files examines at most 4096 entries, stores at most
/// 64 matches, lists 64 omissions plus one aggregate, keeps the manifest
/// under its limit, and the node succeeds.
#[test]
fn a_glob_over_ten_thousand_files_stops_at_its_limits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = one_node("many", ", results: [ { path: \"many/*.txt\" } ]");
    let work = "mkdir -p many; cd many; i=0; while [ $i -lt 10000 ]; do : > f$i.txt; i=$((i+1)); done; echo ok\n";
    seed(root, "many", &yaml, &[("work.sh", work)]);
    let rd = run_ok(root, "many", RunOptions::default());
    let events = read_all(&rd).unwrap();
    let stamp = only_stamp(&events, "work");
    let bytes = fs::read(rd.join(format!("results/snapshots/{}.json", stamp.snapshot_id))).unwrap();
    assert!(bytes.len() < apb_core::result_store::MAX_MANIFEST_BYTES);
    let m = manifest(&rd, &stamp.snapshot_id);
    assert_eq!(m.incomplete, Some(IncompleteReason::EnumerationLimit));
    let files = m
        .entries
        .iter()
        .filter(|e| e.part == EntryPart::File)
        .count();
    assert_eq!(files, apb_core::result_store::MAX_MATCHES);
    assert_eq!(
        m.omitted.len(),
        apb_core::result_store::MAX_OMISSION_RECORDS + 1
    );
    let SnapshotOmission::More(more) = m.omitted.last().unwrap() else {
        panic!("the aggregate is last");
    };
    let examined = apb_core::result_store::MAX_ENTRIES_EXAMINED as u64;
    let listed =
        (apb_core::result_store::MAX_MATCHES + apb_core::result_store::MAX_OMISSION_RECORDS) as u64;
    assert_eq!(more.more, examined - listed);
    assert_eq!(
        more.reasons.get(&OmissionReason::MatchLimit),
        Some(&more.more)
    );
    assert_eq!(
        stamp.omitted as u64,
        examined - apb_core::result_store::MAX_MATCHES as u64
    );
    assert!(stamp.incomplete);
}

/// A failed settlement keeps its output but no files.
#[test]
fn a_failed_node_keeps_its_output_but_not_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = one_node("fail", ", results: [ { path: report.md } ]").replace(
        "  - { from: work, to: done }\n",
        "  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }\n  - { from: work, to: lost, condition: { type: node_status, node: work, equals: failure } }\n",
    ).replace(
        "  - { id: done, type: finish, outcome: success }\n",
        "  - { id: done, type: finish, outcome: success }\n  - { id: lost, type: finish, outcome: failure }\n",
    );
    seed(
        root,
        "fail",
        &yaml,
        &[("work.sh", "echo report > report.md\necho broke\nexit 1\n")],
    );
    let (_, rd) = run_within(root, "fail", RunOptions::default(), 120);
    let events = read_all(&rd).unwrap();
    let m = manifest(&rd, &only_stamp(&events, "work").snapshot_id);
    let parts: Vec<EntryPart> = m.entries.iter().map(|e| e.part).collect();
    assert_eq!(parts, [EntryPart::Output]);
    assert!(object(&rd, &m.entries[0].digest).contains("broke"));
}

/// An output larger than one snapshot may hold is not stored, and the
/// snapshot says so as the snapshot's limit, not the run's: the run still
/// has room for the declared file.
#[test]
fn an_output_over_the_snapshot_limit_is_named_as_such() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "big",
        &one_node("big", ", results: [ { path: report.md } ]"),
        &[(
            "work.sh",
            "echo report > report.md
head -c 17000000 /dev/zero | tr '\\0' a\n",
        )],
    );
    let rd = run_ok(root, "big", RunOptions::default());
    let events = read_all(&rd).unwrap();
    let m = manifest(&rd, &only_stamp(&events, "work").snapshot_id);
    assert_eq!(m.output_omitted, Some(OutputOmitted::SnapshotLimit));
    assert_eq!(file_text(&rd, &m, "report.md").as_deref(), Some("report\n"));
}

/// The legacy `outputs.files` walk of the run directory never captures
/// apb's own storage, and capture never captures it either.
#[test]
fn neither_mechanism_captures_the_others_storage() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: walk
name: walk
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: first, type: script, script: scripts/first.sh, runner: sh, results: [] }
  - { id: second, type: script, script: scripts/second.sh, runner: sh, outputs: { files: ["**/*"] }, results: [ { path: "*", scope: run } ] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: first }
  - { from: first, to: second }
  - { from: second, to: done }
"#;
    seed(
        root,
        "walk",
        yaml,
        &[
            ("first.sh", "echo first\n"),
            (
                "second.sh",
                "echo note > \"$APB_RUN_DIR/note.txt\"\necho second\n",
            ),
        ],
    );
    let rd = run_ok(root, "walk", RunOptions::default());
    let events = read_all(&rd).unwrap();
    let artifacts: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node, artifacts, ..
            } if node == "second" => Some(artifacts.iter().map(|a| a.path.clone())),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(artifacts.iter().any(|p| p == "note.txt"), "{artifacts:?}");
    for internal in apb_core::result_store::INTERNAL_RUN_NAMES {
        assert!(
            !artifacts
                .iter()
                .any(|p| p.split('/').next() == Some(internal)),
            "{internal} in {artifacts:?}"
        );
    }
    let m = manifest(&rd, &only_stamp(&events, "second").snapshot_id);
    let paths: Vec<&str> = m.entries.iter().filter_map(|e| e.path.as_deref()).collect();
    assert!(paths.contains(&"note.txt"), "{paths:?}");
    for internal in apb_core::result_store::INTERNAL_RUN_NAMES {
        assert!(!paths.contains(internal), "{internal} in {paths:?}");
    }
}

/// A capture that cannot write fails alone: the node settles without a
/// stamp and a `result_capture_failed` diagnostic says why.
#[test]
fn a_failed_capture_never_fails_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = one_node("broken", ", results: []");
    // `results` is a plain file: the store cannot be created.
    seed(
        root,
        "broken",
        &yaml,
        &[("work.sh", "echo x > \"$APB_RUN_DIR/results\"\necho ok\n")],
    );
    let rd = run_ok(root, "broken", RunOptions::default());
    let events = read_all(&rd).unwrap();
    assert_eq!(node_stamps(&events, "work"), [None]);
    let failures = apb_engine::diagnostics::capture_failures(&rd);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].node, "work");
    assert_eq!(failures[0].execution, 1);
    assert!(failures[0].reason.starts_with("snapshot not stored: "));
    // The read surfaces' view says it next to the rows.
    let view = apb_engine::result_view::ResultsView::load(&rd).unwrap();
    assert!(view.rows.is_empty());
    assert_eq!(view.not_stored.len(), 1, "{view:?}");
    assert_eq!(view.not_stored[0].node, "work");
    assert_eq!(view.not_stored[0].execution, 1);
    assert_eq!(view.not_stored[0].reason, failures[0].reason);
}

/// A list item's snapshot keeps the item's full output while its
/// settlement keeps the clipped fields.
#[test]
fn a_list_item_snapshot_keeps_the_full_output() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: lst
name: lst
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: work
    type: script
    script: scripts/item.sh
    runner: sh
    results: []
    list:
      items: { inline: [ { id: a } ] }
      batch: { size: 1, retries: 0 }
      item_outputs: [body]
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;
    let body = "y".repeat(3000);
    let script = format!("printf '{{\"body\":\"{body}\"}}\\n'\n");
    seed(root, "lst", yaml, &[("item.sh", &script)]);
    let rd = run_ok(root, "lst", RunOptions::default());
    let events = read_all(&rd).unwrap();
    let settled: Vec<(Option<String>, Option<ResultStamp>)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled {
                item,
                output,
                result,
                ..
            } if item == "a" => Some((output.clone(), result.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0].0.as_deref(), Some(r#"{"truncated":true}"#));
    let m = manifest(
        &rd,
        &settled[0].1.as_ref().expect("an item stamp").snapshot_id,
    );
    assert_eq!(m.subject.item.as_deref(), Some("a"));
    assert!(m.subject.visit_seq.is_some());
    let output = m
        .entries
        .iter()
        .find(|e| e.part == EntryPart::Output)
        .unwrap();
    assert!(object(&rd, &output.digest).contains(&body));
    let fields = m
        .entries
        .iter()
        .find(|e| e.part == EntryPart::Fields)
        .unwrap();
    assert_eq!(
        object(&rd, &fields.digest),
        format!(r#"{{"body":"{body}"}}"#)
    );
    // The list node's own settlement is a snapshot too, of its summary.
    assert!(node_stamps(&events, "work")[0].is_some());
}

/// When the run's secret values cannot all be read, capture reads and
/// stores nothing: every part is omitted `scrub_unavailable`, and the node
/// still settles.
#[test]
fn an_unresolvable_scrub_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = one_node("closed", ", results: [ { path: report.md } ]");
    // The run's config stops parsing before the node settles.
    let work = "echo secret-ish > report.md\nprintf ': : not yaml [' > \"$APB_RUN_DIR/run.yaml\"\necho out\n";
    seed(root, "closed", &yaml, &[("work.sh", work)]);
    let (_, rd) = run_within(root, "closed", RunOptions::default(), 120);
    let events = read_all(&rd).unwrap();
    let stamp = node_stamps(&events, "work")
        .into_iter()
        .next()
        .flatten()
        .expect("a stamp");
    let m = manifest(&rd, &stamp.snapshot_id);
    assert!(m.entries.is_empty(), "{:?}", m.entries);
    assert_eq!(m.output_omitted, Some(OutputOmitted::ScrubUnavailable));
    assert!(
        m.omitted.iter().all(|o| matches!(o, SnapshotOmission::Item(i) if i.reason == OmissionReason::ScrubUnavailable && i.path.is_none())),
        "{:?}",
        m.omitted
    );
    assert!(
        !rd.join("results/objects").exists(),
        "no object was written"
    );
}

/// Members of a concurrent batch are captured as each returns; the branch
/// a won `join: any` cancels settles `cancelled` and keeps no snapshot.
#[test]
fn batch_members_are_captured_and_a_cancelled_loser_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: race
name: race
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: slow, type: script, script: scripts/slow.sh, runner: sh, results: [ { path: slow.md } ] }
  - { id: fast, type: script, script: scripts/fast.sh, runner: sh, results: [ { path: fast.md } ] }
  - { id: merge, type: script, script: scripts/merge.sh, runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: slow }
  - { from: start, to: fast }
  - { from: slow, to: merge, join: any }
  - { from: fast, to: merge, join: any }
  - { from: merge, to: done }
"#;
    seed(
        root,
        "race",
        yaml,
        &[
            ("slow.sh", "sleep 20\necho late > slow.md\necho slow\n"),
            ("fast.sh", "echo quick > fast.md\necho fast\n"),
            ("merge.sh", "echo merged\n"),
        ],
    );
    let rd = run_ok(root, "race", RunOptions::default());
    let events = read_all(&rd).unwrap();
    let fast = only_stamp(&events, "fast");
    let m = manifest(&rd, &fast.snapshot_id);
    assert_eq!(file_text(&rd, &m, "fast.md").as_deref(), Some("quick\n"));
    let slow: Vec<(String, Option<ResultStamp>)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node,
                status,
                result,
                ..
            } if node == "slow" => Some((status.clone(), result.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(slow, [("cancelled".to_string(), None)]);
}

fn git(root: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

/// A cache hit is captured after the restore: the snapshot holds the
/// restored bytes and says it came from the cache.
#[test]
fn a_cache_hit_is_captured_after_the_restore() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: cached
name: cached
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gen, type: script, script: scripts/gen.sh, runner: sh, cache: auto, outputs: { files: ["out.txt"] }, results: [ { path: out.txt } ] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: gen }
  - { from: gen, to: done }
"#;
    fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
    seed(
        root,
        "cached",
        yaml,
        &[("gen.sh", "printf 'built once' > out.txt\necho generated\n")],
    );
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "c1"]);
    let opts = || RunOptions {
        cache: apb_engine::run_config::CacheRunMode::Auto,
        ..RunOptions::default()
    };
    let first = run_ok(root, "cached", opts());
    let m1 = manifest(
        &first,
        &only_stamp(&read_all(&first).unwrap(), "gen").snapshot_id,
    );
    assert!(!m1.provenance.cache_hit);
    fs::remove_file(root.join("out.txt")).unwrap();
    let second = run_ok(root, "cached", opts());
    let events = read_all(&second).unwrap();
    assert!(
        events.iter().any(
            |e| matches!(&e.payload, EventPayload::NodeCacheHit { node, .. } if node == "gen")
        ),
        "the second run is a hit"
    );
    let m2 = manifest(&second, &only_stamp(&events, "gen").snapshot_id);
    assert!(m2.provenance.cache_hit);
    assert_eq!(
        file_text(&second, &m2, "out.txt").as_deref(),
        Some("built once")
    );
    // The tree's commit at capture.
    assert!(m2.provenance.head.as_ref().is_some_and(|h| h.len() == 40));
}

/// The finish answer settles a finish node with a prompt: its declared
/// results keep a snapshot of the composed answer and the declared file,
/// with the attempt that composed it.
#[test]
fn a_finish_answer_is_captured() {
    let _l = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (home, cfg, bin) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    init_project(root).unwrap();
    crate::common::seed_profile(root, "writer", "claude", "haiku", &[]);
    let yaml = "schema: 2\nid: fin\nname: fin\nversion: 1.0.0\ndefaults:\n  profile: writer\nnodes:\n  - { id: s, type: start }\n  - { id: w, type: script, script: scripts/w.sh, runner: sh }\n  - { id: f, type: finish, outcome: success, prompt: \"compose the answer\", results: [ { path: summary.md } ] }\nedges:\n  - { from: s, to: w }\n  - { from: w, to: f }\n";
    seed(
        root,
        "fin",
        yaml,
        &[("w.sh", "echo the summary > summary.md\necho ok\n")],
    );
    let stub = bin.path().join("stub.sh");
    crate::common::write_sync(&stub, "#!/bin/sh\necho FINAL_ANSWER\n");
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let _cmd = crate::common::EnvVar::set("APB_AGENT_CMD", &stub);
    let _home = crate::common::EnvVar::set("HOME", home.path());
    let _cfg = crate::common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let rd = run_ok(root, "fin", RunOptions::default());
    let events = read_all(&rd).unwrap();
    let m = manifest(&rd, &only_stamp(&events, "f").snapshot_id);
    let output = m
        .entries
        .iter()
        .find(|e| e.part == EntryPart::Output)
        .unwrap();
    assert_eq!(object(&rd, &output.digest).trim(), "FINAL_ANSWER");
    assert_eq!(
        file_text(&rd, &m, "summary.md").as_deref(),
        Some("the summary\n")
    );
    let attempt = events.iter().find_map(|e| match &e.payload {
        EventPayload::AttemptStarted { node, .. } if node == "f" => Some(e.seq),
        _ => None,
    });
    assert_eq!(m.subject.attempt, attempt);
}

// The store's transaction, reservation and cleanup under capture.
mod result_txn {
    //! The result snapshot transaction and its cleanup: a fault at each stage,
    //! and a concurrent cleanup while a publisher waits at each stage.
    #![cfg(unix)]

    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use apb_core::result_store::{
        EntryPart, MANIFEST_VERSION, ResultFormat, ResultStamp, ResultTxn, SnapshotEntry,
        SnapshotManifest, SnapshotProvenance, SnapshotSubject, Staged, digest_hex, object_rel,
        snapshot_rel,
    };
    use apb_engine::event::{EventLog, EventPayload};
    use apb_engine::result_clean::{DeadOutcome, clean, survey};

    fn run() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Stages `texts` as output entries of one manifest.
    fn stage(txn: &mut ResultTxn, texts: &[&str]) -> SnapshotManifest {
        let entries = texts
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let Staged::Stored(digest) = txn.stage_object(t.as_bytes()).unwrap() else {
                    panic!("room for {t}");
                };
                SnapshotEntry {
                    part: if i == 0 {
                        EntryPart::Output
                    } else {
                        EntryPart::Fields
                    },
                    scope: None,
                    path: None,
                    size: t.len() as u64,
                    digest,
                    redacted: false,
                    format: ResultFormat::Text,
                }
            })
            .collect();
        SnapshotManifest {
            v: MANIFEST_VERSION,
            run_id: "run-1".into(),
            subject: SnapshotSubject {
                node: "work".into(),
                execution: 1,
                visit_seq: None,
                item: None,
                attempt: Some(3),
            },
            provenance: SnapshotProvenance::default(),
            entries,
            omitted: Vec::new(),
            incomplete: None,
            output_omitted: None,
        }
    }

    /// Journals a settlement carrying the stamp of `id`.
    fn journal_stamp(run_dir: &Path, id: &str) {
        let mut log = EventLog::open(run_dir).unwrap();
        log.append(EventPayload::node_settled(
            "work",
            "succeeded",
            1,
            "done",
            Vec::new(),
            Some(ResultStamp {
                snapshot_id: id.to_string(),
                entries: 1,
                bytes: 4,
                omitted: 0,
                incomplete: false,
            }),
        ))
        .unwrap();
    }

    fn object_path(run_dir: &Path, text: &str) -> PathBuf {
        let digest = apb_core::content::sha256_hex(text.as_bytes());
        run_dir.join(object_rel(digest_hex(&digest).unwrap()))
    }

    /// Every file under `results/` with its bytes.
    fn tree(run_dir: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        let mut stack = vec![run_dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(run_dir).unwrap().display().to_string();
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        out
    }

    fn readable_snapshots(run_dir: &Path) -> Vec<String> {
        let dir = run_dir.join("results/snapshots");
        let mut out: Vec<String> = std::fs::read_dir(&dir)
            .map(|d| {
                d.map(|e| e.unwrap().file_name().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    fn staging_is_empty(run_dir: &Path) -> bool {
        std::fs::read_dir(run_dir.join("results/staging"))
            .map(|mut d| d.next().is_none())
            .unwrap_or(true)
    }

    #[test]
    fn a_fault_after_begin_leaves_nothing_after_cleanup() {
        let t = run();
        let mut txn = ResultTxn::begin(t.path()).unwrap();
        stage(&mut txn, &["only staged"]);
        drop(txn); // the writer dies
        let report = clean(t.path(), false).unwrap();
        assert_eq!(report.dead.len(), 1);
        assert_eq!(report.dead[0].outcome, DeadOutcome::RolledBack);
        assert!(staging_is_empty(t.path()));
        assert!(!object_path(t.path(), "only staged").exists());
        assert!(readable_snapshots(t.path()).is_empty());
        let r: serde_json::Value = serde_json::from_slice(
            &std::fs::read(t.path().join("results/reservation.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(r["reserved_bytes"], 0);
        assert_eq!(r["committed_bytes"], 0);
    }

    /// A cleanup while a transaction is open keeps its bytes out of the
    /// committed total: its own end counts them, once.
    #[test]
    fn a_cleanup_during_an_open_transaction_counts_its_bytes_once() {
        let t = run();
        let mut txn = ResultTxn::begin(t.path()).unwrap();
        let m = stage(&mut txn, &["open while cleaned"]);
        let id = txn.publish(&m).unwrap();
        journal_stamp(t.path(), &id);
        let report = clean(t.path(), false).unwrap();
        assert_eq!(report.open, 1);
        txn.end().unwrap();
        let r: serde_json::Value = serde_json::from_slice(
            &std::fs::read(t.path().join("results/reservation.json")).unwrap(),
        )
        .unwrap();
        let manifest = std::fs::metadata(t.path().join(snapshot_rel(&id)))
            .unwrap()
            .len();
        let object = "open while cleaned".len() as u64;
        assert_eq!(r["committed_bytes"], manifest + object);
        assert_eq!(r["reserved_bytes"], 0);
    }

    fn committed_bytes(run: &std::path::Path) -> u64 {
        let r: serde_json::Value =
            serde_json::from_slice(&std::fs::read(run.join("results/reservation.json")).unwrap())
                .unwrap();
        r["committed_bytes"].as_u64().unwrap()
    }

    fn manifest_bytes(run: &std::path::Path, id: &str) -> u64 {
        std::fs::metadata(run.join(snapshot_rel(id))).unwrap().len()
    }

    /// One transaction moved an object and aborted; another found it in
    /// place and is open during a cleanup. The object is counted once:
    /// the open transaction's end does not count what it did not add.
    #[test]
    fn an_object_an_open_transaction_found_in_place_is_counted_by_cleanup() {
        let t = run();
        let mut a = ResultTxn::begin(t.path()).unwrap();
        stage(&mut a, &["shared object"]);
        a.move_objects().unwrap();
        a.abort().unwrap();
        let mut b = ResultTxn::begin(t.path()).unwrap();
        let m = stage(&mut b, &["shared object"]);
        let id = b.publish(&m).unwrap();
        journal_stamp(t.path(), &id);
        clean(t.path(), false).unwrap();
        b.end().unwrap();
        let object = "shared object".len() as u64;
        assert_eq!(
            committed_bytes(t.path()),
            object + manifest_bytes(t.path(), &id)
        );
    }

    /// One transaction added an object and is still open when another,
    /// which found it in place, settles and a cleanup runs. The object is
    /// counted once: by the transaction that added it, at its end.
    #[test]
    fn an_object_an_open_transaction_added_is_left_to_its_end() {
        let t = run();
        let mut a = ResultTxn::begin(t.path()).unwrap();
        stage(&mut a, &["added by a"]);
        a.move_objects().unwrap();
        let mut b = ResultTxn::begin(t.path()).unwrap();
        let m = stage(&mut b, &["added by a"]);
        let id = b.publish(&m).unwrap();
        journal_stamp(t.path(), &id);
        b.end().unwrap();
        clean(t.path(), false).unwrap();
        a.end().unwrap();
        let object = "added by a".len() as u64;
        assert_eq!(
            committed_bytes(t.path()),
            object + manifest_bytes(t.path(), &id)
        );
    }

    #[test]
    fn a_fault_after_the_objects_moved_collects_them() {
        let t = run();
        let mut txn = ResultTxn::begin(t.path()).unwrap();
        stage(&mut txn, &["moved object"]);
        txn.move_objects().unwrap();
        assert!(object_path(t.path(), "moved object").exists());
        drop(txn);
        let report = clean(t.path(), false).unwrap();
        assert_eq!(report.dead[0].outcome, DeadOutcome::RolledBack);
        assert_eq!(report.removed_objects, 1);
        assert!(!object_path(t.path(), "moved object").exists());
        assert!(staging_is_empty(t.path()));
    }

    #[test]
    fn a_fault_after_the_manifest_rename_without_a_stamp_rolls_back() {
        let t = run();
        let mut txn = ResultTxn::begin(t.path()).unwrap();
        let m = stage(&mut txn, &["published, never stamped"]);
        let id = txn.publish(&m).unwrap();
        assert!(t.path().join(snapshot_rel(&id)).exists());
        drop(txn);
        let report = clean(t.path(), false).unwrap();
        assert_eq!(report.dead[0].outcome, DeadOutcome::RolledBack);
        assert_eq!(report.removed_snapshots, vec![format!("{id}.json")]);
        assert!(readable_snapshots(t.path()).is_empty());
        assert!(!object_path(t.path(), "published, never stamped").exists());
    }

    #[test]
    fn a_fault_after_the_stamp_before_end_completes() {
        let t = run();
        let mut txn = ResultTxn::begin(t.path()).unwrap();
        let m = stage(&mut txn, &["stamped"]);
        let id = txn.publish(&m).unwrap();
        journal_stamp(t.path(), &id);
        drop(txn);
        let report = clean(t.path(), false).unwrap();
        assert_eq!(report.dead[0].outcome, DeadOutcome::Completed);
        assert_eq!(report.kept_snapshots, 1);
        assert!(report.removed_snapshots.is_empty());
        assert_eq!(readable_snapshots(t.path()), vec![format!("{id}.json")]);
        assert!(object_path(t.path(), "stamped").exists());
        assert!(staging_is_empty(t.path()));
    }

    /// A published, stamped and ended snapshot whose object `shared` a later
    /// transaction stages too.
    fn earlier_snapshot(run_dir: &Path) -> String {
        let mut txn = ResultTxn::begin(run_dir).unwrap();
        let m = stage(&mut txn, &["shared"]);
        let id = txn.publish(&m).unwrap();
        journal_stamp(run_dir, &id);
        txn.end().unwrap();
        id
    }

    #[test]
    fn a_cleanup_while_a_publisher_waits_at_each_stage_removes_nothing_it_named() {
        let t = run();
        let earlier = earlier_snapshot(t.path());
        let named = |run_dir: &Path, txn_texts: &[&str]| {
            assert!(run_dir.join(snapshot_rel(&earlier)).exists());
            assert!(object_path(run_dir, "shared").exists());
            for text in txn_texts {
                let staged_or_moved = object_path(run_dir, text).exists()
                    || std::fs::read_dir(run_dir.join("results/staging"))
                        .unwrap()
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().is_dir())
                        .any(|e| {
                            let digest = apb_core::content::sha256_hex(text.as_bytes());
                            e.path().join(digest_hex(&digest).unwrap()).exists()
                        });
                assert!(staged_or_moved, "{text}");
            }
        };

        let mut txn = ResultTxn::begin(t.path()).unwrap();
        let r = clean(t.path(), false).unwrap();
        assert!(r.dead.is_empty() && r.open == 1);
        named(t.path(), &[]);

        let m = stage(&mut txn, &["new text", "shared"]);
        clean(t.path(), false).unwrap();
        named(t.path(), &["new text", "shared"]);

        txn.move_objects().unwrap();
        clean(t.path(), false).unwrap();
        named(t.path(), &["new text", "shared"]);

        let id = txn.publish(&m).unwrap();
        let r = clean(t.path(), false).unwrap();
        assert!(r.removed_snapshots.is_empty(), "{r:?}");
        assert!(t.path().join(snapshot_rel(&id)).exists());
        named(t.path(), &["new text", "shared"]);

        journal_stamp(t.path(), &id);
        clean(t.path(), false).unwrap();
        txn.end().unwrap();
        let r = clean(t.path(), false).unwrap();
        assert!(!r.has_leftovers(), "{r:?}");
        assert_eq!(r.kept_snapshots, 2);
        named(t.path(), &["new text", "shared"]);
        assert!(staging_is_empty(t.path()));
    }

    #[test]
    fn an_aborted_transaction_never_takes_an_object_another_snapshot_names() {
        let t = run();
        let earlier = earlier_snapshot(t.path());
        let mut txn = ResultTxn::begin(t.path()).unwrap();
        let m = stage(&mut txn, &["shared", "only mine"]);
        txn.publish(&m).unwrap();
        txn.abort().unwrap();
        assert_eq!(
            readable_snapshots(t.path()),
            vec![format!("{earlier}.json")]
        );
        let r = clean(t.path(), false).unwrap();
        assert_eq!(r.removed_objects, 1);
        assert!(object_path(t.path(), "shared").exists());
        assert!(!object_path(t.path(), "only mine").exists());
    }

    #[test]
    fn a_line_this_binary_cannot_decode_still_protects_what_it_names() {
        let t = run();
        let mut txn = ResultTxn::begin(t.path()).unwrap();
        let m = stage(&mut txn, &["named by a newer event"]);
        let id = txn.publish(&m).unwrap();
        txn.end().unwrap();
        std::fs::write(
            t.path().join("events.jsonl"),
            format!("{{\"seq\":0,\"type\":\"a_newer_event\",\"ref\":\"r:{id}/output\"}}\n"),
        )
        .unwrap();
        let r = clean(t.path(), false).unwrap();
        assert!(!r.has_leftovers(), "{r:?}");
        assert!(object_path(t.path(), "named by a newer event").exists());
    }

    #[test]
    fn a_kept_manifest_that_does_not_verify_keeps_every_object() {
        let t = run();
        let id = earlier_snapshot(t.path());
        let mut txn = ResultTxn::begin(t.path()).unwrap();
        stage(&mut txn, &["unreferenced"]);
        txn.move_objects().unwrap();
        drop(txn);
        std::fs::write(t.path().join(snapshot_rel(&id)), b"{}").unwrap();
        let r = clean(t.path(), false).unwrap();
        assert!(r.objects_unverified);
        assert_eq!(r.removed_objects, 0);
        assert!(object_path(t.path(), "unreferenced").exists());
        assert!(t.path().join(snapshot_rel(&id)).exists());
    }

    #[test]
    fn a_survey_and_a_dry_run_report_leftovers_and_write_nothing() {
        let t = run();
        earlier_snapshot(t.path());
        let mut txn = ResultTxn::begin(t.path()).unwrap();
        let m = stage(&mut txn, &["left over"]);
        txn.publish(&m).unwrap();
        drop(txn);
        let before = tree(t.path());
        let s = survey(t.path()).unwrap();
        let d = clean(t.path(), true).unwrap();
        assert_eq!(s, d);
        assert!(s.dry_run && s.has_leftovers());
        assert_eq!(s.dead.len(), 1);
        assert_eq!(s.removed_snapshots.len(), 1);
        assert_eq!(s.removed_objects, 1);
        assert_eq!(tree(t.path()), before);
        // A run without a result store gets nothing created.
        let empty = run();
        assert!(!clean(empty.path(), true).unwrap().has_leftovers());
        assert!(!clean(empty.path(), false).unwrap().has_leftovers());
        assert!(!empty.path().join("results").exists());
    }
}
