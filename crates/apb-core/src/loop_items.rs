//! What a loop found so far, from the node's successful outputs alone: the
//! readable visits, the keys they produced and the trailing streak of visits
//! that added no new key. The pure half of the `no_new_items` edge
//! condition (the `loop-exit` capability), shared by the engine's routing
//! and the validator's rules.

/// What a run of successful visits of one node read out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoopItems {
    /// Successful visits read.
    pub visits: usize,
    /// Trailing readable visits that added no new key.
    pub streak: usize,
    /// Every key, first-seen order, no duplicates.
    pub seen: Vec<String>,
    /// New keys in the latest visit.
    pub new_in_last: usize,
}

/// Folds the node's successful visit outputs, in journal order, under
/// `field` (with `key` for object elements). Only successful visits are
/// passed in: failed, timed-out and cancelled ones neither count nor break
/// the streak. The condition matches when
/// `visits >= rounds && streak >= rounds`.
pub fn evaluate(outputs: &[String], field: &str, key: Option<&str>) -> LoopItems {
    let mut out = LoopItems::default();
    // Membership beside the ordered list, so a fold over many keys stays
    // linear instead of scanning `seen` once per key.
    let mut known: std::collections::HashSet<String> = std::collections::HashSet::new();
    for output in outputs {
        out.visits += 1;
        let Some(keys) = visit_keys(output, field, key) else {
            out.streak = 0;
            out.new_in_last = 0;
            continue;
        };
        let mut fresh = 0usize;
        for k in keys {
            if known.insert(k.clone()) {
                out.seen.push(k);
                fresh += 1;
            }
        }
        if fresh == 0 {
            out.streak += 1;
        } else {
            out.streak = 0;
        }
        out.new_in_last = fresh;
    }
    out
}

/// The keys one visit's `field` holds, `None` when the visit is unreadable:
/// the output is not a JSON object, the field is absent, is no array (or a
/// string holding one, the same tolerance as the list loader), or some
/// element yields no valid key. A key repeated within one visit is
/// allowed and counts once, so duplicates are kept.
fn visit_keys(output: &str, field: &str, key: Option<&str>) -> Option<Vec<String>> {
    let doc: serde_json::Value = serde_json::from_str(output).ok()?;
    let obj = doc.as_object()?;
    let values = match obj.get(field)? {
        serde_json::Value::Array(values) => values.clone(),
        serde_json::Value::String(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Array(values)) => values,
            _ => return None,
        },
        _ => return None,
    };
    let mut keys = Vec::with_capacity(values.len());
    for (i, v) in values.iter().enumerate() {
        let k = match key {
            Some(name) => crate::list::items::id_of(v.as_object()?.get(name)?, i),
            None => crate::list::items::id_of(v, i),
        }
        .ok()?;
        keys.push(k);
    }
    Some(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_visit_of_plain_strings_counts_its_keys() {
        assert_eq!(
            evaluate(&[r#"{"findings":["a","b"]}"#.into()], "findings", None),
            LoopItems {
                visits: 1,
                streak: 0,
                seen: vec!["a".into(), "b".into()],
                new_in_last: 2,
            }
        );
    }

    #[test]
    fn integers_count_by_text() {
        assert_eq!(
            evaluate(&[r#"{"f":[3,7,3]}"#.into()], "f", None),
            LoopItems {
                visits: 1,
                streak: 0,
                seen: vec!["3".into(), "7".into()],
                new_in_last: 2,
            }
        );
    }

    #[test]
    fn objects_count_by_their_key_value() {
        assert_eq!(
            evaluate(
                &[r#"{"findings":[{"id":"a"},{"id":"b"}]}"#.into()],
                "findings",
                Some("id")
            ),
            LoopItems {
                visits: 1,
                streak: 0,
                seen: vec!["a".into(), "b".into()],
                new_in_last: 2,
            }
        );
    }

    #[test]
    fn an_array_held_in_a_string_is_read() {
        assert_eq!(
            evaluate(&[r#"{"f":"[\"x\"]"}"#.into()], "f", None).seen,
            vec!["x".to_string()]
        );
    }

    #[test]
    fn every_unreadable_shape_breaks_nothing_and_reads_no_keys() {
        for output in [
            r#"{"other":1}"#,
            r#"{"f":"prose"}"#,
            r#"{"f":3}"#,
            r#"{"f":{"a":1}}"#,
            "not json",
            "3",
        ] {
            let r = evaluate(&[output.into()], "f", None);
            assert_eq!((r.visits, r.streak, r.new_in_last), (1, 0, 0));
            assert!(r.seen.is_empty(), "{output:?}");
        }
        // An element without a key (no `key` given: not a string or integer),
        // and a bad key character, are unreadable too.
        for output in [r#"{"f":[{"id":"a"},7]}"#, r#"{"f":["ok","x`y"]}"#] {
            let r = evaluate(&[output.into()], "f", None);
            assert_eq!(r.visits, 1);
            assert!(r.seen.is_empty(), "{output:?}");
        }
    }

    #[test]
    fn an_empty_array_visit_is_readable_and_dry() {
        assert_eq!(
            evaluate(&[r#"{"f":[]}"#.into()], "f", None),
            LoopItems {
                visits: 1,
                streak: 1,
                seen: vec![],
                new_in_last: 0
            }
        );
    }

    #[test]
    fn two_dry_rounds_match_only_after_the_third_visit() {
        let outs = [r#"{"f":["a","b"]}"#, r#"{"f":["a"]}"#, r#"{"f":["b","a"]}"#];
        let outs: Vec<String> = outs.iter().map(|s| s.to_string()).collect();
        for (n, streak) in [(1, 0), (2, 1), (3, 2)] {
            let r = evaluate(&outs[..n], "f", None);
            assert_eq!((r.visits, r.streak), (n, streak), "after {n} visits");
        }
    }

    #[test]
    fn the_streak_climbs_over_repeated_visits() {
        let outs = [
            r#"{"f":["a"]}"#,
            r#"{"f":["b"]}"#,
            r#"{"f":["a"]}"#,
            r#"{"f":["a"]}"#,
        ];
        let outs: Vec<String> = outs.iter().map(|s| s.to_string()).collect();
        let mut streaks = Vec::new();
        for n in 1..=4 {
            streaks.push(evaluate(&outs[..n], "f", None).streak);
        }
        assert_eq!(streaks, vec![0, 0, 1, 2]);
    }

    #[test]
    fn an_unreadable_visit_resets_the_streak_and_adds_nothing() {
        let outs = [
            r#"{"f":["a"]}"#,
            r#"{"f":["a"]}"#,
            r#"{"other":1}"#,
            r#"{"f":["a"]}"#,
        ];
        let outs: Vec<String> = outs.iter().map(|s| s.to_string()).collect();
        let r = evaluate(&outs, "f", None);
        assert_eq!((r.visits, r.streak, r.new_in_last), (4, 1, 0));
        assert_eq!(r.seen, vec!["a".to_string()]);
    }

    #[test]
    fn seen_keeps_first_seen_order_without_duplicates() {
        let outs = [
            r#"{"f":["b","a"]}"#,
            r#"{"f":["a","c"]}"#,
            r#"{"f":["c","d"]}"#,
        ];
        let outs: Vec<String> = outs.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            evaluate(&outs, "f", None).seen,
            vec![
                "b".to_string(),
                "a".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
    }
}
