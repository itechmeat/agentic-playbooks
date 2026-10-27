//! The run budget end to end: history runs of a stub agent that reports 200
//! tokens per attempt seed the estimate, then a run with a budget pauses
//! before the node (or list batch) whose median no longer fits, keeps its
//! limit and spent amount, refuses a plain resume, and continues on a raise
//! or a one-time waiver. No model is ever called.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use apb_core::budget::{BudgetRequest, OnNoEstimate};
use apb_core::registry::init_project;
use apb_engine::budget::{BudgetResume, GateBasis, GateDecision};
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::pause::PauseBy;
use apb_engine::run_view::RunView;
use apb_engine::scheduler::{ResumeRequest, RunOptions, resume, resume_by, resume_req, run};
use apb_engine::state::RunStatus;

use crate::common;

pub(crate) const LINE: &str = "schema: 2\nid: bud\nname: Budget\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: b, type: agent_task, prompt: b }\n  - { id: c, type: agent_task, prompt: c }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: c }\n  - { from: c, to: done }\n";

const FORK: &str = "schema: 2\nid: bud\nname: Budget\nversion: 1.0.0\ndefaults: { profile: main, max_parallel: 2 }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: b, type: agent_task, prompt: b }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: start, to: b }\n  - { from: a, to: done }\n  - { from: b, to: done }\n";

/// What every attempt prints: a claude JSON result reporting 150 + 50
/// billable tokens and a cost.
fn claude_result() -> String {
    serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": "done\n\n```yaml\nstatus: success\nsummary: done\n```",
        "total_cost_usd": 0.01,
        "usage": {"input_tokens": 150, "output_tokens": 50},
        "modelUsage": {"m": {"inputTokens": 150, "outputTokens": 50, "cacheReadInputTokens": 9000, "cacheCreationInputTokens": 0}}
    })
    .to_string()
}

pub(crate) struct Project {
    dir: tempfile::TempDir,
    _env: Vec<common::EnvVar>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Project {
    pub(crate) fn root(&self) -> &Path {
        self.dir.path()
    }

    fn run_dir(&self, id: &str) -> PathBuf {
        self.root().join(".apb/runs").join(id)
    }

    fn events(&self, id: &str) -> Vec<Event> {
        read_all(&self.run_dir(id)).unwrap()
    }

    fn run(&self, budget: Option<BudgetRequest>) -> apb_engine::scheduler::RunResult {
        run(
            self.root(),
            "bud",
            None,
            RunOptions {
                budget,
                ..RunOptions::default()
            },
        )
        .unwrap()
    }

    /// `n` runs without a budget, the history the estimate reads.
    pub(crate) fn history(&self, n: usize) {
        for _ in 0..n {
            assert_eq!(self.run(None).outcome, RunStatus::Succeeded);
        }
    }
}

/// A project with playbook `bud` (`yaml`), profile `main` and the stub
/// agent; holds the env lock while it lives.
pub(crate) fn project(yaml: &str) -> Project {
    let lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::run_origin::ensure_key().unwrap();
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/bud/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/bud/current"), "1.0.0").unwrap();
    common::seed_main(root);
    fs::write(root.join("out.json"), claude_result()).unwrap();
    let agent = root.join("agent.sh");
    common::write_sync(
        &agent,
        &format!(
            "#!/bin/sh\n[ -n \"$APB_STATUS_FILE\" ] && printf '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"\ncat '{}'\n",
            root.join("out.json").display()
        ),
    );
    let mut p = fs::metadata(&agent).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&agent, p).unwrap();
    let env = vec![common::EnvVar::set("APB_AGENT_CMD", &agent)];
    Project {
        dir,
        _env: env,
        _lock: lock,
    }
}

fn tokens(n: u64) -> Option<BudgetRequest> {
    Some(BudgetRequest {
        tokens: Some(n),
        ..BudgetRequest::default()
    })
}

