//! Forward patches and candidate trials (issue #192), end to end through the
//! engine: a forward patch made from a finished run becomes the candidate,
//! the next start runs it (the gate picks and pins it), and the trial's
//! outcome promotes or rejects it, journaled right before `run_finished`.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::versioning::read_provenance;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::forward_patch::{ForwardPatchRequest, create};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const ID: &str = "trial";

/// `{policy}` goes under `supervisor.policy`; `{p1}` is the first node's
/// script.
fn playbook(policy: &str, p1: &str) -> String {
    format!(
        r#"
schema: 2
id: trial
name: Trial
version: 1.0.0
supervisor:
  policy:
{policy}
nodes:
  - {{ id: start, type: start }}
  - {{ id: p1, type: script, script: "scripts/{p1}", runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: p1 }}
  - {{ from: p1, to: done }}
"#
    )
}

fn seed(root: &Path, policy: &str) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks").join(ID).join("1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), playbook(policy, "ok.sh")).unwrap();
    fs::write(dir.join("scripts/ok.sh"), "echo ok\n").unwrap();
    fs::write(dir.join("scripts/also-ok.sh"), "echo also ok\n").unwrap();
    fs::write(dir.join("scripts/fail.sh"), "echo broken 1>&2\nexit 1\n").unwrap();
    fs::write(
        root.join(".apb/playbooks").join(ID).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// Starts a run the way every surface does: the gate (which may pick the
/// candidate), its permit applied, the version it chose.
fn start(root: &Path) -> (String, RunStatus, Option<String>) {
    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: ID.into(),
        version: None,
    };
    let permit = apb_engine::gate::check_run(root, &wref, true, false).unwrap();
    let chosen = permit.candidate.clone();
    let version = permit.run_version(None);
    let mut opts = RunOptions::default();
    permit.apply(&mut opts);
    let res = run(root, ID, version.as_deref(), opts).unwrap();
    (res.run_id, res.outcome, chosen)
}

fn pointer(root: &Path, name: &str) -> Option<String> {
    fs::read_to_string(root.join(".apb/playbooks").join(ID).join(name))
        .ok()
        .map(|s| s.trim().to_string())
}

fn forward(root: &Path, run_id: &str, yaml: &str) -> String {
    create(
        root,
        run_id,
        &ForwardPatchRequest {
            yaml: yaml.to_string(),
            classification: "improvement".into(),
            rationale: Some("p1 is slow in every run".into()),
            evidence: vec!["seq 3: p1 took 38 min".into(), "node p1".into()],
        },
    )
    .unwrap()
    .version
}

