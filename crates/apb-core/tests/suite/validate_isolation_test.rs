//! V112-V115: a node's `isolation` block. V16 is retired: declared isolation
//! no longer warns, since the engine enforces it.

use apb_core::isolation::{Level, Network};
use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

fn playbook(isolation: &str, extra: &str) -> String {
    format!(
        "schema: 2\nid: iso\nname: Iso\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: do, profile: p, isolation: {isolation}{extra} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: done }}\n"
    )
}

fn issues(yaml: &str) -> Vec<(&'static str, Severity)> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| matches!(i.code, "V112" | "V113" | "V114" | "V115" | "V16"))
        .map(|i| (i.code, i.severity))
        .collect()
}

#[test]
fn stated_levels_raise_nothing_and_v16_is_gone() {
    for iso in [
        "none",
        "best_effort",
        "{ level: full, network: none }",
        "{ level: full, network: open, read_only: [~/.rustup, /opt/sdk], writable: [~/.cargo/registry] }",
    ] {
        assert_eq!(issues(&playbook(iso, "")), [], "{iso}");
    }
}

#[test]
fn v112_warns_on_full_with_an_implicit_network() {
    assert_eq!(issues(&playbook("full", "")), [("V112", Severity::Warning)]);
    assert_eq!(issues(&playbook("{ level: best_effort }", "")), []);
}

#[test]
fn v113_refuses_isolation_with_connectors() {
    let yaml = playbook("{ level: full, network: none }", ", connectors: [jira]");
    assert!(issues(&yaml).contains(&("V113", Severity::Error)));
    let none = playbook("none", ", connectors: [jira]");
    assert!(!issues(&none).iter().any(|(c, _)| *c == "V113"));
}

#[test]
fn v114_refuses_unsafe_extra_paths() {
    for p in [
        "rel/dir",
        "~/a/../b",
        "/",
        "~",
        "~/.ssh",
        "~/.config/gh/hosts.yml",
        "/run/user",
        "~/.git-credentials",
    ] {
        let iso = format!("{{ level: best_effort, read_only: ['{p}'] }}");
        assert_eq!(
            issues(&playbook(&iso, "")),
            [("V114", Severity::Error)],
            "{p}"
        );
        let iso = format!("{{ level: best_effort, writable: ['{p}'] }}");
        assert_eq!(
            issues(&playbook(&iso, "")),
            [("V114", Severity::Error)],
            "{p}"
        );
    }
}

/// A writable path that would let the agent change what the operator runs
/// later (a shell startup file, a user unit, a program directory, the git
/// config) is refused; the same path read-only is fine.
#[test]
fn v114_keeps_writable_paths_off_what_the_operator_runs() {
    for p in [
        "~/.bashrc",
        "~/.local/bin",
        "~/.cargo/bin",
        "~/.config/systemd/user",
        "~/.gitconfig",
    ] {
        let iso = format!("{{ level: full, network: none, writable: ['{p}'] }}");
        assert_eq!(
            issues(&playbook(&iso, "")),
            [("V114", Severity::Error)],
            "{p}"
        );
        let iso = format!("{{ level: full, network: none, read_only: ['{p}'] }}");
        assert_eq!(issues(&playbook(&iso, "")), [], "{p}");
    }
}

#[test]
fn v115_refuses_extras_with_level_none() {
    for iso in [
        "{ level: none, network: none }",
        "{ level: none, read_only: [~/.rustup] }",
        "{ level: none, writable: [~/.cache/x] }",
    ] {
        assert_eq!(
            issues(&playbook(iso, "")),
            [("V115", Severity::Error)],
            "{iso}"
        );
    }
}

#[test]
fn the_node_helpers_read_the_spec() {
    let p = Playbook::from_yaml(&playbook("{ level: full, network: none }", "")).unwrap();
    let w = p.node("w").unwrap();
    let spec = w.kind.isolation().unwrap();
    assert_eq!(spec.level, Level::Full);
    assert_eq!(spec.network(), Network::None);
    assert!(w.kind.is_isolated());
    let none = Playbook::from_yaml(&playbook("none", "")).unwrap();
    assert!(!none.node("w").unwrap().kind.is_isolated());
    assert!(!none.node("start").unwrap().kind.is_isolated());
    // An unknown key in the long form does not parse.
    assert!(Playbook::from_yaml(&playbook("{ level: full, nett: none }", "")).is_err());
}
