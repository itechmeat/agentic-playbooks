//! The seen text of the `no_new_items` loop exit: for every such condition
//! in the playbook, the keys its node has output so far, rendered for the
//! `{{nodes.<id>.seen.<field>}}` template. Fed into the run's
//! `JudgeInputs` so a node prompt, a judge state field and a cache key all
//! read the same text (spec loops-and-graphs D2, "The seen template").

use std::collections::BTreeMap;

use apb_core::schema::{EdgeCondition, Playbook};

use crate::state::RunState;

/// At most this many keys render, the earliest first.
pub(crate) const SEEN_MAX_KEYS: usize = 200;
/// The rendered text stays within this many bytes.
pub(crate) const SEEN_MAX_BYTES: usize = 8 * 1024;

/// The rendered seen text for every `no_new_items` condition in the
/// playbook, keyed `"<node>.<field>"`, each value the condition's node's
/// keys in first-seen order (see [`render_seen`]). A node that has not run
/// yet renders empty. When several conditions share node and field the
/// first in edge order wins (a different `key` between them is a V165
/// error in the validator).
pub fn seen_texts(playbook: &Playbook, state: &RunState) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for edge in &playbook.edges {
        let Some(EdgeCondition::NoNewItems {
            node, field, key, ..
        }) = &edge.condition
        else {
            continue;
        };
        let name = format!("{node}.{field}");
        if out.contains_key(&name) {
            continue;
        }
        let items = apb_core::loop_items::evaluate(
            state
                .success_outputs
                .get(node)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            field,
            key.as_deref(),
        );
        out.insert(name, render_seen(&items.seen));
    }
    out
}

