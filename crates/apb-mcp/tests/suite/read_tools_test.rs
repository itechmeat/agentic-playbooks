use apb_mcp::tools::{DetailMode, playbook_get, playbook_list, playbook_validate};
use std::fs;
use std::path::Path;

const VALID: &str = include_str!("../../../apb-core/tests/fixtures/valid.yaml");

fn seed(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), VALID).unwrap();
    fs::write(root.join(".apb/playbooks/implement-task/current"), "1.0.0").unwrap();
    fs::create_dir_all(root.join(".apb/profiles/architect")).unwrap();
}

#[test]
fn list_returns_playbook() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = playbook_list(dir.path()).unwrap();
    assert_eq!(v[0]["id"], "implement-task");
    assert_eq!(v[0]["current"], "1.0.0");
}

#[test]
fn get_returns_yaml_and_model() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = playbook_get(dir.path(), "implement-task", None, DetailMode::Full).unwrap();
    assert_eq!(v["version"], "1.0.0");
    assert_eq!(v["playbook"]["nodes"][0]["type"], "start");
    assert!(v["yaml"].as_str().unwrap().contains("implement-task"));
}

/// Completes [`seed`] with what a run of the fixture needs on disk: a
/// loadable `architect` profile and the `lint` node's script.
fn seed_runnable(root: &Path) {
    seed(root);
    let profile = root.join(".apb/profiles/architect");
    fs::write(
        profile.join("profile.yaml"),
        "name: architect\ndescription: d\nexecutor:\n  agent: claude\n  model: claude-opus-5-5\n",
    )
    .unwrap();
    fs::write(profile.join("SOUL.md"), "role").unwrap();
    let scripts = root.join(".apb/playbooks/implement-task/1.0.0/scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(scripts.join("node-lint.sh"), "true\n").unwrap();
}

#[test]
fn validate_reports_ok() {
    let dir = tempfile::tempdir().unwrap();
    seed_runnable(dir.path());
    let v = playbook_validate(dir.path(), "implement-task").unwrap();
    assert_eq!(v["valid"], true);
    assert!(v["issues"].as_array().unwrap().is_empty(), "{v}");
}

#[test]
fn validate_reports_a_missing_profile_and_script() {
    // The fixture's `architect` directory holds no profile, and its script
    // was never added: a run would fail at start and on `lint`.
    let _cfg = crate::common::config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = playbook_validate(dir.path(), "implement-task").unwrap();
    assert_eq!(v["valid"], false, "{v}");
    let found: Vec<(&str, &str)> = v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["code"].as_str().unwrap(),
                i["node"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(found, vec![("V91", "plan"), ("V92", "lint")], "{v}");
    // The empty project `architect` directory fails to load: only the
    // project can fix it, so V91 is an error here.
    assert_eq!(v["issues"][0]["severity"], "error", "{v}");
}

#[test]
fn validate_stays_valid_when_only_the_global_store_could_provide_a_profile() {
    // No project profile and none in this machine's global store: the global
    // store was the last place looked, so V91 is a warning (CI or another
    // machine may have the profile there) and the playbook stays valid.
    let _cfg = crate::common::config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    seed_runnable(dir.path());
    fs::remove_dir_all(dir.path().join(".apb/profiles/architect")).unwrap();
    let v = playbook_validate(dir.path(), "implement-task").unwrap();
    assert_eq!(v["valid"], true, "{v}");
    let issues = v["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1, "{v}");
    assert_eq!(issues[0]["code"], "V91", "{v}");
    assert_eq!(issues[0]["severity"], "warning", "{v}");
}

#[test]
fn get_summary_includes_goal_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    let yaml = format!(
        "goal:\n  statement: the task is implemented and verified\n  criteria:\n    - description: tests pass\n      check: {{ type: marker, marker: DONE }}\n{VALID}"
    );
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/implement-task/current"), "1.0.0").unwrap();
    fs::create_dir_all(root.join(".apb/profiles/architect")).unwrap();

    let v = playbook_get(root, "implement-task", None, DetailMode::Summary).unwrap();
    assert_eq!(
        v["goal"]["statement"].as_str(),
        Some("the task is implemented and verified")
    );
    let criteria = v["goal"]["criteria"].as_array().unwrap();
    assert_eq!(criteria[0]["description"].as_str(), Some("tests pass"));
    assert_eq!(criteria[0]["check"]["type"].as_str(), Some("marker"));
    assert_eq!(criteria[0]["check"]["marker"].as_str(), Some("DONE"));
}

#[test]
fn get_summary_omits_goal_key_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = playbook_get(dir.path(), "implement-task", None, DetailMode::Summary).unwrap();
    assert!(
        v.get("goal").is_none(),
        "goal-less playbook must omit the goal key from the summary: {v}"
    );
}

#[test]
fn get_unknown_is_error() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let err = playbook_get(dir.path(), "ghost", None, DetailMode::Full).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

/// The result snapshot tools (`run_results`, `run_result_read`) over
/// snapshots written through the store's transaction and stamped in a
/// hand-built journal.
mod results {
    use std::fs;
    use std::path::PathBuf;

    use apb_core::result_store::{
        EntryPart, ResultScope, ResultStamp, ResultTxn, SnapshotEntry, SnapshotManifest,
        SnapshotProvenance, SnapshotSubject, Staged,
    };
    use apb_engine::event::{EventLog, EventPayload};
    use apb_mcp::tools::ToolError;
    use apb_mcp::tools::results::{run_result_read, run_results};
    use serde_json::{Value, json};

    const RUN: &str = "res-1";

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        run: PathBuf,
        tree: PathBuf,
        /// Where the producing attempt says it ran (its own `workdir`).
        attempt_dir: PathBuf,
        log: EventLog,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        apb_core::registry::init_project(&root).unwrap();
        let run = root.join(".apb/runs").join(RUN);
        let tree = root.join("tree");
        fs::create_dir_all(&run).unwrap();
        fs::create_dir_all(&tree).unwrap();
        fs::write(run.join("run.yaml"), "params: {}\n").unwrap();
        let mut log = EventLog::open(&run).unwrap();
        log.append(EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
            definition: None,
            external_data: false,
        })
        .unwrap();
        // The run's working tree: capture reads `scope: workspace` there.
        log.append(EventPayload::WorktreeResolved {
            path: tree.display().to_string(),
            source: "test".into(),
            node: None,
        })
        .unwrap();
        Fixture {
            _dir: dir,
            root,
            run,
            attempt_dir: tree.clone(),
            tree,
            log,
        }
    }

    impl Fixture {
        /// One execution of `work` in the tree, settled with a snapshot of
        /// `(part, workspace path, text, redacted)`; the snapshot id.
        fn settle(&mut self, parts: &[(EntryPart, Option<&str>, &str, bool)]) -> String {
            let attempt = self
                .log
                .append(
                    serde_json::from_value(json!({
                        "type": "attempt_started", "node": "work", "attempt": 1,
                        "agent": "claude", "workdir": self.attempt_dir.display().to_string(),
                    }))
                    .unwrap(),
                )
                .unwrap()
                .seq;
            let mut txn = ResultTxn::begin(&self.run).unwrap();
            let mut entries = Vec::new();
            for (part, path, text, redacted) in parts {
                let Staged::Stored(digest) = txn.stage_object(text.as_bytes()).unwrap() else {
                    panic!()
                };
                entries.push(SnapshotEntry {
                    part: *part,
                    scope: path.map(|_| ResultScope::Workspace),
                    path: path.map(str::to_string),
                    size: text.len() as u64,
                    digest,
                    redacted: *redacted,
                    format: Default::default(),
                    records_invalid: None,
                });
            }
            let id = txn
                .publish(&SnapshotManifest {
                    v: 1,
                    run_id: RUN.into(),
                    subject: SnapshotSubject {
                        node: "work".into(),
                        execution: 1,
                        visit_seq: None,
                        item: None,
                        attempt: Some(attempt),
                    },
                    provenance: SnapshotProvenance::default(),
                    entries,
                    omitted: Vec::new(),
                    incomplete: None,
                    output_omitted: None,
                })
                .unwrap();
            self.log
                .append(EventPayload::node_settled(
                    "work",
                    "succeeded",
                    1,
                    "",
                    Vec::new(),
                    Some(ResultStamp {
                        snapshot_id: id.clone(),
                        entries: parts.len() as u32,
                        bytes: 0,
                        omitted: 0,
                        incomplete: false,
                    }),
                ))
                .unwrap();
            txn.end().unwrap();
            id
        }

        fn read(&self, reference: &str) -> Result<Value, ToolError> {
            run_result_read(&self.root, RUN, reference, None, None, false)
        }

        fn live(&self, reference: &str) -> Value {
            run_result_read(&self.root, RUN, reference, None, None, true).unwrap()["live"].clone()
        }
    }

    #[test]
    fn a_read_answers_the_stored_bytes_with_their_digest() {
        let mut f = fixture();
        let text = "caf\u{e9} report\n";
        let id = f.settle(&[(EntryPart::File, Some("r.md"), text, false)]);
        let listed = run_results(&f.root, RUN, None).unwrap();
        let digest = listed["rows"][0]["entries"][0]["digest"].clone();
        let v = f.read(&format!("r:{id}/file:workspace:r.md")).unwrap();
        assert_eq!(v["content"]["text"], text);
        assert_eq!(v["content"]["digest"], digest);
        assert_eq!(
            v["content"]["digest"],
            apb_core::content::sha256_hex(text.as_bytes())
        );
        assert_eq!(v["content"]["range"], json!([0, text.len()]));
        assert_eq!(v["content"]["redacted_on_read"], false);
        assert_eq!(v["ref"], format!("r:{id}/file:workspace:r.md"));
    }

    #[test]
    fn a_range_must_sit_on_character_boundaries() {
        let mut f = fixture();
        let id = f.settle(&[(EntryPart::Output, None, "a\u{e9}b", false)]);
        let ok = f.read(&format!("r:{id}/output#[1,3)")).unwrap();
        assert_eq!(ok["content"]["text"], "\u{e9}");
        assert_eq!(ok["content"]["range"], json!([1, 3]));
        for bad in ["#[2,3)", "#[0,9)"] {
            assert!(
                matches!(
                    f.read(&format!("r:{id}/output{bad}")),
                    Err(ToolError::Engine(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_window_stops_at_the_display_cap_on_a_character_boundary() {
        let mut f = fixture();
        // 3-byte characters: the cap (65536) falls inside one.
        let text = "\u{20ac}".repeat(30_000);
        let id = f.settle(&[(EntryPart::Output, None, &text, false)]);
        let v = f.read(&format!("r:{id}/output")).unwrap();
        let shown = v["content"]["text"].as_str().unwrap();
        assert_eq!(shown.len(), 65_535);
        assert_eq!(v["content"]["size"], text.len());
        assert_eq!(v["content"]["range"], json!([0, 65_535]));
        let next = run_result_read(
            &f.root,
            RUN,
            &format!("r:{id}/output"),
            Some(65_535),
            Some(6),
            false,
        )
        .unwrap();
        assert_eq!(next["content"]["text"], "\u{20ac}\u{20ac}");
        assert!(
            run_result_read(
                &f.root,
                RUN,
                &format!("r:{id}/output"),
                Some(1),
                None,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn the_live_comparison_answers_a_word_never_content() {
        let mut f = fixture();
        fs::write(f.tree.join("r.md"), "v1").unwrap();
        let id = f.settle(&[
            (EntryPart::Output, None, "out", false),
            (EntryPart::File, Some("r.md"), "v1", false),
            (EntryPart::File, Some("s.md"), "[redacted]", true),
        ]);
        let file = format!("r:{id}/file:workspace:r.md");
        assert_eq!(f.live(&file), "same");
        fs::write(f.tree.join("r.md"), "v2").unwrap();
        assert_eq!(f.live(&file), "differs");
        let v = run_result_read(&f.root, RUN, &file, None, None, true).unwrap();
        assert!(!v.to_string().contains("v2"), "{v}");
        fs::remove_file(f.tree.join("r.md")).unwrap();
        assert_eq!(f.live(&file), "missing");
        #[cfg(unix)]
        {
            fs::write(f.root.join("elsewhere.md"), "v1").unwrap();
            std::os::unix::fs::symlink(f.root.join("elsewhere.md"), f.tree.join("r.md")).unwrap();
            assert_eq!(f.live(&file), "unreadable");
        }
        assert_eq!(f.live(&format!("r:{id}/output")), "not_comparable");
        assert_eq!(
            f.live(&format!("r:{id}/file:workspace:s.md")),
            "not_comparable"
        );
    }

    /// Capture reads a workspace file from the run's working tree, not
    /// from the directory a node's own `workdir` gave its attempt; the
    /// live comparison reads the same tree, so it compares like with like.
    #[test]
    fn the_live_comparison_reads_the_tree_capture_read() {
        let mut f = fixture();
        fs::write(f.tree.join("r.md"), "v1").unwrap();
        let own = f.root.join("node-dir");
        fs::create_dir_all(&own).unwrap();
        fs::write(own.join("r.md"), "something else").unwrap();
        f.attempt_dir = own;
        let id = f.settle(&[(EntryPart::File, Some("r.md"), "v1", false)]);
        assert_eq!(f.live(&format!("r:{id}/file:workspace:r.md")), "same");
    }

    #[test]
    fn a_tampered_object_is_a_conflict_that_says_corrupt() {
        let mut f = fixture();
        let id = f.settle(&[(EntryPart::Output, None, "stored", false)]);
        let hex = apb_core::content::sha256_hex(b"stored")
            .trim_start_matches("sha256:")
            .to_string();
        fs::write(
            f.run.join("results/objects").join(&hex[..2]).join(&hex),
            "edited",
        )
        .unwrap();
        match f.read(&format!("r:{id}/output")) {
            Err(ToolError::Conflict(m)) => assert!(m.starts_with("corrupt"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            run_results(&f.root, RUN, None).unwrap()["rows"][0]["verified"],
            "corrupt"
        );
    }

    /// Makes `secret` a secret param of the run after capture.
    fn known_now(f: &Fixture, secret: &str) {
        fs::write(
            f.run.join("run.yaml"),
            format!("params:\n  token: \"{secret}\"\n"),
        )
        .unwrap();
        fs::write(
            f.run.join("playbook.yaml"),
            "params:\n  - { name: token, type: text, secret: true }\n",
        )
        .unwrap();
    }

    /// The object file of `text`.
    fn object_file(f: &Fixture, text: &str) -> PathBuf {
        let hex = apb_core::content::sha256_hex(text.as_bytes())
            .trim_start_matches("sha256:")
            .to_string();
        f.run.join("results/objects").join(&hex[..2]).join(&hex)
    }

    /// A secret value known only now, with characters an entry id
    /// percent-encodes, is in no spelling anywhere in the listing or the
    /// read: the path reads `<redacted path 1>` and the id is built from
    /// that, the digest and size are the served text's, and the live
    /// comparison does not confirm the value.
    #[test]
    fn a_secret_known_after_capture_is_replaced_everywhere_on_read() {
        let mut f = fixture();
        let secret = "sw0rd+f1sh!42";
        let text = format!("key {secret}");
        let path = format!("notes/{secret}.md");
        fs::create_dir_all(f.tree.join("notes")).unwrap();
        fs::write(f.tree.join(&path), &text).unwrap();
        let id = f.settle(&[(EntryPart::File, Some(&path), &text, false)]);
        let stored_id = apb_core::result_store::EntryId::File {
            scope: ResultScope::Workspace,
            path: path.clone(),
        }
        .to_string();
        assert!(stored_id.contains("sw0rd%2Bf1sh%2142"), "{stored_id}");
        // Before the value is known, the file compares as the same.
        assert_eq!(f.live(&format!("r:{id}/{stored_id}")), "same");
        known_now(&f, secret);
        let listed = run_results(&f.root, RUN, None).unwrap();
        let all = listed.to_string();
        for spelling in [secret, "sw0rd%2Bf1sh%2142", "f1sh"] {
            assert!(!all.contains(spelling), "{spelling}: {all}");
        }
        let e = &listed["rows"][0]["entries"][0];
        assert_eq!(e["path"], "<redacted path 1>");
        assert_eq!(e["entry"], "file:workspace:%3Credacted%20path%201%3E");
        assert_eq!(e["redacted_on_read"], true);
        let read = f
            .read(&format!("r:{id}/{}", e["entry"].as_str().unwrap()))
            .unwrap();
        assert!(!read.to_string().contains(secret), "{read}");
        let served = read["content"]["text"].as_str().unwrap().to_string();
        assert_eq!(served, "key [redacted]");
        assert_eq!(read["content"]["redacted_on_read"], true);
        let served_digest = apb_core::content::sha256_hex(served.as_bytes());
        assert_eq!(read["content"]["digest"], served_digest);
        assert_eq!(read["content"]["size"], served.len());
        assert_eq!(e["digest"], served_digest);
        assert_eq!(e["size"], served.len());
        // The stored id no longer names it.
        assert!(matches!(
            f.read(&format!("r:{id}/{stored_id}")),
            Err(ToolError::NotFound(_))
        ));
        assert_eq!(
            f.live(&format!("r:{id}/{}", e["entry"].as_str().unwrap())),
            "not_comparable"
        );
    }

    /// A text the redactor changes on read only in its contents keeps its
    /// path, and is not compared with the live file: a `same` would
    /// confirm a guess of the value.
    #[test]
    fn a_text_redacted_on_read_is_not_compared_with_the_live_file() {
        let mut f = fixture();
        let secret = "4829173";
        let text = format!("pin: {secret}\n");
        fs::write(f.tree.join("pin.md"), &text).unwrap();
        let id = f.settle(&[(EntryPart::File, Some("pin.md"), &text, false)]);
        known_now(&f, secret);
        let listed = run_results(&f.root, RUN, None).unwrap();
        let e = &listed["rows"][0]["entries"][0];
        assert_eq!(e["entry"], "file:workspace:pin.md");
        assert_ne!(e["digest"], apb_core::content::sha256_hex(text.as_bytes()));
        assert_eq!(
            f.live(&format!("r:{id}/file:workspace:pin.md")),
            "not_comparable"
        );
    }

    /// The live comparison re-hashes the stored object first, like every
    /// other read: a tampered object is corrupt, never `same`.
    #[test]
    fn a_tampered_object_is_corrupt_for_the_live_comparison_too() {
        let mut f = fixture();
        fs::write(f.tree.join("r.md"), "v1").unwrap();
        let id = f.settle(&[(EntryPart::File, Some("r.md"), "v1", false)]);
        fs::write(object_file(&f, "v1"), "v2").unwrap();
        match run_result_read(
            &f.root,
            RUN,
            &format!("r:{id}/file:workspace:r.md"),
            None,
            None,
            true,
        ) {
            Err(ToolError::Conflict(m)) => assert!(m.starts_with("corrupt"), "{m}"),
            other => panic!("{other:?}"),
        }
        fs::remove_file(object_file(&f, "v1")).unwrap();
        match apb_mcp::tools::results::run_result_live(
            &f.root,
            RUN,
            &id,
            &"file:workspace:r.md".parse().unwrap(),
        ) {
            Err(ToolError::Conflict(m)) => assert!(m.starts_with("corrupt"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    /// An entry whose object is missing or no longer matches its digest
    /// serves no digest and no size: the stored ones describe text nobody
    /// can verify, and with a secret known only now they would let a
    /// reader confirm a guess of the value offline.
    #[test]
    fn an_unverifiable_entry_serves_no_digest_and_no_size() {
        let mut f = fixture();
        let secret = "482913";
        let text = format!("pin: {secret}\n");
        let id = f.settle(&[(EntryPart::Output, None, &text, false)]);
        known_now(&f, secret);
        for damage in ["edited", "deleted"] {
            if damage == "edited" {
                fs::write(object_file(&f, &text), "pin: 000000\n").unwrap();
            } else {
                fs::remove_file(object_file(&f, &text)).unwrap();
            }
            let listed = run_results(&f.root, RUN, None).unwrap();
            let row = &listed["rows"][0];
            assert_eq!(row["verified"], "corrupt", "{damage}");
            let e = &row["entries"][0];
            assert_eq!(e["entry"], "output", "{damage}");
            assert!(e.get("digest").is_none(), "{damage}: {e}");
            assert!(e.get("size").is_none(), "{damage}: {e}");
            let all = listed.to_string();
            assert!(
                !all.contains(&apb_core::content::sha256_hex(text.as_bytes())),
                "{damage}: {all}"
            );
        }
        let _ = id;
    }

    /// A text that grows past the display cap when a secret known only now
    /// is replaced pages to its served end: the size is the served text's,
    /// so a reader sees that more follows.
    #[test]
    fn a_text_that_grows_on_read_pages_to_its_served_end() {
        let mut f = fixture();
        let cap = apb_engine::result_view::DISPLAY_CAP as usize;
        let text = format!("{}s3cr3t", "a".repeat(cap - 7));
        let id = f.settle(&[(EntryPart::Output, None, &text, false)]);
        known_now(&f, "s3cr3t");
        let reference = format!("r:{id}/output");
        let first = run_result_read(&f.root, RUN, &reference, Some(0), None, false).unwrap();
        let size = first["content"]["size"].as_u64().unwrap();
        let end = first["content"]["range"][1].as_u64().unwrap();
        assert_eq!(size as usize, cap - 7 + "[redacted]".len());
        assert!(end < size, "more follows: {end} of {size}");
        let rest = run_result_read(&f.root, RUN, &reference, Some(end), None, false).unwrap();
        assert_eq!(rest["content"]["range"][1].as_u64().unwrap(), size);
        let whole = format!(
            "{}{}",
            first["content"]["text"].as_str().unwrap(),
            rest["content"]["text"].as_str().unwrap()
        );
        assert_eq!(whole.len() as u64, size);
        assert!(whole.ends_with("a[redacted]"));
    }
}
