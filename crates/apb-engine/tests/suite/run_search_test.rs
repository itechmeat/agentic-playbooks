//! Run search fields (U45): which runs have fields, from what, and that a
//! withheld value never leaves a listing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use apb_core::run_search::{SearchFields, parse_terms};
use apb_engine::run_config::{RunConfig, write_run_config};
use apb_engine::{RunFilter, list_runs, list_runs_filtered};
use serde_json::json;

fn definition(version: &str, params: &str) -> String {
    format!(
        "schema: 2\nid: intake\nname: Intake\nversion: {version}\nparams:\n{params}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    )
}

const V1: &str = "  - { name: customer, type: text }\n";
const V2: &str = "  - { name: customer, type: text, search: true }\n  - { name: ticket, type: int, search: true }\n  - { name: deploy_key, type: text, secret: true }\n";

/// A run directory as apb writes it: the journal naming its start
/// definition, the stored definition, the snapshot and `run.yaml`.
fn run(
    root: &Path,
    id: &str,
    ts: u64,
    def: Option<&str>,
    params: &[(&str, &str)],
    origin: Option<&str>,
) -> PathBuf {
    let dir = root.join(".apb/runs").join(id);
    fs::create_dir_all(&dir).unwrap();
    let digest = def.map(|d| apb_engine::run_definitions::store(&dir, d.as_bytes()).unwrap());
    if let Some(d) = def {
        // The run's snapshot, which the run redactor reads for the
        // params the run declares secret.
        fs::write(dir.join("playbook.yaml"), d).unwrap();
    }
    let mut started = json!({"seq": 1, "ts": ts, "type": "run_started", "playbook": "intake", "version": "1.0.0"});
    if let Some(d) = &digest {
        started["definition"] = json!(d);
    }
    let mut lines = vec![started];
    if let Some(o) = origin {
        lines.push(json!({"seq": 2, "ts": ts, "type": "run_provenance", "origin": o}));
    }
    lines.push(json!({"seq": 3, "ts": ts + 1, "type": "run_finished", "outcome": "succeeded"}));
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(dir.join("events.jsonl"), body).unwrap();
    let cfg = RunConfig {
        params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        ..Default::default()
    };
    write_run_config(&dir, &cfg).unwrap();
    dir
}

fn filter(terms: &[&str]) -> RunFilter {
    RunFilter {
        playbook: Some("intake".into()),
        fields: parse_terms(terms).unwrap(),
        ..Default::default()
    }
}

fn ids(l: &apb_engine::RunListing) -> Vec<&str> {
    l.runs.iter().map(|r| r.run_id.as_str()).collect()
}

#[test]
fn only_a_start_definition_that_declares_search_indexes_a_run() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    run(root, "old", 1, None, &[("customer", "Acme")], None);
    run(
        root,
        "v1",
        2,
        Some(&definition("1.0.0", V1)),
        &[("customer", "Acme")],
        None,
    );
    run(
        root,
        "v2",
        3,
        Some(&definition("2.0.0", V2)),
        &[("customer", "Acme"), ("ticket", "042")],
        Some("project"),
    );
    let l = list_runs_filtered(root, &filter(&["customer=acme"])).unwrap();
    assert_eq!(ids(&l), ["v2"]);
    assert_eq!(l.not_indexed, 2, "the old run and the run of version 1");
    let l = list_runs_filtered(root, &filter(&["customer=acme", "ticket=42"])).unwrap();
    assert_eq!(ids(&l), ["v2"]);
    let l = list_runs_filtered(root, &filter(&["ticket=43"])).unwrap();
    assert!(l.runs.is_empty());
    // Without terms every run of the playbook is listed, each with its state.
    let l = list_runs_filtered(root, &filter(&[])).unwrap();
    assert_eq!(ids(&l), ["v2", "v1", "old"]);
    assert_eq!(l.runs[1].search, Some(SearchFields::NotIndexed));
    let Some(SearchFields::Indexed { fields }) = &l.runs[0].search else {
        panic!("v2 is indexed");
    };
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["customer", "ticket"], "never the secret param");
    assert_eq!(l.runs[0].scope.as_deref(), Some("project"));
    assert_eq!(
        l.runs[2].scope.as_deref(),
        Some("project"),
        "no provenance reads project"
    );
}

