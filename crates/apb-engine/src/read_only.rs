//! Entering the read-only mode of an apb process (`apb mcp --read-only`):
//! the core guard set ([`apb_core::process_mode`]) and the decision client's
//! request ban ([`apb_decide::forbid_requests`]), which knows no workspace
//! crate and so keeps its own switch. Entered once, before anything else
//! runs, and never left.

/// Puts this process in read-only mode: nothing is written, no program is
/// started, no decision request is sent.
pub fn enter() {
    apb_core::process_mode::enter_read_only();
    apb_decide::forbid_requests();
}

/// Whether this process is read-only.
pub fn active() -> bool {
    apb_core::process_mode::is_read_only()
}

/// Every guarded operation of the engine, called directly in a read-only
/// child process: each refuses or reads without acting, and leaves no file
/// and no started program behind.
#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use apb_core::process_mode::READ_ONLY_REFUSAL;
    use apb_core::read_only_test;

    /// A stub program `name` in `dir` that leaves `<dir>/<name>.ran` when
    /// it runs.
    #[cfg(unix)]
    fn stub(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(name);
        let marker = dir.join(format!("{name}.ran"));
        std::fs::write(
            &path,
            format!("#!/bin/sh\ntouch '{}'\necho 'x 1.0'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn set_env(key: &str, value: &Path) {
        // SAFETY: a read-only child runs this one test on one thread.
        unsafe { std::env::set_var(key, value) };
    }

    read_only_test! {
        fn the_journal_opens_no_write_handle_and_appends_nothing()
        before log = {
            let dir = tempfile::tempdir().unwrap();
            let run = dir.path().join("r1");
            let log = crate::event::EventLog::create(&run).unwrap();
            (dir, run, log)
        }
        then {
            let (dir, run, mut log) = log;
            super::enter();
            let before = std::fs::read(run.join("events.jsonl")).unwrap();
            let e = log
                .append(crate::event::EventPayload::RunFinished { outcome: "success".into() })
                .unwrap_err();
            assert!(e.to_string().contains(READ_ONLY_REFUSAL), "{e}");
            assert_eq!(std::fs::read(run.join("events.jsonl")).unwrap(), before);
            let fresh = dir.path().join("r2");
            std::fs::create_dir(&fresh).unwrap();
            let e = crate::event::EventLog::open(&fresh).err().unwrap();
            assert!(e.to_string().contains(READ_ONLY_REFUSAL), "{e}");
            assert!(crate::event::EventLog::create(&dir.path().join("r3")).is_err());
            assert!(!fresh.join("events.jsonl").exists(), "the open created the journal");
            assert!(!dir.path().join("r3").exists());
        }
    }

    read_only_test! {
        fn an_output_capture_creates_no_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("stdout.log");
            let mut f = crate::capture::CappedFile::create(&path, 1024);
            f.write(b"echo");
            drop(f);
            assert!(!path.exists());
        }
    }

    #[cfg(unix)]
    read_only_test! {
        fn a_run_directory_is_never_narrowed() {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = tempfile::tempdir().unwrap();
            let f = dir.path().join("run.yaml");
            std::fs::write(&f, "x").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o664)).unwrap();
            crate::run_perms::narrow(dir.path());
            let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o664);
        }
    }

    #[cfg(unix)]
    read_only_test! {
        fn liveness_starts_no_ps_and_reads_unknown() {
            let bin = tempfile::tempdir().unwrap();
            stub(bin.path(), "ps");
            set_env("PATH", bin.path());
            assert!(matches!(
                crate::liveness::process_probe(std::process::id()),
                crate::liveness::Probe::Unknown
            ));
            // A run with a driver claim: the view keeps the journal's status
            // and reports the liveness it did not probe as unknown.
            let runs = tempfile::tempdir().unwrap();
            let run = runs.path().join("r1");
            std::fs::create_dir(&run).unwrap();
            std::fs::write(run.join("driver.pid"), "1").unwrap();
            std::fs::write(run.join("events.jsonl"), "").unwrap();
            let view = crate::run_view::RunView::load(&run, "r1").unwrap();
            assert!(!view.liveness_checked);
            assert_eq!(view.driver_alive, None);
            assert_eq!(view.driver_alive_json(), serde_json::json!("unknown"));
            assert!(!bin.path().join("ps.ran").exists(), "ps ran");
        }
    }

    #[cfg(unix)]
    read_only_test! {
        fn the_sandbox_probe_starts_no_bwrap() {
            let bin = tempfile::tempdir().unwrap();
            let bwrap = stub(bin.path(), "bwrap");
            let cfg = tempfile::tempdir().unwrap();
            std::fs::write(
                cfg.path().join("config.yaml"),
                format!("isolation:\n  bwrap: {}\n", bwrap.display()),
            )
            .unwrap();
            set_env("APB_CONFIG_DIR", cfg.path());
            let facts = crate::sandbox::probe::host_facts();
            // On Linux the configured bwrap is found, so the refusal is
            // what the probe reports, not a missing binary.
            #[cfg(target_os = "linux")]
            assert!(facts.bwrap.is_some(), "the configured bwrap is found");
            if let Some(b) = facts.bwrap {
                assert_eq!(b.version, None);
                assert_eq!(b.trial, Err(READ_ONLY_REFUSAL.to_string()));
            }
            assert!(!bin.path().join("bwrap.ran").exists(), "bwrap ran");
        }
    }

    read_only_test! {
        fn a_decision_chain_built_before_sends_no_request()
        before chain = {
            let stub = apb_decide::testing::StubServer::start(vec![]);
            let provider = apb_decide::SystemOne::new(
                "s1",
                stub.base_url.clone(),
                "m",
                None,
                std::time::Duration::from_secs(5),
            );
            (stub, provider)
        }
        then {
            use apb_decide::DecisionProvider as _;
            let (stub, provider) = chain;
            super::enter();
            let request = apb_decide::DecisionRequest {
                use_site: apb_decide::UseSite::CompletionCheck,
                state_order: Vec::new(),
                state: serde_json::json!("state"),
                questions: [(
                    "q".to_string(),
                    apb_decide::Question::Noul {
                        instructions: serde_json::json!("done?"),
                        criteria: None,
                    },
                )]
                .into_iter()
                .collect(),
            };
            let e = provider.decide(&request).unwrap_err();
            assert!(e.to_string().contains("read-only"), "{e}");
            assert_eq!(stub.count(), 0, "a request reached the provider");
        }
    }

    read_only_test! {
        fn a_standalone_decision_is_skipped_and_logged_nowhere() {
            super::enter();
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join(".apb")).unwrap();
            let cfg = tempfile::tempdir().unwrap();
            std::fs::write(
                cfg.path().join(apb_core::decisions::DECISIONS_FILE),
                "mode: advise\nproviders: [{ id: s, kind: systemone, base_url: 'http://127.0.0.1:1', model: m }]\nuses:\n  catalog_rank: { mode: advise }\n",
            )
            .unwrap();
            let settings = apb_core::decisions::load_file(cfg.path()).unwrap().unwrap();
            let decider =
                crate::decision::standalone::StandaloneDecider::with_settings(settings, root.path());
            let out = decider.decide(
                apb_decide::UseSite::CatalogRank,
                serde_json::json!({ "task": "x" }),
                vec!["task".into()],
                Default::default(),
            );
            assert!(matches!(
                out,
                crate::decision::standalone::StandaloneOutcome::Skipped {
                    reason: crate::decision::standalone::READ_ONLY_SKIP
                }
            ));
            assert_eq!(std::fs::read_dir(root.path().join(".apb")).unwrap().count(), 0);
        }
    }

    read_only_test! {
        fn a_connector_call_is_refused_at_its_entry() {
            let root = tempfile::tempdir().unwrap();
            let (value, ok) = crate::connector::call::play_call(
                root.path(),
                "any",
                None,
                "ping",
                &serde_json::json!({}),
                false,
                false,
            );
            assert!(!ok);
            assert_eq!(value["error"]["code"], "permission", "{value}");
            let (value, ok) = crate::connector::call::healthcheck(root.path(), "any", "a");
            assert!(!ok);
            assert!(value.to_string().contains(READ_ONLY_REFUSAL), "{value}");
        }
    }

    read_only_test! {
        fn a_launch_floor_and_a_handoff_are_never_written() {
            use apb_core::launch::LaunchPolicy;
            use apb_core::launch::floor::FloorKey;
            let dir = tempfile::tempdir().unwrap();
            set_env("APB_CONFIG_DIR", dir.path());
            super::enter();
            let e = apb_core::launch::floor::set_by_person(&FloorKey::global("p"), LaunchPolicy::Deny)
                .unwrap_err();
            assert!(e.to_string().contains(READ_ONLY_REFUSAL), "{e}");
            let grant = crate::gate::launch::LaunchGrant::child(
                "r1",
                &apb_core::launch::LaunchRecord {
                    kind: "start".into(),
                    initiator: "dashboard".into(),
                    confirmation: None,
                    policy: LaunchPolicy::Allow,
                    sources: Vec::new(),
                },
            );
            let e = crate::launch_handoff::write(dir.path(), &crate::launch_handoff::Handoff::Resume(grant))
                .unwrap_err();
            assert!(e.to_string().contains(READ_ONLY_REFUSAL), "{e}");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing was written");
            // Reading the floors writes nothing either.
            assert!(apb_core::launch::floor::read().unwrap().is_empty());
        }
    }
}
