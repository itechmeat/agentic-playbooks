//! Run quality on the tool layer every surface shares (`tools::grade`,
//! `tools::candidate`): a grade stored through it re-decides the
//! playbook's candidate, a person's reason is kept as one clean line, a
//! run step is refused every grading and deciding call, and a stored grade
//! of a playbook without a candidate touches nothing of the playbook. The
//! comparison itself is the engine's (`candidate_quality_test`).

use std::fs;
use std::path::{Path, PathBuf};

use apb_core::quality::compare::Verdict;
use apb_core::quality::grade::{Cite, Level};
use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::step_env::StepCaller;
use apb_engine::candidate::PersonDecision;
use apb_engine::forward_patch::{ForwardPatchRequest, create};
use apb_engine::grade::SubmitOutcome;
use apb_engine::grade::evidence::Proposal;
use apb_engine::grade::reader::ReadRequest;
use apb_engine::scheduler::{RunOptions, run};
use apb_mcp::tools::ToolError;
use apb_mcp::tools::candidate::{candidate_decide, candidate_view};
use apb_mcp::tools::grade::{
    after_stored, backfill, grade_by_rules, grade_read, grade_reopen, grade_start, grade_submit,
};

use crate::common::config_sandbox;

const ID: &str = "trial";

const OWN: &str = "quality:\n  criteria:\n    - id: report-clear\n      reads: [outputs]\n      levels: { good: \"the report names every change\", partial: \"the report misses one change\", poor: \"the report is wrong\" }\n";

fn playbook(script: &str) -> String {
    format!(
        r#"
schema: 2
id: trial
name: Trial
version: 1.0.0
supervisor:
  policy:
    promote_supervisor_patches: {{ when: quality, min_runs: 3, max_trials: 4 }}
{OWN}
nodes:
  - {{ id: start, type: start }}
  - {{ id: p1, type: script, script: "scripts/{script}", runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: p1 }}
  - {{ from: p1, to: done }}
"#
    )
}

fn seed(root: &Path) {
    apb_core::run_origin::ensure_key().unwrap();
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks").join(ID).join("1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), playbook("ok.sh")).unwrap();
    fs::write(dir.join("scripts/ok.sh"), "echo changed the parser\n").unwrap();
    fs::write(dir.join("scripts/also.sh"), "echo changed the parser too\n").unwrap();
    fs::write(
        root.join(".apb/playbooks").join(ID).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// A run started the way every surface starts one (the gate may pick the
/// candidate), the base version trusted.
fn start(root: &Path) -> String {
    let loaded = apb_core::registry::Registry::open(root)
        .unwrap()
        .load(ID, Some("1.0.0"))
        .unwrap();
    apb_core::trust::TrustStore::load()
        .approve(
            &loaded.trust_digest().unwrap(),
            ID,
            apb_core::trust::OriginKind::LocallyApproved,
        )
        .unwrap();
    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: ID.into(),
        version: None,
    };
    let permit = apb_engine::gate::check_run(root, &wref, true, false).unwrap();
    let version = permit.run_version(None);
    let mut opts = RunOptions::default();
    permit.apply(&mut opts);
    run(root, ID, version.as_deref(), opts).unwrap().run_id
}

fn forward(root: &Path, run_id: &str) -> String {
    create(
        root,
        run_id,
        &ForwardPatchRequest {
            yaml: playbook("also.sh"),
            classification: "improvement".into(),
            rationale: Some("p1 repeats work".into()),
            evidence: vec!["node p1".into()],
        },
    )
    .unwrap()
    .version
}

fn pointer(root: &Path, name: &str) -> Option<String> {
    fs::read_to_string(root.join(".apb/playbooks").join(ID).join(name))
        .ok()
        .map(|s| s.trim().to_string())
}

/// A person's or a host's process: not a run step.
fn person() -> StepCaller {
    StepCaller::default()
}

/// Grades `run_id` on `report-clear` through the surface layer, as MCP
/// does: read the node's output, cite it.
fn host_grade(root: &Path, run_id: &str) -> apb_mcp::tools::grade::Submitted {
    let session = grade_start(root, run_id, None, &person()).unwrap().session;
    let read = ReadRequest::Read {
        r#ref: "o:p1".into(),
        span: None,
    };
    grade_read(root, run_id, &session, &read, &person()).unwrap();
    let submitted = grade_submit(
        root,
        run_id,
        &session,
        &[Proposal {
            criterion: "report-clear".into(),
            level: Level::Good,
            reason_code: "names_changes".into(),
            why: "the report names the change".into(),
            cites: vec![Cite::to("o:p1")],
            absence: false,
        }],
        &person(),
    )
    .unwrap();
    assert!(matches!(submitted.outcome, SubmitOutcome::Graded { .. }));
    submitted
}

