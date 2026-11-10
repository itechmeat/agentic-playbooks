//! External data runs (capability `run-external-data`), end to end through
//! the engine: a flagged start records the flag and recalls nothing, its
//! successor inherits it, it never takes a waiting candidate, the versions
//! its patches make stay with it, and nothing it produced reaches a
//! learning store or a reader of earlier runs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_engine::event::{Event, EventLog, EventPayload, read_all};
use apb_engine::external_data;
use apb_engine::gate::launch::{LaunchContext, LaunchKind};
use apb_engine::manifest::RunOrigin;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const ID: &str = "ext";

/// A script playbook with one required param (`customer`) and one with a
/// default.
const PLAYBOOK: &str = r#"
schema: 2
id: ext
name: Ext
version: 1.0.0
params:
  - { name: customer, type: text }
  - { name: region, type: text, default: eu }
supervisor:
  policy:
    promote_supervisor_patches: on_success
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn seed(root: &Path) {
    // The installation key an `apb` process creates at its entry point, so
    // the runs are stamped as created here and readers of earlier runs
    // take them (else a test passes only after another made the key).
    apb_core::run_origin::ensure_key().unwrap();
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks").join(ID).join("1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(dir.join("scripts/ok.sh"), "echo ok\n").unwrap();
    fs::write(
        root.join(".apb/playbooks").join(ID).join("current"),
        "1.0.0",
    )
    .unwrap();
    approve(root, "1.0.0");
}

fn approve(root: &Path, version: &str) {
    let loaded = apb_core::registry::Registry::open(root)
        .unwrap()
        .load(ID, Some(version))
        .unwrap();
    apb_core::trust::TrustStore::load()
        .approve(
            &loaded.trust_digest().unwrap(),
            ID,
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .unwrap();
}

fn params(customer: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("customer".to_string(), customer.to_string())])
}

fn wref(version: Option<&str>) -> PlaybookRef {
    PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: ID.into(),
        version: version.map(str::to_string),
    }
}

/// A start as every surface makes it: the gate with the flag, its permit
/// applied. Returns the run id, its outcome and the permit's
/// `candidate_skipped`.
fn start(
    root: &Path,
    version: Option<&str>,
    flag: bool,
    opts: RunOptions,
) -> (String, RunStatus, Option<String>) {
    let permit = apb_engine::gate::check_run(
        root,
        &wref(version),
        true,
        false,
        &LaunchContext::dashboard(LaunchKind::Start).with_external_data(flag),
    )
    .unwrap();
    let skipped = permit.candidate_skipped.clone();
    let version = permit.run_version(version);
    let mut opts = RunOptions {
        external_data: flag,
        ..opts
    };
    permit.apply(&mut opts);
    let res = run(root, ID, version.as_deref(), opts).unwrap();
    (res.run_id, res.outcome, skipped)
}

fn run_dir(root: &Path, id: &str) -> PathBuf {
    root.join(".apb/runs").join(id)
}

fn events(root: &Path, id: &str) -> Vec<Event> {
    read_all(&run_dir(root, id)).unwrap()
}

fn remembered(root: &Path, id: &str) -> Vec<String> {
    events(root, id)
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::RunProvenance { remembered, .. } => Some(remembered),
            _ => None,
        })
        .unwrap()
}

fn rendered_customer(root: &Path, id: &str) -> Option<String> {
    rendered(root, id, "customer")
}

fn rendered(root: &Path, id: &str, name: &str) -> Option<String> {
    apb_engine::run_config::read_run_config(&run_dir(root, id))
        .unwrap()
        .params
        .get(name)
        .cloned()
}

fn remember_params(root: &Path, values: BTreeMap<String, String>) {
    let loaded = apb_core::registry::Registry::open(root)
        .unwrap()
        .load(ID, None)
        .unwrap();
    let key = apb_core::run_memory::MemoryKey::new(apb_core::scope::Scope::Project, ID).unwrap();
    apb_core::run_memory::remember(
        root,
        &key,
        &loaded.playbook.params,
        &loaded.version,
        apb_core::run_memory::RememberInput {
            params: Some(values),
            instruction: None,
        },
    )
    .unwrap();
}

