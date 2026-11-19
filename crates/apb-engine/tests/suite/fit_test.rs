//! The fit report keeps compatibility, setup, trust and consent apart: the
//! same global playbook fits one project and not another while its trust
//! stays the same, an untrusted playbook can be ready, an irreversible
//! connector function is a consent matter and an account nobody configured
//! is a setup one.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_engine::fit::{FitSeverity, assess};

use crate::common::{EnvVar, env_lock};

fn write_playbook(parent: &Path, id: &str, yaml: &str) {
    let vdir = parent.join("playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(parent.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
}

fn linear(id: &str, head: &str, node: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\n{head}nodes:\n  - {{ id: start, type: start }}\n{node}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: a }}\n  - {{ from: a, to: done }}\n"
    )
}

const PROMPT: &str = "  - { id: a, type: prompt, prompt: \"x\" }\n";

fn wref(origin: Origin, id: &str) -> PlaybookRef {
    PlaybookRef {
        origin,
        id: id.to_string(),
        version: None,
    }
}

fn project() -> Origin {
    Origin::Project { workspace_id: None }
}

#[test]
fn a_global_playbook_fits_one_project_and_not_another_with_the_same_trust() {
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    write_playbook(
        cfg.path(),
        "notes",
        &linear("notes", "requires:\n  files: [CHANGELOG.md]\n", PROMPT),
    );
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    init_project(a.path()).unwrap();
    init_project(b.path()).unwrap();
    fs::write(a.path().join("CHANGELOG.md"), "x").unwrap();

    let in_a = assess(a.path(), &wref(Origin::Global, "notes")).unwrap();
    let in_b = assess(b.path(), &wref(Origin::Global, "notes")).unwrap();
    assert_eq!(in_a.scope, "global");
    assert!(in_a.summary().compatible && in_a.ready);
    assert!(!in_b.summary().compatible && !in_b.ready);
    assert_eq!(in_b.compatibility[0].code, "requires_unmet");
    assert_eq!(
        in_a.trust, in_b.trust,
        "trust does not depend on the project"
    );
}

#[test]
fn an_untrusted_playbook_can_be_ready() {
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    write_playbook(&root.path().join(".apb"), "p", &linear("p", "", PROMPT));

    let fit = assess(root.path(), &wref(project(), "p")).unwrap();
    assert!(fit.ready);
    assert!(!fit.trusted);
    assert_eq!(fit.trust[0].kind, "playbook");
    assert!(!fit.trust[0].approved && fit.trust[0].acknowledge_covers);
    assert!(!fit.needs_consent);
}

