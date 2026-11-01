//! The launch policy of agents (`agent_launch`, the launch floor, the
//! `launch:` settings): the decision table on every launch kind, the nonce
//! bound to the whole permit, the fixed choice of a retry, the lifetime of
//! an authorization, legacy runs without pins, the grant committed with the
//! journal, and the records that never hold a nonce.

use std::fs;
use std::path::{Path, PathBuf};

use apb_core::launch::LaunchPolicy;
use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_engine::event::{EventLog, EventPayload, read_all};
use apb_engine::gate::launch::{
    ASK_POLICY, DENY_POLICY, ENGINE_POLICY, EngineResume, Initiator, LaunchConfirmation,
    LaunchContext, LaunchKind, POLICY_CHANGED, STEP_POLICY, StepChannel,
};
use apb_engine::pause::{PauseBy, ResumeAllAction};
use apb_engine::scheduler::{ResumeRequest, RunOptions, resume_req, run};
use apb_engine::state::RunStatus;
use serde_json::Value;

use crate::common;

/// A config dir of the test's own (floors, the machine `policy.yaml`).
struct Cfg {
    _lock: std::sync::MutexGuard<'static, ()>,
    _var: common::EnvVar,
    dir: tempfile::TempDir,
}

fn cfg() -> Cfg {
    let lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let var = common::EnvVar::set("APB_CONFIG_DIR", dir.path());
    Cfg {
        _lock: lock,
        _var: var,
        dir,
    }
}

impl Cfg {
    /// The machine `policy.yaml` with `launch.agent: <p>`.
    fn machine_agent(&self, p: &str) {
        fs::write(
            self.dir.path().join("policy.yaml"),
            format!("launch:\n  agent: {p}\n"),
        )
        .unwrap();
    }
}

/// start -> s (a script that fails while `<root>/fail` exists) -> done.
fn yaml(id: &str, decl: Option<&str>, extra_nodes: &str, extra_edges: &str) -> String {
    let decl = decl
        .map(|d| format!("agent_launch: {d}\n"))
        .unwrap_or_default();
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\n{decl}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: s, type: script, script: \"scripts/s.sh\", runner: sh }}\n{extra_nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: s }}\n{extra_edges}"
    )
}

fn simple(id: &str, decl: Option<&str>) -> String {
    yaml(id, decl, "", "  - { from: s, to: done }\n")
}

/// Writes version `v` of playbook `id` under `parent/playbooks` and points
/// `current` at it.
fn seed_in(parent: &Path, id: &str, v: &str, body: &str) {
    let vdir = parent.join("playbooks").join(id).join(v);
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        body.replace("version: 1.0.0", &format!("version: {v}")),
    )
    .unwrap();
    fs::write(
        vdir.join("scripts/s.sh"),
        "if [ -f \"$APB_TEST_FAIL\" ]; then exit 1; fi\nif [ -n \"$APB_TEST_TURN\" ]; then sh \"$APB_TEST_TURN\"; fi\necho ok\n",
    )
    .unwrap();
    fs::write(parent.join("playbooks").join(id).join("current"), v).unwrap();
}

fn project(id: &str, body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    apb_core::workspace::ensure_id(dir.path()).unwrap();
    seed_in(&dir.path().join(".apb"), id, "1.0.0", body);
    dir
}

fn wref(id: &str) -> PlaybookRef {
    PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: id.into(),
        version: None,
    }
}

fn mcp(kind: LaunchKind) -> LaunchContext {
    LaunchContext::new(Initiator::AgentMcp { client: None }, kind)
}

fn step(kind: LaunchKind) -> LaunchContext {
    LaunchContext::new(
        Initiator::RunStep {
            channel: StepChannel::Mcp,
            run_id: Some("parent-1".into()),
            host_task: None,
        },
        kind,
    )
}

fn cli(terminal: bool, kind: LaunchKind) -> LaunchContext {
    LaunchContext::new(Initiator::CliUnattested { terminal }, kind)
}

fn policy_of(v: &Value) -> String {
    v["policy"].as_str().unwrap_or_default().to_string()
}

/// One launch kind's check, as a closure over a context.
type Check<'a> = dyn Fn(&LaunchContext) -> Result<(), Value> + 'a;

/// Every cell of the decision table for one launch kind, at `policy`.
/// `kind` names the launch; `check` runs it. A refusal's nonce is what the
/// confirmed cells pass back, with its fixed version and choice.
fn assert_table(policy: LaunchPolicy, kind: LaunchKind, check: &Check<'_>) {
    let fixed = |ctx: LaunchContext, refusal: &Value| {
        ctx.fixed(
            refusal["launch_version"].as_str().map(str::to_string),
            refusal["launch_choice"].as_str(),
        )
    };
    let ok = |ctx: &LaunchContext, what: &str| {
        if let Err(v) = check(ctx) {
            panic!("{policy} {what}: expected to run, refused {v}");
        }
    };
    let refused = |ctx: &LaunchContext, what: &str, code: &str| {
        let v = check(ctx).expect_err(&format!("{policy} {what}: expected a refusal"));
        assert_eq!(policy_of(&v), code, "{policy} {what}: {v}");
        v
    };
    match policy {
        LaunchPolicy::Allow => {
            for (what, ctx) in [
                ("agent", mcp(kind.clone())),
                ("run step", step(kind.clone())),
                ("cli terminal", cli(true, kind.clone())),
                ("cli headless", cli(false, kind.clone())),
                ("dashboard", LaunchContext::dashboard(kind.clone())),
                (
                    "engine",
                    LaunchContext::new(
                        Initiator::Engine {
                            by: EngineResume::PauseWaker,
                        },
                        kind.clone(),
                    ),
                ),
            ] {
                ok(&ctx, what);
            }
        }
        LaunchPolicy::Ask => {
            let r = refused(&mcp(kind.clone()), "agent without a nonce", ASK_POLICY);
            let nonce = r["launch_nonce"]
                .as_str()
                .expect("an ask refusal carries its nonce")
                .to_string();
            let host =
                fixed(mcp(kind.clone()), &r).confirmed(Some(LaunchConfirmation::HostRelayed {
                    nonce: nonce.clone(),
                }));
            ok(&host, "agent with the relayed nonce");
            let forged =
                fixed(mcp(kind.clone()), &r).confirmed(Some(LaunchConfirmation::HostRelayed {
                    nonce: "launch-forged".into(),
                }));
            refused(&forged, "agent with another nonce", ASK_POLICY);
            let in_step =
                fixed(step(kind.clone()), &r).confirmed(Some(LaunchConfirmation::HostRelayed {
                    nonce: nonce.clone(),
                }));
            refused(&in_step, "run step with a valid nonce", STEP_POLICY);
            let prompt = fixed(cli(true, kind.clone()), &r)
                .confirmed(Some(LaunchConfirmation::TerminalPrompt));
            ok(&prompt, "cli terminal prompt");
            let headless = cli(false, kind.clone());
            let r2 = refused(&headless, "cli headless without a flag", ASK_POLICY);
            let flag =
                fixed(cli(false, kind.clone()), &r2).confirmed(Some(LaunchConfirmation::CliFlag {
                    nonce: r2["launch_nonce"].as_str().unwrap().to_string(),
                }));
            ok(&flag, "cli headless with the flag");
            ok(&LaunchContext::dashboard(kind.clone()), "dashboard");
            refused(
                &LaunchContext::new(
                    Initiator::Engine {
                        by: EngineResume::PauseWaker,
                    },
                    kind.clone(),
                ),
                "engine",
                ENGINE_POLICY,
            );
        }
        LaunchPolicy::Deny => {
            let r = refused(&mcp(kind.clone()), "agent", DENY_POLICY);
            assert!(
                r.get("launch_nonce").is_none(),
                "a deny refusal offers no nonce: {r}"
            );
            assert_eq!(r["launch_by_person"], true);
            refused(&step(kind.clone()), "run step", STEP_POLICY);
            let headless = cli(false, kind.clone()).confirmed(Some(LaunchConfirmation::CliFlag {
                nonce: "launch-x".into(),
            }));
            refused(&headless, "cli headless with a flag", DENY_POLICY);
            let prompt = fixed(cli(true, kind.clone()), &r)
                .confirmed(Some(LaunchConfirmation::TerminalPrompt));
            ok(&prompt, "cli terminal prompt");
            ok(&LaunchContext::dashboard(kind.clone()), "dashboard");
            refused(
                &LaunchContext::new(
                    Initiator::Engine {
                        by: EngineResume::QuotaProbe,
                    },
                    kind.clone(),
                ),
                "engine",
                ENGINE_POLICY,
            );
        }
    }
}