/// A flagged start writes the flag into the manifest (boolean, origin,
/// mode suffix) and `run_started`, applies no remembered value, and the
/// gate refuses a required param that only memory would fill.
#[test]
fn a_flagged_start_records_the_flag_and_recalls_nothing() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    // `region` has a default, so a start that gives only `customer` would
    // take the remembered `us` from memory unless recall is off.
    remember_params(
        root,
        BTreeMap::from([
            ("customer".to_string(), "acme".to_string()),
            ("region".to_string(), "us".to_string()),
        ]),
    );

    // The gate's param check: memory counts as empty for a flagged start.
    assert!(apb_engine::gate::check_params(root, &wref(None), &BTreeMap::new()).is_ok());
    let refused =
        apb_engine::gate::check_params_with(root, &wref(None), &BTreeMap::new(), true).unwrap_err();
    assert_eq!(refused["policy"], apb_engine::gate::PARAM_NEEDS_VALUE);

    // An unflagged start recalls the remembered value.
    let (plain, outcome, _) = start(root, None, false, RunOptions::default());
    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(
        remembered(root, &plain),
        vec!["customer".to_string(), "region".to_string()]
    );
    assert_eq!(rendered(root, &plain, "region").as_deref(), Some("us"));
    assert!(!external_data::is_flagged(&run_dir(root, &plain)));

    // A flagged start passes its own value and recalls nothing.
    let (flagged, outcome, _) = start(
        root,
        None,
        true,
        RunOptions {
            params: params("beta"),
            ..Default::default()
        },
    );
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(remembered(root, &flagged).is_empty());
    assert_eq!(rendered_customer(root, &flagged).as_deref(), Some("beta"));
    assert_eq!(rendered(root, &flagged, "region").as_deref(), Some("eu"));
    let m = apb_engine::manifest::read(&run_dir(root, &flagged))
        .unwrap()
        .unwrap();
    assert!(m.external_data);
    assert_eq!(m.origin, Some(RunOrigin::ExternalData));
    assert!(m.is_external());
    assert_eq!(m.base_mode(), "cli");
    assert!(matches!(
        &events(root, &flagged)[0].payload,
        EventPayload::RunStarted {
            external_data: true,
            ..
        }
    ));
    assert!(!external_data::learns_from(&run_dir(root, &flagged)));
}

/// A flagged start whose preparation fails before its manifest (a profile
/// changed after the check) still records the flag on the `run_started`
/// the failure writes, so no reader of earlier runs learns from it.
#[test]
fn a_flagged_start_that_fails_in_preparation_stays_flagged() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: t\nexecutor:\n  agent: claude-code\n  model: sonnet\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
    let vdir = root.join(".apb/playbooks").join(ID).join("1.1.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("scripts/ok.sh"), "echo ok\n").unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        PLAYBOOK
            .replace("version: 1.0.0", "version: 1.1.0")
            .replace(
                "type: script, script: \"scripts/ok.sh\", runner: sh",
                "type: agent_task, prompt: \"do\", profile: main",
            ),
    )
    .unwrap();
    approve(root, "1.1.0");
    let permit = apb_engine::gate::check_run(
        root,
        &wref(Some("1.1.0")),
        true,
        false,
        &LaunchContext::dashboard(LaunchKind::Start).with_external_data(true),
    )
    .unwrap();
    let mut opts = RunOptions {
        params: params("acme"),
        external_data: true,
        ..Default::default()
    };
    permit.apply(&mut opts);
    // The profile changes after the check: preparation refuses the run.
    fs::write(pdir.join("SOUL.md"), "changed").unwrap();
    let err = run(root, ID, Some("1.1.0"), opts).unwrap_err().to_string();
    assert!(err.contains("bundle mismatch"), "{err}");
    let dirs: Vec<PathBuf> = fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert_eq!(dirs.len(), 1);
    let failed = &dirs[0];
    assert!(matches!(
        &read_all(failed).unwrap()[0].payload,
        EventPayload::RunStarted {
            external_data: true,
            ..
        }
    ));
    assert!(external_data::is_flagged(failed));
    assert!(!external_data::learns_from(failed));
}

