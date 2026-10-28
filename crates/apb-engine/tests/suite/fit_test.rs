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
    write_playbook(&root.path().join(".apb"), "p", &linear("p", "", PROMPT));
    let memory = root.path().join(".apb/memory/project");
    fs::create_dir_all(&memory).unwrap();
    fs::write(memory.join("p.json"), "{ not json").unwrap();

    let fit = assess(root.path(), &wref(project(), "p")).unwrap();
    assert!(fit.ready);
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