const POLICIES: [LaunchPolicy; 3] = [LaunchPolicy::Allow, LaunchPolicy::Ask, LaunchPolicy::Deny];

/// Start, continue and execute plan go through `check_run`; trial and eval
/// through the trial's tree check. Every cell of the table, per policy.
#[test]
fn every_cell_of_the_decision_table_for_every_start_kind() {
    let _c = cfg();
    for p in POLICIES {
        let dir = project("pb", &simple("pb", Some(p.as_str())));
        let root = dir.path();
        for kind in [
            LaunchKind::Start,
            LaunchKind::ExecutePlan,
            LaunchKind::ContinueFrom {
                run_id: "earlier-1".into(),
                node: String::new(),
            },
        ] {
            let check = |ctx: &LaunchContext| {
                apb_engine::gate::check_run(root, &wref("pb"), true, false, ctx).map(|_| ())
            };
            assert_table(p, kind, &check);
        }
        let loaded = apb_core::registry::Registry::open(root)
            .unwrap()
            .load("pb", None)
            .unwrap();
        let origin = Origin::Project { workspace_id: None };
        for kind in [LaunchKind::Trial, LaunchKind::Eval] {
            let check = |ctx: &LaunchContext| {
                apb_engine::gate::check_trial_launch(root, &loaded, &origin, false, ctx).map(|_| ())
            };
            assert_table(p, kind, &check);
        }
    }
}

/// A run nothing authorized (started before launch policies), resumed one
/// at a time and in a batch: every cell, with the policy from the machine's
/// settings.
#[test]
fn every_cell_of_the_decision_table_for_resumes() {
    let c = cfg();
    let dir = project("pb", &simple("pb", None));
    let root = dir.path();
    let res = run(root, "pb", None, RunOptions::default()).unwrap();
    assert!(
        apb_engine::gate::launch::run_authorization(&root.join(".apb/runs").join(&res.run_id))
            .is_none()
    );
    for p in POLICIES {
        c.machine_agent(p.as_str());
        let run_id = res.run_id.clone();
        for kind in [
            LaunchKind::Resume {
                run_id: run_id.clone(),
            },
            LaunchKind::ResumeAll {
                run_id: run_id.clone(),
            },
        ] {
            let check = |ctx: &LaunchContext| {
                apb_engine::resume_launch::check_resume_launch(root, &run_id, ctx).map(|_| ())
            };
            assert_table(p, kind, &check);
        }
    }
}

/// The nonce binds the whole permit: a skill of a profile the playbook
/// binds changes between the refusal and the retry, and the old nonce no
/// longer matches.
#[test]
fn a_profile_bundle_change_between_refusal_and_retry_needs_a_fresh_nonce() {
    let _c = cfg();
    let body = yaml(
        "pb",
        Some("ask"),
        "  - { id: a, type: agent_task, prompt: \"x\", profile: main }\n",
        "  - { from: s, to: a }\n  - { from: a, to: done }\n",
    );
    let dir = project("pb", &body);
    let root = dir.path();
    common::seed_main(root);
    let check =
        |ctx: &LaunchContext| apb_engine::gate::check_run(root, &wref("pb"), true, false, ctx);
    let refusal = check(&mcp(LaunchKind::Start)).unwrap_err();
    let nonce = refusal["launch_nonce"].as_str().unwrap().to_string();
    let retry = |nonce: &str| {
        mcp(LaunchKind::Start)
            .fixed(
                refusal["launch_version"].as_str().map(str::to_string),
                refusal["launch_choice"].as_str(),
            )
            .confirmed(Some(LaunchConfirmation::HostRelayed {
                nonce: nonce.into(),
            }))
    };
    assert!(
        check(&retry(&nonce)).is_ok(),
        "the nonce matches an unchanged permit"
    );
    fs::write(root.join(".apb/profiles/main/SOUL.md"), "a changed role\n").unwrap();
    let again = check(&retry(&nonce)).unwrap_err();
    assert_eq!(policy_of(&again), ASK_POLICY, "{again}");
    assert_ne!(
        again["launch_nonce"].as_str().unwrap(),
        nonce,
        "a fresh nonce"
    );
}