#[test]
fn a_patch_that_adds_search_does_not_widen_an_older_run() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = run(
        root,
        "r",
        1,
        Some(&definition("1.0.0", V1)),
        &[("customer", "Acme")],
        None,
    );
    // A patch replaces `playbook.yaml`; the start definition stays.
    fs::write(dir.join("playbook.yaml"), definition("1.0.1", V2)).unwrap();
    let l = list_runs_filtered(root, &filter(&["customer=Acme"])).unwrap();
    assert!(l.runs.is_empty());
    assert_eq!(l.not_indexed, 1);
}

#[test]
fn a_credential_or_a_tree_secret_is_withheld_and_never_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    run(
        root,
        "cred",
        1,
        Some(&definition("2.0.0", V2)),
        &[("customer", token), ("ticket", "1")],
        None,
    );
    // The same value as the run's own declared secret.
    let shared = "correct-horse-staple";
    run(
        root,
        "tree",
        2,
        Some(&definition("2.0.0", V2)),
        &[
            ("customer", shared),
            ("deploy_key", shared),
            ("ticket", "2"),
        ],
        None,
    );
    let all = list_runs_filtered(root, &filter(&[])).unwrap();
    let text = serde_json::to_string(&all).unwrap();
    assert!(!text.contains(token), "{text}");
    assert!(!text.contains(shared), "{text}");
    assert_eq!(text.matches("\"state\":\"withheld\"").count(), 2, "{text}");
    // A term against a withheld value is a non-match, counted apart,
    // whatever the term says: both runs withhold `customer`.
    for value in [token, shared, "anything"] {
        let l = list_runs_filtered(root, &filter(&[&format!("customer={value}")])).unwrap();
        assert!(l.runs.is_empty(), "{value}");
        assert_eq!(l.withheld, 2, "{value}");
    }
    let l = list_runs_filtered(root, &filter(&["ticket=2"])).unwrap();
    assert_eq!(ids(&l), ["tree"]);
}

/// A param the run counts secret is withheld whatever its value: a patch
/// marked it secret mid-run, so a value the scrubber cannot replace by
/// shape (four characters) must never be listed or matched either.
#[test]
fn a_param_the_run_counts_secret_is_withheld_even_when_short() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = run(
        root,
        "midrun",
        1,
        Some(&definition("2.0.0", V2)),
        &[("customer", "abcd"), ("ticket", "3")],
        None,
    );
    apb_engine::manifest::add_secret_params(&dir, "midrun", ["customer".to_string()]).unwrap();
    let all = list_runs_filtered(root, &filter(&[])).unwrap();
    let text = serde_json::to_string(&all).unwrap();
    assert!(!text.contains("abcd"), "{text}");
    assert!(text.contains("\"state\":\"withheld\""), "{text}");
    let l = list_runs_filtered(root, &filter(&["customer=abcd"])).unwrap();
    assert!(l.runs.is_empty());
    assert_eq!(l.withheld, 1);
    // The untouched field of the same run still matches.
    let l = list_runs_filtered(root, &filter(&["ticket=3"])).unwrap();
    assert_eq!(ids(&l), ["midrun"]);
}

/// A definition over the cap is skipped for its length, before the whole
/// file is read and hashed.
#[test]
fn a_definition_over_the_cap_is_skipped_before_it_is_read() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut huge = definition("2.0.0", V2);
    huge.push_str("# ");
    huge.push_str(&"x".repeat(1 << 20));
    run(
        root,
        "big",
        1,
        Some(&huge),
        &[("customer", "Acme"), ("ticket", "1")],
        None,
    );
    let l = list_runs_filtered(root, &filter(&["customer=acme"])).unwrap();
    assert!(l.runs.is_empty(), "a too large definition is not indexed");
    assert_eq!(l.not_indexed, 1);
}

