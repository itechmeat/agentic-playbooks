//! "Notify me", engine side: a run tells a person about its transitions
//! (a review waiting, a question asked, a pause the person did not make,
//! the end of the run) through a connector function they configured.
//!
//! - [`start`] checks a subscription before a run starts and when a person
//!   changes it on a live run, and journals it (`notify_set`).
//! - [`NotifyState`] folds the subscription in force from the journal.
//! - [`transitions`] decides, purely, which transitions are due.
//! - [`notice`] writes the message: the status, the action asked of the
//!   person and a link, never the question text, a prompt or agent output.
//! - [`send`] is the sweep the drive loop runs: each due transition goes out
//!   once through the send-once ledger, under the engine site `@notify`
//!   and the label `notify:<kind>.<seq>`.
//! - [`retry`] retries what the drive's last sweep could not send.
//! - [`view`] is what the run surfaces report.
//! - [`trial`] sends one test message outside any run (`apb notify test`).
//!
//! A notification is not part of the run gate's permit: its target is the
//! person's own subscription, trusted when it was set and checked against
//! the pinned digests at every send. A failure never fails the run.

pub mod health;
pub mod notice;
pub mod record;
pub mod retry;
pub(crate) mod send;
/// The run sweep, for the engine's own integration tests only.
#[cfg(any(test, feature = "testing"))]
pub use send::Notifier;
pub mod start;
pub mod transitions;
pub mod trial;
pub mod view;

use std::path::Path;

use apb_core::notify::{Coverage, FinishOutcome, NotifySource, Transition};

use crate::event::{Event, EventPayload};
use record::NotifyTargetRecord;

pub use start::{ChangedBy, NotifyCheck, NotifyPlan, REFUSED_START, SetError, check, set};
pub use trial::send_trial;
pub use view::RunNotify;

/// The subscription in force for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    pub target: NotifyTargetRecord,
    pub on: Vec<Transition>,
    /// The outcomes a `finished` notice is sent for.
    pub finished: Vec<FinishOutcome>,
    /// The reminder cadence in force (active run milliseconds), from the
    /// latest `notify_set` that named a target.
    pub remind_ms: Vec<u64>,
    pub source: String,
    /// The seq the subscription took effect at: a transition journaled
    /// before it is not news to this subscription.
    pub since_seq: u64,
    /// The seq of the `notify_set` that put it in force as it reads now,
    /// and when (epoch milliseconds; for a sub-playbook, the later of its
    /// own and its parent's latest set). A try the trust gate refused
    /// before it is not held against the subscription: a person who set it
    /// up again gets the still-open transitions through the target they
    /// approved anew.
    pub set_seq: u64,
    pub set_at_ms: u64,
    /// The seq of the `notify_set` in this run's journal that put the
    /// current target in force; a set that keeps the same target
    /// (narrowing the transitions or the outcomes) keeps the previous one.
    /// A reminder goes only to the target its notice was confirmed to,
    /// matched by the target digest its intent records; a notice an older
    /// apb recorded without one counts only when recorded after this.
    pub target_set_seq: u64,
}

impl Active {
    /// What the subscription covers.
    pub fn coverage(&self) -> Coverage {
        Coverage::new(&self.on, &self.finished)
    }

    /// Whether a try recorded at `seq`, `ts_ms` came after the set that put
    /// this subscription in force.
    pub fn is_after_set(&self, seq: u64, ts_ms: u64) -> bool {
        seq > self.set_seq && ts_ms >= self.set_at_ms
    }
}

/// The journal's `notify_set` events, folded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotifyState {
    /// Whether the run ever had a `notify_set`.
    pub recorded: bool,
    pub active: Option<Active>,
    /// Why a subscription could not be used, in journal order.
    pub problems: Vec<String>,
    /// The transitions of the latest subscription the run had, in force or
    /// turned off since; empty when it never had one.
    pub last_on: Vec<Transition>,
    /// The `finished` outcomes of that subscription.
    pub last_finished: Vec<FinishOutcome>,
}

