//! History discovery on the surfaces: the step sequences a person's agent
//! sessions repeat, found in the agents' own history files
//! ([`apb_core::discover`]) and offered as playbook drafts. MCP
//! (`playbook_discover`, `playbook_discover_results`,
//! `playbook_discover_save`, `playbook_discover_dismiss`), `apb discover`
//! and the dashboard's `/api/discover` all go through this module.
//!
//! Reading history is a person's act, so every entry here, reads included,
//! is refused from a run step ([`guard`], the one check every surface
//! reaches). A surface names history roots and projects by name or
//! workspace id only; a raw directory reaches discovery only through
//! [`DiscoverArgs::extra_root`], which only the CLI's `--sessions` (typed
//! by a person) fills. Nothing returned here holds an absolute path.
//!
//! A result names each project by its workspace id: a save or a decline
//! from a surface without a project of its own lands in the project the
//! id resolves to ([`home_project`]), never one that only shares its name.

use std::path::{Path, PathBuf};

use apb_core::discover::{
    self, DiscoverRequest, DiscoverResult, HistoryRoot, ProjectRef, ProjectSeen, ReadLimits,
    RequestEcho, SuggestionStatus, store,
};
use apb_core::step_env::StepCaller;
use serde::Serialize;
use serde_json::Value;

use super::{ToolError, outside_run};

/// What a refusal from a run step names.
const DISCOVERING: &str = "reading agent session history for repeats";

/// Refuses every discovery entry (reads too) from a run step.
pub fn guard(step: &StepCaller) -> Result<(), ToolError> {
    outside_run(step, DISCOVERING)
}

/// A project a discovery reads the sessions of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectPick {
    /// The project at this root (the caller's own, or a path a person typed
    /// in a terminal).
    Root(PathBuf),
    /// A registered workspace by its id.
    Workspace(String),
    /// Every registered project that is there now.
    AllReachable,
}

/// One discovery as a surface asks for it.
#[derive(Debug, Clone, Default)]
pub struct DiscoverArgs {
    pub projects: Vec<ProjectPick>,
    /// History roots by name; empty: every configured root that is there.
    pub roots: Vec<String>,
    /// A directory a person named in a terminal (`--sessions`), read
    /// besides the named roots.
    pub extra_root: Option<HistoryRoot>,
    /// 1 to 365; absent: the configured `discover.since_days`, else 90.
    pub since_days: Option<u32>,
}

/// A history root as the surfaces list it: by name, never by path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RootInfo {
    pub name: String,
    pub agent: String,
    /// Whether it is a real directory here now.
    pub available: bool,
}

/// A discovery result as the surfaces return it: without its MAC, with
/// whether this installation signed it (only a signed result is saved
/// from), whether it is stored, and the person's earlier declines of save
/// suggestions, so a host can compare by meaning as it does for the
/// catalog.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ResultView {
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub result: DiscoverResult,
    pub signed: bool,
    pub stored: bool,
    #[cfg_attr(
        feature = "ts",
        ts(
            type = "Array<{ pattern: string, synopsis: string, kind: string, scope: string, declines: number, snoozed_until: string }>"
        )
    )]
    pub suppressed_suggestions: Vec<Value>,
}

/// One stored result in a listing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ResultSummary {
    pub id: String,
    pub created_at: u64,
    pub request: RequestEcho,
    pub sessions: u32,
    pub suggestions: usize,
    pub saved: usize,
    pub dismissed: usize,
    pub signed: bool,
}

fn settings() -> Result<apb_core::config::DiscoverSettings, ToolError> {
    apb_core::config::GlobalConfig::load()
        .map(|c| c.discover)
        .map_err(|e| ToolError::Engine(format!("the global config does not load: {e}")))
}

/// The configured history roots (or each agent's own directory), with
/// their paths: for the person's own terminal only (`apb discover show
/// --paths`). Every other caller lists them through [`roots`].
pub fn configured_roots() -> Result<Vec<HistoryRoot>, ToolError> {
    Ok(HistoryRoot::configured(&settings()?))
}

/// The configured history roots (or each agent's own directory), by name.
pub fn roots() -> Result<Vec<RootInfo>, ToolError> {
    Ok(configured_roots()?
        .iter()
        .map(|r| RootInfo {
            name: r.name.clone(),
            agent: r.agent.as_str().to_string(),
            available: discover::walk::check_root(r).is_ok(),
        })
        .collect())
}

