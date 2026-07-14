use apb_core::migration::{MigrationError, validate_migration};
use apb_core::schema::Playbook;

const VALID: &str = include_str!("fixtures/valid.yaml");

fn playbook() -> Playbook {
    Playbook::from_yaml(VALID).unwrap()
}

fn change_node_title(playbook: &mut Playbook, id: &str) {
    playbook
        .nodes
        .iter_mut()
        .find(|node| node.id == id)
        .unwrap()
        .title = Some("Updated title".into());
}

#[test]
fn migration_allows_changes_outside_executed_nodes_and_continue_from() {
    let base = playbook();

    let mut changed_unexecuted = base.clone();
    change_node_title(&mut changed_unexecuted, "plan");
    validate_migration(&base, &changed_unexecuted, &["start".into()], "start").unwrap();

    let mut changed_continue_from = base.clone();
    change_node_title(&mut changed_continue_from, "plan");
    validate_migration(&base, &changed_continue_from, &["plan".into()], "plan").unwrap();

    let mut with_new_node = base.clone();
    let mut new_node = with_new_node.nodes[1].clone();
    new_node.id = "new-step".into();
    with_new_node.nodes.push(new_node);
    validate_migration(&base, &with_new_node, &["start".into()], "start").unwrap();
}

#[test]
fn migration_rejects_a_changed_executed_node_outside_continue_from() {
    let base = playbook();
    let mut patched = base.clone();
    change_node_title(&mut patched, "plan");

    let err = validate_migration(&base, &patched, &["plan".into()], "start").unwrap_err();
    assert!(matches!(err, MigrationError::ExecutedNodeChanged(id) if id == "plan"));
}

#[test]
fn migration_rejects_removed_executed_node() {
    let base = playbook();
    let mut patched = base.clone();
    patched.nodes.retain(|node| node.id != "plan");

    let err = validate_migration(&base, &patched, &["plan".into()], "start").unwrap_err();
    assert!(matches!(err, MigrationError::ExecutedNodeRemoved(id) if id == "plan"));
}

#[test]
fn migration_requires_continue_from_in_patched_playbook() {
    let base = playbook();

    let err = validate_migration(&base, &base, &[], "missing-node").unwrap_err();
    assert!(matches!(err, MigrationError::ContinueFromMissing(id) if id == "missing-node"));
}