/// A run started with `continued_from` naming a flagged run is flagged
/// even when its start said nothing.
#[test]
fn a_successor_of_a_flagged_run_is_flagged() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let opts = || RunOptions {
        params: params("acme"),
        ..Default::default()
    };
    let (flagged, _, _) = start(root, None, true, opts());
    let (next, outcome, _) = start(
        root,
        None,
        false,
        RunOptions {
            continued_from: Some(flagged.clone()),
            ..opts()
        },
    );
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(external_data::is_flagged(&run_dir(root, &next)));
    let refusal =
        external_data::for_start(root, Some(false), None, Some(flagged.as_str())).unwrap_err();
    assert_eq!(refusal.code, external_data::IMMUTABLE);
}

/// A run's journal with the run id, the times and the flag taken out, for
/// a comparison of two runs of the same start.
fn normalized(root: &Path, id: &str) -> Vec<serde_json::Value> {
    events(root, id)
        .into_iter()
        .map(|e| {
            let mut v = serde_json::to_value(&e).unwrap();
            let o = v.as_object_mut().unwrap();
            o.remove("ts");
            o.remove("external_data");
            for k in [
                "duration_ms",
                "spawn_ms",
                "execution_root",
                "apb",
                "token",
                "pid",
                "session",
            ] {
                o.remove(k);
            }
            let text = serde_json::to_string(&v).unwrap().replace(id, "RUN");
            serde_json::from_str(&text).unwrap()
        })
        .collect()
}

/// The two journals of the same start, flagged and not, are equal but for
/// `run_started.external_data`, the run ids and the times: on a script
/// node, and on an agent step of the stub agent.
#[test]
fn a_flagged_run_otherwise_journals_as_an_unflagged_one() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let opts = || RunOptions {
        params: params("acme"),
        ..Default::default()
    };
    let (plain, _, _) = start(root, None, false, opts());
    let (flagged, _, _) = start(root, None, true, opts());
    assert_eq!(normalized(root, &plain), normalized(root, &flagged));

    // The same on an agent step: the stub agent prints a fixed answer.
    const AGENT: &str = "schema: 2\nid: agentic\nname: agentic\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"Summarize the input\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";
    let v = root.join(".apb/playbooks/agentic/1.0.0");
    fs::create_dir_all(&v).unwrap();
    fs::write(v.join("playbook.yaml"), AGENT).unwrap();
    fs::write(root.join(".apb/playbooks/agentic/current"), "1.0.0").unwrap();
    crate::common::seed_main(root);
    let agent = root.join("agent.sh");
    crate::common::write_sync(
        &agent,
        "#!/bin/sh\nprintf 'summarized\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'\n",
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let _agent = crate::common::EnvVar::set("APB_AGENT_CMD", &agent);
    let agentic = |flag: bool| {
        let res = run(
            root,
            "agentic",
            None,
            RunOptions {
                external_data: flag,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(res.outcome, RunStatus::Succeeded);
        res.run_id
    };
    let (plain, flagged) = (agentic(false), agentic(true));
    let journal = normalized(root, &plain);
    assert!(
        journal
            .iter()
            .any(|e| e["type"] == "attempt_finished" && e["node"] == "work"),
        "the agent step ran: {journal:#?}"
    );
    assert_eq!(journal, normalized(root, &flagged));
}

/// A flagged start never takes a waiting candidate: it runs `current` and
/// says why; naming the candidate's version runs it as no trial, and the
/// trial record stays byte-identical.
#[test]
fn a_flagged_start_never_takes_a_waiting_candidate() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let opts = || RunOptions {
        params: params("acme"),
        ..Default::default()
    };
    let (first, _, _) = start(root, None, false, opts());
    let candidate = apb_engine::forward_patch::create(
        root,
        &first,
        &apb_engine::forward_patch::ForwardPatchRequest {
            yaml: PLAYBOOK.replace("name: Ext", "name: Ext two"),
            classification: "improvement".into(),
            rationale: Some("a clearer name".into()),
            evidence: vec!["node work".into()],
        },
    )
    .unwrap()
    .version;
    let pb = root.join(".apb/playbooks").join(ID);
    let sidecar = |v: &str| {
        fs::read_dir(pb.join(v))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name() != "playbook.yaml")
            .map(|e| (e.file_name(), fs::read(e.path()).ok()))
            .collect::<Vec<_>>()
    };
    let before = (sidecar(&candidate), fs::read(pb.join("candidate")).unwrap());

    let (run_id, outcome, skipped) = start(root, None, true, opts());
    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(skipped.as_deref(), Some(external_data::CANDIDATE_SKIPPED));
    assert!(matches!(
        &events(root, &run_id)[0].payload,
        EventPayload::RunStarted { version, .. } if version == "1.0.0"
    ));
    let m = apb_engine::manifest::read(&run_dir(root, &run_id))
        .unwrap()
        .unwrap();
    assert_eq!(m.candidate_skipped.as_deref(), Some("external_data"));

    let (run_id, outcome, _) = start(root, Some(&candidate), true, opts());
    assert_eq!(outcome, RunStatus::Succeeded);
    let m = apb_engine::manifest::read(&run_dir(root, &run_id))
        .unwrap()
        .unwrap();
    assert!(!m.candidate_trial);
    let after = (sidecar(&candidate), fs::read(pb.join("candidate")).unwrap());
    assert_eq!(before, after);
}

