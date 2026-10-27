//! Which attempt of its node a connector call belongs to, and what that
//! attempt has already spent of its `max_calls` budget.
//!
//! A call binds to the node's latest `attempt_started`. A script node runs
//! no executor and journals no attempt, so its calls bind to the visit
//! (`node_started`). The binding is open while that attempt has no
//! `attempt_finished` and the caller's claim matches it: an agent process
//! left behind by an attempt that already ended may not act in the name of
//! the next one. Attempt numbers restart with every execution of a node, so
//! the claim names the attempt by its token, which `attempt_started`
//! journals.

use std::path::Path;

use crate::event::{Event, EventPayload};

/// The attempt a caller says it belongs to, from the environment apb gives
/// every attempt: `APB_ATTEMPT` (the attempt number), `APB_ATTEMPT_TOKEN`
/// (the attempt's token) and, for a host task, `APB_HOST_TASK` (the task
/// id). All absent (a person at a terminal, a test) claims nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttemptClaim {
    pub attempt: Option<u32>,
    pub token: Option<String>,
    pub host_task: Option<String>,
}

impl AttemptClaim {
    /// The claim of the current process environment.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        AttemptClaim {
            attempt: var("APB_ATTEMPT").and_then(|v| v.trim().parse().ok()),
            token: var(crate::adapter::ATTEMPT_TOKEN_ENV),
            host_task: var("APB_HOST_TASK"),
        }
    }

    fn is_empty(&self) -> bool {
        self.attempt.is_none() && self.token.is_none() && self.host_task.is_none()
    }
}

/// Which anchor the budget scan floored at, and therefore what window the
/// budget is counted over. A script node runs no executor and emits no
/// `AttemptStarted`, so its unit is the node visit rather than the attempt;
/// `Ungrounded` is the pre-anchor case where every call for the grant counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetUnit {
    Attempt,
    Visit,
    Ungrounded,
}

impl BudgetUnit {
    /// Names the window in the enforcement message, as a trailing clause.
    /// Empty for `Ungrounded`: there is no window to name.
    pub(crate) fn current_phrase(self) -> &'static str {
        match self {
            Self::Attempt => " in this attempt",
            Self::Visit => " in this visit to the node",
            Self::Ungrounded => "",
        }
    }

    /// Names what the floor excluded. Plural: several earlier attempts or
    /// visits aggregate into one `earlier` count. Unreachable for `Ungrounded`
    /// (a zero floor leaves nothing before it), which is why it reads
    /// generically.
    pub(crate) fn earlier_phrase(self) -> &'static str {
        match self {
            Self::Attempt => "earlier attempts",
            Self::Visit => "earlier visits",
            Self::Ungrounded => "earlier work",
        }
    }
}

/// A call's attempt binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttemptBinding {
    /// The attempt number, `None` without an attempt (a script node's
    /// visit, or no anchor at all).
    pub attempt: Option<u32>,
    /// The seq of the attempt's `attempt_started`: attempt numbers restart
    /// with every execution of a node, the seq does not.
    pub attempt_seq: Option<u64>,
    /// Index of the first event that belongs to the attempt (or visit).
    pub floor: usize,
    pub unit: BudgetUnit,
    /// Whether the attempt may still act: it has not ended, and the caller
    /// claims nothing or claims this attempt.
    pub open: bool,
    /// Whether the call may send through a function that declares
    /// `idempotency`: the binding is open and, when the node has an
    /// attempt, the caller proved it belongs to it. A send is recorded in
    /// an attempt's name, so a caller that claims nothing may not send.
    pub may_send: bool,
}

/// The run's journal as a writer outside the drive sees it (a torn last
/// line, the drive mid-append, is forgiven). Empty when it cannot be read:
/// every decision built on it then fails safe (a budget counts from zero,
/// which the next call corrects; a send records its intent under the lock
/// with a fresh read).
pub(crate) fn journal(run_dir: &Path) -> Vec<Event> {
    crate::event::read_all_lossy_tail(run_dir).unwrap_or_default()
}

