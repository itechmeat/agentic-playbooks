//! MCP tool handlers for run quality: a host's grading session over an
//! ended run (`run_grade_start`, `run_grade_read`, `run_grade_submit`),
//! a run's grades (`run_grades`) and a playbook's candidate compared by
//! quality (`playbook_candidate`, read-only: promoting or keeping current
//! is a person's decision, made in the CLI or the dashboard). The logic
//! lives in `crate::tools::grade` and `crate::tools::candidate`, shared
//! with `apb grade`, `apb candidate` and the HTTP API.
//!
//! The grading tools are refused when this server itself runs as a step
//! of a run (`APB_RUN_ID` or `APB_HOST_TASK` in its own environment, as a
//! server an agent CLI started inside a run inherits). Every handler reads
//! or writes files and may wait on a lock, so it runs off the async
//! runtime.

use apb_core::quality::grade::{Cite, Level};
use apb_core::step_env::StepCaller;
use apb_engine::grade::evidence::Proposal;
use apb_engine::grade::reader::ReadRequest;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use serde_json::json;

use super::args::*;
use super::{WfMcp, to_call_tool_result};
use crate::tools;
use crate::tools::grade::to_value;

/// The reading operation a `run_grade_read` call names.
fn read_request(a: &RunGradeReadArgs) -> Result<ReadRequest, String> {
    let view = || {
        a.view
            .clone()
            .ok_or_else(|| format!("op `{}` needs view", a.op))
    };
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
            r#ref: a.r#ref.clone().ok_or("op `read` needs ref")?,
            span: a.span,
        },
        "search" => ReadRequest::Search {
            query: a.query.clone().ok_or("op `search` needs query")?,
            view: a.view.clone(),
        },
        other => {
            return Err(format!(
                "unknown op `{other}`: use outline, children, read, page or search"
            ));
        }
    })
}

/// The proposals of a `run_grade_submit` call.
fn proposals(grades: Vec<GradeProposalArg>) -> Result<Vec<Proposal>, String> {
    grades
        .into_iter()
        .map(|g| {
            let level: Level = serde_json::from_value(json!(g.level)).map_err(|_| {
                format!(
                    "criterion `{}`: unknown level `{}`; use good, partial, poor, not_applicable or not_enough_data",
                    g.criterion, g.level
                )
            })?;
            Ok(Proposal {
                criterion: g.criterion,
                level,
                reason_code: g.reason_code,
                why: g.why,
                cites: g
                    .cites
                    .into_iter()
                    .map(|c| Cite {
                        r#ref: c.r#ref,
                        span: c.span,
                    })
                    .collect(),
                absence: g.absence,
            })
        })
        .collect()
}

/// Runs `work` on the blocking pool and answers its result. The work runs
/// to its end even when the request is cancelled: a submission a client
/// stopped waiting for may still be stored (spending the run's host grade)
/// and re-decide the candidate. A task that panicked says so.
async fn off_runtime<F>(work: F) -> CallToolResult
where
    F: FnOnce() -> Result<serde_json::Value, tools::ToolError> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(work).await.unwrap_or_else(|e| {
        let what = if e.is_panic() {
            let payload = e.into_panic();
            let text = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            format!("panicked: {text}")
        } else {
            e.to_string()
        };
        Err(tools::ToolError::Engine(format!(
            "run quality task failed: {what}"
        )))
    });
    to_call_tool_result(result)
}

fn invalid(detail: String) -> CallToolResult {
    to_call_tool_result(Ok(
        json!({ "error": "invalid_arguments", "detail": detail }),
    ))
}

