//! Where a call goes, resolved before anything else runs: the connector
//! definition, the function, and the account with its non-secret fields.
//!
//! A node's call (and apb's own lookups and item checks) goes to the run's
//! snapshot: the grant in the manifest and the connector copied at start.
//! A call outside a run (the healthcheck, the playground) and a run
//! notification go to a [`LiveTarget`]: the installed connector and the
//! configured account, which must be approved in the trust store, and for a
//! notification must still match the digests pinned when it was set. Every
//! stage after this one is shared.

use super::request::TargetPins;
use super::*;

/// A resolved call target.
pub(super) struct Target {
    pub doc: ConnectorDoc,
    pub function: FunctionSpec,
    pub account: String,
    pub maccount: ManifestAccount,
    /// The grant a snapshot call runs under; `None` for a live target.
    pub grant: Option<ManifestConnectorGrant>,
}

/// The target of `req` for a caller of `ctx`'s origin.
pub(super) fn resolve(req: &CallRequest, ctx: &CallContext) -> Result<Target, CallError> {
    if !ctx.origin.is_live() {
        return snapshot(req, ctx);
    }
    let pins = ctx.pins.as_ref().ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            "a live call carries no trusted digests",
        )
    })?;
    let live = LiveTarget::load(req.root, req.connector, req.account, req.function)?;
    live.require_pins(pins)?;
    live.into_target()
}

