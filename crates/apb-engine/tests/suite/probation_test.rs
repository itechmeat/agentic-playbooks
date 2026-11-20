//! Probation and `--require-ready`: an unattended start is admitted once
//! the playbook's project ledger holds enough eligible attended successes
//! of the exact definition the start loads, for the playbook and every
//! pinned sub-playbook; a waiver is a typed confirmation whose flag
//! reaches every child; a patch of an unattended run is held to the same
//! checks. The ledger is written by the journal's terminal observer only.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use apb_core::versioning::create_patch_version;
use apb_engine::control::{Control, post_control};
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, RunResult, run};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar};

const DEADLINE: Duration = Duration::from_secs(60);

/// A script-only playbook with no `unattended:` block.
const PLAIN: &str = r#"schema: 2
id: pb
name: pb
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: scripts/w.sh, runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// A script-only playbook under probation, one script that fails unless
/// its plan file says otherwise.
const SCRIPTED: &str = r#"schema: 2
id: pb
name: pb
version: 1.0.0
unattended:
  after_attended_successes: 2
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: scripts/w.sh, runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

const W_SH: &str = r#"if [ -f "plan-ok" ]; then echo ok; else echo 'no plan' 1>&2; exit 1; fi
"#;
const C_SH: &str = "exit 0\n";

/// A project with the playbook `pb` (`yaml`), profile `main`, the env lock
/// and its own config dir.
struct Proj {
    dir: tempfile::TempDir,
    _cfg: tempfile::TempDir,
    _env: Vec<EnvVar>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Proj {
    fn plain(yaml: &str) -> Self {
        Self::with_scripts(yaml, &[("w.sh", W_SH), ("c.sh", C_SH)])
    }

    fn with_scripts(yaml: &str, scripts: &[(&str, &str)]) -> Self {
        let lock = common::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        seed(dir.path(), yaml, scripts);
        let env = vec![EnvVar::set("APB_CONFIG_DIR", cfg.path())];
        Proj {
            dir,
            _cfg: cfg,
            _env: env,
            _lock: lock,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn run(&self, opts: RunOptions) -> Result<RunResult, apb_engine::EngineError> {
        let (root, id) = (self.root(), "pb".to_string());
        bounded(move || run(&root, &id, None, opts))
    }

    /// Runs `pb` attended (a person's plain start), to its terminal state.
    fn run_version(
        &self,
        version: Option<&str>,
        opts: RunOptions,
    ) -> Result<RunResult, apb_engine::EngineError> {
        let (root, id) = (self.root(), "pb".to_string());
        let version = version.map(str::to_string);
        bounded(move || run(&root, &id, version.as_deref(), opts))
    }

    fn attended(&self, opts: RunOptions) -> RunResult {
        self.run(opts).expect("the attended run")
    }

    /// One `plan-ok` file in the project tree, so the script's run succeeds.
    fn plan_ok(&self) -> PathBuf {
        let p = self.root().join("plan-ok");
        fs::write(&p, "").unwrap();
        p
    }

    /// The project's probation ledger for `pb`, if any.
    fn ledger(&self, scope: &str) -> Option<apb_core::probation::ProbationFile> {
        let path = self
            .root()
            .join(".apb/probation")
            .join(scope)
            .join("pb.json");
        let raw = fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn entries(&self, scope: &str) -> Vec<apb_core::probation::ProbationEntry> {
        self.ledger(scope).map(|f| f.entries).unwrap_or_default()
    }

    /// Whether the entry of `run` is in the project ledger.
    fn has_entry(&self, run: &str, outcome: &str, eligible: bool) -> bool {
        self.entries("project")
            .iter()
            .any(|e| e.run == run && e.outcome == outcome && e.eligible == eligible)
    }
}

/// Runs `drive` on a helper thread with a deadline.
fn bounded<T: Send + 'static>(drive: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(drive());
    });
    match rx.recv_timeout(DEADLINE) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the drive thread panicked"),
        Err(e) => panic!("the drive did not end within {DEADLINE:?} ({e})"),
    }
}

fn seed(root: &Path, yaml: &str, scripts: &[(&str, &str)]) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/pb/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
    fs::write(root.join(".apb/playbooks/pb/current"), "1.0.0").unwrap();
    common::seed_main(root);
}

fn unattended_opts() -> RunOptions {
    RunOptions {
        unattended: true,
        ..RunOptions::default()
    }
}

/// Options of an unattended start of `pb` as a launch surface makes it:
/// the run gate pins every sub-playbook (`expected_children`).
fn gated_unattended(root: &Path) -> RunOptions {
    let permit = apb_engine::gate::check_run(
        root,
        &apb_core::scope::PlaybookRef {
            origin: apb_core::scope::Origin::Project { workspace_id: None },
            id: "pb".into(),
            version: None,
        },
        true,
        false,
        &apb_engine::gate::launch::LaunchContext::new(
            apb_engine::gate::launch::Initiator::cli(true),
            apb_engine::gate::launch::LaunchKind::Start,
        ),
    )
    .expect("the gate admits the start");
    let mut opts = unattended_opts();
    permit.apply(&mut opts);
    opts
}