fn gates(events: &[Event]) -> Vec<(String, Option<u32>, GateDecision, GateBasis)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::BudgetGate {
                node,
                batch,
                decision,
                basis,
                ..
            } => Some((node.clone(), *batch, *decision, *basis)),
            _ => None,
        })
        .collect()
}

fn started(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .count()
}

fn sets(events: &[Event]) -> Vec<(Option<u64>, Option<apb_core::budget::BudgetSource>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::BudgetSet { tokens, source, .. } => Some((*tokens, *source)),
            _ => None,
        })
        .collect()
}

fn last_pause(events: &[Event]) -> (String, Option<PauseBy>) {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunPaused { reason, by, .. } => Some((reason.clone(), *by)),
            _ => None,
        })
        .expect("a run_paused")
}

#[test]
fn a_budget_pauses_before_the_node_whose_median_exceeds_the_remainder() {
    let p = project(LINE);
    p.history(3);
    let res = p.run(tokens(300));
    assert_eq!(res.outcome, RunStatus::Paused);
    let ev = p.events(&res.run_id);
    assert_eq!(
        gates(&ev),
        vec![("b".into(), None, GateDecision::Paused, GateBasis::Median)]
    );
    let (reason, by) = last_pause(&ev);
    assert!(reason.starts_with("budget ("));
    assert_eq!(by, Some(PauseBy::Engine));
    assert_eq!((started(&ev, "a"), started(&ev, "b")), (1, 0));
    let view = RunView::load(&p.run_dir(&res.run_id), &res.run_id).unwrap();
    assert_eq!(
        view.pause().and_then(|i| i.cause),
        Some(apb_engine::pause::PauseCause::Budget)
    );
}

#[test]
fn the_pause_keeps_limit_and_spent_and_a_plain_resume_is_refused() {
    let p = project(LINE);
    p.history(3);
    let res = p.run(tokens(300));
    let dir = p.run_dir(&res.run_id);
    let view = RunView::load(&dir, &res.run_id).unwrap();
    let b = view.budget(&dir).expect("a budget status");
    assert_eq!(b.limit.tokens, Some(300));
    let t = b.tokens.unwrap();
    assert_eq!(
        (t.spent, t.unknown_attempts, t.remaining),
        (200.0, 0, Some(100.0))
    );
    assert_eq!(b.paused.as_ref().map(|x| x.node.as_str()), Some("b"));
    assert!(apb_engine::run_estimate::snapshot::read(&dir).is_some());
    let before = p.events(&res.run_id).len();
    let err = resume(p.root(), &res.run_id, None).unwrap_err().to_string();
    assert!(err.contains(apb_engine::budget::BUDGET_PAUSED));
    assert_eq!(
        p.events(&res.run_id).len(),
        before,
        "a refusal journals nothing"
    );
}

#[test]
fn a_raise_at_resume_journals_budget_set_and_spent_is_not_reset() {
    let p = project(LINE);
    p.history(3);
    let res = p.run(tokens(300));
    let raise = BudgetResume {
        raise_tokens: Some(400),
        ..BudgetResume::default()
    };
    let req = ResumeRequest {
        budget: Some(&raise),
        ..ResumeRequest::operator()
    };
    let done = resume_req(p.root(), &res.run_id, &req).unwrap();
    assert_eq!(done.outcome, RunStatus::Succeeded);
    let ev = p.events(&res.run_id);
    use apb_core::budget::BudgetSource::{Resume, Run};
    assert_eq!(
        sets(&ev),
        vec![(Some(300), Some(Run)), (Some(700), Some(Resume))]
    );
    let dir = p.run_dir(&res.run_id);
    let b = RunView::load(&dir, &res.run_id)
        .unwrap()
        .budget(&dir)
        .unwrap();
    assert_eq!(b.tokens.unwrap().spent, 600.0);
    assert_eq!(b.sets, 2);
    assert!(b.paused.is_none());
}