fn events_of(root: &Path, run_id: &str) -> Vec<EventPayload> {
    read_all(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

#[test]
fn a_forward_patch_changes_an_executed_node_and_the_next_run_promotes_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "    promote_supervisor_patches: on_success");

    let (first, outcome, chosen) = start(root);
    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(chosen, None);

    // p1 already ran in `first`: a forward patch may change it anyway.
    let version = forward(
        root,
        &first,
        &playbook("    promote_supervisor_patches: on_success", "also-ok.sh"),
    );
    assert_eq!(version, "1.0.1");
    assert_eq!(pointer(root, "candidate").as_deref(), Some("1.0.1"));
    assert_eq!(pointer(root, "current").as_deref(), Some("1.0.0"));
    let prov = read_provenance(root, ID, &version).unwrap().unwrap();
    assert_eq!(prov.created_by, "supervisor");
    assert_eq!(prov.run_id.as_deref(), Some(first.as_str()));
    assert_eq!(prov.classification.as_deref(), Some("improvement"));
    assert_eq!(prov.scope.as_deref(), Some("next_runs"));
    assert_eq!(prov.base_version.as_deref(), Some("1.0.0"));
    assert_eq!(prov.evidence.len(), 2);
    assert!(prov.rationale.is_some());

    // The run the patch came from is untouched: no migration, same version.
    let first_events = events_of(root, &first);
    assert!(!first_events.iter().any(|e| matches!(
        e,
        EventPayload::PatchApplied { .. } | EventPayload::RunMigrated { .. }
    )));

    // The next start runs the candidate and its success promotes it.
    let (second, outcome, chosen) = start(root);
    assert_eq!(outcome, RunStatus::Succeeded);
    assert_eq!(chosen.as_deref(), Some("1.0.1"));
    let events = events_of(root, &second);
    assert!(matches!(
        &events[0],
        EventPayload::RunStarted { version, .. } if version == "1.0.1"
    ));
    let n = events.len();
    assert!(
        matches!(&events[n - 2], EventPayload::CandidatePromoted { version, run_id, successes: 1 } if version == "1.0.1" && *run_id == second),
        "{:?}",
        &events[n - 2..]
    );
    assert!(matches!(events[n - 1], EventPayload::RunFinished { .. }));
    assert_eq!(pointer(root, "current").as_deref(), Some("1.0.1"));
    assert_eq!(pointer(root, "candidate"), None);
    let trial = read_provenance(root, ID, "1.0.1")
        .unwrap()
        .unwrap()
        .trial
        .unwrap();
    assert_eq!(trial.outcome.as_deref(), Some("promoted"));
    assert_eq!(trial.run_id.as_deref(), Some(second.as_str()));

    let summary = apb_engine::candidate::trial_of(
        &root.join(".apb/runs").join(&second),
        &read_all(&root.join(".apb/runs").join(&second)).unwrap(),
    )
    .unwrap();
    assert_eq!(summary.verdict, "promoted");
    assert!(
        apb_engine::candidate::trial_of(
            &root.join(".apb/runs").join(&first),
            &read_all(&root.join(".apb/runs").join(&first)).unwrap()
        )
        .is_none()
    );
    // `apb runs` and the dashboard list read the same verdict.
    let listed = apb_engine::list_runs(root).unwrap();
    let trial_of = |id: &str| {
        listed
            .iter()
            .find(|r| r.run_id == id)
            .unwrap()
            .candidate_trial
            .clone()
    };
    assert_eq!(trial_of(&second).unwrap().verdict, "promoted");
    assert!(trial_of(&first).is_none());
}

#[test]
fn a_failed_trial_rejects_the_candidate_and_the_next_run_uses_current() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "    promote_supervisor_patches: on_success");
    let (first, _, _) = start(root);
    forward(
        root,
        &first,
        &playbook("    promote_supervisor_patches: on_success", "fail.sh"),
    );

    let (second, outcome, chosen) = start(root);
    assert_eq!(chosen.as_deref(), Some("1.0.1"));
    assert_eq!(outcome, RunStatus::Failed);
    let events = events_of(root, &second);
    let n = events.len();
    assert!(
        matches!(&events[n - 2], EventPayload::CandidateRejected { version, run_id, .. } if version == "1.0.1" && *run_id == second),
        "{:?}",
        &events[n - 2..]
    );
    assert_eq!(pointer(root, "candidate"), None);
    assert_eq!(pointer(root, "current").as_deref(), Some("1.0.0"));
    let trial = read_provenance(root, ID, "1.0.1")
        .unwrap()
        .unwrap()
        .trial
        .unwrap();
    assert_eq!(trial.outcome.as_deref(), Some("rejected"));

    let (_, outcome, chosen) = start(root);
    assert_eq!(chosen, None);
    assert_eq!(outcome, RunStatus::Succeeded);
}

#[test]
fn after_n_successes_promotes_on_the_nth_trial() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let policy = "    promote_supervisor_patches: { after_n_successes: 2 }";
    seed(root, policy);
    let (first, _, _) = start(root);
    forward(root, &first, &playbook(policy, "also-ok.sh"));

    let (second, _, chosen) = start(root);
    assert_eq!(chosen.as_deref(), Some("1.0.1"));
    assert!(
        !events_of(root, &second)
            .iter()
            .any(|e| matches!(e, EventPayload::CandidatePromoted { .. }))
    );
    let summary = apb_engine::candidate::trial_of(
        &root.join(".apb/runs").join(&second),
        &read_all(&root.join(".apb/runs").join(&second)).unwrap(),
    )
    .unwrap();
    assert_eq!(summary.verdict, "passed");
    assert_eq!(pointer(root, "current").as_deref(), Some("1.0.0"));

    let (third, _, chosen) = start(root);
    assert_eq!(chosen.as_deref(), Some("1.0.1"));
    assert!(
        events_of(root, &third)
            .iter()
            .any(|e| matches!(e, EventPayload::CandidatePromoted { successes: 2, .. }))
    );
    assert_eq!(pointer(root, "current").as_deref(), Some("1.0.1"));
}

