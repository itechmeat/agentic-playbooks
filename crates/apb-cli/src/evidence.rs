//! `apb evidence read`: one read through an attempt's evidence session, for
//! an agent step with a shell. The answer is the JSON object MCP
//! `run_evidence_read` answers, through the same tool layer
//! ([`apb_mcp::tools::evidence`]), so both give the same text and charge
//! the same budget. Inside a run step the project is the one `APB_RUN_DIR`
//! names, and only that run's sessions are read.

use std::path::Path;
use std::process::ExitCode;

use apb_core::step_env::StepCaller;
use apb_mcp::tools::ToolError;
use apb_mcp::tools::evidence::{ReadArgs, run_evidence_read, step_root};
use clap::Subcommand;

use crate::util::{QUESTION_TEXT_MAX, sanitize_for_terminal};

#[derive(Subcommand)]
pub(crate) enum EvidenceAction {
    /// Read through an evidence session: outline, children, read, page or
    /// search (JSON out)
    Read {
        /// The session the step's prompt names; default: APB_EVIDENCE_SESSION
        #[arg(long)]
        session: Option<String>,
        /// outline | children | read | page | search
        op: String,
        /// read: the record reference
        #[arg(long = "ref", value_name = "REF")]
        reference: Option<String>,
        /// read: a character span `<from>,<to>` instead of the whole record
        #[arg(long, value_parser = parse_span, value_name = "FROM,TO")]
        span: Option<[u64; 2]>,
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

fn parse_span(s: &str) -> Result<[u64; 2], String> {
    let (a, b) = s
        .split_once(',')
        .ok_or_else(|| "a span is <from>,<to>".to_string())?;
    let n = |t: &str| t.trim().parse::<u64>().map_err(|e| e.to_string());
    Ok([n(a)?, n(b)?])
}

pub(crate) fn evidence_cmd(cwd: &Path, action: EvidenceAction) -> ExitCode {
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
    let Some(session) = session.or_else(|| {
        std::env::var(apb_engine::evidence::SESSION_ENV)
            .ok()
            .filter(|s| !s.is_empty())
    }) else {
        eprintln!("evidence: name the session with --session or APB_EVIDENCE_SESSION");
        return ExitCode::from(2);
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
    match run_evidence_read(&step_root(cwd), &session, &args, &StepCaller::from_env()) {
        Ok(v) => match serde_json::to_string_pretty(&v) {
            Ok(s) => {
                println!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("evidence: the answer could not be serialized: {e}");
                ExitCode::from(1)
            }
        },
        Err(e) => {
            eprintln!(
                "evidence: {}",
                sanitize_for_terminal(&e.to_string(), QUESTION_TEXT_MAX * 2)
            );
            ExitCode::from(match e {
                ToolError::NotFound(_) => 2,
                ToolError::Conflict(_) | ToolError::Refused { .. } => 3,
                ToolError::Engine(_) => 1,
            })
        }
    }
}
