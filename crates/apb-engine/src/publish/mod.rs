//! The publish step: a push leaves a run only here, after a person consented
//! to the exact plan, and a push whose result is unknown is never repeated
//! on its own.
//!
//! - [`pin`] fixes each publish node's remote before the first acting node
//!   runs;
//! - [`plan`] reads, without writing anything, what a push would do and
//!   refuses with a code when the tree or the remote drifted;
//! - [`intent`] records the push in the send ledger before it leaves,
//!   settles it, and reconciles an unknown result by reading the remote ref
//!   back;
//! - [`push`] runs the one networked git call through
//!   [`apb_core::git::transport`];
//! - [`view`] folds the journal into what the run surfaces show.
//!
//! The drive's arm is `scheduler::publish`; the consent travels through the
//! review channel (`crate::review`) with the plan's nonce.

pub mod intent;
pub mod pin;
pub mod plan;
pub mod push;
pub mod view;

use apb_core::publish::Refusal;

use crate::event::{Event, EventPayload};

/// A publish node's pinned target, as journaled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub seq: u64,
    pub remote: String,
    pub url: Option<String>,
    pub common_dir: String,
    /// The journaled refusal code, kept as written: a code another apb
    /// version wrote is shown as it is, never as a different reason.
    pub refusal: Option<String>,
}

/// The pin of `node`: its first `publish_target_pinned` (a pin is never
/// replaced).
pub fn pin_of(events: &[Event], node: &str) -> Option<Pin> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::PublishTargetPinned {
            node: n,
            remote,
            url,
            common_dir,
            refusal,
            ..
        } if n == node => Some(Pin {
            seq: e.seq,
            remote: remote.clone(),
            url: url.clone(),
            common_dir: common_dir.clone(),
            refusal: refusal.clone(),
        }),
        _ => None,
    })
}

/// The latest `publish_planned` of `node`, with its seq.
pub fn latest_plan<'a>(events: &'a [Event], node: &str) -> Option<&'a Event> {
    events
        .iter()
        .rev()
        .find(|e| matches!(&e.payload, EventPayload::PublishPlanned { node: n, .. } if n == node))
}

/// The refusal text a node finishes with.
pub fn refused_output(r: Refusal, detail: Option<&str>) -> String {
    let mut out = format!(
        "{}: {}: {}",
        apb_core::publish::output::REFUSED,
        r.code(),
        r.sentence()
    );
    if let Some(d) = detail.filter(|d| !d.is_empty()) {
        out.push(' ');
        out.push_str(d);
    }
    out
}
