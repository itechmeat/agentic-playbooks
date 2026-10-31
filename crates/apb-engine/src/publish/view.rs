//! The publish fold for the run surfaces: per publish node, the pinned
//! target, the latest plan, whether a consent is awaited, the latest push
//! with the facts of the plan it carried (a push of an earlier visit is
//! never shown as the latest plan's), and how the node last finished.

use serde::Serialize;

use crate::connector::intent::{IntentState, ledger};
use crate::event::{Event, EventPayload, PublishCommit};

/// The latest plan of a publish node.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanView {
    pub url: String,
    pub branch: String,
    pub after: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub remote_before: Option<String>,
    pub commits: Vec<PublishCommit>,
    pub omitted: usize,
    /// How many of the commits the push carries this run made, listed or
    /// omitted.
    pub this_run: usize,
    pub outside_push_suspected: bool,
    pub plan_digest: String,
    pub nonce: String,
}

impl std::fmt::Display for PushState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The ledger state of a push.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PushState {
    Pending,
    Confirmed,
    NotSent,
    Unknown,
}

impl PushState {
    /// The state of a ledger verdict.
    pub fn of(s: &IntentState) -> PushState {
        match s {
            IntentState::Pending { .. } => PushState::Pending,
            IntentState::Confirmed { .. } => PushState::Confirmed,
            IntentState::NotSent { .. } => PushState::NotSent,
            IntentState::Unknown { .. } => PushState::Unknown,
        }
    }

    /// The ledger's name of the state.
    pub fn as_str(self) -> &'static str {
        match self {
            PushState::Pending => ledger::PENDING,
            PushState::Confirmed => ledger::CONFIRMED,
            PushState::NotSent => ledger::NOT_SENT,
            PushState::Unknown => ledger::UNKNOWN,
        }
    }
}

/// The push of a publish node in the send ledger.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushView {
    pub intent: String,
    pub state: PushState,
    /// The plan this push carried: what it sent where. It may be an
    /// earlier visit's plan than [`PublishView::plan`].
    pub after: String,
    pub branch: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub remote_before: Option<String>,
    pub plan_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub effect_ref: Option<String>,
}

/// One publish node as the run surfaces show it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublishView {
    pub node: String,
    pub remote: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub url: Option<String>,
    /// The pin's refusal code, when the target could not be pinned.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub pin_refusal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub plan: Option<PlanView>,
    /// A consent to the latest plan is awaited.
    pub awaiting_consent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub push: Option<PushView>,
    /// The output the node last finished with.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub result: Option<String>,
}

impl PublishView {
    /// The push's result is unknown: a person has to look.
    pub fn needs_a_look(&self) -> bool {
        self.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Unknown)
    }
}

/// Every publish node that pinned a target or planned, in journal order,
/// with each push's state at `now_ms`.
pub fn fold(events: &[Event], now_ms: u64) -> Vec<PublishView> {
    let mut nodes: Vec<String> = Vec::new();
    for e in events {
        if let EventPayload::PublishTargetPinned { node, .. }
        | EventPayload::PublishPlanned { node, .. } = &e.payload
            && !nodes.contains(node)
        {
            nodes.push(node.clone());
        }
    }
    nodes.iter().map(|n| view_of(events, n, now_ms)).collect()
}

/// The latest plan of `node` as the surfaces show it.
pub fn plan_of(events: &[Event], node: &str) -> Option<PlanView> {
    super::latest_plan(events, node).and_then(|e| match &e.payload {
        EventPayload::PublishPlanned {
            url,
            branch,
            after,
            remote_before,
            commits,
            omitted,
            this_run,
            outside_push_suspected,
            plan_digest,
            nonce,
            ..
        } => Some(PlanView {
            url: url.clone(),
            branch: branch.clone(),
            after: after.clone(),
            remote_before: remote_before.clone(),
            commits: commits.clone(),
            omitted: *omitted,
            // An older event counted only the listed commits.
            this_run: (*this_run).max(commits.iter().filter(|c| c.this_run).count()),
            outside_push_suspected: *outside_push_suspected,
            plan_digest: plan_digest.clone(),
            nonce: nonce.clone(),
        }),
        _ => None,
    })
}

fn view_of(events: &[Event], node: &str, now_ms: u64) -> PublishView {
    let pin = super::pin_of(events, node);
    let plan = plan_of(events, node);
    let push = super::intent::latest_since(events, node, 0, now_ms).map(|(rec, state)| {
        let carried = super::intent::pushed_plan(events, &rec);
        PushView {
            intent: rec.id.clone(),
            state: PushState::of(&state),
            after: carried
                .as_ref()
                .map(|c| c.after.clone())
                .unwrap_or_default(),
            branch: carried
                .as_ref()
                .map(|c| c.branch.clone())
                .unwrap_or_default(),
            url: carried.as_ref().map(|c| c.url.clone()).unwrap_or_default(),
            remote_before: carried.as_ref().and_then(|c| c.remote_before.clone()),
            plan_digest: carried.map(|c| c.plan_digest).unwrap_or_default(),
            detail: match &state {
                IntentState::Unknown { detail, .. } => detail.clone(),
                _ => None,
            },
            effect_ref: match &state {
                IntentState::Confirmed { effect_ref, .. } => effect_ref.clone(),
                _ => None,
            },
        }
    });
    let result = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::NodeFinished {
            node: n, output, ..
        } if n == node => Some(output.clone()),
        _ => None,
    });
    PublishView {
        node: node.to_string(),
        remote: pin.as_ref().map(|p| p.remote.clone()).unwrap_or_default(),
        url: pin.as_ref().and_then(|p| p.url.clone()),
        pin_refusal: pin.and_then(|p| p.refusal),
        awaiting_consent: crate::event::review_open_count(events, node) > 0,
        plan,
        push,
        result,
    }
}