/// A version a flagged run's patch made records the run, never becomes
/// `current`, and an unflagged start naming it is refused; a flagged one
/// may run it.
#[test]
fn a_patch_version_of_a_flagged_run_stays_with_flagged_runs() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let opts = || RunOptions {
        params: params("acme"),
        ..Default::default()
    };
    let (flagged, _, _) = start(root, None, true, opts());
    let version = apb_core::versioning::create_run_patch_version(
        root,
        ID,
        "1.0.0",
        &PLAYBOOK.replace("name: Ext", "name: Ext patched"),
        &flagged,
        "workaround",
        true,
    )
    .unwrap();
    approve(root, &version);
    let prov = apb_core::versioning::read_provenance(root, ID, &version)
        .unwrap()
        .unwrap();
    assert_eq!(prov.external_data_run.as_deref(), Some(flagged.as_str()));

    let pb = root.join(".apb/playbooks").join(ID);
    let moved = apb_core::versioning::current::move_current(
        &pb,
        &apb_core::versioning::current::MoveCurrent::new(
            &version,
            apb_core::versioning::current::MovedBy::Person {
                surface: apb_core::versioning::current::Surface::Cli,
            },
        ),
    )
    .unwrap_err();
    assert_eq!(moved.code(), external_data::RUN);
    assert_eq!(
        fs::read_to_string(pb.join("current")).unwrap().trim(),
        "1.0.0"
    );

    let refused = run(
        root,
        ID,
        Some(&version),
        RunOptions {
            params: params("acme"),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(refused.contains(external_data::RUN), "{refused}");
    let (_, outcome, _) = start(root, Some(&version), true, opts());
    assert_eq!(outcome, RunStatus::Succeeded);
}

/// A journal of a finished run of `ext` with a tool problem finding and a
/// person's rejecting review note, flagged or not.
fn crafted_run(root: &Path, id: &str, flagged: bool) -> PathBuf {
    let dir = run_dir(root, id);
    fs::create_dir_all(&dir).unwrap();
    let mut log = EventLog::create(&dir).unwrap();
    log.append(EventPayload::RunStarted {
        playbook: ID.into(),
        version: "1.0.0".into(),
        definition: None,
        external_data: flagged,
    })
    .unwrap();
    let notes: EventPayload = serde_json::from_value(serde_json::json!({
        "type": "attempt_notes", "node": "work", "attempt": 1,
        "findings": [{"kind": "tool_problem", "location": "node:work", "evidence": "the linter crashes on empty files", "why_not_fixed": "not ours"}]
    }))
    .unwrap();
    log.append(notes).unwrap();
    log.append(EventPayload::hop("work", "review", false, true, Vec::new()))
        .unwrap();
    log.append(EventPayload::review_requested(
        "review",
        vec!["approve".into(), "reject".into()],
        String::new(),
    ))
    .unwrap();
    log.append(EventPayload::review_decided(
        "review",
        "reject",
        "always use the staging bucket",
        None,
    ))
    .unwrap();
    log.append(EventPayload::RunFinished {
        outcome: "failed".into(),
    })
    .unwrap();
    apb_core::run_origin::stamp(&dir, id).unwrap();
    dir
}

fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((p.clone(), fs::read(&p).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

/// A flagged run adds nothing to the known problems store or the
/// corrections ledger, whichever path reads it; an unflagged run with the
/// same journal does.
#[test]
fn a_flagged_run_adds_nothing_to_known_problems_or_corrections() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let flagged = crafted_run(root, "ext-1", true);
    let before = tree(&root.join(".apb"));
    assert_eq!(
        apb_engine::known_problems::observe(root, &flagged).unwrap(),
        0
    );
    apb_engine::corrections::observe(root, &flagged).unwrap();
    let scan =
        apb_engine::known_problems::scan(root, apb_core::scope::Scope::Project, ID, 10).unwrap();
    assert_eq!(scan.runs, 0);
    let after: Vec<_> = tree(&root.join(".apb"))
        .into_iter()
        .filter(|(p, _)| !p.starts_with(root.join(".apb/runs")))
        .collect();
    let before: Vec<_> = before
        .into_iter()
        .filter(|(p, _)| !p.starts_with(root.join(".apb/runs")))
        .collect();
    assert_eq!(before, after);

    // The same journal unflagged is counted: the guard, not the fixture,
    // kept the stores clean.
    let plain = crafted_run(root, "ext-2", false);
    assert!(apb_engine::known_problems::observe(root, &plain).unwrap() > 0);
}

/// [`crafted_run`] as a scenario run: its manifest keeps the scenario
/// origin and, when `flagged`, carries the flag.
fn crafted_scenario_run(root: &Path, id: &str, flagged: bool) -> PathBuf {
    let dir = crafted_run(root, id, flagged);
    // A prepared run writes its config before its manifest.
    fs::write(dir.join("run.yaml"), "params: {}\n").unwrap();
    apb_engine::manifest::write(
        &dir,
        &apb_engine::manifest::RunExecutionManifest {
            external_data: flagged,
            origin: Some(RunOrigin::Scenario {
                level: apb_core::scenario::ScenarioLevel::Quick,
                scenario: "green".into(),
                scenario_digest: "sha256:x".into(),
            }),
            ..Default::default()
        },
    )
    .unwrap();
    apb_core::run_origin::stamp(&dir, id).unwrap();
    dir
}

/// A flagged scenario run adds nothing to the known problems store or the
/// corrections ledger either, while the same scenario run unflagged still
/// adds its known problem as before (a scenario run never adds
/// corrections).
#[test]
fn a_flagged_scenario_run_adds_nothing_to_known_problems_or_corrections() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let flagged = crafted_scenario_run(root, "ext-s1", true);
    let outside_runs = || -> Vec<(PathBuf, Vec<u8>)> {
        tree(&root.join(".apb"))
            .into_iter()
            .filter(|(p, _)| !p.starts_with(root.join(".apb/runs")))
            .collect()
    };
    let before = outside_runs();
    assert_eq!(
        apb_engine::known_problems::observe(root, &flagged).unwrap(),
        0
    );
    apb_engine::corrections::observe(root, &flagged).unwrap();
    assert_eq!(before, outside_runs());

    let plain = crafted_scenario_run(root, "ext-s2", false);
    assert!(apb_engine::known_problems::observe(root, &plain).unwrap() > 0);
}

/// Tools that learn from one run refuse a flagged run: a forward patch,
/// judge values and examples, and a scenario capture.
#[test]
fn tools_that_learn_from_one_run_refuse_a_flagged_run() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let (flagged, _, _) = start(
        root,
        None,
        true,
        RunOptions {
            params: params("acme"),
            ..Default::default()
        },
    );
    let forward = apb_engine::forward_patch::create(
        root,
        &flagged,
        &apb_engine::forward_patch::ForwardPatchRequest {
            yaml: PLAYBOOK.replace("name: Ext", "name: Ext two"),
            classification: "improvement".into(),
            rationale: None,
            evidence: Vec::new(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(forward.contains(external_data::RUN), "{forward}");
    assert!(
        !root
            .join(".apb/playbooks")
            .join(ID)
            .join("candidate")
            .exists()
    );

    let values = apb_engine::judge_eval::example::run_values(root, &flagged, "work", None)
        .unwrap_err()
        .to_string();
    assert!(values.contains(external_data::RUN), "{values}");
    let examples = || tree(&root.join(".apb/playbooks").join(ID));
    let before = examples();
    let saved = apb_engine::judge_eval::example::save_from_run(
        root,
        &flagged,
        "work",
        &apb_engine::judge_eval::example::ExampleSaveRequest {
            execution: None,
            expected: BTreeMap::new(),
            note: None,
        },
        "person",
    )
    .unwrap_err()
    .to_string();
    assert!(saved.contains(external_data::RUN), "{saved}");
    assert_eq!(before, examples());

    let capture = apb_engine::scenario::capture::capture(
        root,
        &run_dir(root, &flagged),
        &apb_engine::scenario::capture::CaptureRequest {
            name: "flagged".into(),
            nodes: Vec::new(),
        },
    );
    match capture {
        Err(apb_engine::scenario::capture::CaptureError::Engine(e)) => {
            assert!(e.to_string().contains(external_data::RUN), "{e}")
        }
        _ => panic!("a flagged run was captured"),
    }
}

/// The decisions report and replay read no flagged run and say how many
/// they left out.
#[test]
fn the_decisions_report_skips_flagged_runs() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    crafted_run(root, "ext-1", true);
    crafted_run(root, "ext-2", false);
    let (dirs, skipped) = apb_engine::decision::report::report_dirs(&[root.to_path_buf()]);
    assert_eq!(skipped, 1);
    assert_eq!(dirs, vec![run_dir(root, "ext-2")]);
    let report = apb_engine::decision::report::report(
        &[root.to_path_buf()],
        &Default::default(),
        &Default::default(),
    );
    assert_eq!(report.skipped_external, 1);
}

/// Two unflagged finished runs and one flagged one of `ext` under `root`;
/// returns the flagged run's id.
fn two_and_one_flagged(root: &Path) -> String {
    let opts = || RunOptions {
        params: params("acme"),
        ..Default::default()
    };
    start(root, None, false, opts());
    start(root, None, false, opts());
    start(root, None, true, opts()).0
}

/// `apb stats` and the estimate's history count the two unflagged
/// finished runs, never the flagged one.
#[test]
fn readers_of_earlier_runs_skip_a_flagged_run() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let flagged = two_and_one_flagged(root);
    let history = apb_engine::run_history::runs_of(&[root.to_path_buf()], ID, None, 50);
    assert_eq!(history.len(), 2);
    assert!(!history.contains(&run_dir(root, &flagged)));
    let all = apb_engine::run_history::all_runs_of(&[root.to_path_buf()], ID, None);
    assert_eq!(all.len(), 2);
    let stats = apb_engine::run_stats::stats(
        &[root.to_path_buf()],
        &apb_engine::run_stats::StatsFilter {
            playbook: Some(ID.into()),
            ..Default::default()
        },
    );
    assert_eq!(stats.versions.iter().map(|v| v.runs).sum::<usize>(), 2);
}

/// The estimate before a start compares with the two unflagged runs only.
#[test]
fn the_estimate_skips_a_flagged_run() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    two_and_one_flagged(root);
    let estimate = apb_engine::run_estimate::for_playbook(root, ID, None, "cli").unwrap();
    assert_eq!(estimate.comparable.runs, 2, "{:?}", estimate.comparable);
}