fn run_ids(root: &Path) -> Vec<String> {
    fs::read_dir(root.join(".apb/runs"))
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The `patch_rejected` reasons of the journal.
fn patch_reasons(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::PatchRejected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

/// An attended success of the scripted playbook: the run succeeds and the
/// terminal observer writes one eligible entry.
fn one_attended_success(p: &Proj) -> String {
    p.plan_ok();
    let res = p.attended(RunOptions::default());
    assert_eq!(res.outcome, RunStatus::Succeeded, "{}", res.run_id);
    res.run_id
}

/// Two attended successes: the unattended start is admitted and succeeds.
#[test]
fn probation_done_admits_the_start() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    one_attended_success(&p);
    assert_eq!(p.entries("project").len(), 2);
    let res = p.run(unattended_opts()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// A new version restarts probation: two attended successes of 1.0.0, then
/// 1.1.0 with `after_attended_successes: 2` counts 0.
#[test]
fn a_new_version_restarts_probation() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    one_attended_success(&p);
    // The second version, same id.
    let v2 = SCRIPTED.replace("version: 1.0.0", "version: 1.1.0");
    let vdir = p.root().join(".apb/playbooks/pb/1.1.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), &v2).unwrap();
    fs::write(vdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(vdir.join("scripts/c.sh"), C_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "1.1.0").unwrap();
    let err = p.run_version(Some("1.1.0"), unattended_opts()).unwrap_err();
    assert!(err.to_string().contains("probation_required"), "{err}");
    // The two attended runs' directories are all there is: the refused
    // start created none.
    assert_eq!(run_ids(&p.root()).len(), 2, "no new run directory");
}

/// The `playbook.yaml` of 1.0.0 edited in place: its earlier entries no
/// longer count (the definition digest differs).
#[test]
fn an_edited_definition_does_not_count() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    one_attended_success(&p);
    // Edit the loaded definition's bytes in place.
    let path = p.root().join(".apb/playbooks/pb/1.0.0/playbook.yaml");
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("name: pb", "name: pb2");
    fs::write(&path, edited).unwrap();
    let err = p.run(unattended_opts()).unwrap_err();
    assert!(err.to_string().contains("probation_required"), "{err}");
}

/// A changed script resets the count: the trust digest differs.
#[test]
fn a_changed_script_resets_the_count() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    one_attended_success(&p);
    // Change and re-approve a script of the version.
    let scripts = p.root().join(".apb/playbooks/pb/1.0.0/scripts");
    fs::write(scripts.join("c.sh"), "exit 1\n").unwrap();
    let trust = apb_core::scope::definition_digest(
        &fs::read_to_string(p.root().join(".apb/playbooks/pb/1.0.0/playbook.yaml")).unwrap(),
        &p.root().join(".apb/playbooks/pb/1.0.0"),
    )
    .unwrap();
    apb_core::versioning::approval::approve_version(
        &p.root().join(".apb"),
        "pb",
        Some("1.0.0"),
        Some(&trust),
        apb_core::trust::OriginKind::LocallyApproved,
    )
    .unwrap();
    let err = p.run(unattended_opts()).unwrap_err();
    assert!(err.to_string().contains("probation_required"), "{err}");
}

/// An unattended success and a failed run are written with their outcome
/// and count 0; an external-data success, an eval run and a scenario run
/// write no entry at all.
#[test]
fn only_attended_successes_count() {
    let p = Proj::plain(SCRIPTED);
    // An unattended success (the machine file not asked: this playbook
    // asks for probation, so start it attended-like by waiving nothing;
    // an unattended run of an unproven playbook is refused, so this one
    // is written by the attended failure path).
    let failed = p.run(RunOptions::default()).unwrap();
    assert_eq!(failed.outcome, RunStatus::Failed);
    assert!(p.has_entry(&failed.run_id, "failed", false));
    // An attended success.
    let ok = one_attended_success(&p);
    assert!(p.has_entry(&ok, "succeeded", true));
    // The count against the loaded definition is one, so the unattended
    // start is still refused.
    let err = p.run(unattended_opts()).unwrap_err();
    assert!(
        err.to_string().contains("probation_required") && err.to_string().contains("1 so far here"),
        "{err}"
    );
    // An external-data success writes no entry at all.
    p.plan_ok();
    let ext = p
        .run(RunOptions {
            external_data: true,
            ..RunOptions::default()
        })
        .unwrap();
    assert_eq!(ext.outcome, RunStatus::Succeeded);
    assert!(
        !p.entries("project").iter().any(|e| e.run == ext.run_id),
        "an external-data run writes no entry"
    );
    // An unattended success (waived) is written unattended and counts 0.
    let alone = p
        .run(RunOptions {
            unattended: true,
            waive_probation: true,
            ..RunOptions::default()
        })
        .unwrap();
    assert_eq!(alone.outcome, RunStatus::Succeeded);
    assert!(
        p.entries("project")
            .iter()
            .any(|e| e.run == alone.run_id && e.unattended),
        "the unattended run is written unattended"
    );
    // An eval run writes no entry.
    let eval = p
        .run(RunOptions {
            eval: Some(apb_engine::run_config::EvalRunSettings::default()),
            ..RunOptions::default()
        })
        .unwrap();
    assert!(
        !p.entries("project").iter().any(|e| e.run == eval.run_id),
        "an eval run writes no entry"
    );
    // The count against the loaded definition is still one.
    let err = p.run(unattended_opts()).unwrap_err();
    assert!(err.to_string().contains("1 so far here"), "{err}");
}

/// An attended run with a patch that succeeds is written `eligible: false`
/// and counts 0.
#[test]
fn a_patched_run_is_not_eligible() {
    let p = Proj::plain(SCRIPTED);
    // The patch version, made by the run.
    let patched = SCRIPTED.replace("name: pb\n", "name: patched\n");
    p.plan_ok();
    let res = p.attended(RunOptions::default());
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_id = res.run_id.clone();
    let version =
        create_patch_version(&p.root(), "pb", "1.0.0", &patched, &run_id, "workaround").unwrap();
    // A second attended run that applies the patch and succeeds.
    p.plan_ok();
    let (root, id) = (p.root(), "pb".to_string());
    let prepared = apb_engine::scheduler::prepare_supervised_background(
        &root,
        &id,
        None,
        RunOptions::default(),
    )
    .unwrap();
    let rid = prepared.run_id().to_string();
    post_control(
        &root.join(".apb/runs").join(&rid),
        Control::Patch {
            version: version.clone(),
            classification: "workaround".into(),
            continue_from: "start".into(),
        },
    )
    .unwrap();
    bounded(move || {
        let _ = apb_engine::scheduler::drive_prepared(&root, prepared);
    });
    let events = read_all(&p.root().join(".apb/runs").join(&rid)).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::PatchApplied { .. })),
        "the patch applied"
    );
    assert!(p.has_entry(&rid, "succeeded", false), "written ineligible");
    // The count against the loaded definition stays what it was.
    let done = p.entries("project").iter().filter(|e| e.eligible).count();
    assert_eq!(done, 1, "the patched run counts nothing");
    drop(version);
}

/// An attended start with run overrides that succeeds is written
/// `eligible: false`, and an entry forged eligible with its definition
/// still counts 0 against the loaded `playbook.yaml`.
#[test]
fn an_overridden_run_is_not_eligible() {
    // The override targets an agent_task node: the playbook carries one,
    // answered by the stub agent.
    let agent_pb = r#"schema: 2
id: pb
name: pb
version: 1.0.0
unattended:
  after_attended_successes: 2
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do the work" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;
    let p = Proj::plain(agent_pb);
    let prog = crate::deferred_retry_test::agent(&p.root(), "0");
    let _cmd = EnvVar::set("APB_AGENT_CMD", &prog);
    let res = p
        .run(RunOptions {
            overrides: Some(apb_core::overrides::RunOverrides {
                nodes: [(
                    "work".to_string(),
                    apb_core::overrides::NodeOverride {
                        profile: Some(apb_core::profile::QualifiedProfileRef {
                            name: "main".into(),
                            scope: apb_core::profile::ProfileScope::Project,
                        }),
                        ..Default::default()
                    },
                )]
                .into(),
            }),
            ..RunOptions::default()
        })
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert!(p.has_entry(&res.run_id, "succeeded", false));
    // The forged-eligible half: an entry claiming eligibility whose
    // definition is not the loaded playbook's counts 0.
    let mut file = p.ledger("project").unwrap();
    for e in &mut file.entries {
        e.eligible = true;
    }
    file.mac = None;
    let ledger_path = p.root().join(".apb/probation/project/pb.json");
    fs::write(&ledger_path, serde_json::to_string(&file).unwrap()).unwrap();
    let loaded = apb_core::registry::Registry::open_dir(&p.root().join(".apb"))
        .unwrap()
        .load("pb", None)
        .unwrap();
    let view = apb_engine::gate::unattended::probation(&p.root(), "project", &loaded)
        .unwrap()
        .unwrap();
    assert_eq!(view.counted, 0, "a forged eligible entry counts nothing");
}