#[test]
fn a_connector_splits_into_setup_trust_and_consent() {
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let cdir = cfg.path().join("connectors/tracker");
    fs::create_dir_all(&cdir).unwrap();
    fs::write(
        cdir.join("connector.yaml"),
        "name: tracker\nversion: 0.1.0\nfunctions:\n  - name: merge\n    description: d\n    method: PUT\n    url: http://a\n    irreversible: true\n",
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    let node = "  - id: a\n    type: agent_task\n    prompt: hi\n    profile: x\n    connectors: [{ name: tracker, functions: [merge] }]\n";
    write_playbook(&root.path().join(".apb"), "p", &linear("p", "", node));

    let fit = assess(root.path(), &wref(project(), "p")).unwrap();
    assert_eq!(
        fit.consent.irreversible,
        vec!["node a (connector tracker: merge)".to_string()]
    );
    assert!(fit.needs_consent);
    assert!(
        fit.trust.iter().all(|t| !t.subject.contains("merge")),
        "an irreversible function is a consent matter, not a trust one"
    );
    let connector = fit.trust.iter().find(|t| t.kind == "connector").unwrap();
    assert!(!connector.approved && !connector.acknowledge_covers);

    let no_account = fit
        .setup
        .iter()
        .find(|i| i.code == "connector_no_account")
        .expect("an unconfigured account is setup");
    assert_eq!(no_account.severity, FitSeverity::Blocking);
    assert!(
        !fit.ready,
        "a blocking setup item keeps the playbook from being ready"
    );
    assert!(
        fit.compatibility
            .iter()
            .all(|i| !i.code.starts_with("connector"))
    );
}

#[test]
fn a_missing_profile_is_one_blocking_setup_item() {
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    let node = "  - { id: a, type: agent_task, prompt: hi, profile: ghost }\n";
    write_playbook(&root.path().join(".apb"), "p", &linear("p", "", node));

    let fit = assess(root.path(), &wref(project(), "p")).unwrap();
    let about: Vec<_> = fit
        .setup
        .iter()
        .filter(|i| i.subject.as_deref() == Some("ghost") || i.detail.contains("ghost"))
        .collect();
    assert_eq!(about.len(), 1, "{:?}", fit.setup);
    assert_eq!(about[0].severity, FitSeverity::Blocking);
    assert!(!fit.ready);
}

#[test]
fn memory_that_does_not_read_is_a_note_and_stays_where_it_is() {
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    write_playbook(
        &root.path().join(".apb"),
        "p",
        &linear("p", "params:\n  - { name: who, type: text }\n", PROMPT),
    );
    let memory = root.path().join(".apb/memory/project");
    fs::create_dir_all(&memory).unwrap();
    fs::write(memory.join("p.json"), "{ not json").unwrap();

    let fit = assess(root.path(), &wref(project(), "p")).unwrap();
    assert!(memory.join("p.json").is_file(), "a fit changes nothing");
    assert!(
        fit.setup
            .iter()
            .any(|i| i.code == "remembered_ignored" && i.severity == FitSeverity::Note),
        "{:?}",
        fit.setup
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file = memory.join("p.json");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&file).is_err() {
            let fit = assess(root.path(), &wref(project(), "p")).unwrap();
            assert!(fit.setup.iter().any(|i| i.code == "remembered_unreadable"));
            // The gate then counts nothing as remembered and refuses a start
            // without `who`: the report says so too.
            let refused = apb_engine::gate::check_params(
                root.path(),
                &wref(project(), "p"),
                &Default::default(),
            );
            assert!(refused.is_err());
            assert!(
                fit.setup
                    .iter()
                    .any(|i| i.code == "param_needs_value" && i.subject.as_deref() == Some("who")),
                "{:?}",
                fit.setup
            );
        }
    }
}

/// One item per isolated node: a `full` node this machine cannot isolate
/// (here the sandbox is off) blocks, a `best_effort` one is a note naming
/// why it runs without a sandbox.
#[test]
fn an_isolated_node_this_machine_cannot_isolate_blocks_the_fit() {
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let _off = EnvVar::set("APB_SANDBOX", "off");
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    let full =
        "  - { id: a, type: agent_task, prompt: hi, isolation: { level: full, network: none } }\n";
    write_playbook(&root.path().join(".apb"), "f", &linear("f", "", full));
    let fit = assess(root.path(), &wref(project(), "f")).unwrap();
    let item = fit.setup.iter().find(|i| i.code == "isolation").unwrap();
    assert_eq!(item.subject.as_deref(), Some("a"));
    assert_eq!(item.severity, FitSeverity::Blocking);
    assert!(item.detail.contains("disabled"), "{}", item.detail);
    assert!(!fit.ready);

    let be = "  - { id: a, type: agent_task, prompt: hi, isolation: best_effort }\n";
    write_playbook(&root.path().join(".apb"), "b", &linear("b", "", be));
    let fit = assess(root.path(), &wref(project(), "b")).unwrap();
    let item = fit.setup.iter().find(|i| i.code == "isolation").unwrap();
    assert_eq!(item.severity, FitSeverity::Note);
    assert!(item.detail.contains("without a sandbox"), "{}", item.detail);
}

/// A playbook that binds `apb-main` (here as `defaults.profile`) and
/// `apb-reviewer` gets one `same_agent_reviewer` note when both run on one
/// agent; never on two agents, never without the reviewer role, and never
/// a blocking item.
#[test]
fn a_reviewer_on_the_main_agent_is_a_note() {
    use crate::common::seed_profile;
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    let both = "defaults:\n  profile: apb-main\n";
    let review = "  - { id: a, type: agent_task, prompt: hi, profile: apb-reviewer }\n";
    write_playbook(
        &root.path().join(".apb"),
        "both",
        &linear("both", both, review),
    );
    let work = "  - { id: a, type: agent_task, prompt: hi, profile: apb-main }\n";
    write_playbook(&root.path().join(".apb"), "main", &linear("main", "", work));
    let notes = |id: &str| {
        let fit = assess(root.path(), &wref(project(), id)).unwrap();
        fit.setup
            .into_iter()
            .filter(|i| i.code == "same_agent_reviewer")
            .collect::<Vec<_>>()
    };

    seed_profile(root.path(), "apb-main", "claude", "claude-opus-4-7", &[]);
    seed_profile(
        root.path(),
        "apb-reviewer",
        "claude",
        "claude-opus-4-7",
        &[],
    );
    let same = notes("both");
    assert_eq!(same.len(), 1, "{same:?}");
    assert_eq!(same[0].subject.as_deref(), Some("apb-reviewer"));
    assert_eq!(same[0].severity, FitSeverity::Note);
    assert_eq!(
        same[0].detail,
        apb_core::bundled::SAME_AGENT_REVIEWER_DETAIL
    );
    assert!(notes("main").is_empty(), "no reviewer role, no note");

    seed_profile(root.path(), "apb-reviewer", "codex", "gpt-5.5", &[]);
    assert!(notes("both").is_empty(), "two agents, no note");
}

