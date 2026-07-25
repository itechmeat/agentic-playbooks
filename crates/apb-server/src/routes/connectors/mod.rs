//! Connector endpoints: listing, install lifecycle, the public view, usage
//! stats, and the three call-path endpoints (healthcheck, call, approve).

pub mod stats;
pub mod view;

pub(crate) use stats::connector_stats_handler;
pub(crate) use view::{InstallState, connector_public};

use crate::state::*;
use std::path::PathBuf;

use apb_core::connector::{config, secrets, store};
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

/// Trust status of a digest against the trust store: `"approved"` when the
/// current digest is approved, `"changed"` when some OTHER digest of the same
/// `id` was approved before (content moved since), else `"unapproved"`.
/// Shared by the connector-level and account-level trust fields below.
pub(crate) fn digest_trust_status(
    trust: &TrustStore,
    digest: &str,
    id: &str,
    kind: Kind,
) -> &'static str {
    if trust.is_approved(digest) {
        "approved"
    } else if trust.approved_record_ids(kind).iter().any(|x| x == id) {
        "changed"
    } else {
        "unapproved"
    }
}

/// The project roots a connector READ should merge account config from.
/// `Some(workspace)` is the strict single-project view and still errors on an
/// unknown, unreachable or malformed workspace; `None` is the machine-wide
/// view: every reachable project, which on a machine with no registered
/// project is legitimately empty (connectors themselves are installed
/// machine-wide, so an empty root set means "no per-project account config",
/// never "no connectors").
#[allow(clippy::result_large_err)]
pub(crate) fn connector_roots(
    state: &AppState,
    workspace: Option<&str>,
) -> Result<Vec<PathBuf>, Response> {
    match workspace {
        Some(ws) => Ok(vec![resolve_root(state, Some(ws))?]),
        None => Ok(enumerate_workspaces(state)
            .into_iter()
            .map(|(_, _, root)| root)
            .collect()),
    }
}

/// One connector's configured accounts across `roots`, paired with the root
/// they were read from (secret resolution is root-scoped). Keyed by account
/// name so the machine-wide view never lists the same account twice:
/// `config::load_merged` folds the shared global account store into every
/// project's, so a global account would otherwise reappear once per project.
/// First reachable project wins for a name configured in several, which keeps
/// the single-root case byte-identical to a plain `load_merged`.
///
/// A project whose account config fails to parse is skipped when aggregating
/// several roots; with exactly one root the error is returned so the strict
/// single-project view still surfaces it.
#[allow(clippy::result_large_err)]
pub(crate) fn merged_accounts(
    roots: &[PathBuf],
    name: &str,
) -> Result<Vec<(PathBuf, config::Account)>, apb_core::connector::ConnectorError> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out: Vec<(PathBuf, config::Account)> = Vec::new();
    for root in roots {
        let accounts = match config::load_merged(root, name) {
            Ok(a) => a,
            Err(e) if roots.len() == 1 => return Err(e),
            Err(_) => continue,
        };
        for a in accounts {
            if seen.insert(a.name.clone()) {
                out.push((root.clone(), a));
            }
        }
    }
    Ok(out)
}

/// GET /api/connectors: installed connectors with their storefront summary,
/// trust status, and account configuration readiness (spec 9). With
/// `?workspace=<id>` the account numbers describe that one project; without
/// it, the machine-wide connectors page gets an aggregate across every
/// reachable project. Connectors are installed machine-wide and their trust is
/// root-independent, so `store::list` is walked once and every connector
/// appears exactly once no matter how many projects are reachable - only the
/// account counts aggregate, and a machine with no reachable project simply
/// reports zero accounts instead of erroring.
///
/// `trust` is the connector's OWN digest trust (`approved` | `changed` |
/// `unapproved` | `invalid`); `accounts_ready` counts configured accounts
/// whose required secret env vars all currently resolve - a configuration
/// signal, not a trust signal (a ready account can still be untrusted, and
/// vice versa). `store::list` only parses `connector.yaml`, so a connector
/// that gets this far already has a manifest that parses; if `store::load`
/// still fails here (for example the whole-tree digest walk hits an escaping
/// symlink), the connector is fundamentally broken, not merely
/// un-trust-decided - report `invalid` rather than `unapproved` so the
/// dashboard can tell the two apart (spec 9's fourth trust state).
pub(crate) async fn list_connectors_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    let roots = match connector_roots(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let trust = TrustStore::load();
    let mut out = Vec::new();
    for summary in store::list() {
        let loaded = store::load(&summary.name);
        let trust_state = match &loaded {
            Ok(l) => digest_trust_status(&trust, &l.digest, &summary.name, Kind::Connector),
            Err(_) => "invalid",
        };
        let accounts = merged_accounts(&roots, &summary.name).unwrap_or_default();
        let accounts_ready = match &loaded {
            Ok(l) => accounts
                .iter()
                .filter(|(root, a)| {
                    let vars: Vec<String> = config::env_refs(&l.doc, a).into_values().collect();
                    secrets::missing_vars(root, &vars).is_empty()
                })
                .count(),
            Err(_) => 0,
        };
        out.push(serde_json::json!({
            "name": summary.name,
            "version": summary.version,
            "display_name": summary.meta.display_name,
            "summary": summary.meta.summary,
            "tags": summary.meta.tags,
            "trust": trust_state,
            "accounts_total": accounts.len(),
            "accounts_ready": accounts_ready,
        }));
    }
    Json(out).into_response()
}