/// Two attended successes, `after_attended_successes: 2`; the older run is
/// pruned; the count is 2 before and after the prune and the next
/// unattended start passes; a second terminal event for the same run
/// leaves its single entry unchanged.
#[test]
fn one_run_counts_once_across_a_prune() {
    let p = Proj::plain(SCRIPTED);
    let r1 = one_attended_success(&p);
    let r2 = one_attended_success(&p);
    assert_eq!(p.entries("project").len(), 2);
    // Prune the older run (a person's command; the dry run decides).
    let keep = apb_core::policy_file::RetentionSettings {
        keep_days: 0,
        keep_last: 1,
        keep_failed_days: 0,
    };
    let now = apb_core::clock::now_ms_u64();
    let rows = apb_engine::retention::plan(&p.root(), &keep, None, now).unwrap();
    assert!(
        rows.iter()
            .any(|r| r.run_id == r1 && r.action == apb_engine::retention::PruneAction::Remove),
        "the older run is pruned: {rows:?}"
    );
    apb_engine::retention::apply(&p.root(), &keep, None, now).unwrap();
    assert_eq!(p.entries("project").len(), 2, "the ledger is untouched");
    let res = p.run(unattended_opts()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    // A second terminal event for the same run calls the observer again,
    // and its single entry is unchanged (written once).
    let run_dir = p.root().join(".apb/runs").join(&r2);
    let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
    log.append(EventPayload::RunFinished {
        outcome: "succeeded".into(),
    })
    .unwrap();
    drop(log);
    assert_eq!(p.entries("project").len(), 3, "one entry per run");
    let for_r2 = p.entries("project").iter().filter(|e| e.run == r2).count();
    assert_eq!(for_r2, 1, "written once");
}

/// The ledger file with one byte changed: the unattended start is refused
/// `probation_required` with the ledger text naming the path, no run
/// directory is created, the run end of an attended run writes nothing to
/// it and prints the stderr line, `probation::doctor_line` names the path,
/// and a start with `RunOptions.waive_probation` passes.
#[test]
fn an_unverifiable_ledger_refuses() {
    let p = Proj::plain(SCRIPTED);
    let r1 = one_attended_success(&p);
    let r2 = one_attended_success(&p);
    drop((r1, r2));
    // Tamper one byte of the ledger.
    let path = p.root().join(".apb/probation/project/pb.json");
    let raw = fs::read_to_string(&path).unwrap();
    let at = raw.find("\"succeeded\"").unwrap();
    let tampered = format!("{}X{}", &raw[..at + 1], &raw[at + 9..]);
    fs::write(&path, tampered).unwrap();
    let err = p.run(unattended_opts()).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("probation_required"), "{text}");
    assert!(text.contains("does not verify"), "{text}");
    assert!(text.contains("pb.json"), "{text}");
    // The two attended runs' directories are all there is.
    assert_eq!(run_ids(&p.root()).len(), 2, "no new run directory");
    // The run end of an attended run writes nothing: the bad ledger is
    // left as it is. The doctor names the path.
    let before = fs::read(&path).unwrap();
    p.plan_ok();
    let attended = p.attended(RunOptions::default());
    assert_eq!(attended.outcome, RunStatus::Succeeded);
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "the bad ledger is untouched"
    );
    let line = apb_engine::probation::doctor_line(&p.root()).unwrap();
    assert!(line.contains(&path.display().to_string()), "{line}");
    // A waived start passes.
    p.plan_ok();
    let res = p
        .run(RunOptions {
            unattended: true,
            waive_probation: true,
            ..RunOptions::default()
        })
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// 1.0.0 has two attended successes and 1.1.0 is current with none, both
/// asking for two; an unattended start of `--version 1.0.0` passes and one
/// of the current version is refused, and no run directory is created for
/// the refused start.
#[test]
fn the_check_counts_the_version_the_start_loads() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    one_attended_success(&p);
    let v2 = SCRIPTED.replace("version: 1.0.0", "version: 1.1.0");
    let vdir = p.root().join(".apb/playbooks/pb/1.1.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), &v2).unwrap();
    fs::write(vdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(vdir.join("scripts/c.sh"), C_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "1.1.0").unwrap();
    let res = p
        .run_version(
            Some("1.0.0"),
            RunOptions {
                unattended: true,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded, "1.0.0 is proven");
    let err = p
        .run(RunOptions {
            unattended: true,
            ..RunOptions::default()
        })
        .unwrap_err();
    assert!(err.to_string().contains("probation_required"), "{err}");
    // The three earlier runs' directories are all there is.
    assert_eq!(
        run_ids(&p.root()).len(),
        3,
        "the refused start leaves no run"
    );
}

/// A parent that asks for no probation pins a sub-playbook that asks for
/// one and has none: the unattended start through the run gate (which
/// pins the child) is refused before anything runs, the detail names the
/// child, its version and its node, and no run directory of either
/// exists.
#[test]
fn a_child_under_probation_refuses_the_parent_start() {
    let p = Proj::plain(SCRIPTED);
    // The child playbook under probation.
    let child = SCRIPTED.replace("id: pb", "id: child");
    let cdir = p.root().join(".apb/playbooks/child/1.0.0");
    fs::create_dir_all(cdir.join("scripts")).unwrap();
    fs::write(cdir.join("playbook.yaml"), &child).unwrap();
    fs::write(cdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(cdir.join("scripts/c.sh"), C_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/child/current"), "1.0.0").unwrap();
    let parent = PLAIN
        .replace(
            "  - { id: done, type: finish, outcome: success }",
            "  - { id: sub, type: playbook, playbook: child }\n  - { id: done, type: finish, outcome: success }",
        )
        .replace("version: 1.0.0\n", "version: 2.0.0\n")
        .replace(
            "  - { from: work, to: done }",
            "  - { from: work, to: sub }\n  - { from: sub, to: done }",
        );
    let pdir = p.root().join(".apb/playbooks/pb/2.0.0");
    fs::create_dir_all(pdir.join("scripts")).unwrap();
    fs::write(pdir.join("playbook.yaml"), &parent).unwrap();
    fs::write(pdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "2.0.0").unwrap();
    let opts = gated_unattended(&p.root());
    assert!(
        opts.expected_children
            .as_ref()
            .is_some_and(|c| c.contains_key("sub")),
        "the gate pins the child"
    );
    let err = p.run(opts).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("probation_required"), "{text}");
    assert!(
        text.contains("sub-playbook `child` 1.0.0 (node `sub`)"),
        "{text}"
    );
    assert!(run_ids(&p.root()).is_empty(), "no run directory of either");
}

/// Two attended successful parent runs write two eligible attended
/// entries into the child's own ledger: the next unattended start passes.
#[test]
fn an_attended_parent_run_counts_for_its_child() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    one_attended_success(&p);
    // The child playbook under probation, pinned by the parent.
    let child = SCRIPTED.replace("id: pb", "id: child");
    let cdir = p.root().join(".apb/playbooks/child/1.0.0");
    fs::create_dir_all(cdir.join("scripts")).unwrap();
    fs::write(cdir.join("playbook.yaml"), &child).unwrap();
    fs::write(cdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(cdir.join("scripts/c.sh"), C_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/child/current"), "1.0.0").unwrap();
    let parent = PLAIN
        .replace(
            "  - { id: done, type: finish, outcome: success }",
            "  - { id: child, type: playbook, playbook: child }\n  - { id: done, type: finish, outcome: success }",
        )
        .replace("version: 1.0.0\n", "version: 2.0.0\n")
        .replace(
            "  - { from: work, to: done }",
            "  - { from: work, to: child }\n  - { from: child, to: done }",
        );
    let pdir = p.root().join(".apb/playbooks/pb/2.0.0");
    fs::create_dir_all(pdir.join("scripts")).unwrap();
    fs::write(pdir.join("playbook.yaml"), &parent).unwrap();
    fs::write(pdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "2.0.0").unwrap();
    // Two attended parent runs, each starting the child: the child earns
    // its two entries.
    for _ in 0..2 {
        p.plan_ok();
        let res = p.attended(RunOptions::default());
        assert_eq!(res.outcome, RunStatus::Succeeded, "{}", res.run_id);
    }
    let child_entries = fs::read_to_string(p.root().join(".apb/probation/project/child.json"))
        .map(|raw| {
            let f: apb_core::probation::ProbationFile = serde_json::from_str(&raw).unwrap();
            f.entries
        })
        .unwrap_or_default();
    assert_eq!(child_entries.len(), 2, "the child's own ledger");
    // The next unattended start passes.
    let res = p
        .run(RunOptions {
            unattended: true,
            ..RunOptions::default()
        })
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// A start with `waive_probation` records `probation_waived` in its
/// manifest and in its child run's, and the child run is created.
#[test]
fn a_waived_start_covers_its_children() {
    let p = Proj::plain(SCRIPTED);
    let child = SCRIPTED.replace("id: pb", "id: child");
    let cdir = p.root().join(".apb/playbooks/child/1.0.0");
    fs::create_dir_all(cdir.join("scripts")).unwrap();
    fs::write(cdir.join("playbook.yaml"), &child).unwrap();
    fs::write(cdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(cdir.join("scripts/c.sh"), C_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/child/current"), "1.0.0").unwrap();
    let parent = PLAIN
        .replace(
            "  - { id: done, type: finish, outcome: success }",
            "  - { id: child, type: playbook, playbook: child }\n  - { id: done, type: finish, outcome: success }",
        )
        .replace("version: 1.0.0\n", "version: 2.0.0\n")
        .replace(
            "  - { from: work, to: done }",
            "  - { from: work, to: child }\n  - { from: child, to: done }",
        );
    let pdir = p.root().join(".apb/playbooks/pb/2.0.0");
    fs::create_dir_all(pdir.join("scripts")).unwrap();
    fs::write(pdir.join("playbook.yaml"), &parent).unwrap();
    fs::write(pdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "2.0.0").unwrap();
    p.plan_ok();
    let res = p
        .run(RunOptions {
            unattended: true,
            waive_probation: true,
            ..RunOptions::default()
        })
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let manifest = apb_engine::manifest::read(&p.root().join(".apb/runs").join(&res.run_id))
        .unwrap()
        .unwrap();
    assert!(manifest.probation_waived, "the parent records the waiver");
    // The child run was created and its manifest records the waiver.
    let events = read_all(&p.root().join(".apb/runs").join(&res.run_id)).unwrap();
    let child_id = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .expect("the child run");
    let child_manifest = apb_engine::manifest::read(&p.root().join(".apb/runs").join(&child_id))
        .unwrap()
        .unwrap();
    assert!(child_manifest.probation_waived, "the child inherits it");
}

/// A playbook the readiness report passes: no acting node (nothing can
/// act unbounded), done defined by an enforced script criterion, and a
/// budget. Its runs do nothing and succeed, which is what counts the
/// attended successes.
const READY: &str = r#"schema: 2
id: pb
name: pb
version: 1.0.0
budget:
  tokens: 1000
goal:
  statement: done
  enforce: true
  criteria:
    - { description: ok, check: { type: script, path: scripts/goal.sh } }
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
"#;

const GOAL_SH: &str = "exit 0\n";

/// 1.0.0 is ready, the current 1.1.0 has no budget: `--require-ready`
/// (`RunOptions.require_ready`) passes 1.0.0 and refuses the current
/// version `not_ready`, and no new run directory exists for the refused
/// start.
#[test]
fn require_ready_judges_the_loaded_version() {
    let p = Proj::with_scripts(READY, &[("goal.sh", GOAL_SH), ("w.sh", W_SH)]);
    let unbudgeted = READY.replace("version: 1.0.0", "version: 1.1.0\n");
    let unbudgeted = unbudgeted.replace("\nbudget:\n  tokens: 1000", "");
    let vdir = p.root().join(".apb/playbooks/pb/1.1.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), &unbudgeted).unwrap();
    fs::write(vdir.join("scripts/goal.sh"), GOAL_SH).unwrap();
    fs::write(vdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "1.1.0").unwrap();
    let res = p
        .run_version(
            Some("1.0.0"),
            RunOptions {
                unattended: true,
                require_ready: true,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let err = p
        .run(RunOptions {
            unattended: true,
            require_ready: true,
            ..RunOptions::default()
        })
        .unwrap_err();
    assert!(err.to_string().contains("not_ready"), "{err}");
    // The admitted 1.0.0 run's directory is all there is: the refused
    // start created none.
    assert_eq!(run_ids(&p.root()).len(), 1, "no new run directory");
}

/// A ready parent pins a child without a budget: refused `not_ready`
/// naming the child and its node.
#[test]
fn require_ready_refuses_an_unready_child() {
    let p = Proj::with_scripts(READY, &[("goal.sh", GOAL_SH), ("w.sh", W_SH)]);
    // The child has no budget.
    let child = PLAIN.replace("id: pb", "id: child");
    let cdir = p.root().join(".apb/playbooks/child/1.0.0");
    fs::create_dir_all(cdir.join("scripts")).unwrap();
    fs::write(cdir.join("playbook.yaml"), &child).unwrap();
    fs::write(cdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/child/current"), "1.0.0").unwrap();
    let parent = READY
        .replace(
            "  - { id: done, type: finish, outcome: success }",
            "  - { id: sub, type: playbook, playbook: child }\n  - { id: done, type: finish, outcome: success }",
        )
        .replace(
            "  - { from: start, to: done }",
            "  - { from: start, to: sub }\n  - { from: sub, to: done }",
        )
        .replace("version: 1.0.0", "version: 2.0.0");
    let pdir = p.root().join(".apb/playbooks/pb/2.0.0");
    fs::create_dir_all(pdir.join("scripts")).unwrap();
    fs::write(pdir.join("playbook.yaml"), &parent).unwrap();
    fs::write(pdir.join("scripts/goal.sh"), GOAL_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "2.0.0").unwrap();
    let err = p
        .run(RunOptions {
            require_ready: true,
            ..gated_unattended(&p.root())
        })
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains(
            "not_ready: sub-playbook `child` 1.0.0 (node `sub`) is not ready to run alone: "
        ),
        "{text}"
    );
    assert!(text.contains("no budget"), "the child's own item: {text}");
    assert!(run_ids(&p.root()).is_empty(), "no run directory");
}

/// A pinned sub-playbook that no longer reads (its definition changed
/// after the gate pinned it) is refused with the coded `not_ready` text.
#[test]
fn require_ready_refuses_a_child_that_does_not_read_with_its_code() {
    let p = Proj::with_scripts(READY, &[("goal.sh", GOAL_SH), ("w.sh", W_SH)]);
    let child = PLAIN.replace("id: pb", "id: child");
    let cdir = p.root().join(".apb/playbooks/child/1.0.0");
    fs::create_dir_all(cdir.join("scripts")).unwrap();
    fs::write(cdir.join("playbook.yaml"), &child).unwrap();
    fs::write(cdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/child/current"), "1.0.0").unwrap();
    let parent = READY
        .replace(
            "  - { id: done, type: finish, outcome: success }",
            "  - { id: sub, type: playbook, playbook: child }\n  - { id: done, type: finish, outcome: success }",
        )
        .replace(
            "  - { from: start, to: done }",
            "  - { from: start, to: sub }\n  - { from: sub, to: done }",
        )
        .replace("version: 1.0.0", "version: 2.0.0");
    let pdir = p.root().join(".apb/playbooks/pb/2.0.0");
    fs::create_dir_all(pdir.join("scripts")).unwrap();
    fs::write(pdir.join("playbook.yaml"), &parent).unwrap();
    fs::write(pdir.join("scripts/goal.sh"), GOAL_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "2.0.0").unwrap();
    let opts = gated_unattended(&p.root());
    // The child's definition changes after the gate pinned its digest.
    fs::write(
        cdir.join("playbook.yaml"),
        child.replace("name: pb", "name: other"),
    )
    .unwrap();
    let err = p
        .run(RunOptions {
            require_ready: true,
            waive_probation: true,
            ..opts
        })
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("not_ready: sub-playbook `child` (node `sub`) cannot be read"),
        "{text}"
    );
}

/// The engine part of the waiver: a `RunOptions.waive_probation` start
/// passes, a start without it is refused `probation_required` with the
/// per-project text, and the machine file's `waive_probation` passes.
#[test]
fn only_the_machine_file_or_a_waived_start_passes() {
    let p = Proj::plain(SCRIPTED);
    let err = p.run(unattended_opts()).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("probation_required"), "{text}");
    assert!(text.contains("probation is counted per project"), "{text}");
    assert!(run_ids(&p.root()).is_empty(), "no run directory exists");
    // The machine file's flag passes.
    let machine = p._cfg.path().join("policy.yaml");
    fs::write(&machine, "unattended:\n  waive_probation: true\n").unwrap();
    p.plan_ok();
    let res = p.run(unattended_opts()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// A second project `B` with the same playbook and the run `run_id` of
/// `p` copied into it, its journal cut before `run_finished` (`finished:
/// false`) or kept whole and written to the file directly (`true`). No
/// append ran in `B`, so no observer saw the run: `B`'s ledger has no
/// entry for it, the state a crash leaves before the terminal event, or
/// after its sync and before the observer wrote.
fn crashed_copy(p: &Proj, run_id: &str, finished: bool) -> tempfile::TempDir {
    let b = tempfile::tempdir().unwrap();
    seed(b.path(), SCRIPTED, &[("w.sh", W_SH), ("c.sh", C_SH)]);
    let from = p.root().join(".apb/runs").join(run_id);
    let to = b.path().join(".apb/runs").join(run_id);
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    let copied = std::process::Command::new("cp")
        .arg("-a")
        .arg(&from)
        .arg(&to)
        .status()
        .unwrap();
    assert!(copied.success());
    let events = read_all(&to).unwrap();
    let cut = if finished {
        events.len()
    } else {
        events
            .iter()
            .position(|e| matches!(&e.payload, EventPayload::RunFinished { .. }))
            .unwrap()
    };
    let kept: String = events[..cut]
        .iter()
        .map(|e| serde_json::to_string(e).unwrap() + "\n")
        .collect();
    fs::write(to.join("events.jsonl"), &kept).unwrap();
    b
}

/// The entries of `pb`'s project ledger in the project at `root`.
fn entries_at(root: &Path) -> Vec<apb_core::probation::ProbationEntry> {
    let l = apb_core::probation::read(root, apb_core::scope::Scope::Project, "pb").unwrap();
    assert!(l.signed, "the ledger verifies");
    l.file.entries
}

/// An unattended start of `pb` in the project at `root`.
fn unattended_at(root: &Path, opts: RunOptions) -> Result<RunResult, apb_engine::EngineError> {
    let root = root.to_path_buf();
    bounded(move || run(&root, "pb", None, opts))
}

/// A run whose journal ends before `run_finished` (a crash before the
/// terminal event) has no entry; `stop` appends `run_aborted` and the
/// observer writes the run `aborted`, `eligible: false`; the count stays
/// 0, so the next unattended start is refused with `0 so far`.
#[test]
fn a_crash_before_run_finished_then_stop_is_never_eligible() {
    let p = Proj::plain(SCRIPTED);
    let run_id = one_attended_success(&p);
    let b = crashed_copy(&p, &run_id, false);
    assert!(
        !entries_at(b.path()).iter().any(|e| e.run == run_id),
        "no entry without the terminal event"
    );
    let stopped = apb_engine::stop::stop_run(b.path(), &run_id).unwrap();
    assert!(matches!(
        stopped,
        apb_engine::stop::StopOutcome::FinalizedDeadRun
    ));
    let entries = entries_at(b.path());
    assert!(
        entries
            .iter()
            .any(|e| e.run == run_id && e.outcome == "aborted" && !e.eligible),
        "written aborted: {entries:?}"
    );
    let err = unattended_at(b.path(), unattended_opts()).unwrap_err();
    assert!(
        err.to_string().contains("probation_required") && err.to_string().contains("0 so far here"),
        "{err}"
    );
}

/// A run whose `run_finished` is in the file but whose observer never ran
/// (a crash after the sync) stays uncounted: no entry, `stop` answers
/// `AlreadyTerminal` and appends nothing, the ledger still verifies, and
/// the next unattended start says `0 so far`.
#[test]
fn a_crash_after_run_finished_leaves_the_run_uncounted() {
    let p = Proj::plain(SCRIPTED);
    let run_id = one_attended_success(&p);
    let b = crashed_copy(&p, &run_id, true);
    match apb_engine::stop::stop_run(b.path(), &run_id) {
        Ok(s) => assert!(matches!(s, apb_engine::stop::StopOutcome::AlreadyTerminal)),
        Err(e) => panic!("stop: {e}"),
    }
    assert!(
        !entries_at(b.path()).iter().any(|e| e.run == run_id),
        "the run stays uncounted"
    );
    let err = unattended_at(b.path(), unattended_opts()).unwrap_err();
    assert!(
        err.to_string().contains("probation_required") && err.to_string().contains("0 so far here"),
        "{err}"
    );
}

/// Two runs that end at the same moment both land in a ledger that still
/// verifies: the store's lock serializes the two writes.
#[test]
fn two_runs_ending_at_once_both_land() {
    let p = Proj::plain(SCRIPTED);
    let r1 = one_attended_success(&p);
    let r2 = one_attended_success(&p);
    let b = tempfile::tempdir().unwrap();
    seed(b.path(), SCRIPTED, &[("w.sh", W_SH), ("c.sh", C_SH)]);
    for id in [&r1, &r2] {
        let one = crashed_copy(&p, id, false);
        let to = b.path().join(".apb/runs").join(id);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::rename(one.path().join(".apb/runs").join(id), &to).unwrap();
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = [r1.clone(), r2.clone()]
        .into_iter()
        .map(|id| {
            let root = b.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                apb_engine::stop::stop_run(&root, &id).unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let entries = entries_at(b.path());
    for id in [&r1, &r2] {
        assert!(
            entries
                .iter()
                .any(|e| &e.run == id && e.outcome == "aborted"),
            "{id}: {entries:?}"
        );
    }
}

/// An attended run whose node fails ends `run_finished { outcome: failed }`
/// and is written `failed`, `eligible: false`.
#[test]
fn the_outcome_comes_from_the_terminal_event() {
    let p = Proj::plain(SCRIPTED);
    let res = p.run(RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    assert!(p.has_entry(&res.run_id, "failed", false));
}

/// An unattended run of a proven 1.0.0, a patch to 1.1.0 with no attended
/// runs: one `patch_rejected` whose reason starts `probation_required`,
/// the run's `playbook.yaml` unchanged, no `patch_applied`; the same patch
/// on a run started with `RunOptions.waive_probation` applies.
#[test]
fn a_patch_on_an_unattended_run_is_held_to_probation() {
    let p = Proj::plain(SCRIPTED);
    let r1 = one_attended_success(&p);
    let r2 = one_attended_success(&p);
    drop((r1, r2));
    // The patch version, unproven.
    let patched = SCRIPTED.replace("version: 1.0.0", "version: 1.1.0");
    let vdir = p.root().join(".apb/playbooks/pb/1.1.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), &patched).unwrap();
    fs::write(vdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(vdir.join("scripts/c.sh"), C_SH).unwrap();
    // An unattended run of 1.0.0 (its probation is done).
    let prepared = prepare_unattended(&p.root(), None, false);
    let rid = prepared.run_id().to_string();
    let patch =
        create_patch_version(&p.root(), "pb", "1.0.0", &patched, &rid, "workaround").unwrap();
    post_control(
        &p.root().join(".apb/runs").join(&rid),
        Control::Patch {
            version: patch.clone(),
            classification: "workaround".into(),
            continue_from: "start".into(),
        },
    )
    .unwrap();
    let root = p.root();
    bounded(move || {
        let _ = apb_engine::scheduler::drive_prepared(&root, prepared);
    });
    let events = read_all(&p.root().join(".apb/runs").join(&rid)).unwrap();
    let reasons = patch_reasons(&events);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].starts_with("probation_required"), "{reasons:?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::PatchApplied { .. })),
        "the patch was refused"
    );
    // The run's snapshot is unchanged: the next resume keeps 1.0.0.
    let snapshot =
        fs::read_to_string(p.root().join(".apb/runs").join(&rid).join("playbook.yaml")).unwrap();
    assert_eq!(snapshot, SCRIPTED, "the running definition is unchanged");
    drop(patch);

    // The same patch on a waived run applies.
    let prepared = prepare_unattended(&p.root(), None, true);
    let rid = prepared.run_id().to_string();
    let patch =
        create_patch_version(&p.root(), "pb", "1.0.0", &patched, &rid, "workaround").unwrap();
    post_control(
        &p.root().join(".apb/runs").join(&rid),
        Control::Patch {
            version: patch,
            classification: "workaround".into(),
            continue_from: "start".into(),
        },
    )
    .unwrap();
    let root = p.root();
    bounded(move || {
        let _ = apb_engine::scheduler::drive_prepared(&root, prepared);
    });
    let events = read_all(&p.root().join(".apb/runs").join(&rid)).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::PatchApplied { .. })),
        "the waived run's patch applies"
    );
}

/// An unattended run whose manifest is gone after preparation: its
/// `run.yaml` still says unattended, so a patch is not taken for an
/// attended run's; the waiver lived only in the manifest, so the patch is
/// refused even on a waived start.
#[test]
fn a_patch_on_an_unattended_run_without_its_manifest_is_refused() {
    let p = Proj::plain(SCRIPTED);
    let patched = SCRIPTED.replace("version: 1.0.0", "version: 1.1.0");
    let vdir = p.root().join(".apb/playbooks/pb/1.1.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), &patched).unwrap();
    fs::write(vdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(vdir.join("scripts/c.sh"), C_SH).unwrap();
    let prepared = prepare_unattended(&p.root(), None, true);
    let rid = prepared.run_id().to_string();
    let run_dir = p.root().join(".apb/runs").join(&rid);
    fs::remove_file(run_dir.join("manifest.yaml")).unwrap();
    let patch =
        create_patch_version(&p.root(), "pb", "1.0.0", &patched, &rid, "workaround").unwrap();
    post_control(
        &run_dir,
        Control::Patch {
            version: patch,
            classification: "workaround".into(),
            continue_from: "start".into(),
        },
    )
    .unwrap();
    let root = p.root();
    bounded(move || {
        let _ = apb_engine::scheduler::drive_prepared(&root, prepared);
    });
    let events = read_all(&run_dir).unwrap();
    let reasons = patch_reasons(&events);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(
        reasons[0].starts_with("probation_required") && reasons[0].contains("manifest is missing"),
        "{reasons:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::PatchApplied { .. })),
        "the patch was refused"
    );
}

/// A child start whose parent is unattended by its `run.yaml` and whose
/// manifest is gone is refused: the child is never started attended by
/// default, and the waiver and readiness demand cannot be told.
#[test]
fn a_child_of_an_unattended_parent_without_its_manifest_is_refused() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    let parent = p.root().join(".apb/runs/parent-0");
    fs::create_dir_all(&parent).unwrap();
    fs::write(parent.join("run.yaml"), "unattended: true\n").unwrap();
    let opts = RunOptions {
        parent_run: Some("parent-0".into()),
        ..RunOptions::default()
    };
    let err =
        match apb_engine::scheduler::prepare_supervised_background(&p.root(), "pb", None, opts) {
            Ok(_) => panic!("the child start was admitted"),
            Err(e) => e.to_string(),
        };
    assert!(
        err.contains("probation_required") && err.contains("manifest is missing"),
        "{err}"
    );
}

/// An unattended `require_ready` run of a ready version, a patch to a
/// version without a budget: `patch_rejected` starting `not_ready`, and a
/// patch whose pinned child is not ready: refused naming the child.
#[test]
fn a_patch_on_a_require_ready_run_is_held_to_readiness() {
    let p = Proj::with_scripts(READY, &[("goal.sh", GOAL_SH), ("w.sh", W_SH)]);
    // The patch version drops the budget: not ready.
    let unbudgeted = READY
        .replace("version: 1.0.0", "version: 1.1.0")
        .replace("\nbudget:\n  tokens: 1000", "");
    let vdir = p.root().join(".apb/playbooks/pb/1.1.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), &unbudgeted).unwrap();
    fs::write(vdir.join("scripts/goal.sh"), GOAL_SH).unwrap();
    let prepared = prepare_unattended_opts(&p.root(), Some("1.0.0"), true, true);
    let rid = prepared.run_id().to_string();
    let patch =
        create_patch_version(&p.root(), "pb", "1.0.0", &unbudgeted, &rid, "workaround").unwrap();
    post_control(
        &p.root().join(".apb/runs").join(&rid),
        Control::Patch {
            version: patch,
            classification: "workaround".into(),
            continue_from: "start".into(),
        },
    )
    .unwrap();
    let root = p.root();
    bounded(move || {
        let _ = apb_engine::scheduler::drive_prepared(&root, prepared);
    });
    let events = read_all(&p.root().join(".apb/runs").join(&rid)).unwrap();
    let reasons = patch_reasons(&events);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].starts_with("not_ready"), "{reasons:?}");
}

/// The patch version has its own attended successes and passes readiness:
/// `patch_applied`.
#[test]
fn a_patch_to_a_proven_ready_definition_applies() {
    let budgeted = PLAIN.replace(
        "version: 1.0.0\n",
        "version: 1.0.0\nbudget:\n  tokens: 1000\n",
    );
    let p = Proj::plain(&budgeted);
    let unbudgeted = budgeted.replace("version: 1.0.0", "version: 1.1.0");
    let vdir = p.root().join(".apb/playbooks/pb/1.1.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), &unbudgeted).unwrap();
    fs::write(vdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "1.0.0").unwrap();
    // Two attended successes of 1.1.0: the patch version is proven.
    let saved = fs::read_to_string(p.root().join(".apb/playbooks/pb/current")).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "1.1.0").unwrap();
    let _ = one_attended_success(&p);
    let _ = one_attended_success(&p);
    fs::write(p.root().join(".apb/playbooks/pb/current"), saved).unwrap();
    let prepared = prepare_unattended(&p.root(), Some("1.0.0"), true);
    let rid = prepared.run_id().to_string();
    let patch =
        create_patch_version(&p.root(), "pb", "1.0.0", &unbudgeted, &rid, "workaround").unwrap();
    post_control(
        &p.root().join(".apb/runs").join(&rid),
        Control::Patch {
            version: patch,
            classification: "workaround".into(),
            continue_from: "start".into(),
        },
    )
    .unwrap();
    let root = p.root();
    bounded(move || {
        let _ = apb_engine::scheduler::drive_prepared(&root, prepared);
    });
    let events = read_all(&p.root().join(".apb/runs").join(&rid)).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::PatchApplied { .. })),
        "the proven patch applies"
    );
}

/// An attended run patched to an unproven, unready version applies as on
/// main: neither check runs.
#[test]
fn a_patch_on_an_attended_run_checks_neither() {
    let p = Proj::plain(SCRIPTED);
    // 1.1.0 under probation, no attended successes.
    let patched = SCRIPTED.replace("version: 1.0.0", "version: 1.1.0");
    let vdir = p.root().join(".apb/playbooks/pb/1.1.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), &patched).unwrap();
    fs::write(vdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(vdir.join("scripts/c.sh"), C_SH).unwrap();
    p.plan_ok();
    let (root, id) = (p.root(), "pb".to_string());
    let prepared = apb_engine::scheduler::prepare_supervised_background(
        &root,
        &id,
        None,
        RunOptions::default(),
    )
    .unwrap();
    let rid = prepared.run_id().to_string();
    let patch =
        create_patch_version(&p.root(), "pb", "1.0.0", &patched, &rid, "workaround").unwrap();
    post_control(
        &root.join(".apb/runs").join(&rid),
        Control::Patch {
            version: patch,
            classification: "workaround".into(),
            continue_from: "start".into(),
        },
    )
    .unwrap();
    let root = root.clone();
    bounded(move || {
        let _ = apb_engine::scheduler::drive_prepared(&root, prepared);
    });
    let events = read_all(&p.root().join(".apb/runs").join(&rid)).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::PatchApplied { .. })),
        "an attended run's patch applies unchecked"
    );
    assert_eq!(patch_reasons(&events).len(), 0);
}

/// Prepares an unattended background run of `pb` and starts its driver
/// thread? No: returns the prepared run for the caller to patch before
/// driving; `waived` sets `RunOptions.waive_probation`, `version` pins a
/// version.
fn prepare_unattended(
    root: &Path,
    version: Option<&str>,
    waived: bool,
) -> apb_engine::scheduler::PreparedRun {
    prepare_unattended_opts(root, version, waived, false)
}

fn prepare_unattended_opts(
    root: &Path,
    version: Option<&str>,
    waived: bool,
    require_ready: bool,
) -> apb_engine::scheduler::PreparedRun {
    // The patch control is posted before the drive starts, so its first
    // scan applies (or refuses) the patch. The version parameter is the
    // one preparation loads (`current` when None).
    let opts = RunOptions {
        unattended: true,
        waive_probation: waived,
        require_ready,
        ..RunOptions::default()
    };
    apb_engine::scheduler::prepare_supervised_background(root, "pb", version, opts).unwrap()
}

/// The refusal's text names the version the start loads.
#[test]
fn the_refusal_names_the_loaded_version() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    one_attended_success(&p);
    let v2 = SCRIPTED.replace("version: 1.0.0", "version: 1.1.0");
    let vdir = p.root().join(".apb/playbooks/pb/1.1.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), &v2).unwrap();
    fs::write(vdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(vdir.join("scripts/c.sh"), C_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "1.1.0").unwrap();
    let view = apb_engine::gate::unattended::probation(
        &p.root(),
        "project",
        &apb_core::registry::Registry::open_dir(&p.root().join(".apb"))
            .unwrap()
            .load("pb", Some("1.1.0"))
            .unwrap(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(view.counted, 0, "{view:?}");
}

/// The machine asks for one attended run and the project's `policy.yaml`
/// does not read (a project file may not waive probation): the settings
/// cannot be known, so an unattended start of a playbook that asks for
/// nothing itself is refused; a typed waiver passes.
#[test]
fn a_policy_file_that_does_not_read_refuses_an_unattended_start() {
    let p = Proj::plain(PLAIN);
    fs::write(
        p._cfg.path().join("policy.yaml"),
        "unattended:\n  after_attended_successes: 1\n",
    )
    .unwrap();
    fs::write(
        p.root().join(".apb/policy.yaml"),
        "unattended:\n  waive_probation: true\n",
    )
    .unwrap();
    let err = p.run(unattended_opts()).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("probation_required"), "{text}");
    assert!(text.contains("cannot be known"), "{text}");
    assert!(run_ids(&p.root()).is_empty(), "no run directory");
    // A waived start is not refused on probation; the launch check still
    // refuses the file that does not read on its own account.
    let err = p
        .run(RunOptions {
            unattended: true,
            waive_probation: true,
            ..RunOptions::default()
        })
        .unwrap_err();
    let text = err.to_string();
    assert!(!text.contains("probation_required"), "{text}");
    assert!(text.contains("policy_file_invalid"), "{text}");
}

/// A proven playbook started unattended with run overrides runs another
/// executor than its probation counted: refused, unless waived.
#[test]
fn an_unattended_start_with_overrides_is_refused() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    one_attended_success(&p);
    let overrides = || {
        Some(apb_core::overrides::RunOverrides {
            nodes: [(
                "work".to_string(),
                apb_core::overrides::NodeOverride {
                    profile: Some(apb_core::profile::QualifiedProfileRef {
                        name: "main".into(),
                        scope: apb_core::profile::ProfileScope::Project,
                    }),
                    ..Default::default()
                },
            )]
            .into(),
        })
    };
    let err = p
        .run(RunOptions {
            overrides: overrides(),
            ..unattended_opts()
        })
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("probation_required"), "{text}");
    assert!(
        text.contains("start it without --overrides or waive probation at a terminal"),
        "{text}"
    );
    assert_eq!(run_ids(&p.root()).len(), 2, "no new run directory");
    // Without overrides the same start passes.
    let res = p.run(unattended_opts()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// The machine's waiver passes a ledger that does not verify, as the fit
/// report's probation item says.
#[test]
fn a_machine_waiver_passes_an_unverifiable_ledger() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    let path = p.root().join(".apb/probation/project/pb.json");
    fs::write(&path, "{not json").unwrap();
    fs::write(
        p._cfg.path().join("policy.yaml"),
        "unattended:\n  waive_probation: true\n",
    )
    .unwrap();
    let report = apb_engine::fit::assess(
        &p.root(),
        &apb_core::scope::PlaybookRef {
            origin: apb_core::scope::Origin::Project { workspace_id: None },
            id: "pb".into(),
            version: None,
        },
    )
    .unwrap();
    let item = report
        .readiness
        .items
        .iter()
        .find(|i| i.code == "probation")
        .unwrap();
    assert!(item.ok, "{item:?}");
    assert!(item.detail.contains("waived on this machine"), "{item:?}");
    p.plan_ok();
    let res = p.run(unattended_opts()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(fs::read_to_string(&path).unwrap(), "{not json", "untouched");
}

/// A signed ledger of another project, copied in or linked to, does not
/// count here: the file names its project and is read with no link
/// followed, so the unattended start is refused as unverifiable.
#[test]
fn a_ledger_of_another_project_does_not_count() {
    let p = Proj::plain(SCRIPTED);
    one_attended_success(&p);
    one_attended_success(&p);
    let theirs = p.root().join(".apb/probation/project/pb.json");
    for link in [false, true] {
        let b = tempfile::tempdir().unwrap();
        seed(b.path(), SCRIPTED, &[("w.sh", W_SH), ("c.sh", C_SH)]);
        let dir = b.path().join(".apb/probation/project");
        fs::create_dir_all(&dir).unwrap();
        if link {
            std::os::unix::fs::symlink(&theirs, dir.join("pb.json")).unwrap();
        } else {
            fs::copy(&theirs, dir.join("pb.json")).unwrap();
        }
        let err = unattended_at(b.path(), unattended_opts()).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("probation_required") && text.contains("does not verify"),
            "link {link}: {text}"
        );
    }
    // In its own project the same file counts.
    p.plan_ok();
    assert_eq!(
        p.run(unattended_opts()).unwrap().outcome,
        RunStatus::Succeeded
    );
}

/// The patch arm checks every sub-playbook the run pinned: an unattended
/// run whose pinned child (proven by an attended parent run) has a ledger
/// that stops verifying refuses a patch, naming the child.
#[test]
fn a_patch_checks_the_pinned_children() {
    let p = Proj::plain(PLAIN);
    // Only the child asks for probation, so the patched parent (a new,
    // unproven definition) passes on its own and the child decides.
    let child = SCRIPTED
        .replace("id: pb", "id: child")
        .replace("after_attended_successes: 2", "after_attended_successes: 1");
    let cdir = p.root().join(".apb/playbooks/child/1.0.0");
    fs::create_dir_all(cdir.join("scripts")).unwrap();
    fs::write(cdir.join("playbook.yaml"), &child).unwrap();
    fs::write(cdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/child/current"), "1.0.0").unwrap();
    let parent = PLAIN
        .replace(
            "  - { id: done, type: finish, outcome: success }",
            "  - { id: sub, type: playbook, playbook: child }\n  - { id: done, type: finish, outcome: success }",
        )
        .replace("version: 1.0.0\n", "version: 2.0.0\n")
        .replace(
            "  - { from: work, to: done }",
            "  - { from: work, to: sub }\n  - { from: sub, to: done }",
        );
    let pdir = p.root().join(".apb/playbooks/pb/2.0.0");
    fs::create_dir_all(pdir.join("scripts")).unwrap();
    fs::write(pdir.join("playbook.yaml"), &parent).unwrap();
    fs::write(pdir.join("scripts/w.sh"), W_SH).unwrap();
    fs::write(p.root().join(".apb/playbooks/pb/current"), "2.0.0").unwrap();
    // One attended success of the parent counts for its child.
    one_attended_success(&p);
    let opts = gated_unattended(&p.root());
    let prepared =
        apb_engine::scheduler::prepare_supervised_background(&p.root(), "pb", None, opts).unwrap();
    let rid = prepared.run_id().to_string();
    // The child's ledger stops verifying after the start.
    fs::write(
        p.root().join(".apb/probation/project/child.json"),
        "{not json",
    )
    .unwrap();
    let patched = parent.replace("name: pb\n", "name: patched\n");
    let patch =
        create_patch_version(&p.root(), "pb", "2.0.0", &patched, &rid, "workaround").unwrap();
    post_control(
        &p.root().join(".apb/runs").join(&rid),
        Control::Patch {
            version: patch,
            classification: "workaround".into(),
            continue_from: "start".into(),
        },
    )
    .unwrap();
    let root = p.root();
    bounded(move || {
        let _ = apb_engine::scheduler::drive_prepared(&root, prepared);
    });
    let events = read_all(&p.root().join(".apb/runs").join(&rid)).unwrap();
    let reasons = patch_reasons(&events);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].starts_with("probation_required"), "{reasons:?}");
    assert!(
        reasons[0].contains("sub-playbook `child` 1.0.0") && reasons[0].contains("does not verify"),
        "{reasons:?}"
    );
}
