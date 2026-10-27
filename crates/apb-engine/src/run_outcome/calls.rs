//! What a run's connector calls (`connector_call`) say about its outcome: a
//! write that went through is done, a read that came back whole is fine, and
//! a call whose effect nobody can tell (a write that timed out, a response
//! cut short) is not verified.

use std::collections::BTreeMap;
use std::path::Path;

use crate::event::{Event, EventPayload};

pub use crate::connector::effect::{CallClass, CallFacts, classify};

/// One classified call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedCall {
    /// The seq of its `connector_call`.
    pub seq: u64,
    pub node: String,
    pub connector: String,
    pub function: String,
    pub outcome: String,
    pub class: CallClass,
    /// The seq of the `attempt_started` the call ran in, when journaled.
    pub attempt_seq: Option<u64>,
    /// The send intent the call carried out, for a function that declares
    /// `idempotency`: the send ledger, not the call, tells its outcome.
    pub intent: Option<String>,
}

impl ClassifiedCall {
    /// `twenty.update_opportunity`.
    pub fn target(&self) -> String {
        format!("{}.{}", self.connector, self.function)
    }
}

/// Every `connector_call` a node made in `events`, classified against the
/// run's connector snapshots in `run_dir`. A function the snapshot does not
/// describe counts as a write: whether it changed anything is not known.
/// apb's own lookups of an earlier send (`origin: reconcile`) are left out:
/// the send ledger reports what they found.
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
                attempt_seq,
                intent,
                origin: None,
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
                    seq: e.seq,
                    node: node_id.clone(),
                    connector: connector.clone(),
                    function: function.clone(),
                    outcome: outcome.clone(),
                    class: classify(facts, ro),
                    attempt_seq: *attempt_seq,
                    intent: intent.clone(),
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
