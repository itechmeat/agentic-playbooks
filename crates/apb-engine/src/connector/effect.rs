//! What one connector call says about its effect on the world: a read that
//! came back whole or cut short, a write that went through, a write that
//! may or may not have happened, or a call that failed before it could act.
//! Shared by the run outcome (which reports it) and the send ledger (which
//! settles a send intent by it).

use crate::connector::result::CallErrorCode;

/// How one connector call reads in the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallClass {
    /// A read that came back whole.
    ReadOk,
    /// A read whose response was cut short: what it saw is not whole.
    ReadUnknown,
    /// A write that went through.
    WriteOk,
    /// A write that went through but whose response was cut short.
    WriteIncomplete,
    /// A write that may or may not have happened: it timed out, the
    /// network failed, or the service answered with a server error.
    WriteUnknown,
    /// A call that was refused or failed before it could act.
    Failed,
}

/// The fields of a `connector_call` the class depends on.
#[derive(Debug, Clone, Copy)]
pub struct CallFacts<'a> {
    pub outcome: &'a str,
    pub http_status: Option<u16>,
    pub response_complete: Option<bool>,
}

/// Classifies one call of a function that is `read_only` or not.
pub fn classify(call: CallFacts<'_>, read_only: bool) -> CallClass {
    let cut = call.response_complete == Some(false)
        || call.outcome == CallErrorCode::IncompleteResponse.as_str();
    let ok = call.outcome == "ok";
    if read_only {
        return match (ok, cut) {
            (_, true) => CallClass::ReadUnknown,
            (true, false) => CallClass::ReadOk,
            (false, false) => CallClass::Failed,
        };
    }
    let server_error = call.outcome == CallErrorCode::Service.as_str()
        && call.http_status.is_some_and(|s| s >= 500);
    let undetermined = call.outcome == CallErrorCode::Timeout.as_str()
        || call.outcome == CallErrorCode::Network.as_str()
        || server_error;
    let accepted = call.http_status.is_some_and(|s| (200..300).contains(&s));
    let refused = call.http_status.is_some_and(|s| (400..500).contains(&s));
    match (ok, cut, undetermined) {
        (true, true, _) => CallClass::WriteIncomplete,
        (true, false, _) => CallClass::WriteOk,
        (false, _, true) => CallClass::WriteUnknown,
        // A cut response says the write went through only when the service
        // accepted it; a refusal is a failure, anything else is not known.
        (false, true, false) if accepted => CallClass::WriteIncomplete,
        (false, true, false) if refused => CallClass::Failed,
        (false, true, false) => CallClass::WriteUnknown,
        (false, false, false) => CallClass::Failed,
    }
}
