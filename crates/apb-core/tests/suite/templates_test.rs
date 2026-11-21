//! The template gallery: every top-level example playbook of
//! `examples/playbooks/` is a template with its eval files, none carries a
//! script node, and `install_evals` writes the scenarios of a draft once.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use apb_core::templates;
use apb_core::validate::{ValidationContext, validate};

#[test]
fn every_example_is_a_template() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/playbooks");
    let on_disk: BTreeSet<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file() && e.path().extension().is_some_and(|x| x == "yaml"))
        .map(|e| e.path().file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    let all = templates::all();
    let ids: BTreeSet<String> = all.iter().map(|t| t.summary.id.clone()).collect();
    // The gallery holds exactly the top-level files, each id its file stem:
    // a file whose id is not its stem is a release-blocking mismatch.
    assert_eq!(ids, on_disk, "the gallery is the examples directory");
}

#[test]
fn no_template_carries_a_script_node() {
    for t in templates::all() {
        let pb = apb_core::schema::Playbook::from_yaml(&t.yaml).unwrap();
        for n in &pb.nodes {
            assert!(
                !matches!(n.kind, apb_core::schema::NodeKind::Script { .. }),
                "template {} node {} is a script",
                t.summary.id,
                n.id
            );
        }
    }
}

#[test]
fn templates_with_scenarios_carry_them() {
    let all = templates::all();
    let find = |id: &str| all.iter().find(|t| t.summary.id == id).unwrap();
    assert_eq!(find("task-check-first").summary.scenarios, 3);
    assert_eq!(find("design-doc-check").summary.scenarios, 4);
    assert_eq!(find("document-checklist").summary.scenarios, 4);
    let inbox = find("inbox-triage");
    assert_eq!(inbox.summary.scenarios, 0);
    assert!(inbox.evals.is_empty());
}

#[test]
fn every_template_validates_without_errors() {
    for t in templates::all() {
        let pb = apb_core::schema::Playbook::from_yaml(&t.yaml).unwrap();
        let report = validate(&pb, &ValidationContext::default());
        let errors: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.severity == apb_core::validate::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "template {}: {errors:?}", t.summary.id);
    }
}

#[test]
fn the_digest_is_of_the_yaml_bytes() {
    let t = templates::get("inbox-triage").unwrap();
    assert!(t.summary.digest.starts_with("sha256:"));
    assert_eq!(
        t.summary.digest,
        apb_core::content::sha256_hex(t.yaml.as_bytes())
    );
}

#[test]
fn install_evals_writes_the_scenarios_once() {
    let t = templates::get("task-check-first").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let n = templates::install_evals(dir.path(), &t).unwrap();
    assert_eq!(n, 3);
    let scenarios = dir.path().join("evals/scenarios");
    let files: Vec<_> = fs::read_dir(&scenarios)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 3, "{files:?}");
    // An identical second install is a no-op.
    let again = templates::install_evals(dir.path(), &t).unwrap();
    assert_eq!(again, 0);
    // A changed file on disk is never replaced: the install refuses.
    let changed = t
        .evals
        .iter()
        .next()
        .map(|(k, v)| (k.clone(), v.clone()))
        .unwrap();
    let on_disk = dir.path().join("evals").join(&changed.0);
    let written = format!("changed\n{}", String::from_utf8_lossy(&changed.1)).into_bytes();
    fs::write(&on_disk, &written).unwrap();
    assert!(
        templates::install_evals(dir.path(), &t).is_err(),
        "a changed file on disk is never replaced"
    );
    assert_eq!(
        fs::read(&on_disk).unwrap(),
        written,
        "the changed file keeps its bytes"
    );
}
