use apb_core::profile_store::PlaybookOrigin;
use apb_core::schema::Playbook;
use apb_core::validate::{ValidationContext, validate};

// Minimal schema-2 playbook: a single agent_task node with a profile, no executors.
fn playbook_with_node_profile(profile_yaml: &str) -> Playbook {
    let src = format!(
        "schema: 1\nid: w\nname: W\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: t, type: agent_task, prompt: \"do\", profile: {profile_yaml} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: t }}\n  - {{ from: t, to: done }}\n"
    );
    Playbook::from_yaml(&src).expect("parse")
}

fn errors(r: &apb_core::validate::ValidationReport) -> Vec<&str> {
    r.issues
        .iter()
        .filter(|i| matches!(i.severity, apb_core::validate::Severity::Error))
        .map(|i| i.code)
        .collect()
}

#[test]
fn unknown_profile_is_v14() {
    // Explicit `scope: project` - existence is checked against the list of
    // project profiles. `scope: auto`/`global` are not checked for existence
    // in the validator (they may resolve to a global one; the run-start
    // resolver checks that).
    let playbook = playbook_with_node_profile("{ name: ghost, scope: project }");
    let ctx = ValidationContext {
        profiles: vec!["architect".into()],
        ..Default::default()
    };
    let r = validate(&playbook, &ctx);
    assert!(
        errors(&r).contains(&"V14"),
        "expected V14, got {:?}",
        errors(&r)
    );
}

#[test]
fn global_scoped_ref_not_falsely_flagged_in_project_playbook() {
    // A reference to a global profile must not trip V14 just because there's
    // no project profile with the same name (regression from review).
    let playbook = playbook_with_node_profile("{ name: shared, scope: global }");
    let ctx = ValidationContext {
        profiles: vec![],
        ..Default::default()
    };
    let r = validate(&playbook, &ctx);
    assert!(
        !errors(&r).contains(&"V14"),
        "global-scoped ref must not trip V14, got {:?}",
        errors(&r)
    );
}

#[test]
fn known_profile_validates_without_executors() {
    let playbook = playbook_with_node_profile("architect");
    let ctx = ValidationContext {
        profiles: vec!["architect".into()],
        ..Default::default()
    };
    let r = validate(&playbook, &ctx);
    assert!(r.is_valid(), "expected valid, got {:?}", errors(&r));
}

#[test]
fn scope_project_in_global_playbook_is_v14() {
    let playbook = playbook_with_node_profile("{ name: architect, scope: project }");
    let ctx = ValidationContext {
        profiles: vec!["architect".into()],
        playbook_origin: PlaybookOrigin::Global,
    };
    let r = validate(&playbook, &ctx);
    assert!(
        errors(&r).contains(&"V14"),
        "expected V14, got {:?}",
        errors(&r)
    );
}

#[test]
fn node_without_profile_or_executor_is_v18() {
    let src = "schema: 1\nid: w\nname: W\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";
    let playbook = Playbook::from_yaml(src).unwrap();
    let ctx = ValidationContext::default();
    let r = validate(&playbook, &ctx);
    assert!(
        errors(&r).contains(&"V18"),
        "expected V18, got {:?}",
        errors(&r)
    );
}

#[test]
fn node_without_executor_ok_when_defaults_profile_present() {
    let src = "schema: 1\nid: w\nname: W\nversion: 1.0.0\ndefaults:\n  profile: architect\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";
    let playbook = Playbook::from_yaml(src).unwrap();
    let ctx = ValidationContext {
        profiles: vec!["architect".into()],
        ..Default::default()
    };
    let r = validate(&playbook, &ctx);
    assert!(r.is_valid(), "expected valid, got {:?}", errors(&r));
}
