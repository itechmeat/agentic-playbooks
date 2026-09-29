//! `protect` on an agent_task (C6): V75 for a glob that cannot work, V76
//! (preflight) for one that matches nothing in the project.

use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

fn playbook(protect: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: {{ profile: x }}\nnodes:\n  - {{ id: s, type: start }}\n  - {{ id: a, type: agent_task, prompt: hi, protect: {protect} }}\n  - {{ id: f, type: finish, outcome: success }}\nedges:\n  - {{ from: s, to: a }}\n  - {{ from: a, to: f }}\n"
    )
}

fn v75(protect: &str) -> Vec<String> {
    let p = Playbook::from_yaml(&playbook(protect)).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.code == "V75" && i.severity == Severity::Error)
        .map(|i| i.message)
        .collect()
}

#[test]
fn v75_refuses_globs_that_leave_the_tree_or_do_not_parse() {
    assert!(v75(r#"["tests/**", "docs/spec/*.md"]"#).is_empty());
    for bad in [
        r#"["/etc/passwd"]"#,
        r#"["../other/**"]"#,
        r#"["tests/../../x"]"#,
        r#"["  "]"#,
        r#"["tests/[z-a"]"#,
    ] {
        assert_eq!(v75(bad).len(), 1, "{bad}");
    }
}

#[test]
fn protect_round_trips_and_is_left_out_when_empty() {
    let p = Playbook::from_yaml(&playbook(r#"["tests/**"]"#)).unwrap();
    assert_eq!(p.nodes[1].kind.protect_globs(), ["tests/**".to_string()]);
    let yaml = serde_yaml_ng::to_string(&p).unwrap();
    assert!(yaml.contains("protect"));
    let none = Playbook::from_yaml(&playbook("[]")).unwrap();
    assert!(!serde_yaml_ng::to_string(&none).unwrap().contains("protect"));
}

#[test]
fn v76_warns_about_a_glob_that_matches_no_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
    std::fs::write(tmp.path().join("tests/a.rs"), "x").unwrap();
    let p = Playbook::from_yaml(&playbook(r#"["tests/**", "spec/*.md"]"#)).unwrap();
    let found: Vec<(&str, String)> = apb_core::preflight::findings(tmp.path(), &p)
        .into_iter()
        .filter(|(code, _)| *code == "V76")
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].1.contains("`spec/*.md`"), "{}", found[0].1);
}
