//! Task 9: `apb doctor --run <id>`, the per-run doctor.
//!
//! The environment doctor answers "can this machine run playbooks"; this one
//! answers "what is wrong with THIS run", which is the question an operator
//! actually has when a run has been reading `running` for twenty minutes. It
//! is strictly read-only: it names the problem and repairs nothing.
//!
//! The journals here are hand-built because the interesting states cannot be
//! produced on demand by a real run: a *dead* attempt pid and a *stale*
//! driver.pid are, by definition, processes that are no longer there.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

fn apb() -> Command {
    crate::common::apb()
}

/// A real pid that is reliably absent: a child spawned, waited for and reaped,
/// so the number was genuinely valid and is now free.
///
/// Deliberately not `u32::MAX`. An impossible pid exercises the invalid-pid
/// rejection (tested directly in `apb_engine::liveness`) rather than the
/// stale-holder property these fixtures are about, and using one here hid a
/// real bug: `u32::MAX` narrows to `-1`, the `kill(2)` "every process"
/// wildcard, so the probe reported it as running.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn a throwaway child to borrow a pid from");
    let pid = child.id();
    // Bounded by construction: `exit 0` cannot fail to exit.
    child.wait().expect("reap the throwaway child");
    pid
}

fn init(dir: &Path) {
    apb().arg("init").current_dir(dir).assert().success();
}

fn run_dir(root: &Path, run_id: &str) -> std::path::PathBuf {
    let d = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A wedged run: the attempt pid is dead, driver.pid names a dead process, and
/// one control entry sits past the persisted cursor, never applied. All three
/// must be named, and the run must exit non-zero.
#[test]
fn doctor_run_flags_a_wedged_run() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "wedged-1");
    let dead = dead_pid();

    fs::write(
        rd.join("events.jsonl"),
        format!(
            "{{\"seq\":0,\"ts\":1000,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}}\n\
             {{\"seq\":1,\"ts\":2000,\"type\":\"node_started\",\"node\":\"a\",\"attempt\":1}}\n\
             {{\"seq\":2,\"ts\":3000,\"type\":\"attempt_started\",\"node\":\"a\",\"attempt\":1,\"agent\":\"stub\",\"pid\":{dead}}}\n\
             {{\"seq\":3,\"ts\":4000,\"type\":\"supervisor_action\",\"action\":\"retry\",\"node\":\"a\",\"detail\":\"\"}}\n\
             {{\"seq\":4,\"ts\":5000,\"type\":\"supervisor_action\",\"action\":\"retry\",\"node\":\"a\",\"detail\":\"\"}}\n"
        ),
    )
    .unwrap();
    // A stale driver.pid: the sole reason `stop_run` would refuse to finalize.
    fs::write(rd.join("driver.pid"), dead.to_string()).unwrap();
    // One control entry posted and never consumed (no control.cursor at all).
    fs::write(
        rd.join("control.jsonl"),
        "{\"seq\":0,\"cmd\":\"abort\",\"reason\":\"stop requested\"}\n",
    )
    .unwrap();

    let out = apb()
        .arg("doctor")
        .arg("--run")
        .arg("wedged-1")
        .current_dir(dir.path())
        .assert()
        .failure();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();

    // Asserted as whole `[level] subject:` prefixes, not as bare words. Every
    // subject name appears in the report whatever its status, so a test that
    // only looked for "driver" would pass even if the driver check said ok -
    // which is the one thing these lines exist to rule out.
    for line in [
        "[fail] attempt a#1:",
        "[fail] driver:",
        "[warn] control:",
        "[warn] supervisor actions:",
        "[warn] run:",
    ] {
        assert!(
            stdout.contains(line),
            "expected a line starting `{line}` in:\n{stdout}"
        );
    }
    assert!(
        stdout.contains(&dead.to_string()),
        "the dead pid must be named so an operator can correlate it: {stdout}"
    );
}

