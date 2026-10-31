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
//! - [`view`] is what the run surfaces report.
//! - [`trial`] sends one test message outside any run (`apb notify test`).
//!
//! A notification is not part of the run gate's permit: its target is the
//! person's own subscription, trusted when it was set and checked against
//! the pinned digests at every send. A failure never fails the run.

pub mod notice;
pub mod record;
pub(crate) mod send;
pub mod start;
pub mod transitions;
pub mod trial;
pub mod view;

use std::path::Path;

use apb_core::notify::{NotifySource, Transition};

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
}

impl Active {
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
                ..
            } = &e.payload
            else {
                continue;
            };
            state.recorded = true;
            if let Some(p) = problem {
                state.problems.push(p.clone());
            }
            let on = Transition::normalized(
                &on.iter()
                    .filter_map(|t| Transition::parse(t))
                    .collect::<Vec<_>>(),
            );
            state.active = match target {
                Some(target) if !on.is_empty() => {
                    state.last_on = on.clone();
                    Some(Active {
                        target: target.clone(),
                        on,
                        source: source.clone(),
                        since_seq: state.active.as_ref().map_or(e.seq, |a| a.since_seq),
                        set_seq: e.seq,
                        set_at_ms: u64::try_from(e.ts).unwrap_or(u64::MAX),
                    })
                }
                _ => None,
            };
        }
        state
    }

    /// The subscription in force for the run at `run_dir` whose journal is
    /// `events`: its own, or for one inherited from the parent run, the
    /// parent's as it reads now, narrowed to what the child inherited.
    pub fn in_force(run_dir: &Path, events: &[Event]) -> Self {
        let mut state = Self::fold(events);
        if let Some(active) = state.active.take() {
            state.active = if active.source == NotifySource::Parent.as_str() {
                parent_events(run_dir).and_then(|parent| follow_parent(active, &parent))
            } else {
                Some(active)
            };
        }
        state
    }
}

/// The journal of the parent run of the run at `run_dir`, `None` when there
/// is none to read.
fn parent_events(run_dir: &Path) -> Option<Vec<Event>> {
    let cfg = crate::run_config::read_run_config(run_dir).ok()?;
    let parent = cfg.parent_run?;
    if !apb_core::registry::is_safe_segment(&parent) {
        return None;
    }
    let dir = run_dir.parent()?.join(parent);
    crate::event::read_all_lossy_tail(&dir).ok()
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

/// A sub-playbook's inherited subscription `child` as its parent's journal
/// `parent` reads now: the parent's current target, and only the
/// transitions both still cover. `None` once the parent stopped notifying
/// about anything the child inherited.
fn follow_parent(child: Active, parent: &[Event]) -> Option<Active> {
    let current = NotifyState::fold(parent).active?;
    let on: Vec<Transition> = inheritable(&current.on)
        .into_iter()
        .filter(|t| child.on.contains(t))
        .collect();
    // The parent set up again after the child's trust refusals lifts them.
    let set_at_ms = child.set_at_ms.max(current.set_at_ms);
    (!on.is_empty()).then_some(Active {
        target: current.target,
        on,
        set_at_ms,
        ..child
    })
}