#[tool_router(router = quality_router, vis = "pub(crate)")]
impl WfMcp {
    #[tool(
        description = "Grade an ended run on its playbook's own quality criteria, only when the user asks for it. Opens a bounded reading session (one per run, one hour; a run takes one host grade, and only a person reopens it for another) and returns the session, the criteria with their good, partial and poor descriptions, and the outline of the run's views. Read with run_grade_read, then submit with run_grade_submit. Cite only records run_grade_read returned whole (citable: true); outline, children and search answers only route you. The built-in criteria (outcome, loops, scope, consent, skill_use) are graded by apb's rules, not here.",
        annotations(destructive_hint = false)
    )]
    pub(crate) async fn run_grade_start(
        &self,
        Parameters(RunGradeStartArgs {
            run_id,
            budget_chars,
            workspace,
        }): Parameters<RunGradeStartArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        off_runtime(move || {
            tools::grade::grade_start(&root, &run_id, budget_chars, &StepCaller::from_env())
                .and_then(to_value)
        })
        .await
    }

    #[tool(
        description = "One reading step of a grading session: op outline (the views), children (a view's record refs with short snippets), read (one record whole by ref, or a span of it), page (consecutive records of a view, each whole) or search (refs whose text holds a phrase). Every text is redacted and counts against the session's character budget. Only items with citable: true may be cited.",
        annotations(destructive_hint = false)
    )]
    pub(crate) async fn run_grade_read(
        &self,
        Parameters(args): Parameters<RunGradeReadArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(args.workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let req = match read_request(&args) {
            Ok(r) => r,
            Err(detail) => return invalid(detail),
        };
        off_runtime(move || {
            tools::grade::grade_read(
                &root,
                &args.run_id,
                &args.session,
                &req,
                &StepCaller::from_env(),
            )
            .and_then(to_value)
        })
        .await
    }

    #[tool(
        description = "Submit one level per criterion of a grading session: good, partial or poor with the refs it rests on, or not_applicable, or not_enough_data. A claim that something did not happen sets absence: true and needs every view the criterion reads complete and read to its end. Returns status graded with the stored grade (and the candidate decision it led to, when the playbook has a candidate on trial), or status rejected with the problems: correct them and submit once more; a level changed without a record newly read whole becomes not_enough_data. Cite only records of the views the criterion reads. not_applicable never counts toward promoting a candidate. reason_code is a short slug ([a-z0-9_]).",
        annotations(destructive_hint = false)
    )]
    pub(crate) async fn run_grade_submit(
        &self,
        Parameters(RunGradeSubmitArgs {
            run_id,
            session,
            grades,
            workspace,
        }): Parameters<RunGradeSubmitArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        let proposals = match proposals(grades) {
            Ok(p) => p,
            Err(detail) => return invalid(detail),
        };
        off_runtime(move || {
            tools::grade::grade_submit(
                &root,
                &run_id,
                &session,
                &proposals,
                &StepCaller::from_env(),
            )
            .and_then(to_value)
        })
        .await
    }

    #[tool(
        description = "The stored grades of a run, oldest first: per criterion the level, the reason, the cited refs and whether it counts for promotion, plus the grader and a trace of what it read (counts, never text).",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn run_grades(
        &self,
        Parameters(RunGradesArgs { run_id, workspace }): Parameters<RunGradesArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        off_runtime(move || {
            tools::grade::run_grades(&root, &run_id)
                .and_then(|grades| to_value(json!({ "run_id": run_id, "grades": grades })))
        })
        .await
    }

    #[tool(
        description = "A project playbook's candidate version (a forward patch on trial) and how it compares with current by quality: the trials, the gate and, per criterion, the graded runs of each side and the result, with the verdict (promote, keep_current, wait, needs_person) and the decision recorded last. candidate: null when there is none. Read-only: a person promotes or keeps current with apb candidate or in the dashboard.",
        annotations(read_only_hint = true)
    )]
    pub(crate) async fn playbook_candidate(
        &self,
        Parameters(PlaybookIdArgs { id, workspace }): Parameters<PlaybookIdArgs>,
    ) -> CallToolResult {
        let root = match self.effective_root(workspace.as_deref()) {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Ok(e)),
        };
        off_runtime(move || {
            tools::candidate::candidate_view(&root, &id)
                .and_then(|view| to_value(json!({ "id": id, "candidate": view })))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(op: &str) -> RunGradeReadArgs {
        RunGradeReadArgs {
            run_id: "r".into(),
            session: "s".into(),
            op: op.into(),
            r#ref: None,
            span: None,
            view: None,
            from: None,
            count: None,
            query: None,
            workspace: None,
        }
    }

    #[test]
    fn a_read_names_what_its_op_needs() {
        assert_eq!(read_request(&args("outline")), Ok(ReadRequest::Outline));
        assert!(read_request(&args("read")).unwrap_err().contains("ref"));
        assert!(read_request(&args("page")).unwrap_err().contains("view"));
        assert!(read_request(&args("search")).unwrap_err().contains("query"));
        assert!(
            read_request(&args("grep"))
                .unwrap_err()
                .contains("unknown op")
        );
        let mut a = args("page");
        a.view = Some("journal".into());
        a.count = Some(5);
        assert_eq!(
            read_request(&a),
            Ok(ReadRequest::Page {
                view: "journal".into(),
                from: 0,
                count: Some(5)
            })
        );
    }

    #[test]
    fn a_proposal_takes_only_a_known_level() {
        let g = |level: &str| GradeProposalArg {
            criterion: "tone".into(),
            level: level.into(),
            reason_code: String::new(),
            why: String::new(),
            cites: vec![GradeCiteArg {
                r#ref: "o:draft".into(),
                span: None,
            }],
            absence: false,
        };
        let ok = proposals(vec![g("not_enough_data")]).unwrap();
        assert_eq!(ok[0].level, Level::NotEnoughData);
        assert_eq!(ok[0].cites, vec![Cite::to("o:draft")]);
        assert!(
            proposals(vec![g("great")])
                .unwrap_err()
                .contains("unknown level")
        );
    }
}