/// A run whose driver finished cleanly is never reported as wedged, even
/// though its journal still names the pid that drove it. The blocking `[fail]`
/// verdict is reserved for a `driver.pid` that is actually still on disk.
#[test]
fn doctor_run_does_not_fail_a_cleanly_finished_run() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "finished-1");
    fs::write(
        rd.join("events.jsonl"),
        "{\"seq\":0,\"ts\":1000,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":2000,\"type\":\"run_finished\",\"outcome\":\"succeeded\"}\n",
    )
    .unwrap();
    // The drive removed its pid file on exit, exactly as `DriverPidGuard` does.
    assert!(!rd.join("driver.pid").exists());

    let out = apb()
        .arg("doctor")
        .arg("--run")
        .arg("finished-1")
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("[ok]   driver: no driver.pid"),
        "a finished run must report no drive in progress: {stdout}"
    );
}

/// A run that finished cleanly: no open attempts, no driver.pid, no control
/// backlog. Every check reports ok and the command exits zero.
#[test]
fn doctor_run_reports_all_ok_for_a_healthy_run() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "healthy-1");
    fs::write(
        rd.join("events.jsonl"),
        "{\"seq\":0,\"ts\":1000,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":2000,\"type\":\"node_started\",\"node\":\"a\",\"attempt\":1}\n\
         {\"seq\":2,\"ts\":3000,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n\
         {\"seq\":3,\"ts\":4000,\"type\":\"run_finished\",\"outcome\":\"succeeded\"}\n",
    )
    .unwrap();
    // A run apb drove is readable by its owner only; a hand-made one is
    // narrowed the same way, else the files check warns.
    apb_engine::run_perms::narrow(&rd);

    let out = apb()
        .arg("doctor")
        .arg("--run")
        .arg("healthy-1")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("[ok]"));
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("[fail]") && !stdout.contains("[warn]"),
        "a healthy completed run must print only ok checks: {stdout}"
    );
}

/// An unknown run is an error, not an empty ok report.
#[test]
fn doctor_run_rejects_an_unknown_run() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    apb()
        .arg("doctor")
        .arg("--run")
        .arg("ghost-1")
        .current_dir(dir.path())
        .assert()
        .failure();
}

/// #85 finding 1: a node bound to an agent with no known non-interactive
/// permission flag (hermes today) surfaces through the CLI as a `[warn]
/// autonomy:` line, not just in the library-level checks.
#[test]
fn doctor_run_warns_autonomy_for_a_node_with_no_non_interactive_flag() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "no-flag-1");
    fs::write(
        rd.join("events.jsonl"),
        "{\"seq\":0,\"ts\":1000,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":2000,\"type\":\"node_started\",\"node\":\"a\",\"attempt\":1}\n\
         {\"seq\":2,\"ts\":3000,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n\
         {\"seq\":3,\"ts\":4000,\"type\":\"run_finished\",\"outcome\":\"succeeded\"}\n",
    )
    .unwrap();
    fs::write(
        rd.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n",
    )
    .unwrap();
    let invocation = apb_engine::invocation::ResolvedInvocation {
        agent_id: "hermes".to_string(),
        model: "m".into(),
        spec: apb_engine::invocation::builtin("hermes").expect("hermes is a builtin"),
        soul_delivery: apb_core::config::SoulDelivery::Prefix,
        canonical_executable: std::path::PathBuf::from("/bin/true"),
        executable_fingerprint: "0:0".into(),
    };
    let manifest = apb_engine::manifest::RunExecutionManifest {
        profiles: vec![apb_engine::manifest::ManifestProfile {
            scope: "project".into(),
            name: "x".into(),
            profile_digest: "sha256:a".into(),
            bundle_digest: "sha256:b".into(),
            soul: "role".into(),
            soul_requirement: apb_core::profile::SoulRequirement::Any,
            skills: Vec::new(),
            chain: vec![invocation],
            ephemeral: false,
            hermetic: false,
            zcode_mode: None,
            process_env: None,
            tiers: Vec::new(),
            routed_tier: None,
            cascade: 0,
        }],
        node_bindings: std::collections::BTreeMap::from([(
            "a".to_string(),
            "project/x".to_string(),
        )]),
        connectors: Vec::new(),
        connector_grants: std::collections::BTreeMap::new(),
        decisions: None,
        execution: None,
        consent: None,
        candidate_trial: false,
        candidate_skipped: None,
        task_type: None,
        secret_params: Vec::new(),
        launch: None,
        unattended: false,
        origin: None,
    };
    apb_engine::manifest::write(&rd, &manifest).unwrap();

    let out = apb()
        .arg("doctor")
        .arg("--run")
        .arg("no-flag-1")
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("[warn] autonomy:"),
        "expected an `[warn] autonomy:` line in:\n{stdout}"
    );
    assert!(
        stdout.contains("node `a`") && stdout.contains("agent `hermes`"),
        "the offending node and agent must be named: {stdout}"
    );
}