#[test]
fn over_budget_once_admits_the_paused_node_once_then_gates_again() {
    let p = project(LINE);
    p.history(3);
    let res = p.run(tokens(300));
    let once = BudgetResume {
        once: true,
        ..BudgetResume::default()
    };
    let req = ResumeRequest {
        budget: Some(&once),
        ..ResumeRequest::operator()
    };
    let again = resume_req(p.root(), &res.run_id, &req).unwrap();
    assert_eq!(again.outcome, RunStatus::Paused);
    let ev = p.events(&res.run_id);
    let g: Vec<_> = gates(&ev).into_iter().map(|(n, _, d, _)| (n, d)).collect();
    assert_eq!(
        g,
        vec![
            ("b".into(), GateDecision::Paused),
            ("b".into(), GateDecision::Waived),
            ("b".into(), GateDecision::AdmittedWaived),
            ("c".into(), GateDecision::Paused),
        ]
    );
    assert_eq!((started(&ev, "b"), started(&ev, "c")), (1, 0));
}

#[test]
fn a_list_node_pauses_between_batches_and_the_next_batch_stays_pending() {
    let yaml = crate::list_run_test::playbook(
        &format!(
            "items: {{ inline: {} }}\nbatch: {{ size: 1, parallel: 1 }}",
            crate::list_run_test::inline_items(4)
        ),
        "",
        "",
    )
    .replace("id: lst", "id: bud");
    let p = project(&yaml);
    p.history(3);
    let res = p.run(tokens(500));
    assert_eq!(res.outcome, RunStatus::Paused);
    let ev = p.events(&res.run_id);
    assert_eq!(
        gates(&ev),
        vec![(
            "work".into(),
            Some(3),
            GateDecision::Paused,
            GateBasis::Median
        )]
    );
    let batches: Vec<u32> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListBatchStarted { batch, .. } => Some(*batch),
            _ => None,
        })
        .collect();
    assert_eq!(batches, vec![1, 2]);
    let fold = apb_engine::list::ledger::ListFold::of(&ev, "work").unwrap();
    assert_eq!(fold.counts().processed, 2);
}

#[test]
fn parallel_members_reserve_their_need() {
    let p = project(FORK);
    p.history(3);
    let res = p.run(tokens(300));
    assert_eq!(res.outcome, RunStatus::Paused);
    let ev = p.events(&res.run_id);
    // `a` was admitted first and holds 200 of the 300 while it runs, so `b`
    // does not fit, although nothing was spent yet when it was weighed.
    assert_eq!(
        gates(&ev),
        vec![("b".into(), None, GateDecision::Paused, GateBasis::Median)]
    );
    assert_eq!((started(&ev, "a"), started(&ev, "b")), (1, 0));
}

fn policy(on: OnNoEstimate) -> Option<BudgetRequest> {
    Some(BudgetRequest {
        tokens: Some(1_000_000),
        on_no_estimate: Some(on),
        ..BudgetRequest::default()
    })
}

#[test]
fn no_history_continue_journals_continued() {
    let p = project(LINE);
    let res = p.run(policy(OnNoEstimate::Continue));
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let g = gates(&p.events(&res.run_id));
    assert_eq!(g.len(), 3);
    assert!(
        g.iter()
            .all(|(_, _, d, b)| *d == GateDecision::Continued && *b == GateBasis::NoEstimate)
    );
}

#[test]
fn no_history_pause_pauses() {
    let p = project(LINE);
    let res = p.run(policy(OnNoEstimate::Pause));
    assert_eq!(res.outcome, RunStatus::Paused);
    let ev = p.events(&res.run_id);
    assert_eq!(
        gates(&ev),
        vec![(
            "a".into(),
            None,
            GateDecision::Paused,
            GateBasis::NoEstimate
        )]
    );
    assert_eq!(started(&ev, "a"), 0);
}

