use crate::state::*;
use apb_core::launch::floor::SaveChannel;

use apb_core::registry::{PlaybookSummary, Registry, RegistryError};
use apb_core::validate::{Severity, ValidationContext, validate};
use apb_core::versioning::{
    delete_playbook, list_versions_with_provenance, save_definition_by, save_layout, version_diff,
};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

/// Serializes a playbook summary and tags it with its owning project, so the
/// global list can show playbook-to-project affiliation.
pub(crate) fn tag_summary(
    summary: &PlaybookSummary,
    workspace_id: &str,
    project: &str,
) -> serde_json::Value {
    let mut v = serde_json::to_value(summary).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("workspace_id".into(), serde_json::json!(workspace_id));
        obj.insert("project".into(), serde_json::json!(project));
    }
    v
}

#[derive(Deserialize)]
pub(crate) struct CreatePlaybookBody {
    id: String,
    yaml: String,
    /// The lowering of the launch floor the person confirmed in the editor
    /// (see [`editor_channel`]).
    #[serde(default)]
    confirm_lower_floor: Option<apb_core::launch::LaunchPolicy>,
}

#[derive(Deserialize)]
pub(crate) struct UpdatePlaybookBody {
    yaml: String,
    #[serde(default)]
    confirm_lower_floor: Option<apb_core::launch::LaunchPolicy>,
}

/// The save channel of the editor: with keys, a person who lowers a launch
/// floor only to the declaration they confirmed (`confirm_lower_floor`,
/// sent after the 409 `launch_floor_lowering_unconfirmed` the editor shows
/// and confirms); without keys, a save that never lowers a floor (409
/// `launch_floor_lowering_needs_terminal`, pointing to `apb launch floor
/// --lower-to`).
fn editor_channel(
    auth: &crate::auth::AuthState,
    confirm: Option<apb_core::launch::LaunchPolicy>,
) -> SaveChannel {
    if auth.enabled() {
        SaveChannel::Editor { lower_to: confirm }
    } else {
        SaveChannel::KeylessEditor
    }
}

#[derive(Deserialize)]
pub(crate) struct LayoutBody {
    /// YAML string or JSON layout value (coerced to YAML).
    layout: serde_json::Value,
}

