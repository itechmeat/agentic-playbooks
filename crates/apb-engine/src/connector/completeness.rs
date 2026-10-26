//! The one rule for a successful connector result that does not hold the
//! whole picture: an HTTP body that did not read whole on a call that may
//! have changed the world, or an inbox read that found unread events lost to
//! retention.
//!
//! Such a result says so in its envelope, at the top level next to `ok`,
//! never inside `body`: the `response_pick` projection rewrites `body` only,
//! so it cannot hide the loss. A complete result adds no key, so its JSON
//! shape is exactly what it was before completeness was tracked.

use apb_core::connector::inbox::InboxGap;
use serde_json::{Value, json};

/// Why a response body read did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncompleteReason {
    /// The body exceeded the read cap and was cut at it.
    BodyCap,
    /// The read failed partway through (the transport closed or errored).
    ReadError,
}

impl IncompleteReason {
    /// The wire string used in the result JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            IncompleteReason::BodyCap => "body_cap",
            IncompleteReason::ReadError => "read_error",
        }
    }
}

/// One incomplete response body read: why it did not finish, the read cap in
/// bytes when the cap was the reason, and the io error kind when the read
/// failed. The raw error text is never kept, because a transport error can
/// carry request details (a `query`-kind auth secret among them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyIncomplete {
    pub reason: IncompleteReason,
    /// The read cap in bytes when `reason` is `BodyCap`.
    pub cap_bytes: Option<usize>,
    /// The io error kind name when `reason` is `ReadError`.
    pub io_error_kind: Option<String>,
}

impl BodyIncomplete {
    /// The body was cut at the read cap.
    pub(crate) fn cut_at_cap(cap_bytes: usize) -> Self {
        BodyIncomplete {
            reason: IncompleteReason::BodyCap,
            cap_bytes: Some(cap_bytes),
            io_error_kind: None,
        }
    }

    /// The read failed partway through; only the error KIND is kept, under
    /// a fixed wire name ([`io_kind_name`]).
    pub(crate) fn read_error(kind: std::io::ErrorKind) -> Self {
        BodyIncomplete {
            reason: IncompleteReason::ReadError,
            cap_bytes: None,
            io_error_kind: Some(io_kind_name(kind).to_string()),
        }
    }

    /// The detail keys (`reason`, `body_cap_bytes`, `io_error_kind`) shared by
    /// the `incomplete_response` error and an incomplete success.
    pub(crate) fn detail(&self, into: &mut serde_json::Map<String, Value>) {
        into.insert("reason".into(), json!(self.reason.as_str()));
        if let Some(cap) = self.cap_bytes {
            into.insert("body_cap_bytes".into(), json!(cap));
        }
        if let Some(kind) = &self.io_error_kind {
            into.insert("io_error_kind".into(), json!(kind));
        }
    }
}

/// The wire name of an io error kind: a fixed snake_case name for the kinds
/// a body read meets, `other` for the rest. Never the standard library's
/// `Debug` text, which it does not promise to keep.
fn io_kind_name(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind as K;
    match kind {
        K::UnexpectedEof => "unexpected_eof",
        K::ConnectionReset => "connection_reset",
        K::ConnectionAborted => "connection_aborted",
        K::BrokenPipe => "broken_pipe",
        K::TimedOut => "timed_out",
        K::InvalidData => "invalid_data",
        K::Interrupted => "interrupted",
        _ => "other",
    }
}

/// What a successful result did not deliver whole.
pub(crate) enum Incompleteness<'a> {
    /// The body of a call that is not read-only did not read whole. The
    /// effect already happened, so the call stays a success and the body
    /// that was read is returned as it is. Nothing in apb retries such a
    /// call because of this (a retry would repeat the effect); the caller
    /// narrows the request before reading again.
    Body(&'a BodyIncomplete),
    /// An inbox read found the inclusive seq range retention dropped before
    /// this consumer read or acknowledged it.
    InboxGap(&'a InboxGap),
}

impl Incompleteness<'_> {
    /// Writes the loss into the result `envelope` at its top level, the one
    /// place a `response_pick` projection never touches.
    pub(crate) fn annotate(&self, envelope: &mut Value) {
        let Some(top) = envelope.as_object_mut() else {
            return;
        };
        match self {
            Incompleteness::Body(b) => {
                top.insert("effect".into(), json!("committed"));
                top.insert("response_complete".into(), json!(false));
                b.detail(top);
            }
            Incompleteness::InboxGap(g) => {
                top.insert(
                    "gap".into(),
                    json!({ "from": g.from, "through": g.through }),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_io_error_kind_is_named_by_a_fixed_wire_name() {
        let named = |kind| BodyIncomplete::read_error(kind).io_error_kind.unwrap();
        assert_eq!(named(std::io::ErrorKind::UnexpectedEof), "unexpected_eof");
        assert_eq!(named(std::io::ErrorKind::TimedOut), "timed_out");
        assert_eq!(named(std::io::ErrorKind::PermissionDenied), "other");
    }
}