/// Steps 1, 2 and 4 of a run call: the manifest, the grant and its account,
/// and the snapshotted connector definition. The write-once run dir is the
/// integrity boundary: the manifest tree digest covers the whole live
/// connector folder and cannot be recomputed from the one copied file, so
/// drift protection is the snapshot itself.
fn snapshot(req: &CallRequest, ctx: &CallContext) -> Result<Target, CallError> {
    let manifest = manifest::read(req.run_dir)
        .map_err(|e| {
            CallError::new(
                CallErrorCode::Config,
                format!("run manifest unreadable: {e}"),
            )
        })?
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                "this run has no manifest; connectors are unavailable",
            )
        })?;
    let grant = manifest
        .grants_for(req.node_id)
        .iter()
        .find(|g| g.connector == req.connector)
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::Permission,
                format!(
                    "node `{}` has no grant for connector `{}`",
                    req.node_id, req.connector
                ),
            )
        })?;
    if ctx.origin.needs_grant() && !grant.functions.iter().any(|f| f == req.function) {
        return Err(CallError::new(
            CallErrorCode::Permission,
            format!(
                "node `{}` may not call function `{}` on connector `{}`",
                req.node_id, req.function, req.connector
            ),
        ));
    }
    let mconn = manifest.connector(req.connector).ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{}` is not in the run manifest", req.connector),
        )
    })?;
    let account = select_account(req, grant, mconn)?;
    let maccount = mconn
        .accounts
        .iter()
        .find(|a| a.name == account)
        .cloned()
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                format!("granted account `{account}` is not in the connector snapshot"),
            )
        })?;
    let doc = load_snapshot(req.run_dir, req.connector)?;
    let function = doc.function(req.function).cloned().ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            format!(
                "function `{}` is missing from the `{}` snapshot (drift)",
                req.function, req.connector
            ),
        )
    })?;
    Ok(Target {
        doc,
        function,
        account,
        maccount,
        grant: Some(grant.clone()),
    })
}

/// The installed connector and a configured account, outside any run
/// snapshot. The one trust gate every live call passes.
pub(crate) struct LiveTarget {
    name: String,
    doc: ConnectorDoc,
    digest: String,
    function: FunctionSpec,
    account: config::Account,
}

impl LiveTarget {
    /// Loads the installed connector `name`, its function `function` and
    /// the account `account` (the single or default one when `None`) from
    /// the configuration of the workspace at `root`.
    pub(crate) fn load(
        root: &Path,
        name: &str,
        account: Option<&str>,
        function: &str,
    ) -> Result<Self, CallError> {
        let loaded = store::load(name).map_err(|e| {
            CallError::new(CallErrorCode::Config, format!("connector `{name}`: {e}"))
        })?;
        let function = loaded.doc.function(function).cloned().ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                format!("connector `{name}` declares no function `{function}`"),
            )
        })?;
        let accounts = config::load_merged(root, name).map_err(|e| {
            CallError::new(
                CallErrorCode::Config,
                format!("connector `{name}` account config: {e}"),
            )
        })?;
        let account = select_live_account(&accounts, account)
            .ok_or_else(|| account_selection_error(name, account, &accounts))?
            .clone();
        Ok(LiveTarget {
            name: name.to_string(),
            doc: loaded.doc,
            digest: loaded.digest,
            function,
            account,
        })
    }

    pub(crate) fn function(&self) -> &FunctionSpec {
        &self.function
    }

    /// The digests the connector and the account carry now.
    pub(crate) fn pins(&self) -> TargetPins {
        TargetPins {
            connector: self.digest.clone(),
            account: config::account_digest(&self.account),
        }
    }

    /// The trust gate (spec 2026-07-18-connectors-design section 9): a
    /// live call resolves live secrets and sends them to the live config's
    /// base URL, so an unapproved or changed connector or account is never
    /// callable. The connector digest first (a changed folder is a bigger
    /// deal than one account), then the account's own.
    pub(crate) fn require_trust(&self) -> Result<(), CallError> {
        let trust = TrustStore::load();
        if !trust.is_approved(&self.digest) {
            return Err(CallError::new(
                CallErrorCode::Permission,
                format!(
                    "connector `{}` is not approved; approve it before calling (see the connector approve flow)",
                    self.name
                ),
            ));
        }
        if !trust.is_approved(&config::account_digest(&self.account)) {
            return Err(CallError::new(
                CallErrorCode::Permission,
                format!(
                    "account `{}` is not approved; approve it before calling (see the connector approve flow)",
                    account_trust_id(&self.name, &self.account.name)
                ),
            ));
        }
        Ok(())
    }

    /// [`LiveTarget::require_trust`], and the connector and the account
    /// are still the ones `pins` were taken from.
    fn require_pins(&self, pins: &TargetPins) -> Result<(), CallError> {
        self.require_trust()?;
        if &self.pins() != pins {
            return Err(CallError::new(
                CallErrorCode::Permission,
                format!(
                    "connector `{}` or account `{}` changed since the notification was set up; set it up again to trust the new version",
                    self.name, self.account.name
                ),
            ));
        }
        Ok(())
    }

    /// The target, its account checked against the connector's fields and
    /// split into the fields a request may show and the secret references
    /// resolved at send time.
    pub(super) fn into_target(self) -> Result<Target, CallError> {
        let LiveTarget {
            name,
            doc,
            function,
            account,
            ..
        } = self;
        let errors = config::validate_accounts(&doc, std::slice::from_ref(&account));
        if !errors.is_empty() {
            return Err(CallError::new(
                CallErrorCode::Config,
                format!(
                    "connector `{name}` account `{}` is invalid: {}",
                    account.name,
                    errors.join("; ")
                ),
            ));
        }
        let env = config::env_refs(&doc, &account);
        let cmd = config::cmd_refs(&doc, &account);
        let secret_keys: std::collections::HashSet<&str> =
            env.keys().chain(cmd.keys()).map(String::as_str).collect();
        let fields: BTreeMap<String, String> = account
            .fields
            .iter()
            .filter(|(k, _)| !secret_keys.contains(k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let maccount = ManifestAccount {
            name: account.name.clone(),
            default: account.default,
            fields,
            env,
            cmd,
            digest: config::account_digest(&account),
        };
        Ok(Target {
            doc,
            function,
            account: account.name,
            maccount,
            grant: None,
        })
    }
}
