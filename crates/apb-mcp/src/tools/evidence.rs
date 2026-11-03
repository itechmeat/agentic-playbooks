//! Reading through an attempt's evidence session (MCP `run_evidence_read`,
//! `apb evidence read`), over [`apb_engine::evidence::session`], so both
//! surfaces give the same answer and charge the same budget.
//!
//! A session is named by its id only; the run holding it is found under
//! `<root>/.apb/runs`. A run step reads only its own run's sessions
//! (`APB_RUN_ID`, a guard against mistakes), and a session serves only
//! while its attempt runs. Every text was served through the run redactor
//! when the session's views were built.

use std::path::Path;

use apb_core::step_env::StepCaller;
use apb_engine::evidence::session;
use apb_engine::grade::reader::ReadRequest;
use serde_json::{Value, json};

use super::ToolError;

/// One read, as both surfaces take it.
#[derive(Debug, Clone, Default)]
pub struct ReadArgs {
    /// `outline`, `children`, `read`, `page` or `search`.
    pub op: String,
    pub r#ref: Option<String>,
    pub span: Option<[u64; 2]>,
    pub view: Option<String>,
    pub from: Option<u64>,
    pub count: Option<u64>,
    pub phrase: Option<String>,
}

/// The reader's request for `a`.
pub fn request(a: &ReadArgs) -> Result<ReadRequest, ToolError> {
    let needs = |what: &str| ToolError::Engine(format!("op `{}` needs {what}", a.op));
    let view = || a.view.clone().ok_or_else(|| needs("view"));
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
            r#ref: a.r#ref.clone().ok_or_else(|| needs("ref"))?,
            span: a.span,
        },
        "search" => ReadRequest::Search {
            query: a.phrase.clone().ok_or_else(|| needs("phrase"))?,
            view: a.view.clone(),
        },
        other => {
            return Err(ToolError::Engine(format!(
                "unknown op `{other}`: use outline, children, read, page or search"
            )));
        }
    })
}

/// The project an agent step's `apb evidence read` reads in: the root of
/// the run directory `APB_RUN_DIR` names (`<root>/.apb/runs/<id>`), since
/// a step may work in a separate working tree, else `cwd`.
pub fn step_root(cwd: &Path) -> std::path::PathBuf {
    std::env::var_os("APB_RUN_DIR")
        .map(std::path::PathBuf::from)
        .and_then(|run_dir| {
            let runs = run_dir.parent()?;
            let apb = runs.parent()?;
            (runs.file_name()? == "runs" && apb.file_name()? == ".apb")
                .then(|| apb.parent().map(Path::to_path_buf))
                .flatten()
        })
        .unwrap_or_else(|| cwd.to_path_buf())
}

/// One read of evidence session `session` in the project at `root`, by
/// `caller`.
pub fn run_evidence_read(
    root: &Path,
    session_id: &str,
    a: &ReadArgs,
    caller: &StepCaller,
) -> Result<Value, ToolError> {
    let req = request(a)?;
    let run_dir = session::find(root, session_id, caller)?;
    let answer = session::read(&run_dir, session_id, &req, caller)?;
    let mut v = json!(answer);
    v["session"] = json!(session_id);
    Ok(v)
}
