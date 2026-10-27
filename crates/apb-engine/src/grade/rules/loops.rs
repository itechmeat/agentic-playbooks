//! `loops`: signs of going in circles.
//!
//! - the same tool called with the same input [`REPEAT`] times in one
//!   session;
//! - a retry that failed the same way as the attempt before it;
//! - a bounded edge whose `max_traversals` was spent;
//! - the same connector function failing [`REPEAT`] times for one node.
//!
//! No sign is `good`, one is `partial`, two or more are `poor`.

use std::collections::BTreeMap;

use apb_core::quality::grade::Level;

use super::finding::{Finding, j, main_session_records, nothing_seen};
use crate::event::EventPayload;
use crate::grade::sources::RunSources;

/// How many identical calls make a repeat.
pub const REPEAT: usize = 3;

/// Each sign as the citations it rests on.
fn signs(sources: &RunSources) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    // The same tool call, again and again.
    let mut calls: BTreeMap<(String, &str, &str), Vec<String>> = BTreeMap::new();
    for (view, rec) in main_session_records(sources) {
        for t in &rec.tools {
            calls
                .entry((view.id(), t.name.as_str(), t.input_digest.as_str()))
                .or_default()
                .push(view.record_ref(rec.index));
        }
    }
    out.extend(calls.into_values().filter(|c| c.len() >= REPEAT));
    let events = &sources.run.events;
    // A retry that failed the same way.
    let mut last_kind: BTreeMap<&str, (String, u64)> = BTreeMap::new();
    for e in events {
        if let EventPayload::AttemptFinished {
            node, failure_kind, ..
        } = &e.payload
        {
            match failure_kind {
                Some(kind) => {
                    if let Some((prev, seq)) = last_kind.get(node.as_str())
                        && prev == kind
                    {
                        out.push(vec![j(*seq), j(e.seq)]);
                    }
                    last_kind.insert(node, (kind.clone(), e.seq));
                }
                None => {
                    last_kind.remove(node.as_str());
                }
            }
        }
    }
    // A bounded loop spent.
    if let Some(pb) = &sources.run.snapshot {
        let mut counted: BTreeMap<(&str, &str), Vec<u64>> = BTreeMap::new();
        for e in events {
            if let EventPayload::EdgeTraversed {
                from,
                to,
                via_policy: false,
                uncounted: false,
            } = &e.payload
            {
                counted.entry((from, to)).or_default().push(e.seq);
            }
        }
        for edge in &pb.edges {
            if let (Some(cap), Some(seqs)) = (
                edge.max_traversals,
                counted.get(&(edge.from.as_str(), edge.to.as_str())),
            ) && seqs.len() as u64 >= u64::from(cap)
                && cap > 0
            {
                out.push(seqs.iter().map(|s| j(*s)).collect());
            }
        }
    }
    // The same connector call failing again.
    let mut failing: BTreeMap<(&str, &str, &str), Vec<String>> = BTreeMap::new();
    for e in events {
        if let EventPayload::ConnectorCall {
            node_id,
            connector,
            function,
            outcome,
            ..
        } = &e.payload
            && outcome != "ok"
        {
            failing
                .entry((node_id, connector, function))
                .or_default()
                .push(j(e.seq));
        }
    }
    out.extend(failing.into_values().filter(|c| c.len() >= REPEAT));
    out
}

pub(super) fn grade(sources: &RunSources) -> Finding {
    let signs = signs(sources);
    let cites: Vec<String> = signs.iter().flatten().cloned().collect();
    match signs.len() {
        0 => nothing_seen(sources, "no sign of going in circles"),
        1 => Finding::new(
            Level::Partial,
            "one_loop_sign",
            "one sign of going in circles",
        )
        .citing(cites),
        n => Finding::new(
            Level::Poor,
            "loop_signs",
            format!("{n} signs of going in circles"),
        )
        .citing(cites),
    }
}