#[derive(Deserialize)]
pub(crate) struct LayoutQuery {
    version: String,
    workspace: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct DiffQuery {
    from: String,
    to: String,
    workspace: Option<String>,
}

pub(crate) async fn create_playbook(
    State(state): State<AppState>,
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<CreatePlaybookBody>,
) -> impl IntoResponse {
    if !is_safe_id(&body.id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    // The editor is a person saving the version they read: the launch floor
    // rises with its declaration and is lowered only as confirmed.
    let channel = editor_channel(&state.auth, body.confirm_lower_floor);
    match save_definition_by(&root, &body.id, &body.yaml, None, true, channel) {
        Ok(saved) => (
            if saved.unchanged {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            },
            Json(saved.answer(&body.id)),
        )
            .into_response(),
        Err(e) => versioning_error(e),
    }
}

pub(crate) async fn update_playbook(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<UpdatePlaybookBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let dir = root.join(".apb/playbooks").join(&id);
    if !dir.is_dir() {
        return (StatusCode::NOT_FOUND, format!("playbook `{id}` not found")).into_response();
    }
    let channel = editor_channel(&state.auth, body.confirm_lower_floor);
    match save_definition_by(&root, &id, &body.yaml, None, true, channel) {
        Ok(saved) => Json(saved.answer(&id)).into_response(),
        Err(e) => versioning_error(e),
    }
}

#[derive(Deserialize)]
pub(crate) struct FrozenBody {
    frozen: bool,
}

/// PUT /api/playbooks/{id}/frozen: freeze or unfreeze a playbook. Freeze is an
/// operator action exposed only here (the dashboard button); agents have no
/// path to toggle it. A frozen playbook keeps running but refuses every
/// definition change.
pub(crate) async fn set_frozen_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<FrozenBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    match reg.set_frozen(&id, body.frozen) {
        Ok(()) => Json(serde_json::json!({ "id": id, "frozen": body.frozen })).into_response(),
        Err(RegistryError::NotFound(what)) => (StatusCode::NOT_FOUND, what).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct RunBody {
    #[serde(default)]
    instruction: Option<String>,
    #[serde(default)]
    params: std::collections::BTreeMap<String, String>,
    /// Run id to continue as a fresh run (issue #42 finding 10).
    #[serde(default)]
    continued_from: Option<String>,
    /// The run's working tree (issue #67 item 8): overrides the playbook's
    /// `worktree`; the run locks this tree instead of the project root.
    #[serde(default)]
    worktree: Option<String>,
    /// 0.24.0: the person confirmed the playbook's irreversible effects in
    /// the Run dialog: the `consent_nonce` of the 409 refusal the dialog
    /// showed (a bare `true` is accepted for one release with a deprecation
    /// note). Without it such a run is refused (409,
    /// `irreversible_requires_confirmation`, with `sources` and
    /// `consent_nonce`), which is also what an event bridge posting here
    /// without the field gets. The API trusts its authenticated caller: any
    /// holder of the dashboard token can send the field.
    #[serde(default)]
    confirm_irreversible: Option<apb_engine::consent::Confirmation>,
    /// The run's budget (`tokens`, `usd`, `on_no_estimate`), over the
    /// playbook's `budget:` block. With `on_no_estimate` ask and nodes that
    /// have no estimate, the start is refused (409 `budget_needs_decision`,
    /// with the estimate) until the person picks pause or continue.
    #[serde(default)]
    budget: Option<super::estimate::BudgetBody>,
    /// Remember `params` and `instruction` for this playbook in this
    /// project once the start passes the gate (the Run dialog's "Remember
    /// for this project"). The answer lists `remembered` and
    /// `not_remembered` (secret params and credential-shaped values are
    /// never stored).
    #[serde(default)]
    remember: bool,
    /// The run's notifications (the Run dialog's "Notify me"): `true`
    /// through the machine's target, `false` off whatever the machine
    /// default, or `{ on, target }`. A request that cannot be honoured
    /// refuses the start (409 `notify_unavailable`; 400 `invalid_notify`)
    /// before anything is written.
    /// Taken as JSON and parsed here, so a malformed value answers 400
    /// `invalid_notify` like MCP rather than the extractor's plain error.
    #[serde(default)]
    notify: Option<serde_json::Value>,
    /// A dashboard without keys counts as the CLI without a terminal for
    /// the launch policy: an `ask` playbook starts only with the
    /// `launch_nonce` of its 409 refusal here, together with that refusal's
    /// `launch_version` and `launch_choice`; a `deny` one never does. A
    /// dashboard with keys ignores these fields.
    #[serde(default)]
    confirm_launch: Option<String>,
    #[serde(default)]
    launch_version: Option<String>,
    #[serde(default)]
    launch_choice: Option<String>,
}

/// The launch context of a click in the dashboard: a person's with keys;
/// without keys anything on the machine reaches the port, so it is decided
/// like the CLI without a terminal (an `ask` launch only with its nonce, a
/// `deny` launch refused).
pub(crate) fn dashboard_launch(
    auth: &crate::auth::AuthState,
    kind: apb_engine::gate::launch::LaunchKind,
    confirm_launch: Option<String>,
    launch_version: Option<String>,
    launch_choice: Option<&str>,
) -> apb_engine::gate::launch::LaunchContext {
    use apb_engine::gate::launch::{LaunchConfirmation, LaunchContext};
    let keys = auth.enabled();
    let ctx = LaunchContext::dashboard_with(keys, kind);
    if keys {
        return ctx;
    }
    ctx.confirmed(confirm_launch.map(|nonce| LaunchConfirmation::DashboardNonce { nonce }))
        .fixed(launch_version, launch_choice)
}

/// POST /api/playbooks/{id}/run: starts an autonomous run in the background and
/// returns its run_id immediately, so the dashboard can jump straight to the
/// run view.
///
/// The run is prepared here and then driven by a DETACHED `apb __drive-run`
/// process, exactly like `apb run --detach` and MCP `playbook_run` with
/// `background: true`. It used to be driven on a thread of the dashboard
/// process instead, so every dashboard restart (a service restart, a
/// reinstall) orphaned every run the dashboard had started.
///
/// A start that finds the shared workdir already held by another write-run is
/// QUEUED rather than refused (`server.workdir_queue_wait_seconds`, default
/// 15 minutes): the run directory, the playbook snapshot and the caller's
/// parameters are written, the run id comes back 200 as usual, and the engine
/// claims the workdir when the holder releases it. This endpoint is what an
/// inbound-event bridge posts to, and such a caller has nowhere to put a
/// refusal - the event it was carrying is gone the moment the POST fails.
/// Persisting the start IS the queue; there is no second event store. An
/// operator who wants the old behavior back sets the wait to `0` and gets the
/// 429 below.
///
/// Before anything is written the start goes through the one run gate every
/// launch surface uses, `apb_engine::gate::check_run`: a draft or retired
/// playbook, unmet `requires`, an untrusted connector or account, or a broken
/// sub-playbook tree is refused with the gate's structural JSON (409; 404 when
/// the playbook does not resolve), and so is a param the start leaves
/// without a value (`apb_engine::gate::check_params`). The person clicking Run is the trust
/// confirmation, so the call acknowledges untrusted content the way MCP does
/// after asking the user; connector trust is never bypassable. The returned
/// permit is applied to the run verbatim (anti-TOCTOU), and its consent-time
/// warnings ride the 200 answer. Irreversible effects are the exception to
/// "clicking Run is the confirmation": they need `confirm_irreversible`, which
/// the dashboard sends only after its dialog (0.24.0). The run's `budget`
/// is checked next (see `routes::estimate`), then its `notify` request
/// (see `routes::notify`), and the answer carries the run's `estimate` and
/// `budget`.
pub(crate) async fn run_playbook_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<RunBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let launch_kind = match &body.continued_from {
        Some(pred) => apb_engine::gate::launch::LaunchKind::ContinueFrom {
            run_id: pred.clone(),
            node: String::new(),
        },
        None => apb_engine::gate::launch::LaunchKind::Start,
    };
    let launch = dashboard_launch(
        &state.auth,
        launch_kind,
        body.confirm_launch.clone(),
        body.launch_version.clone(),
        body.launch_choice.as_deref(),
    );
    let mut opts = apb_engine::RunOptions {
        instruction: body.instruction,
        params: body.params,
        continued_from: body.continued_from,
        worktree: body.worktree,
        // A busy workdir queues the start instead of refusing it (see the
        // handler doc): the caller is answered with a run id and the run's
        // parameters are already persisted, so nothing depends on the caller
        // still being around when the engine frees up.
        workdir_queue_wait: state.workdir_queue_wait,
        ..Default::default()
    };

    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: id.clone(),
        version: None,
    };
    // A refusal carries the start actions that answer it (`start_actions`).
    let of = apb_engine::next_action::start::StartOf::of(&wref);
    let refused =
        |refusal: serde_json::Value| apb_mcp::tools::actions::refusal_with_actions(of, refusal);
    // The click is the person's launch when the dashboard has keys (design
    // 3.1 item 7); without keys it is decided like the CLI without a
    // terminal (see `dashboard_launch`).
    let permit = match apb_engine::gate::check_run(&root, &wref, true, false, &launch) {
        Ok(p) => p,
        Err(refusal) => {
            let status = if refusal["policy"] == "not_found" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::CONFLICT
            };
            return (status, Json(refused(refusal))).into_response();
        }
    };
    // 0.24.0: an irreversible tree needs the dialog's confirmation, bound
    // to the nonce of the refusal the dialog showed.
    let deprecation = match permit.check_confirmation(body.confirm_irreversible.as_ref()) {
        Ok(note) => note,
        Err(refusal) => return (StatusCode::CONFLICT, Json(refused(refusal))).into_response(),
    };
    if permit.needs_consent() {
        opts.consent = Some(apb_engine::consent::RunConsent::irreversible("dashboard"));
    }
    let mut warnings = permit.warnings.clone();
    warnings.extend(permit.launch_warnings.iter().cloned());
    // Issue #192: the candidate the gate chose (and pinned) is what runs.
    let version = permit.run_version(None);
    // A param the start leaves without a value is refused like the gate
    // refuses, with the start actions that answer it.
    let run_ref = apb_core::scope::PlaybookRef {
        version: version.clone(),
        ..wref.clone()
    };
    // Both read the playbook from disk: off the async runtime.
    let (params_check, short) = {
        let (root, run_ref, params) = (root.clone(), run_ref.clone(), opts.params.clone());
        match tokio::task::spawn_blocking(move || {
            (
                apb_engine::gate::check_params(&root, &run_ref, &params),
                apb_engine::gate::short_secret_warning(&root, &run_ref, &params),
            )
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
    };
    if let Err(refusal) = params_check {
        return (StatusCode::CONFLICT, Json(refused(refusal))).into_response();
    }
    warnings.extend(short);
    // The run's budget and the pre-start decision it may need; the check
    // reads the run history, off the async runtime.
    let checked = {
        let (root, id, version, budget) = (root.clone(), id.clone(), version.clone(), body.budget);
        tokio::task::spawn_blocking(move || {
            super::estimate::start_budget(&root, &id, version.as_deref(), budget.as_ref())
        })
        .await
    };
    match checked {
        Ok(Ok(c)) => {
            opts.budget = c.budget;
            opts.estimate = c.estimate;
            warnings.extend(c.warnings);
        }
        Ok(Err(refusal)) => return refusal.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
    // The notification request, checked like MCP checks it: the engine
    // checks it again at start and pins the target.
    let notify = match body
        .notify
        .as_ref()
        .map(super::notify::parse_request)
        .transpose()
    {
        Ok(req) => req,
        Err(e) => return super::notify::refusal(e),
    };
    let checked = {
        let (root, notify, wref) = (root.clone(), notify.clone(), wref.clone());
        tokio::task::spawn_blocking(move || {
            let key = apb_core::policy_file::PlaybookKey::of(&wref);
            apb_mcp::tools::notify::start_check(&root, Some(&key), notify.as_ref())
        })
        .await
    };
    match checked {
        Ok(Ok(notes)) => warnings.extend(notes),
        Ok(Err(e)) => return super::notify::refusal(e),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
    opts.notify = notify;
    // What the person chose to keep is remembered once the start succeeded.
    let keep = body
        .remember
        .then(|| (opts.params.clone(), opts.instruction.clone()));
    let candidate_trial = permit.candidate.clone();
    let candidate_skipped = permit.candidate_skipped.clone();
    permit.apply(&mut opts);

    match apb_engine::start_detached(&root, &id, version.as_deref(), opts) {
        Ok(run_id) => {
            let mut answer = serde_json::json!({ "run_id": run_id, "warnings": warnings });
            // Issue #192: the candidate this start tries, or why it was left
            // out (a person clicking Run does not stand in for its trust).
            if let Some(v) = candidate_trial {
                answer["candidate_trial"] = serde_json::json!(v);
            }
            if let Some(why) = candidate_skipped {
                answer["candidate_skipped"] = serde_json::json!(why);
            }
            if let Some(note) = deprecation {
                answer["deprecation"] = serde_json::json!(note);
            }
            let after = {
                let (root, id, run_id) = (root.clone(), id.clone(), run_id.clone());
                tokio::task::spawn_blocking(move || after_start(&root, &id, &run_id, keep)).await
            };
            match after {
                Ok(serde_json::Value::Object(m)) => {
                    if let Some(obj) = answer.as_object_mut() {
                        obj.extend(m);
                    }
                }
                Ok(_) => {}
                // The start stands; the answer says the memory step did not run.
                Err(e) => answer["memory_error"] = serde_json::json!(e.to_string()),
            }
            // The estimate the run started with and its budget.
            Json(apb_mcp::tools::budget::with_start_blocks(&root, answer)).into_response()
        }
        Err(apb_engine::EngineError::NotFound(what)) => {
            (StatusCode::NOT_FOUND, what).into_response()
        }
        Err(apb_engine::EngineError::Conflict(what)) => {
            (StatusCode::CONFLICT, what).into_response()
        }
        // The engine's own notify check, should the settings have changed
        // since the one above: the same answer, and still nothing written.
        Err(apb_engine::EngineError::Invalid(what))
            if what.starts_with(apb_engine::notify::REFUSED_START) =>
        {
            let why = what[apb_engine::notify::REFUSED_START.len()..]
                .trim_start_matches(':')
                .trim();
            super::notify::refusal(apb_mcp::tools::notify::NotifyError::Unavailable(
                why.to_string(),
            ))
        }
        // Client precondition failures (e.g. cross-playbook continued_from)
        // must not look like server faults.
        Err(apb_engine::EngineError::Invalid(what)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, what).into_response()
        }
        // #102.5: another write-run holds the workdir. Only reachable now with
        // queueing switched off (`server.workdir_queue_wait_seconds: 0`) - with
        // the default the start is admitted and this arm never fires. The 5s
        // hint mirrors `workdir::HANDOVER_WAIT`, which bounds only the handover
        // race between a preparing process and its detached driver, not this
        // caller's own retry - so it is an honest hint, not a guarantee.
        Err(apb_engine::EngineError::WorkdirBusy(what)) => {
            (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "5")], what).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// What a dashboard start reports once its run exists: `recalled` (the
/// params and instruction the start took from memory) and, for a start
/// with `remember`, what was remembered for the project playbook `id`
/// (checked against its current version). A memory failure is reported in
/// the answer (`memory_error`), never a failure of the start.
fn after_start(
    root: &std::path::Path,
    id: &str,
    run_id: &str,
    keep: Option<(std::collections::BTreeMap<String, String>, Option<String>)>,
) -> serde_json::Value {
    let mut out = serde_json::json!({});
    let recalled = apb_engine::run_view::recalled_input(&root.join(".apb/runs").join(run_id));
    if !recalled.is_empty() {
        out["recalled"] = serde_json::json!(recalled);
    }
    let Some((params, instruction)) = keep else {
        return out;
    };
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: id.to_string(),
        version: None,
    };
    let memory =
        apb_mcp::tools::memory::remember_for_ref(root, &wref, &params, instruction.as_deref())
            .unwrap_or_else(|e| serde_json::json!({ "memory_error": e.to_string() }));
    if let (Some(o), serde_json::Value::Object(m)) = (out.as_object_mut(), memory) {
        o.extend(m);
    }
    out
}

pub(crate) async fn delete_playbook_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match delete_playbook(&root, &id, apb_core::clock::now_ms()) {
        Ok(trashed) => Json(serde_json::json!({
            "trashed": trashed.to_string_lossy(),
        }))
        .into_response(),
        Err(e) => versioning_error(e),
    }
}

pub(crate) async fn put_layout(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<LayoutQuery>,
    Json(body): Json<LayoutBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) || !is_safe_id(&q.version) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let layout_yaml = match body.layout {
        serde_json::Value::String(s) => s,
        other => match yaml_serde::to_string(&other) {
            Ok(s) => s,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "schema", "message": e.to_string() })),
                )
                    .into_response();
            }
        },
    };
    match save_layout(&root, &id, &q.version, &layout_yaml) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => versioning_error(e),
    }
}