/// Binds a call of `node` to its attempt.
///
/// The anchor is durable before the call can read it: the drive journals
/// `attempt_started` from `on_spawn`, right after the agent process exists
/// and before any of its work runs (`scheduler/node.rs`). Were it ever
/// missing, the floor would fall back to the visit or to zero, which counts
/// strictly more calls: the stricter direction.
pub(crate) fn bind(events: &[Event], node: &str, claim: &AttemptClaim) -> AttemptBinding {
    let started = events.iter().rposition(
        |e| matches!(&e.payload, EventPayload::AttemptStarted { node: n, .. } if n == node),
    );
    if let Some(i) = started {
        let EventPayload::AttemptStarted { attempt, token, .. } = &events[i].payload else {
            unreachable!("rposition matched an attempt_started");
        };
        let later = &events[i + 1..];
        let finished = later.iter().any(
            |e| matches!(&e.payload, EventPayload::AttemptFinished { node: n, .. } if n == node),
        );
        let started = Started {
            attempt: *attempt,
            token: token.as_deref(),
            node,
            within: later,
        };
        let claimed = !claim.is_empty() && claim_matches(claim, &started);
        return AttemptBinding {
            attempt: Some(*attempt),
            attempt_seq: Some(events[i].seq),
            floor: i + 1,
            unit: BudgetUnit::Attempt,
            open: !finished && (claim.is_empty() || claimed),
            may_send: !finished && claimed,
        };
    }
    let visit = events.iter().rposition(
        |e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node),
    );
    let (floor, unit) = match visit {
        Some(i) => (i + 1, BudgetUnit::Visit),
        None => (0, BudgetUnit::Ungrounded),
    };
    AttemptBinding {
        attempt: None,
        attempt_seq: None,
        floor,
        unit,
        open: true,
        may_send: true,
    }
}

/// The attempt a claim is checked against.
struct Started<'a> {
    attempt: u32,
    /// Its token; `None` for an attempt an older apb journaled.
    token: Option<&'a str>,
    node: &'a str,
    /// The events after its `attempt_started`.
    within: &'a [Event],
}

/// A claim matches the attempt when it names a host task the drive
/// (re)requested for this node within the attempt (a task a resumed drive
/// adopted keeps the environment of the attempt that first opened it), or
/// carries the attempt's token. Only an attempt journaled without a token
/// is matched by its number.
fn claim_matches(claim: &AttemptClaim, started: &Started<'_>) -> bool {
    let adopted_task = claim.host_task.as_deref().is_some_and(|task| {
        started.within.iter().any(|e| {
            matches!(
                &e.payload,
                EventPayload::HostTaskRequested { task_id, node: n, .. }
                    if task_id == task && n == started.node
            )
        })
    });
    adopted_task
        || match started.token {
            Some(token) => claim.token.as_deref() == Some(token),
            None => claim.attempt == Some(started.attempt),
        }
}

/// Prior `ConnectorCall` events for one `(node, connector)` grant, split at
/// the binding's floor: `current` is what the budget is judged against,
/// `earlier` is what previous attempts (or visits) of the same node spent and
/// is reported but never charged. `unit` says which of the two the floor
/// found, so the enforcement message never invents an attempt a script node
/// never had.
pub(crate) struct PriorCalls {
    pub current: u64,
    pub earlier: u64,
    pub unit: BudgetUnit,
}

