//! `apb judge`: the judge sandbox and a judge node's labelled examples. A
//! thin dispatch over `apb_mcp::tools::judge`, the layer MCP and the
//! dashboard use, so `--json` prints the same objects:
//!
//! - `apb judge eval <id> <node> [--inputs FILE] [--answers FILE]`: one
//!   evaluation outside any run (without `--answers` it asks this machine's
//!   decision providers, inside the daily cap);
//! - `apb judge examples <id> [--node N] [--check] [--live]`;
//! - `apb judge save-example <run> <node> --expect q=v ...`;
//! - `apb judge values <run> <node>`: "fill from a run".
//!
//! A live ask, a live check and a save are refused inside a run step.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_engine::event::DecisionAnswer;
use apb_engine::judge_eval::JudgeInputs;
use apb_engine::judge_eval::JudgeOutcome;
use apb_engine::judge_eval::example::{ExampleSaveRequest, ExpectedAnswer};
use apb_engine::judge_eval::sandbox::{SandboxEvaluation, SandboxRequest};
use apb_mcp::tools::ToolError;
use apb_mcp::tools::judge::{
    JudgeCaller, judge_evaluate, judge_example_save, judge_examples, judge_run_values,
};
use clap::{Args, Subcommand};

use crate::util::{QUESTION_TEXT_MAX, StepCaller, print_json, sanitize_for_terminal};

/// Who labelled an example saved here.
const LABELLED_BY: &str = "cli";

/// The largest inputs or answers file read.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Args)]
pub(crate) struct JudgeArgs {
    /// Machine-readable output (the same objects as MCP)
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    action: JudgeAction,
}

#[derive(Subcommand)]
enum JudgeAction {
    /// Evaluate one judge node outside any run: the raw answers, what the
    /// node outputs on this machine now and at enforce, with the route of
    /// each. Without --answers it asks this machine's decision providers
    Eval {
        /// The project playbook
        playbook: String,
        /// The judge node
        node: String,
        /// A YAML or JSON file of inputs: params, instruction, outputs,
        /// reviews, rejected_outputs, hooks, context
        #[arg(long, value_name = "FILE")]
        inputs: Option<PathBuf>,
        /// The version; current when omitted
        #[arg(long)]
        version: Option<String>,
        /// A YAML or JSON file of answers per question (value, p,
        /// confidence): evaluated without asking anyone
        #[arg(long, value_name = "FILE")]
        answers: Option<PathBuf>,
        /// When the node would hand over to its emulation, also ask the
        /// configured llm_emulation providers (its profile is never started)
        #[arg(long)]
        emulate: bool,
    },
    /// List a playbook's labelled examples; --check says per example
    /// whether it is stale or how its labels agree now
    Examples {
        /// The project playbook
        playbook: String,
        /// One judge node; every node with examples when omitted
        #[arg(long)]
        node: Option<String>,
        /// Check each example against the current version
        #[arg(long)]
        check: bool,
        /// Check by asking the decision providers again (inside the daily
        /// cap); implies --check
        #[arg(long)]
        live: bool,
    },
    /// Save a judge execution of a run as a labelled example
    SaveExample {
        /// The run
        run: String,
        /// The judge node
        node: String,
        /// The judge node's execution, 1-based; the latest when omitted
        #[arg(long)]
        execution: Option<u32>,
        /// An expected answer, `question=value` (true or false for a yes or
        /// no question, else the option, band or level); repeatable
        #[arg(long = "expect", value_name = "QUESTION=VALUE", required = true)]
        expect: Vec<String>,
        /// A note kept with the example
        #[arg(long)]
        note: Option<String>,
    },
    /// The inputs a judge execution of a run had, and the run's other
    /// recorded values, redacted: what `eval --inputs` can start from
    Values {
        /// The run
        run: String,
        /// The judge node
        node: String,
        /// The judge node's execution, 1-based; the latest when omitted
        #[arg(long)]
        execution: Option<u32>,
    },
}