fn pick_roots(args: &DiscoverArgs) -> Result<Vec<HistoryRoot>, ToolError> {
    let configured = configured_roots()?;
    let mut out: Vec<HistoryRoot> = if args.roots.is_empty() {
        if args.extra_root.is_some() {
            Vec::new()
        } else {
            configured
                .into_iter()
                .filter(|r| discover::walk::check_root(r).is_ok())
                .collect()
        }
    } else {
        let mut picked = Vec::new();
        for name in &args.roots {
            let r = configured
                .iter()
                .find(|r| &r.name == name)
                .ok_or_else(|| ToolError::Engine(format!("unknown history root `{name}`")))?;
            if !picked.contains(r) {
                picked.push(r.clone());
            }
        }
        picked
    };
    out.extend(args.extra_root.clone());
    if out.is_empty() {
        return Err(ToolError::Engine(
            "no history root is there to read (see `discover.roots` in the global config)".into(),
        ));
    }
    Ok(out)
}

/// The project at `root`: its registered name (else its directory name)
/// and its workspace id when it has one.
fn project_at(root: &Path) -> ProjectRef {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let workspace_id = apb_core::workspace::read_id(&root).unwrap_or_default();
    let name = apb_core::projects::list_active()
        .into_iter()
        .find(|e| !workspace_id.is_empty() && e.workspace_id == workspace_id)
        .map(|e| e.name)
        .unwrap_or_else(|| {
            root.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".to_string())
        });
    ProjectRef {
        workspace_id,
        name,
        root,
    }
}

fn pick_projects(picks: &[ProjectPick]) -> Result<Vec<ProjectRef>, ToolError> {
    let mut out: Vec<ProjectRef> = Vec::new();
    let mut push = |p: ProjectRef| {
        if !out.iter().any(|q| q.root == p.root) {
            out.push(p);
        }
    };
    for pick in picks {
        match pick {
            ProjectPick::Root(root) => push(project_at(root)),
            ProjectPick::Workspace(id) => {
                let root = apb_core::projects::resolve_root(id).map_err(|e| match e {
                    apb_core::projects::ProjectAccessError::Unknown(w) => {
                        ToolError::NotFound(format!("workspace `{w}`"))
                    }
                    apb_core::projects::ProjectAccessError::Unreachable {
                        workspace_id, ..
                    } => ToolError::Engine(format!("workspace `{workspace_id}` is unreachable")),
                })?;
                push(project_at(&root));
            }
            ProjectPick::AllReachable => {
                for e in apb_core::projects::list_reachable() {
                    push(project_at(Path::new(&e.path)));
                }
            }
        }
    }
    if out.is_empty() {
        return Err(ToolError::Engine("no project to read sessions of".into()));
    }
    Ok(out)
}

/// The roots of every registered project: a session belongs to the
/// deepest of them, selected or not.
fn registered_roots() -> Vec<PathBuf> {
    apb_core::projects::list_active()
        .into_iter()
        .map(|e| {
            let path = PathBuf::from(e.path);
            std::fs::canonicalize(&path).unwrap_or(path)
        })
        .collect()
}

/// The root of a project a result names, when its workspace id is
/// registered and there now.
fn root_of(p: &ProjectSeen) -> Option<PathBuf> {
    (!p.workspace_id.is_empty())
        .then(|| apb_core::projects::resolve_root(&p.workspace_id).ok())
        .flatten()
}

/// The person's declines that suppress save suggestions in the projects at
/// `roots` (and machine-wide), in the catalog's shape.
fn declines(roots: &[PathBuf]) -> Vec<Value> {
    let mut records: Vec<apb_core::dismiss::ScopedRecord> = Vec::new();
    let views = roots
        .iter()
        .map(|r| apb_core::dismiss::active(r))
        .chain(std::iter::once(apb_core::dismiss::active_global()));
    for view in views {
        for r in view.records {
            let seen = records.iter().any(|q| {
                q.scope == r.scope
                    && q.record.pattern == r.record.pattern
                    && q.record.synopsis == r.record.synopsis
            });
            if !seen {
                records.push(r);
            }
        }
    }
    crate::catalog::suppressed_json(&records)
}

fn view(
    mut result: DiscoverResult,
    signed: bool,
    stored: bool,
    declines: Vec<Value>,
) -> ResultView {
    result.mac = None;
    ResultView {
        result,
        signed,
        stored,
        suppressed_suggestions: declines,
    }
}