/// The same for a child that moves from the project store to the global
/// one between the refusal and the retry: the pin's scope is part of the
/// permit.
#[test]
fn a_child_scope_change_between_refusal_and_retry_needs_a_fresh_nonce() {
    let c = cfg();
    let parent = yaml(
        "parent",
        Some("ask"),
        "  - { id: c, type: playbook, playbook: child }\n",
        "  - { from: s, to: c }\n  - { from: c, to: done }\n",
    );
    let dir = project("parent", &parent);
    let root = dir.path();
    seed_in(&root.join(".apb"), "child", "1.0.0", &simple("child", None));
    let check =
        |ctx: &LaunchContext| apb_engine::gate::check_run(root, &wref("parent"), true, false, ctx);
    let refusal = check(&mcp(LaunchKind::Start)).unwrap_err();
    let nonce = refusal["launch_nonce"].as_str().unwrap().to_string();
    // The child now lives in the global store only.
    fs::remove_dir_all(root.join(".apb/playbooks/child")).unwrap();
    seed_in(c.dir.path(), "child", "1.0.0", &simple("child", None));
    let retry = mcp(LaunchKind::Start)
        .fixed(
            refusal["launch_version"].as_str().map(str::to_string),
            refusal["launch_choice"].as_str(),
        )
        .confirmed(Some(LaunchConfirmation::HostRelayed {
            nonce: nonce.clone(),
        }));
    let again = check(&retry).unwrap_err();
    assert_eq!(policy_of(&again), ASK_POLICY, "{again}");
    assert_ne!(again["launch_nonce"].as_str().unwrap(), nonce);
}

/// A nonce of another workspace never confirms a launch here.
#[test]
fn a_nonce_of_another_workspace_is_refused() {
    let _c = cfg();
    let a = project("pb", &simple("pb", Some("ask")));
    let b = project("pb", &simple("pb", Some("ask")));
    let refusal =
        apb_engine::gate::check_run(a.path(), &wref("pb"), true, false, &mcp(LaunchKind::Start))
            .unwrap_err();
    let ctx = mcp(LaunchKind::Start)
        .fixed(Some("1.0.0".into()), Some("current"))
        .confirmed(Some(LaunchConfirmation::HostRelayed {
            nonce: refusal["launch_nonce"].as_str().unwrap().into(),
        }));
    let v = apb_engine::gate::check_run(b.path(), &wref("pb"), true, false, &ctx).unwrap_err();
    assert_eq!(policy_of(&v), ASK_POLICY);
}

/// Floors are keyed by the store that owns the definition: a global
/// playbook made from project A keeps its `deny` floor when project B runs
/// an older `allow` version of it by an explicit version.
#[test]
fn a_global_floor_holds_in_every_workspace_for_an_explicit_older_version() {
    let _c = cfg();
    let a = project("pb", &simple("pb", Some("allow")));
    // A newer `deny` version saved through the one save path raises the
    // floor of project A's key.
    apb_core::versioning::save_definition(
        a.path(),
        "pb",
        &simple("pb", Some("deny")).replace("version: 1.0.0", "version: 1.1.0"),
        None,
        true,
    )
    .unwrap();
    let floors = apb_core::scope_move::CopyFloors::project_to_global(
        &apb_core::workspace::read_id(a.path()).unwrap(),
        "pb",
        apb_core::launch::floor::SaveChannel::Person,
    );
    let global_parent = apb_core::store::global_playbooks_parent().unwrap();
    apb_core::scope_move::copy_playbook_to(
        &a.path().join(".apb/playbooks/pb"),
        &global_parent,
        "pb",
        "project",
        &floors,
    )
    .unwrap();
    // Project B never saw the playbook; it asks for the global 1.0.0.
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let older = PlaybookRef {
        origin: Origin::Global,
        id: "pb".into(),
        version: Some("1.0.0".into()),
    };
    let v = apb_engine::gate::check_run(b.path(), &older, true, false, &mcp(LaunchKind::Start))
        .unwrap_err();
    assert_eq!(policy_of(&v), DENY_POLICY, "{v}");
    // A project playbook of the same id in B is another key.
    seed_in(&b.path().join(".apb"), "pb", "1.0.0", &simple("pb", None));
    assert!(
        apb_engine::gate::check_run(b.path(), &wref("pb"), true, false, &mcp(LaunchKind::Start))
            .is_ok()
    );
}

fn fail_flag(root: &Path) -> (PathBuf, common::EnvVar) {
    let flag = root.join("fail");
    fs::write(&flag, "").unwrap();
    (flag.clone(), common::EnvVar::set("APB_TEST_FAIL", &flag))
}