/// The environment doctor is untouched: a bare `apb doctor` still runs the
/// whole-project checks.
#[test]
fn bare_doctor_still_checks_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    apb()
        .arg("doctor")
        .current_dir(dir.path())
        .assert()
        .stdout(predicate::str::contains("[ok]"));
}

/// A node attempt that failed under `process_env: allowlist`: the doctor
/// names what the agent did not get, as a warning that does not fail.
#[test]
fn doctor_run_names_what_an_allowlist_withheld_from_a_failed_attempt() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "env-1");
    fs::write(
        rd.join("events.jsonl"),
        "{\"seq\":0,\"ts\":1000,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":2000,\"type\":\"agent_env_applied\",\"node\":\"build\",\"attempt\":1,\"agent\":\"codex\",\"kept\":20,\"withheld\":[\"AWS_PROFILE\"],\"absent\":[\"NPM_TOKEN\"]}\n\
         {\"seq\":2,\"ts\":3000,\"type\":\"attempt_finished\",\"node\":\"build\",\"attempt\":1,\"status\":\"failed\"}\n\
         {\"seq\":3,\"ts\":4000,\"type\":\"run_finished\",\"outcome\":\"failed\"}\n",
    )
    .unwrap();
    let out = apb()
        .arg("doctor")
        .arg("--run")
        .arg("env-1")
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains(
            "[warn] process env: node `build` attempt 1 (codex) failed under process_env allowlist; withheld: AWS_PROFILE; not set in the operator environment: NPM_TOKEN"
        ),
        "{stdout}"
    );
}

/// Every file under `dir` with its bytes and mode.
#[cfg(unix)]
fn files_of(dir: &Path) -> std::collections::BTreeMap<String, (Vec<u8>, u32)> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let meta = fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(dir).unwrap().display().to_string();
            if meta.is_dir() {
                stack.push(p);
                out.insert(rel, (Vec::new(), meta.permissions().mode()));
            } else {
                out.insert(rel, (fs::read(&p).unwrap(), meta.permissions().mode()));
            }
        }
    }
    out
}

/// A result transaction whose writer died is reported with the command
/// that cleans it, and the doctor resolves nothing: the run directory is
/// byte for byte what it was.
#[cfg(unix)]
#[test]
fn doctor_run_reports_a_dead_result_transaction_and_writes_nothing() {
    use apb_core::result_store::{ResultTxn, Staged};
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "results-1");
    fs::write(
        rd.join("events.jsonl"),
        "{\"seq\":0,\"ts\":1000,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":2000,\"type\":\"run_finished\",\"outcome\":\"succeeded\"}\n",
    )
    .unwrap();
    let mut txn = ResultTxn::begin(&rd).unwrap();
    assert!(matches!(
        txn.stage_object(b"the report").unwrap(),
        Staged::Stored(_)
    ));
    txn.move_objects().unwrap();
    drop(txn); // the writer died here
    let before = files_of(&rd);

    let out = apb()
        .args(["doctor", "--run", "results-1"])
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains(
            "[warn] results: 0 unreferenced snapshot(s), 1 unreferenced object(s), 10 bytes, 1 transaction(s) whose writer ended; remove them with: apb results clean results-1"
        ),
        "{stdout}"
    );
    assert_eq!(files_of(&rd), before);
}

// `apb results`: the listing, show and clean (PR 20a), next to the run doctor's results line.
mod results_cli {
    //! `apb results`: listing a run's snapshots, showing an entry, cleaning up,
    //! and the results lines of `apb runs <id>`. The snapshots are written
    //! through the store's transaction directly and stamped in a hand-built
    //! journal, exactly as the capture leaves them.

    use std::fs;
    use std::path::{Path, PathBuf};

    use apb_core::result_store::{
        EntryPart, ResultScope, ResultStamp, ResultTxn, SnapshotEntry, SnapshotManifest,
        SnapshotProvenance, SnapshotSubject, Staged,
    };
    use apb_engine::event::{EventLog, EventPayload};
    use serde_json::{Value, json};

