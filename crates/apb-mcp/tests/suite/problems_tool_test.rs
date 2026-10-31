//! Known problems on the tool layer every surface shares
//! (`tools::problems`): a backfill scan collects them, the view tells what
//! is shown and what changed, a person confirms, dismisses, reopens and
//! closes with a quote of new source text, a quote that is not new is
//! refused, and a run step is refused every act that writes the store
//! while reading stays allowed.

use std::fs;
use std::path::Path;

use apb_core::known_problems::{ProblemStatus, RecheckVerdict, ScopeKind};
use apb_core::registry::init_project;
use apb_core::scope::Scope;
use apb_core::step_env::StepCaller;
use apb_engine::known_problems::{ProblemAction, RecheckBy, SourceNow};
use apb_mcp::tools::ToolError;
use apb_mcp::tools::problems::{
    Target, outside_host_task, problem_decide, problem_detail, problem_recheck, problems_scan,
    problems_view, proposal_of, recheck_changed_by_model,
};

use crate::common::config_sandbox;

const EVENTS: &str = include_str!("../../../apb-engine/tests/fixtures/known_problems/events.jsonl");
const ID: &str = "fix-build";
const BUILD_SH: &str = "#!/bin/sh\nset -e\ncc -Werror=deprecated -c vendor/zlib.c\n";
const FIXED_LINE: &str = "cc -Wno-error=deprecated -c vendor/zlib.c";

fn person() -> StepCaller {
    StepCaller::default()
}

fn steps() -> [StepCaller; 2] {
    [
        StepCaller {
            run_id: Some("r-step".into()),
            host_task: None,
        },
        StepCaller {
            run_id: None,
            host_task: Some("t-1".into()),
        },
    ]
}

/// A project with the `fix-build` playbook (the block on), its build
/// script, and two ended runs whose attempts reported the same problems.
fn project(root: &Path) {
    apb_core::run_origin::ensure_key().unwrap();
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks").join(ID).join("1.0.0");
    fs::create_dir_all(&dir).unwrap();
    let yaml = format!(
        "schema: 2\nid: {ID}\nname: Fix build\nversion: 1.0.0\nknown_problems: {{ min_runs: 2 }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: fix, type: agent_task, prompt: \"Fix the build\", profile: main }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: fix }}\n  - {{ from: fix, to: done }}\n"
    );
    fs::write(dir.join("playbook.yaml"), &yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(ID).join("current"),
        "1.0.0",
    )
    .unwrap();
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::write(root.join("scripts/build.sh"), BUILD_SH).unwrap();
    for run in ["fix-build-1", "fix-build-2"] {
        let d = root.join(".apb/runs").join(run);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("events.jsonl"), EVENTS).unwrap();
        fs::write(d.join("playbook.yaml"), &yaml).unwrap();
        apb_core::run_origin::stamp(&d, run).unwrap();
    }
}

fn target(root: &Path) -> Target {
    Target::resolve(root, ID, None).unwrap()
}

/// The id of the problem the build script is the source of.
fn build_problem(root: &Path) -> String {
    problems_view(root, &target(root))
        .unwrap()
        .problems
        .into_iter()
        .find(|p| p.problem.scope.kind == ScopeKind::Path)
        .expect("the build-script problem")
        .problem
        .id
}

fn status(root: &Path, id: &str) -> ProblemStatus {
    problem_detail(root, &target(root), id)
        .unwrap()
        .problem
        .problem
        .status
}