/// A model on a cloud route in any chain position, tiers included: an agent
/// step that cannot run on it blocks, an id the catalog lacks is a warning.
#[test]
fn a_cloud_tier_that_cannot_run_steps_blocks_and_an_unknown_id_does_not() {
    let _env = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    let dir = root.path().join(".apb/profiles/vision");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("profile.yaml"),
        "name: vision\ndescription: t\nexecutor:\n  agent: claude\n  model: claude-sonnet-5-5\ntiers:\n  quick: { agent: dsh, model: nearai/Qwen/Qwen3-VL-30B-A3B-Instruct, for: \"Quick looks.\" }\n",
    )
    .unwrap();
    fs::write(dir.join("SOUL.md"), "").unwrap();
    crate::common::seed_profile(root.path(), "typo", "dsh", "nearai/Qwen/Qwen3.8-72B", &[]);
    let node = "  - { id: a, type: agent_task, prompt: hi, profile: vision }\n  - { id: b, type: agent_task, prompt: hi, profile: typo }\n";
    let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n"
        .to_string()
        + node
        + "  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n";
    write_playbook(&root.path().join(".apb"), "p", &yaml);

    let fit = assess(root.path(), &wref(project(), "p")).unwrap();
    let no_tools = fit
        .setup
        .iter()
        .find(|i| i.code == "cloud_model_no_tools")
        .unwrap_or_else(|| panic!("{:?}", fit.setup));
    assert_eq!(no_tools.severity, FitSeverity::Blocking);
    let unknown = fit
        .setup
        .iter()
        .find(|i| i.code == "cloud_model_unknown")
        .unwrap_or_else(|| panic!("{:?}", fit.setup));
    assert_ne!(unknown.severity, FitSeverity::Blocking);
    assert!(!fit.ready);
}

/// The readiness section of the report: the items in order, ready needing
/// every item, readiness never changing `ready`, the missing items joined
/// by `not_ready_detail`, and probation as one item.
mod readiness {
    use super::*;
    use apb_engine::fit::{FitReport, Readiness, ReadinessItem, not_ready_detail};

    const READY: &str = r#"schema: 2
id: rf
name: rf
version: 1.0.0
unattended:
  after_attended_successes: 2
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