/// GET /api/connectors/available: the embedded official connectors that are
/// NOT currently installed, so the dashboard can offer them for connecting.
/// Each entry carries the same storefront fields the installed listing exposes
/// (`name`, `version`, `display_name`, `summary`, `tags`), read from the
/// embedded `PUBLIC.md` rather than from disk.
///
/// Like `GET /api/connectors`, this is machine-wide: the embedded set comes out
/// of the binary and the store is global, so no project root and therefore no
/// `?workspace=` parameter is involved at all.
pub(crate) async fn available_connectors_handler() -> impl IntoResponse {
    let installed: std::collections::BTreeSet<String> =
        store::list().into_iter().map(|s| s.name).collect();
    let out: Vec<serde_json::Value> = apb_core::connector::official::list()
        .into_iter()
        .filter(|o| !installed.contains(&o.name))
        .map(|o| {
            let meta = o.meta();
            serde_json::json!({
                "name": o.name,
                "version": o.version,
                "display_name": meta.display_name,
                "summary": meta.summary,
                "tags": meta.tags,
            })
        })
        .collect();
    Json(out).into_response()
}

/// Query params of the install endpoint. `force` overwrites a target that
/// already exists and differs from the embedded version; without it that case
/// is a 409 so the dashboard can ask the user before clobbering local edits.
#[derive(Deserialize, Default)]
pub(crate) struct ConnectorInstallQuery {
    #[serde(default)]
    force: Option<bool>,
}

/// Builds the `{ "error": ..., "detail": ... }` body every failing connector
/// lifecycle response carries. A JSON body in every case (never a bare string)
/// is what lets the dashboard render a specific message per failure instead of
/// parsing prose.
pub(crate) fn lifecycle_error(status: StatusCode, code: &str, detail: String) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": code, "detail": detail })),
    )
        .into_response()
}

/// POST /api/connectors/{name}/install: installs the embedded official
/// connector `name` into the global store and records its trust as `Bundled`,
/// through the same `apb_core::connector::install::install_official` the CLI
/// runs. Machine-wide like the store itself, so it needs no `?workspace=`.
///
/// 200 with `no_op: true` when the exact same tree digest is already installed
/// (a reinstall is idempotent, not an error), 400 for a name that is not a
/// valid slug, 404 when no embedded connector carries that name, 409 when a
/// DIFFERING version is already installed and `?force=true` was not passed, and
/// 500 when there is no config directory or a filesystem step fails.
pub(crate) async fn install_connector_handler(
    AxPath(name): AxPath<String>,
    Query(q): Query<ConnectorInstallQuery>,
) -> impl IntoResponse {
    match apb_core::connector::install::install_official(&name, q.force.unwrap_or(false)) {
        Ok(report) => Json(serde_json::json!({
            "ok": true,
            "name": report.name,
            "version": report.version,
            "digest": report.digest,
            "no_op": report.no_op,
            "trust_recorded": report.trust_warning.is_none(),
            "trust_warning": report.trust_warning,
        }))
        .into_response(),
        Err(e) => {
            use apb_core::connector::install::InstallError;
            let (status, code) = match &e {
                InstallError::InvalidName { .. } => (StatusCode::BAD_REQUEST, "invalid_name"),
                InstallError::NotEmbedded(_) => (StatusCode::NOT_FOUND, "not_found"),
                InstallError::NeedsForce { .. } => (StatusCode::CONFLICT, "needs_force"),
                InstallError::NoConfigDir => (StatusCode::INTERNAL_SERVER_ERROR, "no_config_dir"),
                InstallError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "io_error"),
            };
            lifecycle_error(status, code, e.to_string())
        }
    }
}