#[test]
fn unresolved_ask_pauses() {
    let p = project(LINE);
    let res = p.run(tokens(1_000_000));
    assert_eq!(res.outcome, RunStatus::Paused);
    let ev = p.events(&res.run_id);
    assert_eq!(
        gates(&ev),
        vec![(
            "a".into(),
            None,
            GateDecision::Paused,
            GateBasis::NoEstimate
        )]
    );
    // Deciding at resume continues it.
    let cont = BudgetResume {
        on_no_estimate: Some(OnNoEstimate::Continue),
        ..BudgetResume::default()
    };
    let req = ResumeRequest {
        budget: Some(&cont),
        ..ResumeRequest::operator()
    };
    let done = resume_req(p.root(), &res.run_id, &req).unwrap();
    assert_eq!(done.outcome, RunStatus::Succeeded);
}

#[test]
fn a_run_without_budget_journals_no_budget_event() {
    let p = project(LINE);
    let res = p.run(None);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = p.events(&res.run_id);
    assert!(sets(&ev).is_empty() && gates(&ev).is_empty());
    let dir = p.run_dir(&res.run_id);
    assert!(
        !dir.join("estimate.json").exists(),
        "no budget and no estimate shown: no history scan, no snapshot"
    );
    assert!(
        RunView::load(&dir, &res.run_id)
            .unwrap()
            .budget(&dir)
            .is_none()
    );
}

#[test]
fn a_playbook_budget_applies_and_a_run_argument_overrides_it() {
    let p = project(LINE);
    p.history(3);
    fs::write(
        p.root().join(".apb/playbooks/bud/1.0.0/playbook.yaml"),
        LINE.replace(
            "defaults:",
            "budget: { tokens: 300, on_no_estimate: continue }\ndefaults:",
        ),
    )
    .unwrap();
    use apb_core::budget::BudgetSource::{Playbook, Run};
    let paused = p.run(None);
    assert_eq!(paused.outcome, RunStatus::Paused);
    assert_eq!(
        sets(&p.events(&paused.run_id)),
        vec![(Some(300), Some(Playbook))]
    );
    let wide = p.run(tokens(5_000));
    assert_eq!(wide.outcome, RunStatus::Succeeded);
    assert_eq!(
        sets(&p.events(&wide.run_id)),
        vec![(Some(5_000), Some(Run))]
    );
}

#[test]
fn estimate_snapshot_is_written_once_at_start_and_children_have_none() {
    let p = project(LINE);
    let child = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: hi }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n";
    let vdir = p.root().join(".apb/playbooks/child/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), child).unwrap();
    fs::write(p.root().join(".apb/playbooks/child/current"), "1.0.0").unwrap();
    let parent = LINE.replace(
        "{ id: c, type: agent_task, prompt: c }",
        "{ id: c, type: playbook, playbook: child }",
    );
    fs::write(
        p.root().join(".apb/playbooks/bud/1.0.0/playbook.yaml"),
        parent,
    )
    .unwrap();
    let res = p.run(tokens(1_000_000));
    let parent_dir = p.run_dir(&res.run_id);
    let snap = parent_dir.join("estimate.json");
    let first = fs::read(&snap).unwrap();
    let est = apb_engine::run_estimate::snapshot::read(&parent_dir).unwrap();
    assert_eq!(est.playbook, "bud");
    // A later write leaves the snapshot as it was.
    let mut other = est.clone();
    other.version = "9.9.9".into();
    assert!(!apb_engine::run_estimate::snapshot::write(&parent_dir, &other).unwrap());
    assert_eq!(fs::read(&snap).unwrap(), first);
    // Resolve the child's ask so it runs, then look at it.
    let cont = BudgetResume {
        on_no_estimate: Some(OnNoEstimate::Continue),
        ..BudgetResume::default()
    };
    let req = ResumeRequest {
        budget: Some(&cont),
        ..ResumeRequest::operator()
    };
    assert_eq!(
        resume_req(p.root(), &res.run_id, &req).unwrap().outcome,
        RunStatus::Succeeded
    );
    let child_id = p
        .events(&res.run_id)
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id),
            _ => None,
        })
        .unwrap();
    let child_dir = p.run_dir(&child_id);
    assert!(!child_dir.join("estimate.json").exists());
    assert!(sets(&p.events(&child_id)).is_empty());
}

