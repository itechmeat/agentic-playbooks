//! The `reviews` row of `apb doctor --run`: decisions in the review channel
//! that a gate binding snapshots or taking only a person's decision never
//! consumes (no request stamp, a request it never opened, another binding,
//! or a channel that is not a person's). Writes nothing.

use std::path::Path;

use super::check::{RunCheck, WARN};
use crate::event::{Event, EventPayload};
use crate::review::{ReviewEntry, is_stamped_gate, read_reviews_after};

const SUBJECT: &str = "reviews";

/// One row when the channel holds entries no drive consumes, or when it
/// does not read (a clean channel and an unreadable one never look alike);
/// none otherwise.
pub(super) fn review_check(run_dir: &Path, events: &[Event]) -> Option<RunCheck> {
    let playbook = crate::progress::load_run_playbook(run_dir)?;
    let entries = match read_reviews_after(run_dir, None) {
        Ok(entries) => entries,
        Err(e) => {
            return Some(RunCheck::new(
                WARN,
                SUBJECT,
                format!("the review channel does not read, so its decisions were not checked: {e}"),
            ));
        }
    };
    let ignored: Vec<String> = entries
        .iter()
        .filter_map(|e| {
            let gate = playbook.node(&e.cmd.node).filter(|g| is_stamped_gate(g))?;
            ignored_because(run_dir, events, e, gate.person_only)
                .map(|why| format!("#{} for `{}` ({why})", e.seq, e.cmd.node))
        })
        .collect();
    if ignored.is_empty() {
        return None;
    }
    Some(RunCheck::new(
        WARN,
        SUBJECT,
        format!(
            "{} decision(s) in the review channel this apb's drive never takes: {}",
            ignored.len(),
            ignored.join(", ")
        ),
    ))
}

/// Why the drive never consumes entry `e` of a stamped gate; `None` when it
/// answers one of the gate's requests.
fn ignored_because(
    run_dir: &Path,
    events: &[Event],
    e: &ReviewEntry,
    person_only: bool,
) -> Option<&'static str> {
    let Some(request) = e.request else {
        return Some("no request stamp");
    };
    let binding = events.iter().find_map(|ev| match &ev.payload {
        EventPayload::ReviewRequested { node, binding, .. }
            if ev.seq == request && *node == e.cmd.node =>
        {
            Some(binding.as_ref())
        }
        _ => None,
    });
    let Some(binding) = binding else {
        return Some("a request the gate never opened");
    };
    if binding.is_some_and(|b| e.nonce.as_deref() != Some(b.nonce.as_str())) {
        return Some("not the request's binding");
    }
    if person_only && !crate::review::decides_person_only_entry(run_dir, e) {
        return Some("not a person's channel");
    }
    if person_only && !crate::review::passkey_stamp_ok(run_dir, e, person_only) {
        return Some(if e.passkey_proof.is_some() {
            "a passkey assertion that does not verify"
        } else {
            "no passkey assertion"
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A channel that does not read is a warning, never the silence of a
    /// clean one.
    #[test]
    fn an_unreadable_channel_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("playbook.yaml"),
            "schema: 2\nid: g\nname: g\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: g, type: human_review, person_only: true }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: g }\n  - { from: g, to: done }\n",
        )
        .unwrap();
        assert!(review_check(dir.path(), &[]).is_none(), "an empty channel");
        std::fs::write(dir.path().join("reviews.jsonl"), "{not json\n").unwrap();
        let row = review_check(dir.path(), &[]).expect("a row");
        assert_eq!(row.status, WARN);
        assert!(
            row.detail.starts_with("the review channel does not read"),
            "{}",
            row.detail
        );
    }
}