#[test]
fn trial_candidates_off_keeps_starts_on_current() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let policy = "    trial_candidates: \"off\"";
    seed(root, policy);
    let (first, _, _) = start(root);
    forward(root, &first, &playbook(policy, "also-ok.sh"));
    assert_eq!(pointer(root, "candidate").as_deref(), Some("1.0.1"));

    let (second, _, chosen) = start(root);
    assert_eq!(chosen, None);
    assert!(
        apb_engine::candidate::trial_of(
            &root.join(".apb/runs").join(&second),
            &read_all(&root.join(".apb/runs").join(&second)).unwrap(),
        )
        .is_none()
    );
    assert_eq!(pointer(root, "candidate").as_deref(), Some("1.0.1"));
}

#[test]
fn a_newer_forward_patch_replaces_the_candidate_and_keeps_the_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let policy = "    promote_supervisor_patches: manual";
    seed(root, policy);
    let (first, _, _) = start(root);
    forward(root, &first, &playbook(policy, "also-ok.sh"));
    // A manual policy: the trial passes, the candidate stays.
    let (second, _, chosen) = start(root);
    assert_eq!(chosen.as_deref(), Some("1.0.1"));
    assert_eq!(pointer(root, "candidate").as_deref(), Some("1.0.1"));
    // A forward patch from the trial run builds on the candidate.
    let newer = forward(root, &second, &playbook(policy, "ok.sh"));
    assert_eq!(newer, "1.0.2");
    assert_eq!(pointer(root, "candidate").as_deref(), Some("1.0.2"));
    let prov = read_provenance(root, ID, "1.0.2").unwrap().unwrap();
    assert_eq!(prov.base_version.as_deref(), Some("1.0.1"));
    let old = read_provenance(root, ID, "1.0.1")
        .unwrap()
        .unwrap()
        .trial
        .unwrap();
    assert_eq!(old.outcome.as_deref(), Some("superseded"));
    assert_eq!(old.successes, 1);
}

#[test]
fn a_goal_criterion_that_does_not_hold_rejects_the_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let policy = "    promote_supervisor_patches: on_success";
    let with_goal = |script: &str| {
        playbook(policy, script).replace(
            "version: 1.0.0\n",
            "version: 1.0.0\ngoal:\n  statement: \"say hello\"\n  criteria:\n    - description: \"hello is said\"\n      check: { type: marker, marker: \"hello\" }\n",
        )
    };
    seed(root, policy);
    let v1 = root.join(".apb/playbooks").join(ID).join("1.0.0");
    fs::write(v1.join("scripts/hello.sh"), "echo hello\n").unwrap();
    fs::write(v1.join("playbook.yaml"), with_goal("hello.sh")).unwrap();
    let (first, outcome, _) = start(root);
    assert_eq!(outcome, RunStatus::Succeeded);
    forward(root, &first, &with_goal("ok.sh"));

    // The criterion is not enforced, so the run succeeds, but the
    // candidate does not hold the goal.
    let (second, outcome, chosen) = start(root);
    assert_eq!(chosen.as_deref(), Some("1.0.1"));
    assert_eq!(outcome, RunStatus::Succeeded);
    assert!(events_of(root, &second).iter().any(|e| matches!(
        e,
        EventPayload::CandidateRejected { reason, .. } if reason.contains("hello is said")
    )));
    assert_eq!(pointer(root, "candidate"), None);
    assert_eq!(pointer(root, "current").as_deref(), Some("1.0.0"));
}