/// The engine's own rule, whoever calls it: a resume by a supervisor (or by
/// the engine) neither continues a budget pause nor changes a budget. That a
/// person stands behind an operator resume is each surface's rule (MCP asks
/// for a confirmation nonce, see the MCP server's budget tests).
#[test]
fn a_supervisor_resume_neither_continues_a_budget_pause_nor_changes_the_budget() {
    let p = project(LINE);
    p.history(3);
    let res = p.run(tokens(300));
    let err = resume_by(p.root(), &res.run_id, None, false, PauseBy::Supervisor)
        .unwrap_err()
        .to_string();
    assert!(err.contains("resumed by a person"));
    let raise = BudgetResume {
        raise_tokens: Some(1_000),
        ..BudgetResume::default()
    };
    let req = ResumeRequest {
        budget: Some(&raise),
        by: PauseBy::Supervisor,
        ..ResumeRequest::operator()
    };
    let err = resume_req(p.root(), &res.run_id, &req)
        .unwrap_err()
        .to_string();
    assert!(err.contains("only a person"));
    assert_eq!(sets(&p.events(&res.run_id)).len(), 1);
}

fn waivers(events: &[Event]) -> usize {
    gates(events)
        .iter()
        .filter(|(_, _, d, _)| *d == GateDecision::Waived)
        .count()
}

/// Rewrites the stub agent with another size: the run's executor binary
/// changed since it started (environment drift).
fn drift_agent(p: &Project) {
    let agent = p.root().join("agent.sh");
    let body = fs::read_to_string(&agent).unwrap() + "# changed\n";
    common::write_sync(&agent, &body);
    let mut perm = fs::metadata(&agent).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&agent, perm).unwrap();
}

#[test]
fn a_resume_refused_after_the_budget_check_journals_no_budget_change() {
    let p = project(LINE);
    p.history(3);
    let res = p.run(tokens(300));
    drift_agent(&p);
    let before = p.events(&res.run_id).len();
    let change = BudgetResume {
        raise_tokens: Some(400),
        once: true,
        ..BudgetResume::default()
    };
    let req = ResumeRequest {
        budget: Some(&change),
        ..ResumeRequest::operator()
    };
    let err = resume_req(p.root(), &res.run_id, &req).unwrap_err();
    assert!(err.to_string().contains("environment drift"));
    let ev = p.events(&res.run_id);
    assert_eq!(ev.len(), before, "a refused resume journals nothing");
    assert_eq!((sets(&ev).len(), waivers(&ev)), (1, 0));
    // The pause is still unresolved: a plain resume is still refused.
    let err = resume_by(p.root(), &res.run_id, None, true, PauseBy::Operator).unwrap_err();
    assert!(err.to_string().contains(apb_engine::budget::BUDGET_PAUSED));
}

#[test]
fn a_budget_change_on_a_finished_run_journals_nothing() {
    let p = project(LINE);
    let id = p.run(None).run_id;
    let before = p.events(&id).len();
    let set = BudgetResume {
        tokens: Some(1_000),
        ..BudgetResume::default()
    };
    let req = ResumeRequest {
        budget: Some(&set),
        ..ResumeRequest::operator()
    };
    assert!(resume_req(p.root(), &id, &req).is_err());
    assert_eq!(p.events(&id).len(), before);
}

