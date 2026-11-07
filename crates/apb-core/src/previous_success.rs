//! The `run.previous_success` namespace: what a routine reads about its
//! latest earlier successful run (see the grammar below), and the
//! `previous_success` block that names the times a first run renders.
//!
//! A reference that starts with [`PREFIX`] is one of:
//!
//! | Reference | Renders |
//! | --- | --- |
//! | `run.previous_success.id` | the earlier run's id, empty when there is none |
//! | `run.previous_success.started_at` | its `run_started` time, RFC 3339 UTC |
//! | `run.previous_success.finished_at` | the time of its last `run_finished` |
//! | `run.previous_success.nodes.<id>.output[.<field>]` | that run's latest output of node `<id>` (`.report` is the same value) |
//!
//! `parse` owns the grammar, the validator's V176 reports what it rejects,
//! and the engine reads the referenced values once at start and journals
//! them on `run_provenance`.

use std::collections::BTreeSet;

use crate::schema::Playbook;

/// The namespace prefix, without the trailing dot.
pub const PREFIX: &str = "run.previous_success";

/// One reference into the namespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PrevRef {
    Id,
    StartedAt,
    FinishedAt,
    /// `nodes.<node>.output[.<field>]`; `report` is the same value.
    Output {
        node: String,
        field: Option<String>,
    },
}

/// `None` when `key` (a trimmed template reference) is not in the
/// namespace; `Some(Err(why))` when the key is in the namespace but
/// malformed. The `Err` text is the reason the V176 message shows.
pub fn parse(key: &str) -> Option<Result<PrevRef, String>> {
    // The namespace owns the bare prefix and everything after it and a
    // dot. A longer word that merely starts with the prefix
    // (`run.previous_successfoo`) is another key and stays out.
    let rest = key.strip_prefix(PREFIX)?;
    if rest.is_empty() {
        return Some(Err("unknown key".into()));
    }
    // A longer word that merely starts with the prefix
    // (`run.previous_successfoo`) is another key and stays out.
    let rest = rest.strip_prefix('.')?;
    let segs: Vec<&str> = rest.split('.').collect();
    match segs.as_slice() {
        ["id"] => Some(Ok(PrevRef::Id)),
        ["started_at"] => Some(Ok(PrevRef::StartedAt)),
        ["finished_at"] => Some(Ok(PrevRef::FinishedAt)),
        ["nodes", node, "output" | "report"] => Some(Ok(PrevRef::Output {
            node: (*node).to_string(),
            field: None,
        })),
        ["nodes", node, "output" | "report", field] if !field.trim().is_empty() => {
            Some(Ok(PrevRef::Output {
                node: (*node).to_string(),
                field: Some((*field).to_string()),
            }))
        }
        // One field only, never a path; a trailing dot names no field.
        ["nodes", _, "output" | "report", ..] => Some(Err("one field only, never a path".into())),
        _ => Some(Err("unknown key".into())),
    }
}

/// The snapshot key of a reference: the reference text after
/// `run.previous_success.`, with `report` spelled `output`
/// (`nodes.a.output.items`), so `.output` and `.report` share one value.
pub fn key(r: &PrevRef) -> String {
    match r {
        PrevRef::Id => "id".to_string(),
        PrevRef::StartedAt => "started_at".to_string(),
        PrevRef::FinishedAt => "finished_at".to_string(),
        PrevRef::Output { node, field } => match field {
            Some(f) => format!("nodes.{node}.output.{f}"),
            None => format!("nodes.{node}.output"),
        },
    }
}

/// Every well-formed reference the definition reads, sorted, without
/// repeats, from the texts that render through a run's template inputs:
/// the prompt and judge state texts, the node `workdir` templates and the
/// declared cache keys. The template-form `writes` is not scanned: V170
/// already refuses a `writes` template that reads anything but
/// `nodes.<id>.output.<field>` or `params.<name>`, so the namespace never
/// renders into a write scope.
pub fn refs(playbook: &Playbook) -> Vec<PrevRef> {
    let mut found = BTreeSet::new();
    let texts = crate::schema::template_texts(playbook)
        .into_iter()
        .chain(crate::schema::workdir_texts(playbook))
        .chain(crate::schema::cache_key_texts(playbook));
    for (_, text) in texts {
        for cap in crate::template::refs(text) {
            if let Some(Ok(r)) = parse(&cap) {
                found.insert(r);
            }
        }
    }
    found.into_iter().collect()
}

/// Whether any of those texts reads the namespace at all (a malformed
/// reference like `run.previous_success.ended` included).
pub fn reads(playbook: &Playbook) -> bool {
    let texts = crate::schema::template_texts(playbook)
        .into_iter()
        .chain(crate::schema::workdir_texts(playbook))
        .chain(crate::schema::cache_key_texts(playbook));
    for (_, text) in texts {
        for cap in crate::template::refs(text) {
            if parse(&cap).is_some() {
                return true;
            }
        }
    }
    false
}

