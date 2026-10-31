//! What a connector call is asked to do and by whom: the request itself,
//! the caller's context (send label, attempt claim, origin), and the check
//! of its arguments against the function's schema. A leaf the call path's
//! stages share.

use std::path::Path;

use serde_json::Value;

use super::attempt::AttemptClaim;
use crate::connector::result::{CallError, CallErrorCode};

/// One connector call request, assembled by the CLI from its arguments and the
/// run context env variables.
pub struct CallRequest<'a> {
    /// `runs/<id>` - the manifest, connector snapshots, and event log live here.
    pub run_dir: &'a Path,
    /// Project root, for secret resolution (`.apb/secrets.env`).
    pub root: &'a Path,
    pub node_id: &'a str,
    pub connector: &'a str,
    pub function: &'a str,
    /// Explicit `--account`; `None` selects the single or default granted account.
    pub account: Option<&'a str>,
    /// The call arguments (validated against `args_schema` when present).
    pub args: Value,
    /// `--dry-run`: render and print without executing, resolving no secrets.
    pub dry_run: bool,
    /// `--full`: return the complete response body, skipping the function's
    /// `response_pick` projection (spec 4.5 debugging escape).
    pub full: bool,
}

/// Who makes a call, beyond the request itself: the caller's `--intent`
/// label for a send, the attempt it claims to belong to, and whether it is
/// a node's own call or one apb makes itself (a lookup of an earlier send,
/// an item check, a run notification). The default is a node's call with no
/// label and no claim. Only the engine makes its own calls (`origin` is not
/// public).
#[derive(Debug, Clone, Default)]
pub struct CallContext<'a> {
    pub intent: Option<&'a str>,
    pub claim: AttemptClaim,
    pub(super) origin: CallOrigin,
    /// For a notification: the digests its target was trusted with when the
    /// subscription was set. The live connector and account must still
    /// match them.
    pub(super) pins: Option<TargetPins>,
}

/// The digests a live call target was approved with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetPins {
    pub connector: String,
    pub account: String,
}

impl<'a> CallContext<'a> {
    /// A node's own call, with its `--intent` label and attempt claim.
    pub fn node(intent: Option<&'a str>, claim: AttemptClaim) -> Self {
        CallContext {
            intent,
            claim,
            origin: CallOrigin::Node,
            pins: None,
        }
    }

    /// apb's check of list item `item`, through the `<node>#check` grant.
    pub(crate) fn item_check(item: &str) -> Self {
        CallContext {
            intent: None,
            claim: AttemptClaim {
                item: Some(item.to_string()),
                ..AttemptClaim::default()
            },
            origin: CallOrigin::Check,
            pins: None,
        }
    }

    /// apb's notification of a person, under the engine-made `label`, to a
    /// live target that must still match `pins`.
    pub(crate) fn notify(label: &'a str, pins: TargetPins) -> Self {
        CallContext {
            intent: Some(label),
            claim: AttemptClaim::default(),
            origin: CallOrigin::Notify,
            pins: Some(pins),
        }
    }
}

/// Who makes a call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum CallOrigin {
    /// A node (an agent, a script, the supervisor).
    #[default]
    Node,
    /// apb itself, reading back an earlier send. It may read any read-only
    /// function of the granted connector and is never charged to
    /// `max_calls`.
    Reconcile,
    /// apb itself, checking a list item through the `<node>#check` grant:
    /// read-only functions only, never charged to `max_calls`.
    Check,
    /// apb itself, telling a person about a run transition through their
    /// own subscription: a live, trust-checked target instead of a grant,
    /// sent once per transition through the ledger.
    Notify,
    /// apb itself, reading back an earlier notification: the same live,
    /// pinned target as [`CallOrigin::Notify`], read-only functions only.
    NotifyReconcile,
}

impl CallOrigin {
    /// The journal's `origin` field: absent for a node's call.
    pub(super) fn journal_tag(self) -> Option<String> {
        match self {
            CallOrigin::Node => None,
            CallOrigin::Reconcile | CallOrigin::NotifyReconcile => Some("reconcile".to_string()),
            CallOrigin::Check => Some("check".to_string()),
            CallOrigin::Notify => Some("notify".to_string()),
        }
    }

    /// Whether a send through a function that declares `idempotency` goes
    /// through the send-once ledger.
    pub(super) fn tracks_sends(self) -> bool {
        matches!(self, CallOrigin::Node | CallOrigin::Notify)
    }

    /// Whether the call counts against the grant's `max_calls` and must
    /// come from the node's open attempt.
    pub(super) fn charges_budget(self) -> bool {
        self == CallOrigin::Node
    }

    /// Whether the grant must list the function. A lookup of an earlier
    /// send reads functions the node may not call itself; a notification
    /// has no grant at all.
    pub(super) fn needs_grant(self) -> bool {
        matches!(self, CallOrigin::Node | CallOrigin::Check)
    }

    /// Whether only read-only functions may be called.
    pub(super) fn read_only_only(self) -> bool {
        matches!(
            self,
            CallOrigin::Reconcile | CallOrigin::Check | CallOrigin::NotifyReconcile
        )
    }

    /// Whether the call goes to a live, trust-checked target rather than
    /// the run's snapshotted grant.
    pub(super) fn is_live(self) -> bool {
        matches!(self, CallOrigin::Notify | CallOrigin::NotifyReconcile)
    }

    /// The origin of apb's read-back of an earlier send made by this one:
    /// it reads where the send went.
    pub(super) fn read_back(self) -> CallOrigin {
        match self {
            CallOrigin::Notify | CallOrigin::NotifyReconcile => CallOrigin::NotifyReconcile,
            _ => CallOrigin::Reconcile,
        }
    }

    /// Whether the send label is the engine's own (`notify:<transition>`)
    /// rather than a caller's `--intent`, which must follow the slug rule.
    pub(super) fn labels_itself(self) -> bool {
        self == CallOrigin::Notify
    }
}

/// Validates `args` against the function's `args_schema`, failing as
/// `InvalidArgs` naming the first offending instance path.
pub(super) fn validate_args(schema: &Value, args: &Value) -> Result<(), CallError> {
    let validator = jsonschema::validator_for(schema).map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("function args_schema is not a valid JSON schema: {e}"),
        )
    })?;
    if let Some(err) = validator.iter_errors(args).next() {
        let path = err.instance_path().to_string();
        let where_ = if path.is_empty() {
            "(root)".to_string()
        } else {
            path
        };
        return Err(CallError::new(
            CallErrorCode::InvalidArgs,
            format!("args failed schema validation at `{where_}`: {err}"),
        ));
    }
    Ok(())
}