#[test]
fn two_resumes_with_the_same_raise_apply_it_once() {
    let p = project(LINE);
    p.history(3);
    let res = p.run(tokens(300));
    let raise = BudgetResume {
        raise_tokens: Some(1_000),
        ..BudgetResume::default()
    };
    let req = ResumeRequest {
        budget: Some(&raise),
        ..ResumeRequest::operator()
    };
    let outcomes: Vec<bool> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..2)
            .map(|_| s.spawn(|| resume_req(p.root(), &res.run_id, &req).is_ok()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(outcomes.iter().filter(|ok| **ok).count(), 1, "{outcomes:?}");
    use apb_core::budget::BudgetSource::{Resume, Run};
    assert_eq!(
        sets(&p.events(&res.run_id)),
        vec![(Some(300), Some(Run)), (Some(1_300), Some(Resume))]
    );
}

#[test]
fn a_cache_hit_in_a_batch_gives_its_reservation_back() {
    let yaml = FORK.replace(
        "{ id: a, type: agent_task, prompt: a }",
        "{ id: a, type: agent_task, prompt: a, cache: auto }",
    );
    let p = project(&yaml);
    crate::list_run_test::git_tree(p.root());
    p.history(3);
    // `a` hits the cache and spends nothing, so all of the 300 is left for
    // `b` (200); had `a` kept its 200 reserved, `b` would not fit.
    let res = p.run(tokens(300));
    let ev = p.events(&res.run_id);
    assert!(
        ev.iter().any(|e| matches!(
            &e.payload,
            EventPayload::NodeCacheHit { node, .. } if node == "a"
        )),
        "`a` is served from the cache"
    );
    assert_eq!(res.outcome, RunStatus::Succeeded, "{:?}", gates(&ev));
    assert!(gates(&ev).is_empty());
}

#[test]
fn a_script_list_node_is_not_gated() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _lock = common::env_lock();
    let yaml = crate::list_run_test::playbook(
        &format!(
            "items: {{ inline: {} }}\nbatch: {{ size: 1 }}",
            crate::list_run_test::inline_items(2)
        ),
        "",
        "",
    )
    .replace(
        "    type: agent_task\n    prompt: \"Handle one item of the list.\"\n",
        "    type: script\n    script: scripts/item.sh\n    runner: sh\n",
    );
    crate::list_run_test::seed(root, &yaml, &[("item.sh", "echo '{}'\n")]);
    let res = run(
        root,
        "lst",
        None,
        RunOptions {
            budget: policy(OnNoEstimate::Pause),
            ..RunOptions::default()
        },
    )
    .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(gates(&ev).is_empty(), "{:?}", gates(&ev));
}

#[test]
fn a_shown_estimate_becomes_the_snapshot_of_a_run_without_budget() {
    let p = project(LINE);
    p.history(1);
    let mut shown = apb_engine::run_estimate::for_playbook(p.root(), "bud", None, "cli").unwrap();
    shown.note = Some("what the person saw".into());
    let res = run(
        p.root(),
        "bud",
        None,
        RunOptions {
            estimate: Some(shown.clone()),
            ..RunOptions::default()
        },
    )
    .unwrap();
    let snap = apb_engine::run_estimate::snapshot::read(&p.run_dir(&res.run_id)).unwrap();
    assert_eq!(snap, shown);
}

#[test]
fn a_shown_estimate_is_recomputed_for_the_bindings_of_run_overrides() {
    let p = project(LINE);
    p.history(1);
    common::seed_profile(p.root(), "fast", "claude-code", "sonnet", &[]);
    let mut shown = apb_engine::run_estimate::for_playbook(p.root(), "bud", None, "cli").unwrap();
    shown.note = Some("what the person saw".into());
    assert!(shown.unestimated.is_empty());
    let res = run(
        p.root(),
        "bud",
        None,
        RunOptions {
            estimate: Some(shown),
            overrides: Some(
                apb_core::overrides::RunOverrides::from_yaml("nodes:\n  a: { profile: fast }\n")
                    .unwrap(),
            ),
            ..RunOptions::default()
        },
    )
    .unwrap();
    let snap = apb_engine::run_estimate::snapshot::read(&p.run_dir(&res.run_id)).unwrap();
    // The overridden node runs on another profile, so it has no history.
    assert_ne!(snap.note.as_deref(), Some("what the person saw"));
    assert_eq!(snap.unestimated, vec!["a".to_string()]);
}
