//! `apb evidence read`: one reading step of an attempt's evidence session,
//! for an agent with a shell (any agent CLI). The answer is the JSON MCP
//! `run_evidence_read` gives for the same step: both go through
//! [`apb_mcp::tools::evidence`].

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::step_env::StepCaller;
use apb_mcp::tools::ToolError;
use apb_mcp::tools::evidence::{ReadArgs, evidence_read, read_request};
use clap::Subcommand;

use crate::util::{QUESTION_TEXT_MAX, sanitize_for_terminal};

#[derive(Subcommand)]
pub(crate) enum EvidenceAction {
    /// One reading step of an evidence session (JSON out): outline,
    /// children, read, page or search
    Read {
        /// The session handle `<run_id>:<session>` (APB_EVIDENCE_SESSION)
        #[arg(long)]
        session: String,
        /// outline | children | read | page | search
        op: String,
        /// read: the record reference
        #[arg(long = "ref", value_name = "REF")]
        reference: Option<String>,
        /// read: a character span `a,b` of the record (not a full read)
        #[arg(long, value_name = "A,B")]
        span: Option<String>,
        /// children, page: the view; search: only this view
        #[arg(long)]
        view: Option<String>,
        /// children, page: the first record index
        #[arg(long)]
        from: Option<u64>,
        /// children, page: how many records (at most 200)
        #[arg(long)]
        count: Option<u64>,
        /// search: the phrase to find
        #[arg(long)]
        phrase: Option<String>,
    },
}

fn span_of(s: &str) -> Option<[u64; 2]> {
    let (a, b) = s.split_once(',')?;
    Some([a.trim().parse().ok()?, b.trim().parse().ok()?])
}

pub(crate) fn evidence_cmd(root: &Path, action: EvidenceAction) -> ExitCode {
    let EvidenceAction::Read {
        session,
        op,
        reference,
        span,
        view,
        from,
        count,
        phrase,
    } = action;
    let span = match span.as_deref().map(span_of) {
        Some(None) => {
            eprintln!("evidence: --span is `a,b` with two character offsets");
            return ExitCode::from(2);
        }
        Some(s) => s,
        None => None,
    };
    let args = ReadArgs {
        op,
        r#ref: reference,
        span,
        view,
        from,
        count,
        phrase,
    };
    let hint = std::env::var_os("APB_RUN_DIR").map(PathBuf::from);
    let answer = read_request(&args).and_then(|req| {
        evidence_read(
            root,
            &session,
            &req,
            &StepCaller::from_env(),
            hint.as_deref(),
        )
    });
    match answer {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!(
                "evidence: {}",
                sanitize_for_terminal(&e.to_string(), QUESTION_TEXT_MAX * 2)
            );
            match e {
                ToolError::NotFound(_) => ExitCode::from(2),
                _ => ExitCode::from(1),
            }
        }
    }
}