/// A run authorized under `ask` stays authorized for its lifetime: after the
/// policy tightens to `deny`, an agent's and the engine's resumes need no
/// new confirmation.
#[test]
fn an_authorization_covers_its_runs_lifetime() {
    let c = cfg();
    let dir = project("pb", &simple("pb", Some("ask")));
    let root = dir.path();
    let (flag, _fail) = fail_flag(root);
    let check =
        |ctx: &LaunchContext| apb_engine::gate::check_run(root, &wref("pb"), true, false, ctx);
    let refusal = check(&mcp(LaunchKind::Start)).unwrap_err();
    let ctx = mcp(LaunchKind::Start)
        .fixed(Some("1.0.0".into()), Some("current"))
        .confirmed(Some(LaunchConfirmation::HostRelayed {
            nonce: refusal["launch_nonce"].as_str().unwrap().into(),
        }));
    let permit = check(&ctx).unwrap();
    let mut opts = RunOptions::default();
    permit.apply(&mut opts);
    let res = run(root, "pb", None, opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let record = apb_engine::gate::launch::run_authorization(&run_dir).unwrap();
    assert_eq!(record.policy, LaunchPolicy::Ask);
    assert_eq!(record.confirmation.as_deref(), Some("host_relayed"));
    c.machine_agent("deny");
    for ctx in [
        mcp(LaunchKind::Resume {
            run_id: res.run_id.clone(),
        }),
        LaunchContext::engine(EngineResume::PauseWaker, &res.run_id),
        cli(
            false,
            LaunchKind::Resume {
                run_id: res.run_id.clone(),
            },
        ),
    ] {
        assert_eq!(
            apb_engine::resume_launch::check_resume_launch(root, &res.run_id, &ctx),
            Ok(None)
        );
    }
    fs::remove_file(&flag).unwrap();
    let resumed = resume_req(
        root,
        &res.run_id,
        &ResumeRequest {
            from_node: Some("s"),
            ..ResumeRequest::operator()
        },
    )
    .unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
}

/// A legacy parent without pins whose child turns `deny` before the
/// confirmation: the resume is refused for the child's policy.
#[test]
fn a_legacy_unpinned_parent_with_a_child_turned_deny_is_refused() {
    let _c = cfg();
    let parent = yaml(
        "parent",
        None,
        "  - { id: c, type: playbook, playbook: child }\n",
        "  - { from: s, to: c }\n  - { from: c, to: done }\n",
    );
    let dir = project("parent", &parent);
    let root = dir.path();
    seed_in(&root.join(".apb"), "child", "1.0.0", &simple("child", None));
    let (_flag, _fail) = fail_flag(root);
    let res = run(root, "parent", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    seed_in(
        &root.join(".apb"),
        "child",
        "1.1.0",
        &simple("child", Some("deny")),
    );
    let v = apb_engine::resume_launch::check_resume_launch(
        root,
        &res.run_id,
        &mcp(LaunchKind::Resume {
            run_id: res.run_id.clone(),
        }),
    )
    .unwrap_err();
    assert_eq!(policy_of(&v), DENY_POLICY, "{v}");
}

/// The same parent, its child turned `deny` after the confirmation. Before
/// the commit, the floor the new version raised changes the policy: the
/// resume is refused and nothing runs. After the commit (here, by the
/// parent's own step right before the child's spawn), the pin the commit
/// persisted holds: the checked version is the one that runs.
#[test]
fn a_child_turned_deny_after_the_confirmation_does_not_run() {
    let c = cfg();
    let parent = yaml(
        "parent",
        None,
        "  - { id: c, type: playbook, playbook: child }\n",
        "  - { from: s, to: c }\n  - { from: c, to: done }\n",
    );
    let dir = project("parent", &parent);
    let root = dir.path();
    seed_in(&root.join(".apb"), "child", "1.0.0", &simple("child", None));
    let (flag, _fail) = fail_flag(root);
    let res = run(root, "parent", None, RunOptions::default()).unwrap();
    c.machine_agent("ask");
    let confirmed = || {
        let kind = LaunchKind::Resume {
            run_id: res.run_id.clone(),
        };
        let refusal =
            apb_engine::resume_launch::check_resume_launch(root, &res.run_id, &mcp(kind.clone()))
                .unwrap_err();
        let ctx = mcp(kind)
            .fixed(
                refusal["launch_version"].as_str().map(str::to_string),
                refusal["launch_choice"].as_str(),
            )
            .confirmed(Some(LaunchConfirmation::HostRelayed {
                nonce: refusal["launch_nonce"].as_str().unwrap().into(),
            }));
        apb_engine::resume_launch::check_resume_launch(root, &res.run_id, &ctx)
            .unwrap()
            .expect("an unauthorized run gets a grant")
    };
    fs::remove_file(&flag).unwrap();
    let resume = |grant| {
        resume_req(
            root,
            &res.run_id,
            &ResumeRequest {
                from_node: Some("s"),
                launch: Some(grant),
                ..ResumeRequest::operator()
            },
        )
    };

    // Between the confirmation and the commit.
    let grant = confirmed();
    assert!(
        grant.pins.is_some(),
        "the checked child tree travels with the grant"
    );
    let child_dir = root.join(".apb/playbooks/child");
    seed_in(
        &root.join(".apb"),
        "child",
        "1.1.0",
        &simple("child", Some("deny")),
    );
    let err = resume(&grant).unwrap_err();
    assert!(err.to_string().contains(POLICY_CHANGED), "{err}");
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    assert!(apb_engine::gate::launch::run_authorization(&run_dir).is_none());
    // Back to the allow child, which the scan alone held (no record).
    fs::remove_dir_all(child_dir.join("1.1.0")).unwrap();
    fs::write(child_dir.join("current"), "1.0.0").unwrap();

    // Between the commit and the spawn: the parent's step turns it.
    let grant = confirmed();
    let turn = root.join("turn.sh");
    let deny = simple("child", Some("deny")).replace("version: 1.0.0", "version: 1.1.0");
    fs::write(
        &turn,
        format!(
            "mkdir -p '{d}/1.1.0/scripts' && cat > '{d}/1.1.0/playbook.yaml' <<'EOF'\n{deny}EOF\ncp '{d}/1.0.0/scripts/s.sh' '{d}/1.1.0/scripts/s.sh'\nprintf 1.1.0 > '{d}/current'\n",
            d = child_dir.display()
        ),
    )
    .unwrap();
    let _turn = common::EnvVar::set("APB_TEST_TURN", &turn);
    let resumed = resume(&grant).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(child_dir.join("current")).unwrap(),
        "1.1.0",
        "the step turned it"
    );
    let events = read_all(&run_dir).unwrap();
    // The resume's child (the first run's own child ran before it failed).
    let child = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .unwrap();
    let child_events = read_all(&root.join(".apb/runs").join(&child)).unwrap();
    let version = child_events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunStarted { version, .. } => Some(version.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(version, "1.0.0", "the pinned child ran, not the deny one");
    let record =
        apb_engine::gate::launch::run_authorization(&root.join(".apb/runs").join(&child)).unwrap();
    assert_eq!(record.kind, "child");
    let cfg = apb_engine::run_config::read_run_config(&run_dir).unwrap();
    assert_eq!(
        cfg.expected_children, grant.pins,
        "the pins persisted are exactly the tree the grant checked"
    );
}

/// A policy change between the check and the commit refuses the resume,
/// journals nothing, and leaves no authorization a later engine resume
/// could reuse.
#[test]
fn a_policy_change_before_the_commit_refuses_and_leaves_nothing() {
    let c = cfg();
    let dir = project("pb", &simple("pb", None));
    let root = dir.path();
    let (flag, _fail) = fail_flag(root);
    let res = run(root, "pb", None, RunOptions::default()).unwrap();
    c.machine_agent("ask");
    let kind = LaunchKind::Resume {
        run_id: res.run_id.clone(),
    };
    let refusal =
        apb_engine::resume_launch::check_resume_launch(root, &res.run_id, &mcp(kind.clone()))
            .unwrap_err();
    let ctx = mcp(kind)
        .fixed(Some("1.0.0".into()), Some("explicit"))
        .confirmed(Some(LaunchConfirmation::HostRelayed {
            nonce: refusal["launch_nonce"].as_str().unwrap().into(),
        }));
    let grant = apb_engine::resume_launch::check_resume_launch(root, &res.run_id, &ctx)
        .unwrap()
        .unwrap();
    c.machine_agent("deny");
    fs::remove_file(&flag).unwrap();
    let before = read_all(&root.join(".apb/runs").join(&res.run_id))
        .unwrap()
        .len();
    let err = resume_req(
        root,
        &res.run_id,
        &ResumeRequest {
            from_node: Some("s"),
            launch: Some(&grant),
            ..ResumeRequest::operator()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains(POLICY_CHANGED), "{err}");
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    assert_eq!(
        read_all(&run_dir).unwrap().len(),
        before,
        "nothing journaled"
    );
    assert!(apb_engine::gate::launch::run_authorization(&run_dir).is_none());
    // The engine still may not resume it.
    let v = apb_engine::resume_launch::check_resume_launch(
        root,
        &res.run_id,
        &LaunchContext::engine(EngineResume::ResumeAll, &res.run_id),
    )
    .unwrap_err();
    assert_eq!(policy_of(&v), ENGINE_POLICY);
}

/// The same for a start: a floor or a setting changed between the gate and
/// the engine's preparation refuses the start instead of recording an
/// authorization for a policy nobody was asked about.
#[test]
fn a_policy_change_between_the_gate_and_the_start_refuses_it() {
    let c = cfg();
    let dir = project("pb", &simple("pb", None));
    let root = dir.path();
    let permit =
        apb_engine::gate::check_run(root, &wref("pb"), true, false, &mcp(LaunchKind::Start))
            .unwrap();
    c.machine_agent("deny");
    let mut opts = RunOptions::default();
    permit.apply(&mut opts);
    let err = run(root, "pb", None, opts).unwrap_err();
    assert!(err.to_string().contains(POLICY_CHANGED), "{err}");
}

/// A start that skipped every launch surface runs only an `allow`
/// playbook.
#[test]
fn an_unchecked_start_runs_only_an_allow_playbook() {
    let _c = cfg();
    let dir = project("pb", &simple("pb", Some("ask")));
    let err = run(dir.path(), "pb", None, RunOptions::default()).unwrap_err();
    assert!(err.to_string().contains("launch_unchecked"), "{err}");
}

/// The engine's own resumes of an unauthorized `deny` run: the waker of a
/// pause with `until` is refused and the run stays paused; `resume --all`
/// lists the run as refused by the launch policy before any probe and does
/// not mark it driverless.
#[test]
fn the_engine_never_resumes_an_unauthorized_deny_run() {
    let c = cfg();
    let dir = project("pb", &simple("pb", None));
    let root = dir.path();
    let res = run(root, "pb", None, RunOptions::default()).unwrap();
    c.machine_agent("deny");
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let now = u64::try_from(apb_core::clock::now_ms()).unwrap();
    let mut log = EventLog::open(&run_dir).unwrap();
    log.append(
        apb_engine::pause::PauseRequest {
            reason: Some("later".into()),
            by: Some(PauseBy::Operator),
            until: Some(now - 1_000),
            ..apb_engine::pause::PauseRequest::default()
        }
        .to_event(),
    )
    .unwrap();
    drop(log);
    let seq = read_all(&run_dir).unwrap().last().unwrap().seq;
    let outcome = apb_engine::pause::run_waker(root, &res.run_id, seq).unwrap();
    match outcome {
        apb_engine::pause::WakeOutcome::Refused(why) => {
            assert!(why.contains(ENGINE_POLICY), "{why}")
        }
        other => panic!("the waker must be refused: {other:?}"),
    }
    let before = read_all(&run_dir).unwrap().len();
    let entries = apb_engine::pause::resume_all(root).unwrap();
    let action = entries
        .iter()
        .find(|e| e.run_id == res.run_id)
        .map(|e| e.action.clone());
    assert!(
        matches!(action, Some(ResumeAllAction::LaunchRefused { .. })),
        "{entries:?}"
    );
    assert_eq!(
        read_all(&run_dir).unwrap().len(),
        before,
        "not marked driverless"
    );
    assert_eq!(
        apb_engine::state::RunState::fold(&read_all(&run_dir).unwrap()).run_status,
        RunStatus::Paused
    );
}

/// The manifest and the journal keep who launched and how, never a nonce:
/// neither for a confirmed start nor for a confirmed resume of a run that
/// had no authorization.
#[test]
fn the_records_never_hold_a_nonce() {
    let c = cfg();
    let dir = project("pb", &simple("pb", None));
    let root = dir.path();
    let (flag, _fail) = fail_flag(root);
    // A run never authorized, resumed with a confirmed grant.
    let plain = run(root, "pb", None, RunOptions::default()).unwrap();
    c.machine_agent("ask");
    let kind = LaunchKind::Resume {
        run_id: plain.run_id.clone(),
    };
    let r = apb_engine::resume_launch::check_resume_launch(root, &plain.run_id, &mcp(kind.clone()))
        .unwrap_err();
    let resume_nonce = r["launch_nonce"].as_str().unwrap().to_string();
    let grant = apb_engine::resume_launch::check_resume_launch(
        root,
        &plain.run_id,
        &mcp(kind)
            .fixed(Some("1.0.0".into()), Some("explicit"))
            .confirmed(Some(LaunchConfirmation::HostRelayed {
                nonce: resume_nonce.clone(),
            })),
    )
    .unwrap()
    .unwrap();
    // A confirmed start.
    let refusal =
        apb_engine::gate::check_run(root, &wref("pb"), true, false, &mcp(LaunchKind::Start))
            .unwrap_err();
    let start_nonce = refusal["launch_nonce"].as_str().unwrap().to_string();
    let permit = apb_engine::gate::check_run(
        root,
        &wref("pb"),
        true,
        false,
        &mcp(LaunchKind::Start)
            .fixed(Some("1.0.0".into()), Some("current"))
            .confirmed(Some(LaunchConfirmation::HostRelayed {
                nonce: start_nonce.clone(),
            })),
    )
    .unwrap();
    let mut opts = RunOptions::default();
    permit.apply(&mut opts);
    let started = run(root, "pb", None, opts).unwrap();
    fs::remove_file(&flag).unwrap();
    resume_req(
        root,
        &plain.run_id,
        &ResumeRequest {
            from_node: Some("s"),
            launch: Some(&grant),
            ..ResumeRequest::operator()
        },
    )
    .unwrap();
    for run_id in [&plain.run_id, &started.run_id] {
        let d = root.join(".apb/runs").join(run_id);
        assert!(
            apb_engine::gate::launch::run_authorization(&d).is_some(),
            "{run_id} is authorized"
        );
        for f in ["manifest.yaml", "events.jsonl", "run.yaml"] {
            let body = fs::read_to_string(d.join(f)).unwrap_or_default();
            for n in [&start_nonce, &resume_nonce] {
                assert!(
                    !body.contains(n.as_str()),
                    "{} holds a nonce",
                    d.join(f).display()
                );
            }
        }
    }
}

/// A child run without an authorization of its own is continued under its
/// parent only when its snapshot is exactly the parent's pin for the node:
/// an authorized parent's record never covers an unverified child.
#[test]
fn a_reattached_child_must_match_its_pin() {
    let _c = cfg();
    let dir = project("pb", &simple("pb", None));
    let root = dir.path();
    // An authorized parent (a dashboard start) and a child run started
    // without a record of its own (as a live resolution would).
    let permit = apb_engine::gate::check_run(
        root,
        &wref("pb"),
        true,
        false,
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .unwrap();
    let mut opts = RunOptions::default();
    permit.apply(&mut opts);
    let parent = run(root, "pb", None, opts).unwrap();
    let parent_dir = root.join(".apb/runs").join(&parent.run_id);
    let child = run(root, "pb", None, RunOptions::default()).unwrap();
    let child_dir = root.join(".apb/runs").join(&child.run_id);
    let yaml = fs::read_to_string(child_dir.join("playbook.yaml")).unwrap();
    let digest = apb_core::scope::definition_digest(&yaml, &child_dir).unwrap();
    let pin = |d: &str| apb_engine::run_config::ChildExpectation {
        id: "pb".into(),
        scope: apb_core::profile::ProfileScope::Project,
        version: "1.0.0".into(),
        playbook_digest: d.into(),
        profile_bundles: Default::default(),
        connectors: Default::default(),
        connector_accounts: Default::default(),
        children: Default::default(),
    };
    use apb_engine::gate::launch::child_covered;
    assert!(child_covered(root, &parent_dir, Some(&pin(&digest)), &child.run_id).is_ok());
    let e =
        child_covered(root, &parent_dir, Some(&pin("sha256:other")), &child.run_id).unwrap_err();
    assert!(
        e.starts_with(apb_engine::gate::launch::CHILD_NOT_COVERED),
        "{e}"
    );
    assert!(child_covered(root, &parent_dir, None, &child.run_id).is_err());
    // A parent nothing authorized keeps its children as they always were.
    assert!(child_covered(root, &child_dir, None, &child.run_id).is_ok());
}

/// A resume refused for a busy lock journals no launch and leaves no
/// authorization behind: the grant dies with the request, and a later
/// engine resume is still refused.
#[test]
fn a_resume_refused_for_a_busy_lock_leaves_no_authorization() {
    let c = cfg();
    let dir = project("pb", &simple("pb", None));
    let root = dir.path();
    let (flag, _fail) = fail_flag(root);
    let res = run(root, "pb", None, RunOptions::default()).unwrap();
    c.machine_agent("ask");
    let kind = LaunchKind::Resume {
        run_id: res.run_id.clone(),
    };
    let refusal =
        apb_engine::resume_launch::check_resume_launch(root, &res.run_id, &mcp(kind.clone()))
            .unwrap_err();
    let ctx = mcp(kind)
        .fixed(Some("1.0.0".into()), Some("explicit"))
        .confirmed(Some(LaunchConfirmation::HostRelayed {
            nonce: refusal["launch_nonce"].as_str().unwrap().into(),
        }));
    let grant = apb_engine::resume_launch::check_resume_launch(root, &res.run_id, &ctx)
        .unwrap()
        .unwrap();
    fs::remove_file(&flag).unwrap();
    // Another live write-run holds the tree.
    fs::write(
        root.join(".apb/workdir.lock"),
        std::process::id().to_string(),
    )
    .unwrap();
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let before = read_all(&run_dir).unwrap().len();
    let err = resume_req(
        root,
        &res.run_id,
        &ResumeRequest {
            from_node: Some("s"),
            launch: Some(&grant),
            ..ResumeRequest::operator()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("workdir"), "{err}");
    assert_eq!(
        read_all(&run_dir).unwrap().len(),
        before,
        "nothing journaled"
    );
    assert!(apb_engine::gate::launch::run_authorization(&run_dir).is_none());
    fs::remove_file(root.join(".apb/workdir.lock")).unwrap();
    let v = apb_engine::resume_launch::check_resume_launch(
        root,
        &res.run_id,
        &LaunchContext::engine(EngineResume::PauseWaker, &res.run_id),
    )
    .unwrap_err();
    assert_eq!(policy_of(&v), ENGINE_POLICY);
}

/// Deleting the project's `.apb/policy.yaml` never loosens a policy the
/// playbook declares: the declaration and its floor still hold.
#[test]
fn deleting_the_project_file_does_not_loosen_a_declared_policy() {
    let _c = cfg();
    let dir = project("pb", &simple("pb", Some("ask")));
    let root = dir.path();
    fs::write(root.join(".apb/policy.yaml"), "launch:\n  agent: deny\n").unwrap();
    let v = apb_engine::gate::check_run(root, &wref("pb"), true, false, &mcp(LaunchKind::Start))
        .unwrap_err();
    assert_eq!(policy_of(&v), DENY_POLICY, "{v}");
    fs::remove_file(root.join(".apb/policy.yaml")).unwrap();
    let v = apb_engine::gate::check_run(root, &wref("pb"), true, false, &mcp(LaunchKind::Start))
        .unwrap_err();
    assert_eq!(policy_of(&v), ASK_POLICY, "{v}");
}

/// A trial runs the child tree its launch check verified: a child whose
/// `current` moves to a `deny` version between the check and the spawn
/// (here, the parent's own step moves it) is not what runs; the pinned
/// version is, under the trial's authorization.
#[test]
fn a_trial_runs_the_pinned_child_when_its_current_moves_to_deny() {
    let _c = cfg();
    let parent = yaml(
        "parent",
        None,
        "  - { id: c, type: playbook, playbook: child }\n",
        "  - { from: s, to: c }\n  - { from: c, to: done }\n",
    );
    let dir = project("parent", &parent);
    let root = dir.path();
    seed_in(&root.join(".apb"), "child", "1.0.0", &simple("child", None));
    let loaded = apb_core::registry::Registry::open(root)
        .unwrap()
        .load("parent", None)
        .unwrap();
    let origin = Origin::Project { workspace_id: None };
    let checked = apb_engine::gate::check_trial_launch(
        root,
        &loaded,
        &origin,
        false,
        &mcp(LaunchKind::Trial),
    )
    .unwrap();
    let (children, grant) = (checked.children, checked.grant);
    assert_eq!(children["c"].version, "1.0.0");

    let child_dir = root.join(".apb/playbooks/child");
    let deny = simple("child", Some("deny")).replace("version: 1.0.0", "version: 1.1.0");
    let turn = root.join("turn.sh");
    fs::write(
        &turn,
        format!(
            "mkdir -p '{d}/1.1.0/scripts' && cat > '{d}/1.1.0/playbook.yaml' <<'EOF'\n{deny}EOF\ncp '{d}/1.0.0/scripts/s.sh' '{d}/1.1.0/scripts/s.sh'\nprintf 1.1.0 > '{d}/current'\n",
            d = child_dir.display()
        ),
    )
    .unwrap();
    let _turn = common::EnvVar::set("APB_TEST_TURN", &turn);
    let res = run(
        root,
        "parent",
        None,
        RunOptions {
            expected_children: Some(children),
            launch: Some(grant),
            ..RunOptions::default()
        },
    )
    .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(child_dir.join("current")).unwrap(),
        "1.1.0",
        "the step moved it"
    );
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let child = read_all(&run_dir)
        .unwrap()
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .unwrap();
    let child_dir = root.join(".apb/runs").join(&child);
    let version = read_all(&child_dir)
        .unwrap()
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunStarted { version, .. } => Some(version.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(version, "1.0.0", "the pinned child ran");
    let record = apb_engine::gate::launch::run_authorization(&child_dir).unwrap();
    assert_eq!(record.kind, "child");
    assert_eq!(
        apb_engine::gate::launch::run_authorization(&run_dir)
            .unwrap()
            .kind,
        "trial"
    );
}

/// The current-run patch keeps the run's own launch policy: after a person
/// lowered the floor (so an agent's save of a looser version passes), a
/// supervisor patch to that version is still rejected for a run whose
/// snapshot declares `deny`.
#[test]
fn a_supervisor_patch_never_loosens_the_runs_launch_policy() {
    use apb_core::launch::floor::{self, FloorKey};
    use apb_engine::control::Control;
    use apb_engine::scheduler::{
        RunMode, drive_prepared, post_supervisor_command, prepare_supervised_background,
    };
    let _c = cfg();
    let dir = project("pb", &simple("pb", Some("deny")));
    let root = dir.path();
    floor::set_by_person(
        &FloorKey::project_of(root, "pb").unwrap(),
        LaunchPolicy::Allow,
    )
    .unwrap();
    let permit = apb_engine::gate::check_run(
        root,
        &wref("pb"),
        true,
        true,
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .unwrap();
    let mut opts = RunOptions {
        mode: RunMode::Supervised,
        ..RunOptions::default()
    };
    permit.apply(&mut opts);
    let prepared = prepare_supervised_background(root, "pb", None, opts).unwrap();
    let run_id = prepared.run_id().to_string();
    let version = apb_core::versioning::create_patch_version(
        root,
        "pb",
        "1.0.0",
        &simple("pb", None),
        &run_id,
        "improvement",
    )
    .expect("the floor a person lowered admits the agent's save");
    post_supervisor_command(
        root,
        &run_id,
        Control::Patch {
            version,
            classification: "improvement".into(),
            continue_from: "s".into(),
        },
    )
    .unwrap();
    let res = drive_prepared(root, prepared).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&run_id)).unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::PatchRejected { reason } if reason.contains("launch_policy_loosened")
        )),
        "the looser patch is rejected"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::PatchApplied { .. })),
        "nothing was applied"
    );
}

