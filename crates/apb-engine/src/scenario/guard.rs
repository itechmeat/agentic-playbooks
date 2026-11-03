//! Entering a scenario process (`apb __scenario-run`): the core guard set
//! ([`apb_core::process_mode::enter_scenario`]) and, at quick, the decision
//! client's request ban ([`apb_decide::forbid_requests`]), which knows no
//! workspace crate and so keeps its own switch. Entered once, before
//! anything else runs, and never left. The quick ban does not hang on this
//! entry alone: a run's decision runner asks
//! [`apb_core::process_mode::guard_decision_request`] before any provider.
//!
//! From then on every guarded operation consults the mode: a program start
//! (an agent, a script, git, a detection probe, a secret helper, the
//! sandbox probe, a detached driver or waker) and a connector call are
//! refused at every level, a decision request at quick, and a file write
//! through `fsutil` anywhere but below the write root. A boundary site the
//! scenario session missed therefore fails with `scenario_guard:
//! <operation>` instead of acting.

use std::path::Path;

use apb_core::process_mode::ScenarioLevel;

/// Puts this process in scenario mode at `level`, writing only below
/// `write_root` (absolute): at [`ScenarioLevel::Quick`] no decision request
/// is sent either. Refused in a read-only process, for a relative root and
/// when the process already is a scenario process.
pub fn enter(level: ScenarioLevel, write_root: &Path) -> std::io::Result<()> {
    apb_core::process_mode::enter_scenario(level, write_root)?;
    if level == ScenarioLevel::Quick {
        apb_decide::forbid_requests();
    }
    Ok(())
}

/// Every guarded operation of the engine, called directly in a scenario
/// process entered through [`enter`]: each refuses before it acts, and
/// leaves no file and no started program behind.
#[cfg(all(test, unix))]
mod tests {
    use std::path::{Path, PathBuf};

    use apb_core::process_mode::{SCENARIO_REFUSAL, ScenarioLevel};