/// Three graded runs of `current`, a forward patch, and `trials` trial
/// runs of it; returns the candidate version and the trial run ids.
fn line_with_trials(root: &Path, trials: usize) -> (String, Vec<String>) {
    seed(root);
    let mut base = String::new();
    for _ in 0..3 {
        base = start(root);
        host_grade(root, &base);
    }
    let version = forward(root, &base);
    let runs = (0..trials).map(|_| start(root)).collect();
    (version, runs)
}

#[test]
fn a_grade_stored_through_the_surface_re_decides_the_candidate() {
    let _cfg = config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (version, trials) = line_with_trials(root, 1);
    let view = candidate_view(root, ID).unwrap().expect("a candidate");
    assert_eq!((view.version.as_str(), view.trials), (version.as_str(), 1));
    // Both a host grade and a rules grade go through `after_stored`.
    let host = host_grade(root, &trials[0]).after;
    assert_eq!(host.candidate.expect("re-decided").verdict, Verdict::Wait);
    assert_eq!(host.candidate_error, None);
    let rules = grade_by_rules(root, &trials[0], &person()).unwrap().after;
    assert_eq!(rules.candidate.expect("re-decided").verdict, Verdict::Wait);
}

#[test]
fn a_persons_reason_is_kept_as_one_clean_line() {
    let _cfg = config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    line_with_trials(root, 1);
    let rec = candidate_decide(
        root,
        ID,
        PersonDecision::KeepCurrent {
            reason: Some("  the old report\nreads\u{202e} better ".into()),
        },
        "dashboard",
        &person(),
    )
    .unwrap();
    assert_eq!(rec.verdict, Verdict::KeepCurrent);
    assert_eq!(
        (rec.reason.as_str(), rec.decided_by.as_str()),
        ("the old report reads better", "dashboard")
    );
    assert_eq!(pointer(root, "candidate"), None);
    let again = candidate_decide(root, ID, PersonDecision::Promote, "cli", &person()).unwrap_err();
    assert!(matches!(again, ToolError::NotFound(_)), "{again:?}");
}

#[test]
fn a_run_step_is_refused_every_grading_and_deciding_call() {
    let _cfg = config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (_, trials) = line_with_trials(root, 1);
    let run = &trials[0];
    for step in [
        StepCaller {
            run_id: Some("r-step".into()),
            host_task: None,
        },
        StepCaller {
            run_id: None,
            host_task: Some("t-1".into()),
        },
    ] {
        let refused = |r: Result<(), ToolError>| {
            let e = r.unwrap_err();
            assert!(matches!(e, ToolError::Conflict(_)), "{e:?}");
            assert!(e.to_string().contains("run step"), "{e}");
        };
        refused(grade_by_rules(root, run, &step).map(drop));
        refused(grade_start(root, run, None, &step).map(drop));
        refused(grade_read(root, run, "s", &ReadRequest::Outline, &step).map(drop));
        refused(grade_submit(root, run, "s", &[], &step).map(drop));
        refused(grade_reopen(root, run, "cli", &step).map(drop));
        refused(backfill(root, ID, None, 5, &step).map(drop));
        refused(candidate_decide(root, ID, PersonDecision::Promote, "cli", &step).map(drop));
    }
    // Nothing was decided: the candidate is still on trial.
    assert!(pointer(root, "candidate").is_some());
}

#[test]
fn a_host_graded_run_is_graded_again_only_after_a_person_reopens_it() {
    let _cfg = config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let run = start(root);
    host_grade(root, &run);
    let again = grade_start(root, &run, None, &person()).unwrap_err();
    assert!(matches!(again, ToolError::Conflict(_)), "{again:?}");
    grade_reopen(root, &run, "cli", &person()).unwrap();
    host_grade(root, &run);
}

#[test]
fn a_grade_without_a_candidate_leaves_the_playbook_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    init_project(&root).unwrap();
    let after = after_stored(&root, "never-saved");
    assert_eq!(after, Default::default());
    assert!(!root.join(".apb/playbooks/never-saved").exists());
    let refused = backfill(&root, "../escape", None, 5, &person()).unwrap_err();
    assert!(matches!(refused, ToolError::NotFound(_)));
    assert!(matches!(
        candidate_view(&root, "../escape").unwrap_err(),
        ToolError::NotFound(_)
    ));
    // Nothing ran, so nothing to backfill and nothing re-decided.
    let report = backfill(&root, "never-saved", None, 5, &person()).unwrap();
    assert!(report.runs.is_empty());
    assert_eq!(report.after, Default::default());
}