fn discover_error(e: discover::DiscoverError) -> ToolError {
    match e {
        discover::DiscoverError::Busy => ToolError::Conflict(format!("busy: {e}")),
        other => ToolError::Engine(format!("{}: {other}", other.code())),
    }
}

fn store_error(e: store::StoreError) -> ToolError {
    match e {
        store::StoreError::NotFound(_) | store::StoreError::NoSuggestion(_) => {
            ToolError::NotFound(e.to_string())
        }
        store::StoreError::Unsigned(_) | store::StoreError::Resolved(..) => {
            ToolError::Conflict(e.to_string())
        }
        other => ToolError::Engine(format!("{}: {other}", other.code())),
    }
}

impl From<store::StoreError> for ToolError {
    fn from(e: store::StoreError) -> Self {
        store_error(e)
    }
}

/// Runs one discovery and stores its result (when there is a config
/// directory). Makes no model call and no network call; refused from a
/// run step, and while another discovery runs (`busy`).
pub fn run_discover(args: &DiscoverArgs, step: &StepCaller) -> Result<ResultView, ToolError> {
    guard(step)?;
    let since_days = match args.since_days {
        Some(d) => d,
        None => settings()?
            .since_days
            .unwrap_or(discover::DEFAULT_SINCE_DAYS),
    };
    let req = DiscoverRequest {
        roots: pick_roots(args)?,
        projects: pick_projects(&args.projects)?,
        registered: registered_roots(),
        limits: ReadLimits {
            since_days,
            ..ReadLimits::default()
        },
        mine: discover::MineParams::default(),
    };
    let mut result = discover::discover(&req).map_err(discover_error)?;
    let stored = match store::save(&mut result) {
        Ok(()) => true,
        Err(store::StoreError::NoStore) => false,
        Err(e) => return Err(store_error(e)),
    };
    let roots: Vec<PathBuf> = req.projects.iter().map(|p| p.root.clone()).collect();
    Ok(view(result, stored, stored, declines(&roots)))
}

/// The stored results, newest first. Refused from a run step.
pub fn results_list(step: &StepCaller) -> Result<Vec<ResultSummary>, ToolError> {
    guard(step)?;
    Ok(store::list()
        .into_iter()
        .map(|s| {
            let count = |want: fn(&SuggestionStatus) -> bool| {
                s.result
                    .suggestions
                    .iter()
                    .filter(|x| want(&x.status))
                    .count()
            };
            ResultSummary {
                saved: count(|st| matches!(st, SuggestionStatus::Saved { .. })),
                dismissed: count(|st| matches!(st, SuggestionStatus::Dismissed)),
                id: s.result.id.clone(),
                created_at: s.result.created_at,
                request: s.result.request.clone(),
                sessions: s.result.read.sessions,
                suggestions: s.result.suggestions.len(),
                signed: s.signed,
            }
        })
        .collect())
}

/// The stored result `id`, signed or not, with the declines that hold now
/// in the projects it read (by workspace id) and machine-wide. Refused from
/// a run step.
pub fn result_get(id: &str, step: &StepCaller) -> Result<ResultView, ToolError> {
    guard(step)?;
    let stored = store::load(id)?;
    let roots: Vec<PathBuf> = stored
        .result
        .request
        .projects
        .iter()
        .filter_map(root_of)
        .collect();
    Ok(view(stored.result, stored.signed, true, declines(&roots)))
}

/// The registered project a suggestion of `id` was seen in first, by its
/// workspace id: where a surface without a project of its own (the
/// machine-wide dashboard) saves or dismisses it in project scope. Not
/// found: no such result or suggestion; refused: none of its projects is
/// registered and there now.
pub fn home_project(id: &str, slug: &str) -> Result<PathBuf, ToolError> {
    let result = store::load(id)?.result;
    let s = result
        .suggestions
        .iter()
        .find(|s| s.slug == slug)
        .ok_or_else(|| store::StoreError::NoSuggestion(slug.to_string()))?;
    s.support.projects.iter().find_map(root_of).ok_or_else(|| {
        ToolError::Engine(
            "the suggestion's project is not registered here: name a workspace to save it in"
                .into(),
        )
    })
}

