//! The open deferred retries of a run, as the event log keeps them.
//!
//! A deferred retry (`retry_deferred`) is state the engine needs to continue
//! a run: an apb that does not know it would fold the node as interrupted
//! and re-run it at once with a fresh retry budget. Such an apb skips an
//! unknown event once any later checkpoint exists, so while a deferral is
//! open no checkpoint may be the journal's last event: every `node_finished`
//! and `run_paused` appended then is followed, in the same write, by a
//! `deferred_retry_guard` naming the open deferrals. The guard is itself an
//! unknown event newer than the last checkpoint for that apb, which then
//! refuses to continue the run; stopping it stays possible for every binary.
//!
//! This module only keeps the set; the event log maps each event to its
//! [`Change`] and writes the guard.

use std::collections::BTreeSet;

/// What one event does to the open set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change<'a> {
    /// A retry of the node was deferred.
    Open(&'a str),
    /// The node's next attempt started, or the node finished.
    Close(&'a str),
    /// A re-run revived these nodes: a waiting retry of theirs ends, as
    /// the run state folds it.
    CloseEach(&'a [String]),
    /// The run was stopped: every deferral ends.
    CloseAll,
    None,
}

/// The open deferrals, with how far into the journal file they are folded.
#[derive(Debug, Default)]
pub(super) struct DeferralTracker {
    open: BTreeSet<String>,
    /// The byte length of the journal the set reflects, always on a line
    /// boundary.
    folded_to: u64,
}

/// Applies `change` to `open`.
pub(super) fn apply(open: &mut BTreeSet<String>, change: Change<'_>) {
    match change {
        Change::Open(node) => {
            open.insert(node.to_string());
        }
        other => close(open, other),
    }
}

/// The closing half of `change` on any set keyed by node: what both the
/// event log's tracker and the run state fold apply. An `Open` changes
/// nothing here (the caller records what it opens).
pub(crate) fn close<S: NodeSet>(open: &mut S, change: Change<'_>) {
    match change {
        Change::Close(node) => open.remove_node(node),
        Change::CloseEach(nodes) => nodes.iter().for_each(|n| open.remove_node(n)),
        Change::CloseAll => open.clear_nodes(),
        Change::Open(_) | Change::None => {}
    }
}

/// A set of nodes [`close`] works on.
pub(crate) trait NodeSet {
    fn remove_node(&mut self, node: &str);
    fn clear_nodes(&mut self);
}

impl NodeSet for BTreeSet<String> {
    fn remove_node(&mut self, node: &str) {
        self.remove(node);
    }
    fn clear_nodes(&mut self) {
        self.clear();
    }
}

impl<V> NodeSet for std::collections::BTreeMap<String, V> {
    fn remove_node(&mut self, node: &str) {
        self.remove(node);
    }
    fn clear_nodes(&mut self) {
        self.clear();
    }
}

/// What one event of a journal means to its guard.
pub(super) struct Step<'a> {
    pub(super) seq: u64,
    pub(super) checkpoint: bool,
    pub(super) guard: bool,
    pub(super) change: Change<'a>,
}

/// The deferrals open after `steps` (in journal order), and the seq of the
/// last checkpoint when it was written with a deferral open and no guard
/// came after it. An apb that does not know deferrals refuses to continue
/// a run while any record it does not know comes after the last
/// checkpoint, so a guard anywhere after it protects the deferrals.
pub(super) fn unguarded<'a>(
    steps: impl Iterator<Item = Step<'a>>,
) -> (BTreeSet<String>, Option<u64>) {
    let mut open = BTreeSet::new();
    // The last checkpoint, whether a deferral stayed open across it, and
    // whether a guard came after it.
    let mut last: Option<(u64, bool, bool)> = None;
    for step in steps {
        if step.guard
            && let Some(cp) = last.as_mut()
        {
            cp.2 = true;
        }
        apply(&mut open, step.change);
        if step.checkpoint {
            last = Some((step.seq, !open.is_empty(), false));
        }
    }
    let unguarded = match last {
        Some((seq, true, false)) if !open.is_empty() => Some(seq),
        _ => None,
    };
    (open, unguarded)
}

impl DeferralTracker {
    /// The set `open`, folded up to byte `folded_to` of the journal.
    pub(super) fn new(open: BTreeSet<String>, folded_to: u64) -> Self {
        Self { open, folded_to }
    }

    #[cfg(test)]
    pub(super) fn open(&self) -> &BTreeSet<String> {
        &self.open
    }

    pub(super) fn any_open(&self) -> bool {
        !self.open.is_empty()
    }

    pub(super) fn apply(&mut self, change: Change<'_>) {
        apply(&mut self.open, change);
    }

    pub(super) fn folded_to(&self) -> u64 {
        self.folded_to
    }

    pub(super) fn set_folded_to(&mut self, len: u64) {
        self.folded_to = len;
    }

    /// Starts over from the beginning of the journal (a file shorter than
    /// what was folded: never the case for an append-only journal).
    pub(super) fn reset(&mut self) {
        self.open.clear();
        self.folded_to = 0;
    }

    /// The nodes a guard after a checkpoint with `change` must name: the
    /// set as the checkpoint leaves it, `None` when nothing stays open.
    pub(super) fn open_after(&self, change: Change<'_>) -> Option<Vec<String>> {
        let mut after = self.open.clone();
        apply(&mut after, change);
        (!after.is_empty()).then(|| after.into_iter().collect())
    }
}