/// POST /api/connectors/{name}/uninstall: removes `<config_dir>/connectors/
/// {name}/` and nothing else, through `apb_core::connector::install::uninstall`.
/// Account configuration lives in a separate `connector-config/` tree and is
/// deliberately left alone, so disconnecting keeps the user's accounts and
/// reconnecting picks them straight back up; the trust record is left in place
/// for the same reason (a reinstall of the same version digests identically).
///
/// 200 with `no_op: true` when the connector was not installed to begin with
/// (removing what is already gone is what the caller asked for), 400 for a name
/// that is not a valid slug, and 500 when there is no config directory or the
/// directory cannot be removed. There is no 404: an absent connector is a
/// successful no-op, not a missing resource.
pub(crate) async fn uninstall_connector_handler(AxPath(name): AxPath<String>) -> impl IntoResponse {
    match apb_core::connector::install::uninstall(&name) {
        Ok(report) => Json(serde_json::json!({
            "ok": true,
            "name": report.name,
            "no_op": report.no_op,
        }))
        .into_response(),
        Err(e) => {
            use apb_core::connector::install::UninstallError;
            let (status, code) = match &e {
                UninstallError::InvalidName { .. } => (StatusCode::BAD_REQUEST, "invalid_name"),
                UninstallError::NoConfigDir => (StatusCode::INTERNAL_SERVER_ERROR, "no_config_dir"),
                UninstallError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "io_error"),
            };
            lifecycle_error(status, code, e.to_string())
        }
    }
}

/// GET /api/connectors/{name}: the manifest (functions, account fields),
/// storefront body, and the merged account list with non-secret fields,
/// missing env var NAMES (never values), and per-account trust status (spec
/// 9). `missing_env` never carries a value, only the env var name.
///
/// With `?workspace=<id>` the account rows are that one project's; without it
/// (the machine-wide connectors page links to a connector without pinning a
/// project) the connector identity, manifest and trust are read the same way,
/// since all three are root-independent, and the account rows are the union
/// across every reachable project, each account listed once. A machine with no
/// reachable project still gets the connector, with an empty account list.
///
/// A connector that is not installed but IS embedded answers with the same
/// shape and `installed: false`, so the dashboard can show what a connector
/// does before the user connects it. Account rows are still real there:
/// account config lives in a separate `connector-config/` tree that survives
/// (and precedes) installation. `trust` describes bytes on disk that do not
/// exist yet, so it reports its own `not_installed` state rather than
/// borrowing `unapproved`, which would read as a trust decision nobody made.
/// 404 is reserved for a name that is neither installed nor embedded.
pub(crate) async fn get_connector_handler(
    State(state): State<AppState>,
    AxPath(name): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    let roots = match connector_roots(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let Some((public, install)) = connector_public(&name) else {
        return (
            StatusCode::NOT_FOUND,
            format!("connector `{name}` is not installed and is not an official connector"),
        )
            .into_response();
    };
    let accounts_cfg = match merged_accounts(&roots, &name) {
        Ok(a) => a,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let trust = TrustStore::load();
    let secret_fields = public.doc.secret_fields();

    let functions: Vec<serde_json::Value> = public
        .doc
        .functions
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name,
                "description": f.description,
                "read_only": f.read_only,
                "deprecated": f.deprecated,
                "args_schema": f.args_schema,
            })
        })
        .collect();

    let accounts: Vec<serde_json::Value> = accounts_cfg
        .iter()
        .map(|(root, a)| {
            let vars: Vec<String> = config::env_refs(&public.doc, a).into_values().collect();
            let missing_env = secrets::missing_vars(root, &vars);
            // Non-secret fields only: a secret field's config value is the raw
            // `{{env.VAR}}` reference, not the value itself, but the detail
            // endpoint must never surface anything secret-shaped, even by proxy.
            let fields: serde_json::Map<String, serde_json::Value> = a
                .fields
                .iter()
                .filter(|(k, _)| !secret_fields.iter().any(|s| s == *k))
                .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                .collect();
            let digest = config::account_digest(a);
            let id = account_trust_id(&name, &a.name);
            let acct_trust = digest_trust_status(&trust, &digest, &id, Kind::ConnectorAccount);
            serde_json::json!({
                "name": a.name,
                "default": a.default,
                "fields": serde_json::Value::Object(fields),
                "missing_env": missing_env,
                "trust": acct_trust,
            })
        })
        .collect();

    let connector_trust = match &install {
        InstallState::Installed(l) => {
            digest_trust_status(&trust, &l.digest, &name, Kind::Connector)
        }
        InstallState::NotInstalled => "not_installed",
        InstallState::Invalid => "invalid",
    };
    // On disk either way (parseable or broken); only `NotInstalled` is absent.
    let installed = !matches!(install, InstallState::NotInstalled);

    Json(serde_json::json!({
        "name": name,
        "version": public.doc.version,
        "installed": installed,
        "trust": connector_trust,
        "meta": public.meta,
        "body_md": public.body_md,
        "functions": functions,
        "accounts": accounts,
    }))
    .into_response()
}

