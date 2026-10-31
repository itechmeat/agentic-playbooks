//! `apb intent`: the send intents of a run. `list` shows what the send
//! ledger knows about each send; `resolve` records a person's decision on a
//! send whose outcome nobody can tell, so a repeat of it is answered or sent
//! again.

use std::path::Path;
use std::process::ExitCode;

use apb_mcp::tools::{IntentCaller, ToolError, run_intent_resolve, run_intents};
use clap::Subcommand;

use crate::util::{QUESTION_TEXT_MAX, sanitize_for_terminal};

/// Who a send intent settled through `apb intent resolve` names.
const RESOLVED_BY: &str = "a person, through apb intent resolve";

#[derive(Subcommand)]
pub(crate) enum IntentAction {
    /// List the send intents of a run and what became of each
    List {
        run_id: String,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Record your decision on a send whose outcome is not known: it went
    /// out (--sent) or it did not (--not-sent). Check the service first
    #[command(group(clap::ArgGroup::new("decision").required(true).args(["sent", "not_sent"])))]
    Resolve {
        run_id: String,
        /// The intent id (`si-...`), as `apb intent list` and the run
        /// outcome name it
        intent: String,
        /// The send went out: a repeat answers already_sent
        #[arg(long)]
        sent: bool,
        /// The send did not go out: a repeat sends it again
        #[arg(long)]
        not_sent: bool,
        /// What you checked, kept with the decision
        #[arg(long)]
        note: Option<String>,
    },
}

pub(crate) fn intent_cmd(root: &Path, action: IntentAction) -> ExitCode {
    match action {
        IntentAction::List { run_id, json } => list(root, &run_id, json),
        IntentAction::Resolve {
            run_id,
            intent,
            sent,
            note,
            ..
        } => resolve(root, &run_id, &intent, sent, note),
    }
}

fn list(root: &Path, run_id: &str, json: bool) -> ExitCode {
    let intents = match run_intents(root, run_id) {
        Ok(i) => i,
        Err(e) => return fail("intent list", &e),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&intents).unwrap_or_else(|_| "[]".into())
        );
        return ExitCode::SUCCESS;
    }
    if intents.is_empty() {
        println!("run `{run_id}` recorded no send intents");
    }
    for i in &intents {
        println!(
            "{}",
            sanitize_for_terminal(&i.line(), QUESTION_TEXT_MAX * 2)
        );
    }
    let open = intents.iter().filter(|i| i.is_open_unknown()).count();
    if open > 0 {
        println!(
            "{open} {} not known; check the service, then: apb intent resolve {run_id} <intent> --sent|--not-sent",
            if open == 1 { "send is" } else { "sends are" }
        );
    }
    ExitCode::SUCCESS
}

fn resolve(root: &Path, run_id: &str, intent: &str, sent: bool, note: Option<String>) -> ExitCode {
    // The CLI runs in its caller's process, so its environment tells an
    // agent step or a host task's subagent apart from a person: inside a
    // run the decision is refused.
    let step = crate::util::StepCaller::from_env();
    let caller = IntentCaller {
        by: RESOLVED_BY,
        run_id: step.run_id.as_deref(),
        host_task: step.host_task.as_deref(),
    };
    match run_intent_resolve(root, run_id, intent, sent, note, caller) {
        Ok(_) => {
            if sent {
                println!("intent {intent} settled as sent: a repeat answers already_sent");
            } else {
                println!("intent {intent} settled as not sent: a repeat sends it again");
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail("intent resolve", &e),
    }
}

fn fail(what: &str, e: &ToolError) -> ExitCode {
    eprintln!(
        "{what}: {}",
        sanitize_for_terminal(&e.to_string(), QUESTION_TEXT_MAX * 2)
    );
    ExitCode::from(match e {
        ToolError::NotFound(_) => 2,
        _ => 1,
    })
}