    /// A stub program `name` in `dir` that leaves `<dir>/<name>.ran` when
    /// it runs.
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
        // SAFETY: a child process runs this one test on one thread.
        unsafe { std::env::set_var(key, value) };
    }

    /// A fresh write root of this child process.
    fn write_root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        (dir, root)
    }

    /// A provider whose requests reach `stub`.
    fn provider(stub: &apb_decide::testing::StubServer) -> apb_decide::SystemOne {
        apb_decide::SystemOne::new(
            "s1",
            stub.base_url.clone(),
            "m",
            None,
            std::time::Duration::from_secs(5),
        )
    }

    fn ask(provider: &apb_decide::SystemOne) -> Result<apb_decide::DecisionResponse, String> {
        use apb_decide::DecisionProvider as _;
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
        provider.decide(&request).map_err(|e| e.to_string())
    }

    fn scenario_refused(text: &str) {
        assert!(
            text.contains(SCENARIO_REFUSAL),
            "not a scenario refusal: {text}"
        );
    }

    #[test]
    fn a_quick_process_asks_no_provider_starts_nothing_and_calls_no_connector() {
        apb_core::process_mode::testing::in_plain_child(
            "scenario::guard::tests::a_quick_process_asks_no_provider_starts_nothing_and_calls_no_connector",
            || {
                let (_keep, root) = write_root();
                let project = tempfile::tempdir().unwrap();
                let bin = tempfile::tempdir().unwrap();
                let cfg = tempfile::tempdir().unwrap();
                let bwrap = stub(bin.path(), "bwrap");
                std::fs::write(
                    cfg.path().join("config.yaml"),
                    format!("isolation:\n  bwrap: {}\n", bwrap.display()),
                )
                .unwrap();
                set_env("APB_CONFIG_DIR", cfg.path());
                let decisions = apb_decide::testing::StubServer::start(vec![]);
                let provider = provider(&decisions);

                super::enter(ScenarioLevel::Quick, &root).unwrap();
                assert!(
                    super::enter(ScenarioLevel::Smoke, &root).is_err(),
                    "entered twice"
                );

                // A decision provider is never asked.
                assert!(ask(&provider).is_err());
                assert_eq!(decisions.count(), 0, "a request reached the provider");

                // A script node and its success check start nothing.
                let version = project.path().join("version");
                std::fs::create_dir(&version).unwrap();
                let marker = bin.path().join("script.ran");
                std::fs::write(
                    version.join("s.sh"),
                    format!("touch '{}'\n", marker.display()),
                )
                .unwrap();
                let e =
                    crate::script::run_script(&version, project.path(), "s.sh", "sh", None, None)
                        .unwrap_err();
                scenario_refused(&e.to_string());
                let e =
                    crate::script::run_check(&version, project.path(), "s.sh", None).unwrap_err();
                scenario_refused(&e.to_string());
                let e = crate::proc::run_capture(
                    std::process::Command::new(stub(bin.path(), "p")),
                    None,
                    None,
                )
                .err()
                .expect("the program was refused");
                scenario_refused(&e.to_string());

                // No detached driver, no waker lease in the project.
                let e = crate::driver::spawn_detached_driver(
                    project.path(),
                    "r1",
                    &crate::driver::DriverArgs::default(),
                )
                .unwrap_err();
                scenario_refused(&e.to_string());
                let run = project.path().join(".apb/runs/r1");
                std::fs::create_dir_all(&run).unwrap();
                let e = crate::waker::Lease::try_take(&run, crate::waker::WakerFor::Notify)
                    .unwrap_err();
                scenario_refused(&e.to_string());
                assert!(!run.join("wakers").exists(), "a lease was created");

                // A connector call and a health check are refused at the entry.
                let (value, ok) = crate::connector::call::play_call(
                    project.path(),
                    "any",
                    None,
                    "ping",
                    &serde_json::json!({}),
                    false,
                    false,
                );
                assert!(!ok);
                assert_eq!(value["error"]["code"], "permission", "{value}");
                scenario_refused(&value.to_string());
                let (value, ok) = crate::connector::call::healthcheck(project.path(), "any", "a");
                assert!(!ok);
                scenario_refused(&value.to_string());

                // The sandbox probe finds the configured bwrap and runs it not.
                let facts = crate::sandbox::probe::host_facts();
                if cfg!(target_os = "linux") {
                    assert!(facts.bwrap.is_some(), "the configured bwrap was not found");
                }
                if let Some(b) = facts.bwrap {
                    assert_eq!(b.path, bwrap);
                    assert_eq!(b.version, None);
                    scenario_refused(&b.trial.unwrap_err());
                }

                for name in ["bwrap", "p", "script"] {
                    assert!(
                        !bin.path().join(format!("{name}.ran")).exists(),
                        "{name} ran"
                    );
                }
                assert_eq!(
                    std::fs::read_dir(cfg.path()).unwrap().count(),
                    1,
                    "a file was written"
                );
            },
        );
    }

    #[test]
    fn a_smoke_process_asks_its_providers_and_still_starts_nothing() {
        apb_core::process_mode::testing::in_plain_child(
            "scenario::guard::tests::a_smoke_process_asks_its_providers_and_still_starts_nothing",
            || {
                let (_keep, root) = write_root();
                let bin = tempfile::tempdir().unwrap();
                let decisions = apb_decide::testing::StubServer::start(vec![]);
                let provider = provider(&decisions);
                super::enter(ScenarioLevel::Smoke, &root).unwrap();
                // The stub answers 500, so the answer is an error, but the
                // request left: smoke spends on its providers.
                let _ = ask(&provider);
                assert!(decisions.count() >= 1, "smoke sent no request");
                let e = crate::proc::run_capture(
                    std::process::Command::new(stub(bin.path(), "p")),
                    None,
                    None,
                )
                .err()
                .expect("the program was refused");
                scenario_refused(&e.to_string());
                assert!(!bin.path().join("p.ran").exists(), "p ran");
            },
        );
    }
}