/// One save of a suggestion.
#[derive(Debug, Clone, Copy)]
pub struct SaveRequest<'a> {
    pub id: &'a str,
    pub slug: &'a str,
    /// `project` or `global`.
    pub scope: &'a str,
    /// The draft as the host rewrote it; absent: the suggestion's own.
    pub yaml: Option<&'a str>,
}

/// The profile stores a playbook saved in `scope` under `root` resolves
/// its profile in, in order: the project's then the machine's for a
/// project playbook, the machine's alone for a global one.
fn profile_stores(root: &Path, scope: &str) -> Vec<PathBuf> {
    let global = apb_core::store::global_playbooks_parent();
    match scope {
        "global" => global.into_iter().collect(),
        _ => std::iter::once(root.join(".apb")).chain(global).collect(),
    }
}

/// Saves suggestion `slug` of the signed result `id` as a draft playbook
/// through [`super::capture_draft`] (provenance `created_by: discover`,
/// with `edited: true` when the host rewrote the draft), in `root`'s
/// project or globally, and marks it saved; the suggestion's own draft
/// names the first profile where it is saved. The check that it is new,
/// the save and the mark run once, under the discovery store's lock. A
/// capture that refused (a secret-like value, a duplicate) is returned as
/// it is and marks nothing; a save whose mark could not be recorded is
/// returned with a `warning`. Refused from a run step.
pub fn discover_save(
    root: &Path,
    req: SaveRequest<'_>,
    step: &StepCaller,
) -> Result<Value, ToolError> {
    guard(step)?;
    let (out, unrecorded) = store::resolve(req.id, req.slug, |s| {
        let own = req.yaml.is_none();
        let yaml = match req.yaml {
            Some(y) => y.to_string(),
            None => {
                let stores = profile_stores(root, req.scope);
                let stores: Vec<&Path> = stores.iter().map(PathBuf::as_path).collect();
                discover::draft::retarget(&s.yaml, &stores)
            }
        };
        let out = super::capture_draft(
            root,
            super::CaptureRequest {
                synopsis: &s.synopsis,
                scope: req.scope,
                yaml: &yaml,
                origin: super::CaptureOrigin::Discover {
                    pattern: req.slug,
                    result: req.id,
                    edited: !own,
                },
            },
        )?;
        if out.get("rejected").is_some() {
            return Ok(store::Outcome::Refused(out));
        }
        let playbook = out["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ToolError::Engine("the capture returned no playbook id".into()))?
            .to_string();
        let status = SuggestionStatus::Saved {
            playbook,
            version: out["version"].as_str().unwrap_or_default().to_string(),
            scope: req.scope.to_string(),
        };
        let saved_in = if req.scope == "global" {
            store::SavedIn::Global
        } else {
            // The project the playbook was written into gets its id now if
            // it had none, so a later discovery of it can match the save.
            store::SavedIn::Project(apb_core::workspace::ensure_id(root).unwrap_or_default())
        };
        Ok::<_, ToolError>(store::Outcome::Resolved(out, status, Some(saved_in)))
    })?;
    Ok(with_warning(out, unrecorded))
}

/// `out` with a `warning` when what it did could not be recorded in the
/// discovery (it stands all the same).
fn with_warning(mut out: Value, unrecorded: Option<store::StoreError>) -> Value {
    if let Some(e) = unrecorded {
        out["warning"] = Value::String(format!(
            "done, but the discovery could not record it ({})",
            e.code()
        ));
    }
    out
}

/// Records a person's decline of suggestion `slug` of the signed result
/// `id` (`soft`: not now, `hard`: never) through the same store
/// `suggestion_dismiss` writes, with the suggestion's title as the
/// synopsis, and marks it dismissed, once, under the discovery store's
/// lock. Refused from a run step.
pub fn discover_dismiss(
    root: &Path,
    id: &str,
    slug: &str,
    kind: &str,
    scope: Option<&str>,
    step: &StepCaller,
) -> Result<Value, ToolError> {
    guard(step)?;
    let (out, unrecorded) = store::resolve(id, slug, |s| {
        let out = super::suggestion_dismiss(
            root,
            super::DismissRequest {
                pattern: slug,
                synopsis: &s.title,
                kind: Some(kind),
                scope,
                ttl_days: None,
            },
        )?;
        Ok::<_, ToolError>(if out.get("rejected").is_some() {
            store::Outcome::Refused(out)
        } else {
            store::Outcome::Resolved(out, SuggestionStatus::Dismissed, None)
        })
    })?;
    Ok(with_warning(out, unrecorded))
}
