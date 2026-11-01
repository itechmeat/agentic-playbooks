//! The edge address fixture (`tests/fixtures/edge_addresses.json`), shared
//! with the dashboard's port (`web/src/lib/edgeaddress.test.ts`): both
//! derive, render and parse the same addresses.

use apb_core::edge_address::{AddressedEdge, CheckOrder, EdgeAddress, edges_addressed, resolve};
use apb_core::schema::{Edge, Playbook};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
    strings: Vec<StringCase>,
    invalid: Vec<String>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    edges: Vec<Edge>,
    expected: Vec<Expected>,
}

#[derive(Deserialize)]
struct Expected {
    address: EdgeAddress,
    rendered: String,
    rank: usize,
    check: CheckOrder,
}

#[derive(Deserialize)]
struct StringCase {
    address: EdgeAddress,
    rendered: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("../fixtures/edge_addresses.json")).unwrap()
}

#[test]
fn derivation_matches_the_shared_fixture() {
    for case in fixture().cases {
        let got: Vec<AddressedEdge> = edges_addressed(&case.edges);
        assert_eq!(got.len(), case.expected.len(), "{}", case.name);
        for (i, (g, x)) in got.iter().zip(&case.expected).enumerate() {
            assert_eq!(g.index, i, "{}", case.name);
            assert_eq!(g.address, x.address, "{} #{i}", case.name);
            assert_eq!(g.rendered, x.rendered, "{} #{i}", case.name);
            assert_eq!(g.rank, x.rank, "{} #{i}", case.name);
            assert_eq!(g.check, x.check, "{} #{i}", case.name);
        }
    }
}

#[test]
fn rendering_and_parsing_match_the_shared_fixture() {
    let f = fixture();
    for s in &f.strings {
        assert_eq!(s.address.render(), s.rendered);
        assert_eq!(EdgeAddress::parse(&s.rendered).unwrap(), s.address);
    }
    for bad in &f.invalid {
        assert!(EdgeAddress::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn an_address_resolves_to_exactly_its_edge() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "true", runner: sh }
  - { id: fin, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: fin, condition: { type: node_status, node: a, equals: success } }
  - { id: other, from: a, to: fin, fallback: true }
  - { from: a, to: fin, fallback: true }
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let second = EdgeAddress::Derived {
        from: "a".into(),
        to: "fin".into(),
        ordinal: 2,
    };
    assert_eq!(resolve(&pb, &second), Some(3));
    assert_eq!(
        resolve(&pb, &EdgeAddress::Authored { id: "other".into() }),
        Some(2)
    );
    let third = EdgeAddress::Derived {
        from: "a".into(),
        to: "fin".into(),
        ordinal: 3,
    };
    assert_eq!(resolve(&pb, &third), None);
}