/// A grade backfill grades the two unflagged runs, never the flagged one.
#[test]
fn a_grade_backfill_skips_a_flagged_run() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let flagged = two_and_one_flagged(root);
    let graded =
        apb_engine::grade::backfill(root, ID, None, 10, apb_engine::grade::GraderChoice::Rules);
    let graded: Vec<&str> = graded.iter().map(|b| b.run_id.as_str()).collect();
    assert_eq!(graded.len(), 2, "{graded:?}");
    assert!(!graded.contains(&flagged.as_str()), "{graded:?}");
}

/// The retrospective of a later run compares it with the two unflagged
/// runs only.
#[test]
fn the_retro_baseline_skips_a_flagged_run() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let flagged = two_and_one_flagged(root);
    let (last, _, _) = start(
        root,
        None,
        false,
        RunOptions {
            params: params("acme"),
            ..Default::default()
        },
    );
    let retro = apb_engine::run_retro::retro(root, &run_dir(root, &last), 10).unwrap();
    let baseline = retro.baseline.expect("a baseline");
    assert_eq!(baseline.run_ids.len(), 2, "{:?}", baseline.run_ids);
    assert!(!baseline.run_ids.contains(&flagged));
}

/// A cache-eligible node of a flagged run adds no record or object under
/// `.apb/cache`, and a later unflagged run of the same node misses.
#[test]
fn a_flagged_run_adds_nothing_to_the_node_cache() {
    let _env = crate::common::env_lock();
    use std::process::Command;
    const CACHED: &str = r#"
schema: 2
id: cached
name: Cached
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: lint, type: script, script: "scripts/lint.sh", runner: sh, cache: auto }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: lint }
  - { from: lint, to: done }