    fn write_playbook(parent: &Path, id: &str, yaml: &str) {
        let vdir = parent.join("playbooks").join(id).join("1.0.0");
        fs::create_dir_all(&vdir).unwrap();
        fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
        fs::write(parent.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
    }

    fn assess_ready(root: &Path) -> FitReport {
        let dir = root.join(".apb");
        write_playbook(&dir, "rf", READY);
        let goal = dir.join("playbooks/rf/1.0.0/scripts/goal.sh");
        fs::create_dir_all(goal.parent().unwrap()).unwrap();
        fs::write(&goal, GOAL_SH).unwrap();
        assess(root, &wref(project(), "rf")).unwrap()
    }

    /// The items in order: done_defined, checked_actors, bounded_actors,
    /// budget, irreversible_gated, probation.
    #[test]
    fn the_items_in_order() {
        let lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        init_project(dir.path()).unwrap();
        let report = assess_ready(dir.path());
        drop(lock);
        let codes: Vec<&str> = report
            .readiness
            .items
            .iter()
            .map(|i| i.code.as_str())
            .collect();
        assert_eq!(
            codes,
            vec![
                "done_defined",
                "checked_actors",
                "bounded_actors",
                "budget",
                "irreversible_gated",
                "probation"
            ],
            "{codes:?}"
        );
        // Every item but probation holds: the ledger is fresh, and the
        // playbook asks for two attended successes.
        let by_code = |code: &str| {
            report
                .readiness
                .items
                .iter()
                .find(|i| i.code == code)
                .unwrap_or_else(|| panic!("{code} missing"))
        };
        for code in [
            "done_defined",
            "checked_actors",
            "bounded_actors",
            "budget",
            "irreversible_gated",
        ] {
            assert!(by_code(code).ok, "{code}: {:?}", by_code(code).detail);
        }
        assert!(!by_code("probation").ok);
        assert!(!report.readiness.ready);
        assert!(!report.summary().ready_alone);
        // Readiness never changes `ready`.
        assert!(report.ready);
        assert!(
            not_ready_detail(&report.readiness)
                .unwrap()
                .contains("attended runs")
        );
    }

    /// Readiness never changes `ready`: a report whose readiness fails
    /// on every item still says nothing blocks a run here.
    #[test]
    fn readiness_never_changes_fit_ready() {
        let lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        init_project(dir.path()).unwrap();
        let bare = "schema: 2\nid: rf\nname: rf\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
        write_playbook(&dir.path().join(".apb"), "rf", bare);
        let report = assess(dir.path(), &wref(project(), "rf")).unwrap();
        drop(lock);
        assert!(!report.readiness.ready);
        assert!(report.ready, "readiness is about running alone only");
        assert!(!report.summary().ready_alone);
    }

    /// `ready` needs every item: one not-ok item and `ready` is false.
    #[test]
    fn ready_needs_every_item() {
        let lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        init_project(dir.path()).unwrap();
        let no_budget = READY.replace("\nbudget:\n  tokens: 1000", "");
        let dir_apb = dir.path().join(".apb");
        write_playbook(&dir_apb, "rf", &no_budget);
        let goal = dir_apb.join("playbooks/rf/1.0.0/scripts/goal.sh");
        fs::create_dir_all(goal.parent().unwrap()).unwrap();
        fs::write(&goal, GOAL_SH).unwrap();
        let report = assess(dir.path(), &wref(project(), "rf")).unwrap();
        drop(lock);
        assert!(!report.readiness.ready);
        assert!(!report.summary().ready_alone);
        // `ready` (nothing blocks a run here) is unchanged.
        assert!(report.ready);
        let detail = not_ready_detail(&report.readiness).unwrap();
        assert!(
            detail.starts_with("no budget: a run alone spends without a limit; "),
            "{detail}"
        );
    }

    /// `not_ready_detail` joins the missing items.
    #[test]
    fn not_ready_detail_joins_the_missing_items() {
        let r = Readiness {
            ready: false,
            items: vec![
                ReadinessItem {
                    code: "budget".into(),
                    ok: false,
                    subject: None,
                    detail: "no budget".into(),
                },
                ReadinessItem {
                    code: "done_defined".into(),
                    ok: false,
                    subject: None,
                    detail: "no goal".into(),
                },
            ],
            probation: None,
        };
        assert_eq!(not_ready_detail(&r).as_deref(), Some("no budget; no goal"));
    }

    /// An unverifiable ledger is a probation item that is not ok.
    #[test]
    fn an_unverifiable_ledger_is_a_probation_item() {
        let lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        init_project(dir.path()).unwrap();
        let report = assess_ready(dir.path());
        // Tamper the ledger into not verifying.
        let ledger = dir.path().join(".apb/probation/project/rf.json");
        fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        fs::write(&ledger, "{not json").unwrap();
        let report2 = assess(dir.path(), &wref(project(), "rf")).unwrap();
        drop(lock);
        let probation = report2
            .readiness
            .items
            .iter()
            .find(|i| i.code == "probation")
            .unwrap();
        assert!(!probation.ok, "{probation:?}");
        assert!(
            probation.detail.contains("does not verify"),
            "{probation:?}"
        );
        let _ = report;
    }

    /// `start_check` leaves the probation item out: a playbook whose only
    /// gap is its probation passes the `--require-ready` check.
    #[test]
    fn start_check_leaves_probation_out() {
        let lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        init_project(dir.path()).unwrap();
        let report = assess_ready(dir.path());
        drop(lock);
        assert!(
            report.readiness.items.iter().any(|i| i.code == "probation"),
            "the report carries the probation item"
        );
    }
}