pub(crate) async fn get_diff(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<DiffQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) || !is_safe_id(&q.from) || !is_safe_id(&q.to) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match version_diff(&root, &id, &q.from, &q.to) {
        Ok(diff) => Json(diff).into_response(),
        Err(e) => versioning_error(e),
    }
}

pub(crate) async fn list_versions_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    match list_versions_with_provenance(&root, &id) {
        Ok(infos) => Json(infos).into_response(),
        Err(e) => versioning_error(e),
    }
}

/// Aggregated playbook list across every reachable project (global server) or
/// the single pinned root (test harness). Each entry is tagged with its owning
/// workspace_id and project name so the dashboard shows affiliation. A project
/// that fails to open is skipped rather than failing the whole list.
pub(crate) async fn list_playbooks(State(state): State<AppState>) -> impl IntoResponse {
    let mut out: Vec<serde_json::Value> = Vec::new();
    for (workspace_id, project, root) in enumerate_workspaces(&state) {
        let Ok(reg) = Registry::open(&root) else {
            continue;
        };
        let Ok(list) = reg.list() else {
            continue;
        };
        for summary in &list {
            out.push(tag_summary(summary, &workspace_id, &project));
        }
    }
    Json(out).into_response()
}

/// The issues the playbook detail and the editor's draft validation show:
/// the validator's, plus the missing profile and script references of the
/// version directory `version_dir` (V91, V92) as warnings, because the
/// editor saves the YAML first and the profile or the scripts may follow;
/// `apb validate` errors on them. Reads only.
pub(crate) fn validation_issues(
    reg: &Registry,
    root: &std::path::Path,
    version_dir: &std::path::Path,
    playbook: &apb_core::schema::Playbook,
) -> Vec<apb_core::validate::IssueView> {
    let ctx =
        ValidationContext::for_registry(reg, apb_core::profile_store::PlaybookOrigin::Project)
            .with_child_outputs(playbook, reg);
    let mut report = validate(playbook, &ctx);
    report.issues.extend(
        apb_core::preflight::missing_references(
            root,
            apb_core::profile_store::PlaybookOrigin::Project,
            version_dir,
            playbook,
        )
        .into_iter()
        .map(|mut i| {
            i.severity = Severity::Warning;
            i
        }),
    );
    report.issues.iter().map(Into::into).collect()
}

#[derive(Deserialize)]
pub(crate) struct DetailQuery {
    version: Option<String>,
    workspace: Option<String>,
}

pub(crate) async fn get_playbook(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<DetailQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    match reg.load(&id, q.version.as_deref()) {
        Ok(loaded) => {
            let validation = validation_issues(
                &reg,
                &root,
                &reg.version_dir(&id, &loaded.version),
                &loaded.playbook,
            );
            Json(serde_json::json!({
                "id": id,
                "version": loaded.version,
                "yaml": loaded.yaml,
                "playbook": loaded.playbook,
                // What a run may touch as the run policy counts it: inferred
                // from the nodes, plus the playbook's and each node's own
                // declaration. The declared list alone can understate it.
                "effective_effects": apb_core::effects::effective(&loaded.playbook),
                "layout": loaded.layout,
                "validation": validation,
                // The address of every edge of the shown definition, aligned
                // with `playbook.edges`.
                "edge_addresses": apb_core::edge_address::edge_addresses(&loaded.playbook),
                "frozen": reg.is_frozen(&id),
            }))
            .into_response()
        }
        Err(RegistryError::NotFound(what)) => (StatusCode::NOT_FOUND, what).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