"#;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/work.txt"), "hello\n").unwrap();
    fs::write(root.join(".gitignore"), ".apb/\n").unwrap();
    init_project(root).unwrap();
    let v = root.join(".apb/playbooks/cached/1.0.0");
    fs::create_dir_all(v.join("scripts")).unwrap();
    fs::write(v.join("playbook.yaml"), CACHED).unwrap();
    crate::common::write_sync(&v.join("scripts/lint.sh"), "echo linted\n");
    fs::write(root.join(".apb/playbooks/cached/current"), "1.0.0").unwrap();
    let git = |args: &[&str]| {
        let mut full = vec!["-C", root.to_str().unwrap(), "-c", "commit.gpgsign=false"];
        full.extend_from_slice(args);
        assert!(
            Command::new("git")
                .args(&full)
                .output()
                .unwrap()
                .status
                .success()
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "c1"]);

    let cache_files = || tree(&root.join(".apb/cache"));
    let before = cache_files();
    let flagged = run(
        root,
        "cached",
        None,
        RunOptions {
            external_data: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(flagged.outcome, RunStatus::Succeeded);
    let ev = events(root, &flagged.run_id);
    assert!(
        !ev.iter()
            .any(|e| matches!(e.payload, EventPayload::NodeCacheStored { .. }))
    );
    assert!(
        ev.iter()
            .any(|e| matches!(e.payload, EventPayload::NodeCacheRejected { .. }))
    );
    assert_eq!(before, cache_files());

    let plain = run(root, "cached", None, RunOptions::default()).unwrap();
    let ev = events(root, &plain.run_id);
    assert!(
        ev.iter()
            .any(|e| matches!(e.payload, EventPayload::NodeCacheMiss { .. }))
    );
    assert!(
        !ev.iter()
            .any(|e| matches!(e.payload, EventPayload::NodeCacheHit { .. }))
    );
}

/// A sub-playbook child of a flagged run is flagged whatever its start
/// asked: its manifest and its `run_started` carry the flag.
#[test]
fn a_child_of_a_flagged_run_is_flagged() {
    let _env = crate::common::env_lock();
    const PARENT: &str = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: child }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n";
    const CHILD: &str = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: f }\n";
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    for (id, yaml) in [("parent", PARENT), ("child", CHILD)] {
        let v = root.join(".apb/playbooks").join(id).join("1.0.0");
        fs::create_dir_all(&v).unwrap();
        fs::write(v.join("playbook.yaml"), yaml).unwrap();
        fs::write(
            root.join(".apb/playbooks").join(id).join("current"),
            "1.0.0",
        )
        .unwrap();
    }
    let res = run(
        root,
        "parent",
        None,
        RunOptions {
            external_data: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let child = events(root, &res.run_id)
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id),
            _ => None,
        })
        .unwrap();
    let child_dir = run_dir(root, &child);
    assert!(
        apb_engine::manifest::read(&child_dir)
            .unwrap()
            .unwrap()
            .external_data
    );
    assert!(matches!(
        &events(root, &child)[0].payload,
        EventPayload::RunStarted {
            external_data: true,
            ..
        }
    ));
}

/// The run report page says a flagged run keeps nothing for learning and
/// shows no `+external` suffix; an unflagged run's page says nothing.
#[test]
fn the_report_page_shows_the_flag() {
    let _env = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let opts = || RunOptions {
        params: params("acme"),
        ..Default::default()
    };
    let (flagged, _, _) = start(root, None, true, opts());
    let (plain, _, _) = start(root, None, false, opts());
    let page = apb_engine::report_html::render(root, &run_dir(root, &flagged)).unwrap();
    assert!(page.contains("keeps nothing for learning"));
    assert!(!page.contains("+external"));
    let page = apb_engine::report_html::render(root, &run_dir(root, &plain)).unwrap();
    assert!(!page.contains("keeps nothing for learning"));
}