pub use crate::schema::{PreviousSuccessBlock, WhenNone};

#[cfg(test)]
mod tests {
    use super::*;

    fn full(rest: &str) -> String {
        format!("{PREFIX}.{rest}")
    }

    #[test]
    fn parse_accepts_the_five_reference_forms() {
        assert_eq!(parse(&full("id")), Some(Ok(PrevRef::Id)));
        assert_eq!(parse(&full("started_at")), Some(Ok(PrevRef::StartedAt)));
        assert_eq!(parse(&full("finished_at")), Some(Ok(PrevRef::FinishedAt)));
        assert_eq!(
            parse(&full("nodes.a.output")),
            Some(Ok(PrevRef::Output {
                node: "a".into(),
                field: None
            }))
        );
        // `report` is an alias: same ref, same key.
        assert_eq!(
            parse(&full("nodes.a.report.f")),
            Some(Ok(PrevRef::Output {
                node: "a".into(),
                field: Some("f".into())
            }))
        );
        assert_eq!(
            key(&PrevRef::Output {
                node: "a".into(),
                field: Some("f".into())
            }),
            "nodes.a.output.f"
        );
    }

    #[test]
    fn parse_rejects_the_malformed_forms() {
        // One field only, never a path.
        assert!(parse(&full("nodes.a.output.f.g")).is_some_and(|r| r.is_err()));
        // A trailing dot names no field.
        assert!(parse(&full("nodes.a.output.")).is_some_and(|r| r.is_err()));
        // Unknown key after the prefix.
        assert!(parse(&full("foo")).is_some_and(|r| r.is_err()));
        // The bare prefix.
        assert!(parse(PREFIX).is_some_and(|r| r.is_err()));
        // A different namespace is not ours to judge.
        assert_eq!(parse("run.id"), None);
    }

    #[test]
    fn refs_of_a_playbook_are_sorted_and_deduplicated() {
        let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: read, type: agent_task, prompt: \"{{run.previous_success.started_at}} {{run.previous_success.nodes.a.output}} {{run.previous_success.started_at}} {{run.previous_success.nodes.a.report.f}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: read }\n  - { from: read, to: done }\n";
        let pb = Playbook::from_yaml(yaml).unwrap();
        let found = refs(&pb);
        assert_eq!(
            found,
            vec![
                PrevRef::StartedAt,
                PrevRef::Output {
                    node: "a".into(),
                    field: None
                },
                PrevRef::Output {
                    node: "a".into(),
                    field: Some("f".into())
                },
            ]
        );
        assert!(reads(&pb));
    }

    #[test]
    fn reads_counts_a_malformed_reference_and_refs_drops_it() {
        let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: read, type: agent_task, prompt: \"{{run.previous_success.ended}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: read }\n  - { from: read, to: done }\n";
        let pb = Playbook::from_yaml(yaml).unwrap();
        assert!(reads(&pb));
        assert!(refs(&pb).is_empty());
        let plain = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
        let pb = Playbook::from_yaml(plain).unwrap();
        assert!(!reads(&pb));
        assert!(refs(&pb).is_empty());
    }

    #[test]
    fn the_block_round_trips_and_refuses_unknown_keys() {
        let with = "previous_success:\n  when_none:\n    started_at: \"2026-01-01T00:00:00Z\"\n";
        let pb = Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\n{with}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
        ))
        .unwrap();
        let block = pb.previous_success.as_ref().unwrap();
        let wn = block.when_none.as_ref().unwrap();
        assert_eq!(wn.started_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(wn.finished_at, None);
        let wire = serde_json::to_value(&pb).unwrap();
        assert_eq!(
            wire["previous_success"]["when_none"]["started_at"],
            "2026-01-01T00:00:00Z"
        );
        let yaml = yaml_serde::to_string(&pb).unwrap();
        assert!(yaml.contains("previous_success:"), "{yaml}");
        assert!(yaml.contains("when_none:"), "{yaml}");

        // Without `when_none` the block serializes empty.
        let bare = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nprevious_success: {}\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        )
        .unwrap();
        assert!(bare.previous_success.as_ref().unwrap().when_none.is_none());
        let yaml = yaml_serde::to_string(&bare).unwrap();
        assert!(yaml.contains("previous_success:"), "{yaml}");
        assert!(!yaml.contains("when_none"), "{yaml}");

        // An unknown key inside the block is a parse error, not a silence.
        let bad = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nprevious_success:\n  when_no: {}\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        );
        assert!(bad.is_err());
    }
}
