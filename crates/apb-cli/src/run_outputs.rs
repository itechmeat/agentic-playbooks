//! `apb runs <id> --outputs-json` (exactly the run's projected outputs, for
//! a pipeline to read) and the "outputs" section of `apb runs <id>`. Values
//! come from agent output, so the text form goes through the terminal
//! sanitizer and both forms through the run's redactor.

use std::path::Path;
use std::process::ExitCode;

use apb_engine::run_outputs::{OutputState, ProjectedOutput, RunOutputs};

use crate::util::{QUESTION_TEXT_MAX, sanitize_for_terminal};

/// Prints the projection as JSON: `{ final, definition, outputs }`, with an
/// empty `outputs` for a playbook that declares none.
pub(crate) fn outputs_json_cmd(root: &Path, run_id: &str) -> ExitCode {
    if !apb_core::registry::is_safe_segment(run_id) {
        eprintln!("runs: invalid run id `{run_id}`");
        return ExitCode::from(2);
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        eprintln!("runs: run `{run_id}` not found");
        return ExitCode::from(2);
    }
    // A read-only surface: a torn last line (a write in progress, or a
    // writer that died mid-line) is dropped, as every run surface does.
    let events = match apb_engine::event::read_all_lossy_tail(&run_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("runs failed: {e}");
            return ExitCode::from(2);
        }
    };
    let outputs = apb_engine::run_outputs::of_run(&run_dir, &events);
    let mut value = serde_json::to_value(&outputs).unwrap_or_default();
    apb_engine::run_redact::RunRedactor::for_run(root, &run_dir).redact_json(&mut value);
    say!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
    ExitCode::SUCCESS
}

/// The "outputs" lines of `apb runs <id>`, one per declared output.
pub(crate) fn lines(outputs: &RunOutputs) -> Vec<String> {
    let mut out = vec![format!(
        "outputs ({}{}):",
        if outputs.is_final {
            "final"
        } else {
            "not final"
        },
        match outputs.definition {
            apb_engine::run_outputs::OutputsDefinition::Stored => "",
            apb_engine::run_outputs::OutputsDefinition::Current => ", current definition",
        }
    )];
    out.extend(outputs.outputs.iter().map(line));
    out
}

fn line(o: &ProjectedOutput) -> String {
    let what = match o.state {
        OutputState::Missing => format!(
            "missing {}",
            o.reason.map(|r| r.as_str()).unwrap_or_default()
        ),
        OutputState::Null => "null".to_string(),
        OutputState::Value => o
            .value
            .as_ref()
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => crate::util::shown_value(other.clone()).to_string(),
            })
            .unwrap_or_default(),
    };
    let from = o
        .source
        .as_ref()
        .map(|s| format!(" (from {} #{})", s.execution.node, s.execution.ordinal))
        .or_else(|| {
            o.values
                .as_ref()
                .map(|v| format!(" ({} candidates)", v.len()))
        })
        .unwrap_or_default();
    format!(
        "  {}",
        sanitize_for_terminal(&format!("{}: {what}{from}", o.name), QUESTION_TEXT_MAX)
    )
}