#[test]
fn a_scan_collects_and_a_person_decides_and_closes_with_new_text() {
    let _cfg = config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    project(root);
    let t = target(root);
    assert_eq!(t.scope, Scope::Project);
    let report = problems_scan(root, &t, 10, &person()).unwrap();
    assert_eq!((report.runs, report.sightings > 0), (2, true));
    // Scanning the same runs again counts nothing twice.
    problems_scan(root, &t, 10, &person()).unwrap();
    let id = build_problem(root);
    let detail = problem_detail(root, &t, &id).unwrap();
    let p = &detail.problem;
    assert_eq!(p.problem.runs, 2);
    assert!(p.significant && !p.needs_look);
    assert_eq!(p.source_now, Some(SourceNow::Unchanged));
    assert!(detail.changed_passages.is_empty());

    let confirmed = problem_decide(
        root,
        &t,
        &id,
        ProblemAction::Confirm,
        Some(" seen\nagain "),
        "mcp",
        &person(),
    )
    .unwrap();
    let decided = confirmed.confirmed.expect("confirmed");
    assert_eq!(
        (decided.by.as_str(), decided.reason.as_str()),
        ("mcp", "seen again")
    );
    problem_decide(
        root,
        &t,
        &id,
        ProblemAction::Dismiss,
        None,
        "mcp",
        &person(),
    )
    .unwrap();
    assert_eq!(status(root, &id), ProblemStatus::Dismissed);
    problem_decide(root, &t, &id, ProblemAction::Reopen, None, "mcp", &person()).unwrap();
    assert_eq!(status(root, &id), ProblemStatus::Open);

    // The script changes: the view says so and lists the new passage.
    fs::write(
        root.join("scripts/build.sh"),
        format!("#!/bin/sh\nset -e\n{FIXED_LINE}\n"),
    )
    .unwrap();
    let detail = problem_detail(root, &t, &id).unwrap();
    assert_eq!(detail.problem.source_now, Some(SourceNow::Changed));
    assert!(
        detail.problem.needs_look,
        "a changed source waits for a look"
    );
    assert!(
        detail.changed_passages.iter().any(|q| q.text == FIXED_LINE),
        "{:?}",
        detail.changed_passages
    );
    // A quote the old text has too proves nothing.
    let old = problem_decide(
        root,
        &t,
        &id,
        ProblemAction::Close {
            quote: "#!/bin/sh\nset -e".into(),
        },
        None,
        "mcp",
        &person(),
    )
    .unwrap_err();
    assert!(old.to_string().contains("quote_not_new"), "{old}");
    assert_eq!(status(root, &id), ProblemStatus::Open);
    let closed = problem_decide(
        root,
        &t,
        &id,
        ProblemAction::Close {
            quote: FIXED_LINE.into(),
        },
        None,
        "mcp",
        &person(),
    )
    .unwrap();
    assert_eq!(closed.status, ProblemStatus::Closed);
    let check = closed.check.expect("the close's recheck");
    assert_eq!(check.verdict, RecheckVerdict::Closed);
    assert_eq!(check.quote.expect("the quote").text, FIXED_LINE);
}

#[test]
fn a_hosts_close_waits_for_a_person_and_a_model_recheck_needs_the_use_on() {
    let _cfg = config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    project(root);
    let t = target(root);
    problems_scan(root, &t, 10, &person()).unwrap();
    let id = build_problem(root);
    fs::write(
        root.join("scripts/build.sh"),
        format!("#!/bin/sh\nset -e\n{FIXED_LINE}\n"),
    )
    .unwrap();
    // No decision model is configured: a recheck of every changed source is
    // refused up front, and so is a model recheck of one.
    let e = recheck_changed_by_model(root, &t, "cli", &person()).unwrap_err();
    assert!(
        matches!(&e, ToolError::Conflict(m) if m.contains("no decision model is on for problem_recheck")),
        "{e:?}"
    );
    let e = problem_recheck(root, &t, &id, RecheckBy::Model, "mcp", &person()).unwrap_err();
    assert!(
        matches!(&e, ToolError::Conflict(m) if m.contains("no decision model is on for problem_recheck")),
        "{e:?}"
    );
    let host = proposal_of(
        "closed",
        Some(FIXED_LINE.into()),
        Some("flag dropped".into()),
    )
    .unwrap();
    let rec = problem_recheck(root, &t, &id, RecheckBy::Host(host), "mcp", &person()).unwrap();
    assert_eq!(rec.verdict, RecheckVerdict::ClosedProposed);
    let view = problem_detail(root, &t, &id).unwrap().problem;
    assert_eq!(view.problem.status, ProblemStatus::Open);
    assert!(view.needs_look);
    // The host's recheck was against the new text: nothing is left to
    // recheck, so nothing is asked.
    let all = recheck_changed_by_model(root, &t, "cli", &person()).unwrap();
    assert!(all.is_empty(), "{all:?}");
}