pub(crate) fn judge_cmd(root: &Path, args: JudgeArgs) -> ExitCode {
    let json = args.json;
    let step = StepCaller::from_env();
    let caller = JudgeCaller::of(&step);
    match args.action {
        JudgeAction::Eval {
            playbook,
            node,
            inputs,
            version,
            answers,
            emulate,
        } => {
            let req = match request(inputs.as_deref(), answers.as_deref(), version, emulate) {
                Ok(r) => r,
                Err(e) => return refused("judge eval", &e),
            };
            match judge_evaluate(root, &playbook, &node, &req, caller) {
                Ok(e) if json => print(serde_json::json!(e)),
                Ok(e) => {
                    print_evaluation(&e);
                    ExitCode::SUCCESS
                }
                Err(e) => failed("judge eval", e),
            }
        }
        JudgeAction::Examples {
            playbook,
            node,
            check,
            live,
        } => match judge_examples(root, &playbook, node.as_deref(), check, live, caller) {
            Ok(v) if json => print(serde_json::json!(v)),
            Ok(v) => {
                for n in &v.nodes {
                    println!("{}: {} examples", n.node, n.examples.len());
                    for ex in &n.examples {
                        println!("  {}  {}", ex.id, text(&labels(&ex.expected)));
                    }
                }
                for c in v.checks.iter().flatten() {
                    println!(
                        "{}: {} agree, {} disagree{}",
                        c.node,
                        c.agree,
                        c.disagree,
                        match c.requests {
                            0 => String::new(),
                            n => format!(", {n} requests"),
                        }
                    );
                    for r in &c.examples {
                        let status = serde_json::to_value(r.status).unwrap_or_default();
                        println!("  {}  {}", r.id, status.as_str().unwrap_or_default());
                    }
                }
                ExitCode::SUCCESS
            }
            Err(e) => failed("judge examples", e),
        },
        JudgeAction::SaveExample {
            run,
            node,
            execution,
            expect,
            note,
        } => {
            let expected = match expected(&expect) {
                Ok(e) => e,
                Err(e) => return refused("judge save-example", &e),
            };
            let req = ExampleSaveRequest {
                execution,
                expected,
                note,
            };
            match judge_example_save(root, &run, &node, &req, LABELLED_BY, caller) {
                Ok(ex) if json => print(serde_json::json!(ex)),
                Ok(ex) => {
                    println!(
                        "saved {} of `{}` node `{}` ({})",
                        ex.id,
                        ex.playbook,
                        ex.node,
                        text(&labels(&ex.expected))
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => failed("judge save-example", e),
            }
        }
        JudgeAction::Values {
            run,
            node,
            execution,
        } => match judge_run_values(root, &run, &node, execution) {
            Ok(v) if json => print(serde_json::json!(v)),
            Ok(v) => {
                println!("{} `{}` execution {}", v.run_id, v.node, v.execution);
                for r in &v.recorded {
                    println!("  {}: {}", r.label, text(&r.text));
                }
                ExitCode::SUCCESS
            }
            Err(e) => failed("judge values", e),
        },
    }
}

/// The sandbox request of `apb judge eval`'s files and flags.
fn request(
    inputs: Option<&Path>,
    answers: Option<&Path>,
    version: Option<String>,
    emulate: bool,
) -> Result<SandboxRequest, String> {
    let inputs: JudgeInputs = match inputs {
        Some(p) => serde_json::from_value(read_yaml(p)?).map_err(|e| format!("inputs: {e}"))?,
        None => JudgeInputs::default(),
    };
    let answers: Option<BTreeMap<String, DecisionAnswer>> = match answers {
        Some(p) => {
            Some(serde_json::from_value(read_yaml(p)?).map_err(|e| format!("answers: {e}"))?)
        }
        None => None,
    };
    Ok(SandboxRequest {
        version,
        inputs,
        answers,
        emulate,
    })
}

/// A YAML (or JSON) file of at most [`MAX_FILE_BYTES`], read strictly.
fn read_yaml(path: &Path) -> Result<serde_json::Value, String> {
    let shown = path.display();
    let len = std::fs::metadata(path)
        .map_err(|e| format!("{shown}: {e}"))?
        .len();
    if len > MAX_FILE_BYTES {
        return Err(format!("{shown}: over {MAX_FILE_BYTES} bytes"));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{shown}: {e}"))?;
    yaml_serde::from_str(&text).map_err(|e| format!("{shown}: {e}"))
}

/// `question=value` pairs as expected answers: `true` and `false` are a
/// yes or no, anything else a name.
fn expected(pairs: &[String]) -> Result<BTreeMap<String, ExpectedAnswer>, String> {
    pairs
        .iter()
        .map(|p| {
            let (q, v) = p
                .split_once('=')
                .filter(|(q, v)| !q.is_empty() && !v.is_empty())
                .ok_or_else(|| format!("--expect `{p}`: use question=value"))?;
            let answer = match v {
                "true" => ExpectedAnswer::Yes(true),
                "false" => ExpectedAnswer::Yes(false),
                name => ExpectedAnswer::Value(name.to_string()),
            };
            Ok((q.to_string(), answer))
        })
        .collect()
}

fn labels(expected: &BTreeMap<String, ExpectedAnswer>) -> String {
    expected
        .iter()
        .map(|(q, a)| match a {
            ExpectedAnswer::Yes(b) => format!("{q}={b}"),
            ExpectedAnswer::Value(v) => format!("{q}={v}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn print_evaluation(e: &SandboxEvaluation) {
    let ev = &e.evaluation;
    let mode = ev
        .mode
        .map_or_else(|| "not configured".to_string(), |m| m.as_str().to_string());
    println!(
        "{} {} `{}` (judge_node: {mode})",
        e.playbook, e.version, e.node
    );
    for (q, a) in &ev.raw {
        let value = a
            .value
            .as_ref()
            .map_or_else(|| "-".to_string(), |v| v.to_string());
        match &a.invalid {
            Some(why) => println!("  {q}: invalid ({})", text(why)),
            None => println!(
                "  {q}: {} p={} confidence={}",
                text(&value),
                a.p.map_or_else(|| "-".to_string(), |p| format!("{p:.2}")),
                a.confidence
                    .map_or_else(|| "-".to_string(), |c| format!("{c:.2}"))
            ),
        }
    }
    println!(
        "  at enforce: {}{}",
        outcome(&ev.at_enforce),
        route(&e.route_at_enforce)
    );
    println!(
        "  on this machine: {}{}",
        outcome(&ev.as_configured),
        route(&e.route_as_configured)
    );
    println!(
        "  requests: {} (today {} of {}){}",
        ev.requests,
        e.requests_today,
        e.daily_cap,
        ev.cost_usd
            .map_or_else(String::new, |c| format!(", ${c:.4}"))
    );
    println!("  {}", e.route_note);
}

fn outcome(o: &JudgeOutcome) -> String {
    match o {
        JudgeOutcome::Output { output } => format!("output {}", text(&compact(output))),
        JudgeOutcome::Fallback { reason, output } => {
            format!("fallback ({}) {}", text(reason), text(&compact(output)))
        }
        JudgeOutcome::Fail { reason } => format!("fail: {}", text(reason)),
        JudgeOutcome::Emulate { reason } => format!("emulate: {}", text(reason)),
    }
}

fn compact(output: &serde_json::Map<String, serde_json::Value>) -> String {
    serde_json::to_string(output).unwrap_or_default()
}

fn route(edges: &[String]) -> String {
    match edges {
        [] => String::new(),
        e => format!(" -> {}", e.join(", ")),
    }
}

fn text(s: &str) -> String {
    sanitize_for_terminal(s, QUESTION_TEXT_MAX)
}

fn print(v: serde_json::Value) -> ExitCode {
    print_json(&v);
    ExitCode::SUCCESS
}

fn refused(what: &str, detail: &str) -> ExitCode {
    eprintln!("{what}: {}", sanitize_for_terminal(detail, 600));
    ExitCode::from(2)
}

fn failed(what: &str, e: ToolError) -> ExitCode {
    eprintln!(
        "{what} failed: {}",
        sanitize_for_terminal(&e.to_string(), 600)
    );
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--expect` reads a yes or no as a boolean and anything else as a
    /// name; a pair without both halves is refused.
    #[test]
    fn an_expected_answer_reads_as_a_boolean_or_a_name() {
        let got = expected(&["risky=false".into(), "verdict=needs_fix".into()]).unwrap();
        assert_eq!(got["risky"], ExpectedAnswer::Yes(false));
        assert_eq!(got["verdict"], ExpectedAnswer::Value("needs_fix".into()));
        for bad in ["verdict", "=x", "verdict="] {
            assert!(expected(&[bad.to_string()]).is_err(), "{bad}");
        }
    }
}
