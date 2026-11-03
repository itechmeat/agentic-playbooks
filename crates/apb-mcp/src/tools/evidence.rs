//! Reading an attempt's evidence session on every surface: MCP
//! `run_evidence_read` and `apb evidence read` build the same request
//! ([`read_request`]) and answer through the same call ([`evidence_read`]),
//! so their answers and budgets are equal. The reader, the session and the
//! check live in [`apb_engine::evidence`].
//!
//! A session is named by its handle `<run_id>:<session>`; the run is looked
//! up under `<root>/.apb/runs`, or at the caller's own `APB_RUN_DIR` when
//! that is the handle's run. A step of another run is refused, and so is a
//! read once the attempt ended.

use std::path::Path;

use apb_core::step_env::StepCaller;
use apb_engine::grade::reader::ReadRequest;
use serde_json::Value;

use super::ToolError;

/// The arguments of one reading step, as both surfaces take them.
#[derive(Debug, Clone, Default)]
pub struct ReadArgs {
    /// outline | children | read | page | search.
    pub op: String,
    pub r#ref: Option<String>,
    pub span: Option<[u64; 2]>,
    pub view: Option<String>,
    pub from: Option<u64>,
    pub count: Option<u64>,
    pub phrase: Option<String>,
}

/// The reader's request for `a`.
pub fn read_request(a: &ReadArgs) -> Result<ReadRequest, ToolError> {
    let need = |what: &str| ToolError::Engine(format!("op `{}` needs {what}", a.op));
    let view = || a.view.clone().ok_or_else(|| need("view"));
    Ok(match a.op.as_str() {
        "outline" => ReadRequest::Outline,
        "children" => ReadRequest::Children {
            view: view()?,
            from: a.from.unwrap_or(0),
            count: a.count,
        },
        "page" => ReadRequest::Page {
            view: view()?,
            from: a.from.unwrap_or(0),
            count: a.count,
        },
        "read" => ReadRequest::Read {
            r#ref: a.r#ref.clone().ok_or_else(|| need("ref"))?,
            span: a.span,
        },
        "search" => ReadRequest::Search {
            query: a.phrase.clone().ok_or_else(|| need("phrase"))?,
            view: a.view.clone(),
        },
        other => {
            return Err(ToolError::Engine(format!(
                "unknown op `{other}`: use outline, children, read, page or search"
            )));
        }
    })
}

/// One reading step of the evidence session `session` (a handle
/// `<run_id>:<session>`), for `caller`. `run_dir_hint` is the caller's
/// `APB_RUN_DIR`, where a child run's session sits.
pub fn evidence_read(
    root: &Path,
    session: &str,
    req: &ReadRequest,
    caller: &StepCaller,
    run_dir_hint: Option<&Path>,
) -> Result<Value, ToolError> {
    let dir = apb_engine::evidence::session::run_dir_for(root, session, run_dir_hint)?;
    let answer = apb_engine::evidence::session::read(&dir, session, req, caller.run_id.as_deref())?;
    serde_json::to_value(answer).map_err(|e| ToolError::Engine(e.to_string()))
}

/// The evidence sessions of the run at `dir` (only `node`'s), for the
/// results answers: each claim with its verdict, wording, flags and cited
/// fragments. Empty when the run has none or its text is withheld.
pub fn sessions_value(dir: &Path, node: Option<&str>) -> Value {
    apb_engine::evidence::view(dir, node)
        .ok()
        .and_then(|v| serde_json::to_value(v).ok())
        .unwrap_or_else(|| Value::Array(Vec::new()))
}
