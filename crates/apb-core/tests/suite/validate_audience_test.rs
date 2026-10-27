//! `audience` on a node, V101 for a setting that cannot apply, and V68 for
//! node ids the engine reserves.

use apb_core::schema::{Audience, NodeKind, Playbook};
use apb_core::validate::{Severity, ValidationContext, validate};

fn playbook(finish: &str, work: &str) -> String {
    format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: {work}, type: agent_task, prompt: hi, profile: x, audience: people }}\n  - {{ id: done, type: finish, outcome: success{finish} }}\nedges:\n  - {{ from: start, to: {work} }}\n  - {{ from: {work}, to: done }}\n"
    )
}

fn issues(yaml: &str) -> Vec<(&'static str, Severity)> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, &ValidationContext::default())
        .issues
        .into_iter()
        .map(|i| (i.code, i.severity))
        .collect()
}

#[test]
fn audience_parses_and_decides_who_reads_the_output() {
    let p = Playbook::from_yaml(&playbook(", prompt: sum up, audience: agents", "work")).unwrap();
    let work = &p.node("work").unwrap().kind;
    assert!(matches!(
        work,
        NodeKind::AgentTask {
            audience: Some(Audience::People),
            ..
        }
    ));
    assert!(work.writes_for_people());
    assert!(!p.node("done").unwrap().kind.writes_for_people());

    let plain = Playbook::from_yaml(&playbook(", prompt: sum up", "work")).unwrap();
    assert!(plain.node("done").unwrap().kind.writes_for_people());
    assert!(!plain.node("start").unwrap().kind.writes_for_people());
}

#[test]
fn v101_warns_on_audience_for_a_finish_that_composes_nothing() {
    let got = issues(&playbook(", audience: people", "work"));
    assert!(got.contains(&("V101", Severity::Warning)), "{got:?}");
    let with_prompt = issues(&playbook(", prompt: sum up, audience: people", "work"));
    assert!(!with_prompt.iter().any(|(c, _)| *c == "V101"));
}

#[test]
fn v68_reserves_every_id_that_starts_with_an_at_sign() {
    for id in ["\"@x\"", "\"@notify\""] {
        let got = issues(&playbook("", id));
        assert!(got.contains(&("V68", Severity::Error)), "{id}: {got:?}");
    }
    assert!(
        !issues(&playbook("", "work"))
            .iter()
            .any(|(c, _)| *c == "V68")
    );
}