#[test]
fn a_scan_reads_only_runs_of_the_scope_it_names() {
    let _cfg = config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    project(root);
    let global = Target::resolve(root, ID, Some("global")).unwrap();
    let report = problems_scan(root, &global, 10, &person()).unwrap();
    assert_eq!((report.runs, report.sightings), (0, 0));
    assert!(
        problems_view(root, &target(root))
            .unwrap()
            .problems
            .is_empty()
    );
    let report = problems_scan(root, &target(root), 10, &person()).unwrap();
    assert_eq!(report.runs, 2);
}

#[test]
fn mcp_decisions_wait_while_a_run_of_the_playbook_waits_on_its_host() {
    let _cfg = config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    project(root);
    let t = target(root);
    assert!(outside_host_task(root, &t).is_ok());
    // A host-mode run of the playbook asked its host session for a step.
    let run = root.join(".apb/runs/fix-build-3");
    let task = run.join("tasks/t-1");
    fs::create_dir_all(&task).unwrap();
    fs::write(
        run.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            r#"{"seq":0,"ts":3000,"type":"run_started","playbook":"fix-build","version":"1.0.0"}"#,
            r#"{"seq":1,"ts":3001,"type":"host_task_requested","task_id":"t-1","node":"fix","attempt":1,"prompt_ref":"tasks/t-1/prompt.md"}"#
        ),
    )
    .unwrap();
    fs::write(
        task.join("task.json"),
        r#"{"task_id":"t-1","node":"fix","attempt":1,"workdir":"."}"#,
    )
    .unwrap();
    fs::write(task.join("prompt.md"), "Fix the build").unwrap();
    apb_core::run_origin::stamp(&run, "fix-build-3").unwrap();
    let e = outside_host_task(root, &t).unwrap_err();
    assert!(
        matches!(&e, ToolError::Conflict(m) if m.contains("waits on a host task") && m.contains("fix-build-3")),
        "{e:?}"
    );
    // A playbook of the same id in the other scope is not held up.
    let global = Target::resolve(root, ID, Some("global")).unwrap();
    assert!(outside_host_task(root, &global).is_ok());
    // A task past its deadline holds nothing up.
    fs::write(
        task.join("task.json"),
        r#"{"task_id":"t-1","node":"fix","attempt":1,"workdir":".","deadline_ms":1}"#,
    )
    .unwrap();
    assert!(outside_host_task(root, &t).is_ok());
    fs::write(
        task.join("task.json"),
        r#"{"task_id":"t-1","node":"fix","attempt":1,"workdir":"."}"#,
    )
    .unwrap();
    assert!(outside_host_task(root, &t).is_err());
    // Once the host submitted, decisions are a person's again.
    fs::write(task.join("submission.json"), "{}").unwrap();
    assert!(outside_host_task(root, &t).is_ok());
}

#[test]
fn a_run_step_is_refused_every_write_and_may_still_read() {
    let _cfg = config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    project(root);
    let t = target(root);
    for step in steps() {
        let refused = |r: Result<(), ToolError>| {
            let e = r.unwrap_err();
            assert!(matches!(e, ToolError::Conflict(_)), "{e:?}");
            assert!(e.to_string().contains("run step"), "{e}");
        };
        refused(problems_scan(root, &t, 10, &step).map(drop));
        refused(
            problem_decide(
                root,
                &t,
                "kp-0000000000",
                ProblemAction::Confirm,
                None,
                "mcp",
                &step,
            )
            .map(drop),
        );
        refused(
            problem_recheck(root, &t, "kp-0000000000", RecheckBy::Model, "mcp", &step).map(drop),
        );
        refused(recheck_changed_by_model(root, &t, "cli", &step).map(drop));
    }
    // Nothing was collected; reading an empty store is fine.
    assert!(problems_view(root, &t).unwrap().problems.is_empty());
}

#[test]
fn an_unsafe_id_or_an_unknown_scope_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let e = Target::resolve(root, "../escape", None).unwrap_err();
    assert!(matches!(e, ToolError::NotFound(_)), "{e:?}");
    let e = Target::resolve(root, ID, Some("team")).unwrap_err();
    assert!(e.to_string().contains("unknown scope"), "{e}");
    assert_eq!(
        Target::resolve(root, ID, Some("global")).unwrap().scope,
        Scope::Global
    );
}