/// Counts prior calls for the grant, of any outcome. Filtering by connector
/// too keeps each grant's budget independent when one node is granted
/// several connectors. apb's own lookups of an earlier send
/// (`origin: reconcile`) are never charged.
pub(crate) fn prior_calls(
    events: &[Event],
    node: &str,
    connector: &str,
    binding: &AttemptBinding,
) -> PriorCalls {
    let is_call = |e: &Event| {
        matches!(
            &e.payload,
            EventPayload::ConnectorCall { node_id: n, connector: c, origin, .. }
                if n == node && c == connector && origin.is_none()
        )
    };
    let floor = binding.floor.min(events.len());
    PriorCalls {
        current: events[floor..].iter().filter(|e| is_call(e)).count() as u64,
        earlier: events[..floor].iter().filter(|e| is_call(e)).count() as u64,
        unit: binding.unit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: 0,
            payload,
        }
    }

    fn started(seq: u64, attempt: u32) -> Event {
        ev(
            seq,
            EventPayload::AttemptStarted {
                node: "n".into(),
                attempt,
                agent: "a".into(),
                soul_delivery: None,
                skills_mode: None,
                pid: None,
                spawn_ms: None,
                model: None,
                workdir: None,
                transcript: None,
                token: Some(format!("tok-{seq}")),
            },
        )
    }

    fn finished(seq: u64, attempt: u32) -> Event {
        ev(
            seq,
            EventPayload::AttemptFinished {
                node: "n".into(),
                attempt,
                status: "failed".into(),
                duration_ms: None,
                session: None,
                summary: None,
                rejected_output: None,
                partial_output: None,
                failure_kind: None,
                usage: None,
                output_truncated: Vec::new(),
                transcript_incomplete: false,
            },
        )
    }

    fn claim(attempt: Option<u32>, token: Option<&str>, task: Option<&str>) -> AttemptClaim {
        AttemptClaim {
            attempt,
            token: token.map(str::to_string),
            host_task: task.map(str::to_string),
        }
    }

    #[test]
    fn a_call_binds_to_the_latest_attempt_and_its_seq() {
        let events = vec![started(3, 1), started(9, 1)];
        let b = bind(&events, "n", &claim(Some(1), Some("tok-9"), None));
        assert_eq!((b.attempt, b.attempt_seq, b.floor), (Some(1), Some(9), 2));
        assert!(b.open && b.may_send);
        assert_eq!(b.unit, BudgetUnit::Attempt);
    }

    /// Attempt numbers restart with every execution of a node: an agent
    /// left behind by an earlier execution carries the same number but
    /// another token, and may neither act nor send.
    #[test]
    fn a_stale_token_from_an_earlier_execution_closes_the_binding() {
        let events = vec![started(3, 1), finished(4, 1), started(9, 1)];
        let stale = bind(&events, "n", &claim(Some(1), Some("tok-3"), None));
        assert!(!stale.open && !stale.may_send);
    }

    /// A caller that claims nothing may read and write untracked, but may
    /// not send in an attempt's name; a script node's visit has no attempt
    /// to claim.
    #[test]
    fn an_empty_claim_may_not_send_where_the_node_has_an_attempt() {
        let b = bind(&[started(3, 1)], "n", &AttemptClaim::default());
        assert!(b.open && !b.may_send);
        let visit = bind(&[], "n", &AttemptClaim::default());
        assert!(visit.open && visit.may_send);
    }

    /// An attempt an older apb journaled carries no token: its number still
    /// binds.
    #[test]
    fn an_attempt_without_a_token_is_matched_by_its_number() {
        let mut old = started(3, 2);
        if let EventPayload::AttemptStarted { token, .. } = &mut old.payload {
            *token = None;
        }
        let events = vec![old];
        assert!(bind(&events, "n", &claim(Some(2), None, None)).may_send);
        assert!(!bind(&events, "n", &claim(Some(1), None, None)).open);
    }

    #[test]
    fn a_task_the_attempt_requested_matches_whatever_token_it_carries() {
        let request = |seq, task: &str| {
            ev(
                seq,
                EventPayload::HostTaskRequested {
                    task_id: task.into(),
                    node: "n".into(),
                    attempt: 1,
                    prompt_ref: String::new(),
                    role_prompt_ref: None,
                    skills: Vec::new(),
                    workdir: String::new(),
                    outputs: None,
                    deadline_ms: None,
                    model_hint: None,
                    hint_source: None,
                    fallback_of: None,
                    hint_note: None,
                },
            )
        };
        let events = vec![request(2, "t-1"), started(5, 1), request(6, "t-1")];
        assert!(bind(&events, "n", &claim(Some(4), Some("tok-1"), Some("t-1"))).may_send);
        let before_only = vec![request(2, "t-0"), started(5, 1)];
        assert!(
            !bind(
                &before_only,
                "n",
                &claim(Some(4), Some("tok-1"), Some("t-0"))
            )
            .open
        );
    }
}
