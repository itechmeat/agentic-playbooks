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
/// a node's own call or apb's lookup of an earlier send. The default is a
/// node's call with no label and no claim. Only the call path itself makes
/// a lookup (`origin` is not public): a lookup skips the grant's function
/// list and its budget.
#[derive(Debug, Clone, Default)]
pub struct CallContext<'a> {
    pub intent: Option<&'a str>,
    pub claim: AttemptClaim,
    pub(super) origin: CallOrigin,
}

impl<'a> CallContext<'a> {
    /// A node's own call, with its `--intent` label and attempt claim.
    pub fn node(intent: Option<&'a str>, claim: AttemptClaim) -> Self {
        CallContext {
            intent,
            claim,
            origin: CallOrigin::Node,
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
}

impl CallOrigin {
    /// The journal's `origin` field: absent for a node's call.
    pub(super) fn journal_tag(self) -> Option<String> {
        match self {
            CallOrigin::Node => None,
            CallOrigin::Reconcile => Some("reconcile".to_string()),
            CallOrigin::Check => Some("check".to_string()),
        }
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