/// A run without pins, a `deny` playbook and no authorization; a person
/// lowered the floor, so a looser version can be saved by an agent only
/// after that. Without the lowering, the supervisor's patch save itself is
/// refused (`launch_policy_loosened`) and nothing is written.
#[test]
fn a_supervisor_patch_version_cannot_loosen_a_held_floor() {
    let _c = cfg();
    let dir = project("pb", &simple("pb", Some("deny")));
    let root = dir.path();
    let res = run(
        root,
        "pb",
        None,
        RunOptions {
            launch: Some(dashboard_grant(root, "pb")),
            ..RunOptions::default()
        },
    )
    .unwrap();
    let before = apb_core::registry::list_versions(&root.join(".apb/playbooks/pb")).unwrap();
    for loose in [simple("pb", None), simple("pb", Some("ask"))] {
        let e = apb_core::versioning::create_patch_version(
            root,
            "pb",
            "1.0.0",
            &loose,
            &res.run_id,
            "improvement",
        )
        .unwrap_err();
        assert!(e.to_string().contains("launch_policy_loosened"), "{e}");
    }
    assert_eq!(
        apb_core::registry::list_versions(&root.join(".apb/playbooks/pb")).unwrap(),
        before
    );
}

/// A dashboard (with keys) grant for a start of `id`, applied as every
/// surface applies it.
fn dashboard_grant(root: &Path, id: &str) -> apb_engine::gate::launch::LaunchGrant {
    let permit = apb_engine::gate::check_run(
        root,
        &wref(id),
        true,
        false,
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .unwrap();
    permit.launch.expect("a gated start carries its grant")
}

/// A run prepared without an authorization (a driver whose start predates
/// the launch policy) is driven by `__drive-run` without `--resume` only
/// while its playbook is `allow` for agents now: that entry takes no
/// handoff and never decides a launch.
#[test]
fn a_driver_started_without_a_resume_checks_the_authorization() {
    use apb_engine::scheduler::{RunMode, prepare_supervised_background};
    let c = cfg();
    let dir = project("pb", &simple("pb", None));
    let root = dir.path();
    let prepared = prepare_supervised_background(
        root,
        "pb",
        None,
        RunOptions {
            mode: RunMode::Supervised,
            ..RunOptions::default()
        },
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    drop(prepared);
    let run_dir = root.join(".apb/runs").join(&run_id);
    assert!(apb_engine::gate::launch::run_authorization(&run_dir).is_none());
    c.machine_agent("deny");
    let err = apb_engine::drive_run_from_dir(root, &run_id).unwrap_err();
    assert!(err.to_string().contains("launch_unchecked"), "{err}");
    assert!(
        apb_engine::state::RunState::fold(&read_all(&run_dir).unwrap())
            .nodes
            .is_empty(),
        "nothing ran"
    );
}

/// The engine's backstops read the settings strictly: a damaged project
/// `policy.yaml` (agent writable) never drops the machine's `ask` for an
/// unchecked start or an unchecked resume.
#[test]
fn a_damaged_project_file_never_drops_the_machine_section_in_a_backstop() {
    let c = cfg();
    let dir = project("pb", &simple("pb", None));
    let root = dir.path();
    let (flag, _fail) = fail_flag(root);
    let res = run(root, "pb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    fs::remove_file(&flag).unwrap();
    c.machine_agent("ask");
    fs::write(root.join(".apb/policy.yaml"), "launch: [not, a, mapping\n").unwrap();
    let err = resume_req(
        root,
        &res.run_id,
        &ResumeRequest {
            from_node: Some("s"),
            ..ResumeRequest::operator()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("policy_file_invalid"), "{err}");
    let err = run(root, "pb", None, RunOptions::default()).unwrap_err();
    assert!(err.to_string().contains("policy_file_invalid"), "{err}");
}

/// A floor raised while a start is being prepared, after its first check:
/// the final recheck holds the floor lock until the manifest is written, so
/// the publication lands before it (and refuses the start) or after the
/// record, never in between.
#[test]
fn a_floor_raised_during_preparation_refuses_the_start() {
    let _c = cfg();
    let dir = project("pb", &simple("pb", None));
    let root = dir.path().to_path_buf();
    let permit =
        apb_engine::gate::check_run(&root, &wref("pb"), true, false, &mcp(LaunchKind::Start))
            .unwrap();
    // Held as a publication holds it: the start's final recheck waits.
    let hold = apb_core::launch::floor::hold().unwrap().unwrap();
    let starter = {
        let root = root.clone();
        let config = std::env::var_os("APB_CONFIG_DIR").unwrap();
        std::thread::spawn(move || {
            let _ = config;
            let mut opts = RunOptions::default();
            permit.apply(&mut opts);
            run(&root, "pb", None, opts)
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(400));
    // The publication's floor write, under the lock it holds.
    let key = apb_core::launch::floor::FloorKey::project_of(&root, "pb").unwrap();
    fs::write(
        apb_core::launch::floor::path().unwrap(),
        format!("{{\"floors\":{{\"{key}\":\"deny\"}}}}"),
    )
    .unwrap();
    drop(hold);
    let err = starter.join().unwrap().unwrap_err();
    assert!(err.to_string().contains(POLICY_CHANGED), "{err}");
    let runs = root.join(".apb/runs");
    for entry in fs::read_dir(&runs).into_iter().flatten().flatten() {
        assert!(
            apb_engine::gate::launch::run_authorization(&entry.path()).is_none(),
            "no run was authorized"
        );
    }
}

/// A child run started without pins of its own (by an older apb) and
/// reattached under a parent resumed with a grant: the child gets the
/// parent's verified pin for its own children, so a grandchild published as
/// `deny` after the check is not what it runs.
#[test]
fn a_reattached_legacy_child_runs_its_parents_pinned_descendants() {
    let _c = cfg();
    let parent = yaml(
        "parent",
        None,
        "  - { id: c, type: playbook, playbook: child }\n",
        "  - { from: s, to: c }\n  - { from: c, to: done }\n",
    );
    let child = yaml(
        "child",
        None,
        "  - { id: g, type: playbook, playbook: grand }\n",
        "  - { from: s, to: g }\n  - { from: g, to: done }\n",
    );
    let dir = project("parent", &parent);
    let root = dir.path();
    seed_in(&root.join(".apb"), "child", "1.0.0", &child);
    seed_in(&root.join(".apb"), "grand", "1.0.0", &simple("grand", None));
    // A legacy start: no grant, no pins anywhere.
    let res = run(root, "parent", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let child_of = |dir: &Path| {
        read_all(dir)
            .unwrap()
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
                _ => None,
            })
            .unwrap()
    };
    let child_run = child_of(&run_dir);
    let child_dir = root.join(".apb/runs").join(&child_run);
    // Both drivers died mid-way, as a crash would leave them: the parent
    // right after it started the child, the child inside its own step.
    let cut = |dir: &Path, keep_through: &dyn Fn(&str) -> bool| {
        let journal = dir.join("events.jsonl");
        let mut kept = Vec::new();
        for line in fs::read_to_string(&journal).unwrap().lines() {
            kept.push(line.to_string());
            if keep_through(line) {
                break;
            }
        }
        fs::write(&journal, kept.join("\n") + "\n").unwrap();
    };
    cut(&run_dir, &|l| l.contains("\"child_run_started\""));
    cut(&child_dir, &|l| {
        l.contains("\"node_started\"") && l.contains("\"node\":\"s\"")
    });
    assert!(
        apb_engine::run_config::read_run_config(&child_dir)
            .unwrap()
            .expected_children
            .is_none()
    );
    // The resume of the parent is checked (allow) and gets its pins.
    let grant = apb_engine::resume_launch::check_resume_launch(
        root,
        &res.run_id,
        &mcp(LaunchKind::Resume {
            run_id: res.run_id.clone(),
        }),
    )
    .unwrap()
    .expect("a never-authorized run gets a grant");
    let pinned = grant.pins.clone().unwrap();
    // The grandchild turns `deny` after the commit: the child's own step
    // publishes it right before the grandchild's spawn.
    let grand_dir = root.join(".apb/playbooks/grand");
    let deny = simple("grand", Some("deny")).replace("version: 1.0.0", "version: 1.1.0");
    let turn = root.join("turn.sh");
    fs::write(
        &turn,
        format!(
            "mkdir -p '{d}/1.1.0/scripts' && cat > '{d}/1.1.0/playbook.yaml' <<'EOF'\n{deny}EOF\ncp '{d}/1.0.0/scripts/s.sh' '{d}/1.1.0/scripts/s.sh'\nprintf 1.1.0 > '{d}/current'\n",
            d = grand_dir.display()
        ),
    )
    .unwrap();
    let _turn = common::EnvVar::set("APB_TEST_TURN", &turn);
    let resumed = resume_req(
        root,
        &res.run_id,
        &ResumeRequest {
            launch: Some(&grant),
            ..ResumeRequest::operator()
        },
    )
    .unwrap();
    assert_eq!(
        resumed.outcome,
        RunStatus::Succeeded,
        "parent {:#?}\nchild {:#?}",
        read_all(&run_dir).unwrap(),
        read_all(&child_dir).unwrap()
    );
    assert_eq!(
        fs::read_to_string(grand_dir.join("current")).unwrap(),
        "1.1.0",
        "the step turned it"
    );
    assert_eq!(
        apb_engine::run_config::read_run_config(&child_dir)
            .unwrap()
            .expected_children
            .as_ref(),
        Some(&pinned["c"].children),
        "the reattached child carries its parent's pin"
    );
    let grand_run = read_all(&child_dir)
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .unwrap();
    let version = read_all(&root.join(".apb/runs").join(&grand_run))
        .unwrap()
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunStarted { version, .. } => Some(version.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        version, "1.0.0",
        "the pinned grandchild ran, not the deny one"
    );
}