/// The keys one per line, first-seen order, at most [`SEEN_MAX_KEYS`] keys
/// and [`SEEN_MAX_BYTES`] bytes. When keys are left out, a last line reads
/// `(N more not shown)`. Keys cannot hold a newline (the item id rules
/// forbid one), so the format is unambiguous.
pub(crate) fn render_seen(keys: &[String]) -> String {
    let mut text = String::new();
    let mut shown = 0usize;
    let mut bytes = 0usize;
    for key in keys {
        if shown == SEEN_MAX_KEYS || bytes + key.len() + 1 > SEEN_MAX_BYTES {
            break;
        }
        bytes += key.len() + 1;
        text.push_str(key);
        text.push('\n');
        shown += 1;
    }
    let mut hidden = keys.len() - shown;
    if hidden > 0 {
        // The note line is part of the text: when it would cross the byte
        // bound, the last shown key gives way until the whole text fits.
        loop {
            let note = format!("({hidden} more not shown)");
            if shown == 0 || bytes + note.len() <= SEEN_MAX_BYTES {
                text.push_str(&note);
                break;
            }
            let key = &keys[shown - 1];
            bytes -= key.len() + 1;
            text.truncate(text.len() - key.len() - 1);
            shown -= 1;
            hidden += 1;
        }
    } else {
        text.truncate(text.len().saturating_sub(1));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn keys_render_one_per_line_in_order() {
        assert_eq!(render_seen(&keys(&["a", "b", "c"])), "a\nb\nc");
    }

    #[test]
    fn empty_seen_renders_empty() {
        assert_eq!(render_seen(&[]), "");
    }

    #[test]
    fn more_than_200_keys_leave_a_more_line() {
        let many: Vec<String> = (0..250).map(|i| format!("k{i:03}")).collect();
        let text = render_seen(&many);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 201, "200 keys plus the note line");
        assert_eq!(lines[0], "k000");
        assert_eq!(lines[199], "k199");
        assert_eq!(lines[200], "(50 more not shown)");
    }

    #[test]
    fn exactly_200_keys_leave_no_more_line() {
        let many: Vec<String> = (0..200).map(|i| format!("k{i:03}")).collect();
        let text = render_seen(&many);
        assert_eq!(text.lines().count(), 200);
        assert!(!text.contains("more not shown"));
    }

    #[test]
    fn the_text_stays_within_8_kib_and_names_the_rest() {
        // 8 KiB fits about 273 keys of this length, so the key bound is not
        // what stops the list: the byte bound is.
        let many: Vec<String> = (0..400).map(|i| format!("{:032}", i)).collect();
        let text = render_seen(&many);
        assert!(text.len() <= SEEN_MAX_BYTES, "{} bytes", text.len());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.last().copied().unwrap(),
            &format!("({} more not shown)", 400 - (lines.len() - 1))
        );
    }

    #[test]
    fn a_key_that_would_cross_the_byte_bound_is_left_out() {
        // The first key fits with room for the note but not for a second
        // one of this length.
        let long = "x".repeat(8100);
        let second = "y".repeat(100);
        let text = render_seen(&keys(&[&long, &second]));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], long);
        assert_eq!(lines[1], "(1 more not shown)");
        assert!(text.len() <= SEEN_MAX_BYTES, "{} bytes", text.len());
    }

    #[test]
    fn a_key_that_leaves_no_room_for_the_note_gives_way() {
        // The long key fits alone but so little is left that a second key
        // is dropped AND the note line would cross the bound: the long key
        // is dropped too, so the whole text, note included, stays within
        // the limit.
        let long = "x".repeat(8190);
        let text = render_seen(&keys(&[&long, "t"]));
        assert_eq!(text, "(2 more not shown)");
    }

    fn playbook_with(edges: &[&str]) -> Playbook {
        let mut yaml = String::from(
            "schema: 2\nid: g\nname: g\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: hunt, type: script, script: 'true', runner: sh }\n  - { id: probe, type: script, script: 'true', runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: hunt }\n",
        );
        for e in edges {
            yaml.push_str(&format!("  - {e}\n"));
        }
        Playbook::from_yaml(&yaml).unwrap()
    }

    fn state_with(node: &str, outputs: &[&str]) -> RunState {
        let events: Vec<crate::event::Event> = std::iter::once(crate::event::Event {
            seq: 0,
            ts: 0,
            payload: crate::event::EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
                definition: None,
            },
        })
        .chain(
            outputs
                .iter()
                .enumerate()
                .map(|(i, o)| crate::event::Event {
                    seq: 1 + i as u64,
                    ts: 0,
                    payload: crate::event::EventPayload::node_finished(
                        node,
                        "succeeded",
                        1 + i as u32,
                        *o,
                    ),
                }),
        )
        .collect();
        RunState::fold(&events)
    }

    #[test]
    fn every_condition_gets_an_entry_keyed_node_dot_field() {
        let pb = playbook_with(&[
            "{ from: hunt, to: done, condition: { type: no_new_items, node: hunt, field: findings, rounds: 2 } }",
        ]);
        let state = state_with("hunt", &[r#"{"findings":["a","b"]}"#]);
        let texts = seen_texts(&pb, &state);
        assert_eq!(texts.get("hunt.findings").map(String::as_str), Some("a\nb"));
    }

    #[test]
    fn a_node_that_never_ran_renders_empty() {
        let pb = playbook_with(&[
            "{ from: hunt, to: done, condition: { type: no_new_items, node: probe, field: findings, rounds: 1 } }",
        ]);
        let state = state_with("hunt", &[r#"{"findings":["a"]}"#]);
        let texts = seen_texts(&pb, &state);
        assert_eq!(texts.get("probe.findings").map(String::as_str), Some(""));
    }

    #[test]
    fn two_conditions_on_one_node_and_field_take_the_first_in_edge_order() {
        let pb = playbook_with(&[
            "{ from: hunt, to: done, condition: { type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 } }",
            "{ from: probe, to: done, condition: { type: no_new_items, node: hunt, field: findings, rounds: 1 } }",
        ]);
        let state = state_with("hunt", &[r#"{"findings":[{"id":"a"},{"id":"b"}]}"#]);
        let texts = seen_texts(&pb, &state);
        assert_eq!(texts.get("hunt.findings").map(String::as_str), Some("a\nb"));
    }

    #[test]
    fn the_condition_key_decides_what_counts() {
        let pb = playbook_with(&[
            "{ from: hunt, to: done, condition: { type: no_new_items, node: hunt, field: findings, key: name, rounds: 1 } }",
        ]);
        let state = state_with("hunt", &[r#"{"findings":[{"id":"1","name":"a"}]}"#]);
        let texts = seen_texts(&pb, &state);
        assert_eq!(texts.get("hunt.findings").map(String::as_str), Some("a"));
    }

    #[test]
    fn a_playbook_without_the_condition_has_no_entries() {
        let pb = playbook_with(&[
            "{ from: hunt, to: done, condition: { type: node_status, node: hunt, equals: success } }",
        ]);
        let state = state_with("hunt", &[r#"{"findings":["a"]}"#]);
        assert!(seen_texts(&pb, &state).is_empty());
    }
}