impl NotifyState {
    /// The latest `notify_set` is in force. A change to a subscription
    /// already in force keeps the seq it took effect at; turning it on
    /// again starts afresh.
    pub fn fold(events: &[Event]) -> Self {
        let mut state = NotifyState::default();
        for e in events {
            let EventPayload::NotifySet {
                target,
                on,
                source,
                problem,
                remind_ms,
                ..
            } = &e.payload
            else {
                continue;
            };
            state.recorded = true;
            if let Some(p) = problem {
                state.problems.push(p.clone());
            }
            let Coverage { on, finished } = Coverage::from_journal_names(on);
            state.active = match target {
                Some(target) if !on.is_empty() => {
                    state.last_on = on.clone();
                    state.last_finished = finished.clone();
                    // A set that keeps the same target keeps the target's
                    // set seq and time (narrowing the transitions or the
                    // outcomes); any change of target or pins starts them.
                    let target_set_seq = state
                        .active
                        .as_ref()
                        .filter(|a| a.target == *target)
                        .map_or(e.seq, |a| a.target_set_seq);
                    Some(Active {
                        target: target.clone(),
                        on,
                        finished,
                        remind_ms: remind_ms.clone(),
                        source: source.clone(),
                        since_seq: state.active.as_ref().map_or(e.seq, |a| a.since_seq),
                        set_seq: e.seq,
                        set_at_ms: u64::try_from(e.ts).unwrap_or(u64::MAX),
                        target_set_seq,
                    })
                }
                _ => None,
            };
        }
        state
    }

    /// The subscription in force for the run at `run_dir` whose journal is
    /// `events`: its own, or for one inherited from the parent run, the
    /// parent's as it is in force now (for a parent that inherited its own,
    /// the one at the top of the chain), narrowed to what the child
    /// inherited.
    pub fn in_force(run_dir: &Path, events: &[Event]) -> Self {
        Self::in_force_within(run_dir, events, apb_core::schema::MAX_SUBPLAYBOOK_DEPTH)
    }

    /// [`NotifyState::in_force`] following at most `hops` parent runs: a
    /// chain longer than any sub-playbook nesting (or one that loops) is in
    /// force nowhere.
    fn in_force_within(run_dir: &Path, events: &[Event], hops: usize) -> Self {
        let mut state = Self::fold(events);
        if let Some(active) = state.active.take() {
            state.active = if active.source == NotifySource::Parent.as_str() {
                hops.checked_sub(1).and_then(|left| {
                    let (dir, parent) = parent_events(run_dir)?;
                    let current = Self::in_force_within(&dir, &parent, left).active?;
                    follow_parent(active, current)
                })
            } else {
                Some(active)
            };
        }
        state
    }
}

/// The directory and journal of the parent run of the run at `run_dir`,
/// `None` when there is none to read.
fn parent_events(run_dir: &Path) -> Option<(std::path::PathBuf, Vec<Event>)> {
    let cfg = crate::run_config::read_run_config(run_dir).ok()?;
    let parent = cfg.parent_run?;
    if !apb_core::registry::is_safe_segment(&parent) {
        return None;
    }
    let dir = run_dir.parent()?.join(parent);
    let events = crate::event::read_all_lossy_tail(&dir).ok()?;
    Some((dir, events))
}

/// The transitions a sub-playbook inherits: only what still waits on the
/// person inside it.
const INHERITED: [Transition; 2] = [Transition::Review, Transition::Question];

/// The transitions of `on` a sub-playbook inherits.
pub(crate) fn inheritable(on: &[Transition]) -> Vec<Transition> {
    on.iter()
        .copied()
        .filter(|t| INHERITED.contains(t))
        .collect()
}

/// A sub-playbook's inherited subscription `child` under its parent's
/// subscription in force now, `current`: the parent's current target and
/// reminder cadence, and only the transitions both still cover. `None`
/// once the parent stopped notifying about anything the child inherited.
fn follow_parent(child: Active, current: Active) -> Option<Active> {
    let on: Vec<Transition> = inheritable(&current.on)
        .into_iter()
        .filter(|t| child.on.contains(t))
        .collect();
    // The parent set up again after the child's trust refusals lifts them.
    let set_at_ms = child.set_at_ms.max(current.set_at_ms);
    (!on.is_empty()).then_some(Active {
        target: current.target,
        on,
        // A child's reminders follow the parent's subscription: its
        // cadence, and its target matched by digest.
        remind_ms: current.remind_ms,
        set_at_ms,
        ..child
    })
}
