//! What a run's connector calls (`connector_call`) say about its outcome: a
//! write that went through is done, a read that came back whole is fine, and
//! a call whose effect nobody can tell (a write that timed out, a response
//! cut short) is not verified.

use std::collections::BTreeMap;
use std::path::Path;

use crate::connector::result::CallErrorCode;
use crate::event::{Event, EventPayload};

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

/// One classified call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedCall {
    pub node: String,
    pub connector: String,
    pub function: String,
    pub outcome: String,
    pub class: CallClass,
}

impl ClassifiedCall {
    /// `twenty.update_opportunity`.
    pub fn target(&self) -> String {
        format!("{}.{}", self.connector, self.function)
    }
}

/// Every `connector_call` of `events`, classified against the run's
/// connector snapshots in `run_dir`. A function the snapshot does not
/// describe counts as a write: whether it changed anything is not known.
pub fn classified_calls(run_dir: &Path, events: &[Event]) -> Vec<ClassifiedCall> {
    let mut read_only: BTreeMap<(String, String), bool> = BTreeMap::new();
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ConnectorCall {
                node_id,
                connector,
                function,
                outcome,
                http_status,
                response_complete,
                ..
            } => {
                let ro = *read_only
                    .entry((connector.clone(), function.clone()))
                    .or_insert_with(|| is_read_only(run_dir, connector, function));
                let facts = CallFacts {
                    outcome,
                    http_status: *http_status,
                    response_complete: *response_complete,
                };
                Some(ClassifiedCall {
                    node: node_id.clone(),
                    connector: connector.clone(),
                    function: function.clone(),
                    outcome: outcome.clone(),
                    class: classify(facts, ro),
                })
            }
            _ => None,
        })
        .collect()
}

fn is_read_only(run_dir: &Path, connector: &str, function: &str) -> bool {
    crate::connector::call::load_snapshot(run_dir, connector)
        .ok()
        .and_then(|doc| doc.function(function).map(|f| f.read_only))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(outcome: &str, http_status: Option<u16>, complete: Option<bool>) -> CallFacts<'_> {
        CallFacts {
            outcome,
            http_status,
            response_complete: complete,
        }
    }

    #[test]
    fn a_write_with_an_undetermined_effect_is_unknown_and_a_refused_one_failed() {
        let table = [
            (facts("ok", Some(200), Some(true)), CallClass::WriteOk),
            (
                facts("ok", Some(200), Some(false)),
                CallClass::WriteIncomplete,
            ),
            (facts("timeout", None, None), CallClass::WriteUnknown),
            (facts("network", None, None), CallClass::WriteUnknown),
            (facts("service", Some(503), None), CallClass::WriteUnknown),
            (facts("service", Some(400), None), CallClass::Failed),
            (facts("auth", Some(401), None), CallClass::Failed),
            // A cut response: accepted, refused, or nobody can tell.
            (
                facts("incomplete_response", Some(201), Some(false)),
                CallClass::WriteIncomplete,
            ),
            (facts("service", Some(422), Some(false)), CallClass::Failed),
            (
                facts("incomplete_response", None, Some(false)),
                CallClass::WriteUnknown,
            ),
        ];
        for (call, class) in table {
            assert_eq!(classify(call, false), class, "{}", call.outcome);
        }
    }

    #[test]
    fn a_read_cut_short_is_unknown() {
        assert_eq!(
            classify(facts("ok", Some(200), None), true),
            CallClass::ReadOk
        );
        assert_eq!(
            classify(facts("incomplete_response", Some(200), Some(false)), true),
            CallClass::ReadUnknown
        );
        assert_eq!(
            classify(facts("timeout", None, None), true),
            CallClass::Failed
        );
    }
}