/// POST /api/connectors/{name}/healthcheck/{account}: runs the connector's
/// declared healthcheck function LIVE (spec 9's dashboard probe button) and
/// returns the call executor's JSON verbatim. A mock healthcheck needs no
/// network; an HTTP healthcheck actually reaches the URL - that live
/// reachability probe is the point of the button.
pub(crate) async fn healthcheck_connector_handler(
    State(state): State<AppState>,
    AxPath((name, account)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let (value, _ok) = apb_engine::connector::call::healthcheck(&root, &name, &account);
    Json(value).into_response()
}

#[derive(Deserialize)]
pub(crate) struct ConnectorCallBody {
    function: String,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    args: serde_json::Value,
    #[serde(default)]
    dry_run: bool,
    /// `--full` bypasses the function's `response_pick` projection (spec 4.5
    /// / 2026-07-19-official-connectors-design section 7 post-review fix);
    /// omitted or `false` (the default) applies the projection like a
    /// normal agent call.
    #[serde(default)]
    full: bool,
}

/// POST /api/connectors/{name}/call: the dashboard playground's manual call
/// (spec 2026-07-19-official-connectors-design section 7). Wraps the same
/// live execution path the healthcheck probe uses
/// (`apb_engine::connector::call::play_call`), extended with an arbitrary
/// function name, args, a dry-run flag, and a `full` flag. Like the
/// healthcheck probe, the server answers HTTP 200 even for a refused or
/// failed call - the outcome is carried in the body's `ok`/`error`, never as
/// an HTTP error status. Account defaulting (an omitted or null `account`)
/// is resolved inside `play_call`, mirroring the CLI's single-or-default
/// selection rule.
pub(crate) async fn call_connector_handler(
    State(state): State<AppState>,
    AxPath(name): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<ConnectorCallBody>,
) -> impl IntoResponse {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    // An absent/null `args` in the request body deserializes to
    // `Value::Null`; the executor's schema validation and template
    // rendering both expect an object, so normalize here rather than push
    // that concern into the engine.
    let args = if body.args.is_null() {
        serde_json::json!({})
    } else {
        body.args
    };
    let (value, _ok) = apb_engine::connector::call::play_call(
        &root,
        &name,
        body.account.as_deref(),
        &body.function,
        &args,
        body.dry_run,
        body.full,
    );
    Json(value).into_response()
}

#[derive(Deserialize)]
pub(crate) struct ConnectorApproveBody {
    name: String,
    #[serde(default)]
    account: Option<String>,
}

/// POST /api/connectors/approve: approves the connector's current tree
/// digest, or (with `account` set) that account's current non-secret-field
/// digest instead - the dashboard's approve flow for the trust gate that
/// guards secret egress (spec 7/9). Mirrors `apb connector approve`.
pub(crate) async fn approve_connector_handler(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<ConnectorApproveBody>,
) -> impl IntoResponse {
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let loaded = match store::load(&body.name) {
        Ok(l) => l,
        Err(e) => return (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    };
    let mut trust = TrustStore::load();
    match body.account.as_deref() {
        None => {
            if let Err(e) = trust.approve_kind(
                &loaded.digest,
                &body.name,
                Kind::Connector,
                OriginKind::LocallyApproved,
            ) {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
        Some(acct_name) => {
            let accounts = match config::load_merged(&root, &body.name) {
                Ok(a) => a,
                Err(e) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                }
            };
            let Some(account) = accounts.iter().find(|a| a.name == acct_name) else {
                return (
                    StatusCode::NOT_FOUND,
                    format!("account `{acct_name}` not configured for `{}`", body.name),
                )
                    .into_response();
            };
            let digest = config::account_digest(account);
            let id = account_trust_id(&body.name, acct_name);
            if let Err(e) = trust.approve_kind(
                &digest,
                &id,
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            ) {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}