    fn apb() -> assert_cmd::Command {
        crate::common::apb()
    }

    const RUN: &str = "res-1";

    /// One stored entry of a snapshot to write.
    struct Part<'a> {
        part: EntryPart,
        file: Option<(ResultScope, &'a str)>,
        text: &'a str,
        redacted: bool,
    }

    fn output(text: &str) -> Part<'_> {
        Part {
            part: EntryPart::Output,
            file: None,
            text,
            redacted: false,
        }
    }

    fn file<'a>(path: &'a str, text: &'a str) -> Part<'a> {
        Part {
            part: EntryPart::File,
            file: Some((ResultScope::Workspace, path)),
            text,
            redacted: false,
        }
    }

    /// A project with one run whose journal this builds event by event.
    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        run: PathBuf,
        log: EventLog,
    }

    fn payload(v: Value) -> EventPayload {
        serde_json::from_value(v).unwrap()
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        apb_core::registry::init_project(&root).unwrap();
        let run = root.join(".apb/runs").join(RUN);
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("run.yaml"), "params: {}\n").unwrap();
        let mut log = EventLog::open(&run).unwrap();
        log.append(EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
            definition: None,
        })
        .unwrap();
        Fixture {
            _dir: dir,
            root,
            run,
            log,
        }
    }

    impl Fixture {
        /// Starts an execution of `node` with one attempt in `workdir`; the
        /// attempt id.
        fn start(&mut self, node: &str, workdir: &Path) -> u64 {
            self.log
                .append(payload(
                    json!({"type": "node_started", "node": node, "attempt": 1}),
                ))
                .unwrap();
            self.log
                .append(payload(json!({
                    "type": "attempt_started", "node": node, "attempt": 1,
                    "agent": "claude", "workdir": workdir.display().to_string(),
                })))
                .unwrap()
                .seq
        }

        /// Publishes a snapshot of `parts`, journals its stamp on the
        /// execution's `node_finished` and ends the transaction; the stamp.
        fn settle(
            &mut self,
            node: &str,
            execution: u32,
            attempt: Option<u64>,
            parts: &[Part<'_>],
        ) -> ResultStamp {
            let mut txn = ResultTxn::begin(&self.run).unwrap();
            let mut entries = Vec::new();
            for p in parts {
                let Staged::Stored(digest) = txn.stage_object(p.text.as_bytes()).unwrap() else {
                    panic!("staging refused")
                };
                entries.push(SnapshotEntry {
                    part: p.part,
                    scope: p.file.map(|f| f.0),
                    path: p.file.map(|f| f.1.to_string()),
                    size: p.text.len() as u64,
                    digest,
                    redacted: p.redacted,
                    format: Default::default(),
                    records_invalid: None,
                });
            }
            let manifest = SnapshotManifest {
                v: 1,
                run_id: RUN.into(),
                subject: SnapshotSubject {
                    node: node.into(),
                    execution,
                    visit_seq: None,
                    item: None,
                    attempt,
                },
                provenance: SnapshotProvenance::default(),
                entries,
                omitted: Vec::new(),
                incomplete: None,
                output_omitted: None,
            };
            let id = txn.publish(&manifest).unwrap();
            let stamp = ResultStamp {
                snapshot_id: id,
                entries: parts.len() as u32,
                bytes: parts.iter().map(|p| p.text.len() as u64).sum(),
                omitted: 0,
                incomplete: false,
            };
            self.log
                .append(EventPayload::node_settled(
                    node,
                    "succeeded",
                    1,
                    "done",
                    Vec::new(),
                    Some(stamp.clone()),
                ))
                .unwrap();
            txn.end().unwrap();
            stamp
        }

        fn results_json(&self) -> Value {
            let out = apb()
                .args(["results", RUN, "--json"])
                .current_dir(&self.root)
                .assert()
                .success();
            serde_json::from_slice(&out.get_output().stdout).unwrap()
        }

        fn show(&self, reference: &str) -> assert_cmd::assert::Assert {
            apb()
                .args(["results", "show", RUN, reference])
                .current_dir(&self.root)
                .assert()
        }

        fn object_path(&self, digest: &str) -> PathBuf {
            let hex = digest.strip_prefix("sha256:").unwrap();
            self.run.join("results/objects").join(&hex[..2]).join(hex)
        }
    }

    fn stdout(a: &assert_cmd::assert::Assert) -> String {
        String::from_utf8(a.get_output().stdout.clone()).unwrap()
    }

    fn stderr(a: &assert_cmd::assert::Assert) -> String {
        String::from_utf8(a.get_output().stderr.clone()).unwrap()
    }

    /// A loop node that rewrote `report.md` on three executions keeps three
    /// snapshots, and each shows its own execution's bytes after the working
    /// tree is gone.
    #[test]
    fn each_execution_keeps_its_own_bytes_after_the_tree_is_gone() {
        let mut f = fixture();
        let tree = f.root.join("tree");
        fs::create_dir_all(&tree).unwrap();
        let mut stamps = Vec::new();
        for n in 1..=3u32 {
            let text = format!("# report {n}\n\u{e9}t\u{e9}\n");
            fs::write(tree.join("report.md"), &text).unwrap();
            let attempt = f.start("work", &tree);
            stamps.push((
                attempt,
                text.clone(),
                f.settle(
                    "work",
                    n,
                    Some(attempt),
                    &[output(&format!("out {n}")), file("report.md", &text)],
                ),
            ));
        }
        fs::remove_dir_all(&tree).unwrap();

        let v = f.results_json();
        let rows = v["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3, "{v}");
        for (i, (row, (attempt, text, stamp))) in rows.iter().zip(&stamps).enumerate() {
            assert_eq!(row["execution"]["node"], "work");
            assert_eq!(row["execution"]["ordinal"], i as u64 + 1);
            assert_eq!(row["snapshot_id"], stamp.snapshot_id.as_str());
            assert_eq!(row["produced_by_attempt"], *attempt);
            // The number people read, beside the id (a journal seq).
            assert_eq!(row["attempt_number"], 1, "{row}");
            assert_eq!(row["verified"], "ok");
            assert!(row["settlement_seq"].as_u64().unwrap() > *attempt);
            let entries: Vec<&str> = row["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["entry"].as_str().unwrap())
                .collect();
            assert_eq!(entries, ["output", "file:workspace:report.md"]);
            let shown = f.show(&format!("r:{}/file:workspace:report.md", stamp.snapshot_id));
            assert_eq!(stdout(&shown.success()), *text);
        }

        let text = apb()
            .args(["results", RUN])
            .current_dir(&f.root)
            .assert()
            .success();
        let text = stdout(&text);
        assert!(
            text.contains(&format!("work #2  succeeded  {}", stamps[1].2.snapshot_id)),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "r:{}/file:workspace:report.md  {} bytes",
                stamps[2].2.snapshot_id,
                stamps[2].1.len()
            )),
            "{text}"
        );
    }

    /// `--out` writes the stored bytes exactly, and `--node` narrows the list.
    #[test]
    fn show_writes_the_exact_bytes_and_node_narrows_the_list() {
        let mut f = fixture();
        let text = "line one\n\tline two\n";
        let a = f.start("a", &f.root.clone());
        let s = f.settle("a", 1, Some(a), &[output(text)]);
        let b = f.start("b", &f.root.clone());
        f.settle("b", 1, Some(b), &[output("other")]);
        let out = f.root.join("out.txt");
        apb()
            .args([
                "results",
                "show",
                RUN,
                &format!("r:{}/output", s.snapshot_id),
            ])
            .arg("--out")
            .arg(&out)
            .current_dir(&f.root)
            .assert()
            .success();
        assert_eq!(fs::read_to_string(&out).unwrap(), text);

        let out = apb()
            .args(["results", RUN, "--node", "b", "--json"])
            .current_dir(&f.root)
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        let nodes: Vec<&str> = v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["execution"]["node"].as_str().unwrap())
            .collect();
        assert_eq!(nodes, ["b"]);
    }

    /// A citation's range `#[a,b)` shows those bytes only, as MCP reads
    /// them; a range off a character boundary is refused, not widened.
    #[test]
    fn show_honours_the_range_of_a_citation() {
        let mut f = fixture();
        let text = "caf\u{e9} report\n";
        let a = f.start("a", &f.root.clone());
        let s = f.settle("a", 1, Some(a), &[output(text)]);
        let shown = f.show(&format!("r:{}/output#[6,12)", s.snapshot_id));
        assert_eq!(stdout(&shown.success()), "report");
        f.show(&format!("r:{}/output#[0,4)", s.snapshot_id))
            .failure();
    }

    /// A hand-edited object, or a hand-edited manifest, reads `corrupt` in the
    /// listing and refuses to show any text.
    #[test]
    fn a_hand_edited_object_or_manifest_reads_corrupt() {
        let mut f = fixture();
        let a = f.start("work", &f.root.clone());
        let s = f.settle("work", 1, Some(a), &[output("the stored text")]);
        let v = f.results_json();
        let digest = v["rows"][0]["entries"][0]["digest"]
            .as_str()
            .unwrap()
            .to_string();

        let object = f.object_path(&digest);
        fs::write(&object, "the edited text").unwrap();
        assert_eq!(f.results_json()["rows"][0]["verified"], "corrupt");
        let shown = f.show(&format!("r:{}/output", s.snapshot_id)).failure();
        assert!(stdout(&shown).is_empty());
        assert!(stderr(&shown).contains("corrupt"), "{}", stderr(&shown));

        // The same bytes back: it verifies again.
        fs::write(&object, "the stored text").unwrap();
        assert_eq!(f.results_json()["rows"][0]["verified"], "ok");

        let manifest = f
            .run
            .join("results/snapshots")
            .join(format!("{}.json", s.snapshot_id));
        let edited = fs::read_to_string(&manifest)
            .unwrap()
            .replace("\"redacted\":false", "\"redacted\":true");
        fs::write(&manifest, edited).unwrap();
        assert_eq!(f.results_json()["rows"][0]["verified"], "corrupt");
        let shown = f.show(&format!("r:{}/output", s.snapshot_id)).failure();
        assert!(stderr(&shown).contains("corrupt"), "{}", stderr(&shown));

        fs::remove_file(&manifest).unwrap();
        assert_eq!(f.results_json()["rows"][0]["verified"], "missing");
    }

    /// A manifest no stamp names is neither listed nor shown, one stamped for
    /// another node reads corrupt, and a reference that does not parse or a run
    /// outside `.apb/runs` is refused.
    #[test]
    fn only_stamped_snapshots_are_read() {
        let mut f = fixture();
        // A stamped snapshot of the same node exists; it names only itself.
        let a = f.start("work", &f.root.clone());
        let stamped = f.settle("work", 1, Some(a), &[output("stamped")]);
        let mut txn = ResultTxn::begin(&f.run).unwrap();
        let Staged::Stored(digest) = txn.stage_object(b"never stamped").unwrap() else {
            panic!()
        };
        let unstamped = txn
            .publish(&SnapshotManifest {
                v: 1,
                run_id: RUN.into(),
                subject: SnapshotSubject {
                    node: "work".into(),
                    execution: 1,
                    visit_seq: None,
                    item: None,
                    attempt: None,
                },
                provenance: SnapshotProvenance::default(),
                entries: vec![SnapshotEntry {
                    part: EntryPart::Output,
                    scope: None,
                    path: None,
                    size: 13,
                    digest,
                    redacted: false,
                    format: Default::default(),
                    records_invalid: None,
                }],
                omitted: Vec::new(),
                incomplete: None,
                output_omitted: None,
            })
            .unwrap();
        txn.end().unwrap();

        let rows = f.results_json()["rows"].clone();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["snapshot_id"], stamped.snapshot_id.as_str());
        let shown = f.show(&format!("r:{unstamped}/output")).failure();
        assert!(stdout(&shown).is_empty());
        assert!(stderr(&shown).contains("no stamp"), "{}", stderr(&shown));

        // A stamp of another node naming that manifest does not make it that
        // node's result: the row reads corrupt and nothing is shown.
        let stamp = ResultStamp {
            snapshot_id: unstamped.clone(),
            entries: 1,
            bytes: 13,
            omitted: 0,
            incomplete: false,
        };
        f.log
            .append(EventPayload::node_settled(
                "other",
                "succeeded",
                1,
                "x",
                Vec::new(),
                Some(stamp),
            ))
            .unwrap();
        assert_eq!(f.results_json()["rows"][1]["verified"], "corrupt");
        let shown = f.show(&format!("r:{unstamped}/output")).failure();
        assert!(stdout(&shown).is_empty());
        assert!(stderr(&shown).contains("corrupt"), "{}", stderr(&shown));

        let bad = f.show("r:nothex/output").failure();
        assert!(
            stderr(&bad).contains("invalid result reference"),
            "{}",
            stderr(&bad)
        );
        apb()
            .args(["results", "../../etc", "--json"])
            .current_dir(&f.root)
            .assert()
            .code(2);
    }

    /// A secret value the run knows only now (declared after the capture
    /// stored the text as it was) is replaced on every read, and the read says
    /// so (the digest and size are then the served text's).
    #[test]
    fn a_secret_known_after_capture_is_hidden_on_read() {
        let mut f = fixture();
        let a = f.start("work", &f.root.clone());
        let s = f.settle(
            "work",
            1,
            Some(a),
            &[output("deploy key k3y-v4lue-0987 used")],
        );
        fs::write(f.run.join("run.yaml"), "params:\n  token: k3y-v4lue-0987\n").unwrap();
        fs::write(
            f.run.join("playbook.yaml"),
            "params:\n  - { name: token, type: text, secret: true }\n",
        )
        .unwrap();
        let shown = f.show(&format!("r:{}/output", s.snapshot_id)).success();
        let text = stdout(&shown);
        assert!(!text.contains("k3y-v4lue-0987"), "{text}");
        assert!(
            text.starts_with("deploy key ") && text.ends_with(" used"),
            "{text}"
        );
        assert!(stderr(&shown).contains("replaced on this read"));
    }

    /// A settlement whose capture failed (a `result_capture_failed`
    /// diagnostic, written here as the writer writes it) is listed with why,
    /// in the text and in `--json`.
    #[test]
    fn a_snapshot_that_was_not_stored_says_why() {
        let f = fixture();
        let record = json!({
            "v": 1, "kind": "result_capture_failed", "after_seq": 1, "key": "work|1||1",
            "node": "work", "execution": 1, "reason": "snapshot not stored: the store is full",
        });
        fs::write(f.run.join("diagnostics.jsonl"), format!("{record}\n")).unwrap();
        let listed = apb()
            .args(["results", RUN])
            .current_dir(&f.root)
            .assert()
            .success();
        assert!(
            stdout(&listed).contains("work #1: snapshot not stored: the store is full"),
            "{}",
            stdout(&listed)
        );
        let v = f.results_json();
        assert_eq!(v["not_stored"][0]["node"], "work");
        assert_eq!(
            v["not_stored"][0]["reason"],
            "snapshot not stored: the store is full"
        );
    }

    /// When the run's secret values cannot all be read, no text is served.
    #[test]
    fn a_run_whose_secrets_cannot_be_read_serves_no_text() {
        let mut f = fixture();
        let a = f.start("work", &f.root.clone());
        let s = f.settle("work", 1, Some(a), &[output("anything")]);
        fs::remove_file(f.run.join("run.yaml")).unwrap();
        fs::write(f.run.join("manifest.yaml"), "run_id: res-1\n").unwrap();
        let shown = f.show(&format!("r:{}/output", s.snapshot_id)).failure();
        assert!(stdout(&shown).is_empty());
        assert!(
            stderr(&shown).contains("secrets_unresolved"),
            "{}",
            stderr(&shown)
        );
    }

    /// Artifact refs recorded without a snapshot are listed as digests with no
    /// stored bytes.
    #[test]
    fn refs_without_a_snapshot_are_listed_as_recorded_digests() {
        let mut f = fixture();
        f.start("old", &f.root.clone());
        f.log
            .append(EventPayload::node_settled(
                "old",
                "succeeded",
                1,
                "done",
                vec![apb_core::cache::ArtifactRef {
                    name: "report.md".into(),
                    digest: format!("sha256:{}", "ab".repeat(32)),
                    scope: apb_core::cache::ArtifactScope::Workspace,
                    path: "docs/report.md".into(),
                }],
                None,
            ))
            .unwrap();
        let v = f.results_json();
        assert_eq!(v["rows"], json!([]));
        assert_eq!(v["legacy"][0]["path"], "docs/report.md");
        assert_eq!(v["legacy"][0]["scope"], "workspace");
        let text = apb()
            .args(["results", RUN])
            .current_dir(&f.root)
            .assert()
            .success();
        let text = stdout(&text);
        assert!(
            text.contains("recorded digests (no stored bytes):"),
            "{text}"
        );
        assert!(
            text.contains("old #1  workspace:docs/report.md  sha256:abababababab"),
            "{text}"
        );
    }

    /// `apb runs <id>` names the snapshots with the command that lists them;
    /// a run without any prints no results line.
    #[test]
    fn run_detail_names_the_snapshots() {
        let mut f = fixture();
        let detail = |f: &Fixture| {
            let out = apb()
                .args(["runs", RUN])
                .current_dir(&f.root)
                .assert()
                .success();
            stdout(&out)
        };
        assert!(!detail(&f).contains("results:"));
        let a = f.start("work", &f.root.clone());
        let s = f.settle("work", 1, Some(a), &[output("abc"), file("r.md", "de")]);
        let text = detail(&f);
        assert!(
            text.contains("  results: 1 snapshot(s); apb results res-1"),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "  result work #1: 2 entries, 5 bytes, {}",
                &s.snapshot_id[..12]
            )),
            "{text}"
        );
    }

    /// `results clean --dry-run` reports a manifest no stamp names and writes
    /// nothing; `results clean` removes it and keeps the stamped snapshot
    /// readable.
    #[cfg(unix)]
    #[test]
    fn clean_removes_only_what_no_stamp_names() {
        let mut f = fixture();
        let a = f.start("work", &f.root.clone());
        let kept = f.settle("work", 1, Some(a), &[output("kept")]);
        // A writer that published and died before journaling its stamp.
        let mut txn = ResultTxn::begin(&f.run).unwrap();
        let Staged::Stored(digest) = txn.stage_object(b"orphan").unwrap() else {
            panic!()
        };
        let orphan = txn
            .publish(&SnapshotManifest {
                v: 1,
                run_id: RUN.into(),
                subject: SnapshotSubject {
                    node: "work".into(),
                    execution: 2,
                    visit_seq: None,
                    item: None,
                    attempt: None,
                },
                provenance: SnapshotProvenance::default(),
                entries: vec![SnapshotEntry {
                    part: EntryPart::Output,
                    scope: None,
                    path: None,
                    size: 6,
                    digest,
                    redacted: false,
                    format: Default::default(),
                    records_invalid: None,
                }],
                omitted: Vec::new(),
                incomplete: None,
                output_omitted: None,
            })
            .unwrap();
        drop(txn);

        let before = tree_of(&f.run);
        let dry = apb()
            .args(["results", "clean", RUN, "--dry-run"])
            .current_dir(&f.root)
            .assert()
            .success();
        assert!(
            stdout(&dry).contains("res-1: would remove 1 snapshot(s) and 1 object(s)"),
            "{}",
            stdout(&dry)
        );
        assert_eq!(tree_of(&f.run), before);

        let done = apb()
            .args(["results", "clean", RUN, "--json"])
            .current_dir(&f.root)
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&done.get_output().stdout).unwrap();
        assert_eq!(v["removed_snapshots"], json!([orphan]));
        assert_eq!(v["dead"][0]["outcome"], "rolled_back");
        assert!(
            !f.run
                .join(format!("results/snapshots/{orphan}.json"))
                .exists()
        );
        assert_eq!(
            stdout(&f.show(&format!("r:{}/output", kept.snapshot_id)).success()),
            "kept"
        );
    }

    #[cfg(unix)]
    fn tree_of(dir: &Path) -> std::collections::BTreeMap<String, (Vec<u8>, u32)> {
        use std::os::unix::fs::PermissionsExt;
        let mut out = std::collections::BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                let meta = fs::symlink_metadata(&p).unwrap();
                let rel = p.strip_prefix(dir).unwrap().display().to_string();
                if meta.is_dir() {
                    stack.push(p);
                    out.insert(rel, (Vec::new(), meta.permissions().mode()));
                } else {
                    out.insert(rel, (fs::read(&p).unwrap(), meta.permissions().mode()));
                }
            }
        }
        out
    }
}