/// Without field terms nothing is left out, so nothing counts as
/// not indexed: the count names only what a term filter dropped.
#[test]
fn a_listing_without_terms_counts_nothing_not_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    run(root, "old", 1, None, &[("customer", "Acme")], None);
    run(
        root,
        "v2",
        2,
        Some(&definition("2.0.0", V2)),
        &[("customer", "Acme"), ("ticket", "1")],
        None,
    );
    let l = list_runs_filtered(root, &filter(&[])).unwrap();
    assert_eq!(ids(&l), ["v2", "old"]);
    assert_eq!(l.not_indexed, 0);
    let l = list_runs_filtered(root, &filter(&["customer=acme"])).unwrap();
    assert_eq!(ids(&l), ["v2"]);
    assert_eq!(l.not_indexed, 1);
}

#[test]
fn a_child_run_uses_its_own_definition() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    run(
        root,
        "parent",
        1,
        Some(&definition("1.0.0", V1)),
        &[("customer", "Acme")],
        None,
    );
    let child = run(
        root,
        "child",
        2,
        Some(&definition("2.0.0", V2)),
        &[("customer", "Acme"), ("ticket", "7")],
        None,
    );
    let mut cfg = apb_engine::run_config::read_run_config(&child).unwrap();
    cfg.parent_run = Some("parent".into());
    write_run_config(&child, &cfg).unwrap();
    let l = list_runs_filtered(root, &filter(&["customer=acme"])).unwrap();
    assert_eq!(ids(&l), ["child"]);
    assert_eq!(l.runs[0].parent_run.as_deref(), Some("parent"));
}

#[test]
fn a_project_and_a_global_playbook_of_one_id_both_match_unscoped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let def = definition("2.0.0", V2);
    run(
        root,
        "p",
        1,
        Some(&def),
        &[("customer", "Acme")],
        Some("project"),
    );
    run(
        root,
        "g",
        2,
        Some(&def),
        &[("customer", "Acme")],
        Some("global"),
    );
    let l = list_runs_filtered(root, &filter(&["customer=acme"])).unwrap();
    assert_eq!(ids(&l), ["g", "p"]);
    assert_eq!(l.runs[0].scope.as_deref(), Some("global"));
    let mut f = filter(&["customer=acme"]);
    f.scope = Some("global".into());
    assert_eq!(ids(&list_runs_filtered(root, &f).unwrap()), ["g"]);
}

#[test]
fn the_plain_listing_is_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    run(
        root,
        "v2",
        1,
        Some(&definition("2.0.0", V2)),
        &[("customer", "Acme")],
        Some("global"),
    );
    let plain = serde_json::to_value(list_runs(root).unwrap()).unwrap();
    assert!(plain[0].get("scope").is_none());
    assert!(plain[0].get("search").is_none());
}

#[test]
fn a_real_run_records_what_its_search_fields_need() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    apb_core::registry::init_project(root).unwrap();
    let v = root.join(".apb/playbooks/intake/2.0.0");
    fs::create_dir_all(&v).unwrap();
    fs::write(v.join("playbook.yaml"), definition("2.0.0", V2)).unwrap();
    fs::write(root.join(".apb/playbooks/intake/current"), "2.0.0").unwrap();
    let params: BTreeMap<String, String> = [("customer", "Acme"), ("ticket", "12")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let done = apb_engine::run(
        root,
        "intake",
        None,
        apb_engine::RunOptions {
            params,
            ..Default::default()
        },
    )
    .unwrap();
    let l = list_runs_filtered(root, &filter(&["customer=ACME", "ticket=12"])).unwrap();
    assert_eq!(ids(&l), [done.run_id.as_str()]);
    assert_eq!(l.runs[0].scope.as_deref(), Some("project"));
}
