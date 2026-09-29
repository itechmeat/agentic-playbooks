//! `apb tasks`: the host tasks of host-execution-mode runs (0.23.0), for a
//! scripted or manual hand-off. `apb tasks [run]` lists what waits for a
//! host; `apb tasks submit` hands a reply back.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_engine::host_task::{self, PendingHostTask, SubmitRequest, SubmitStatus, SubmittedUsage};
use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum TasksAction {
    /// Submit the reply to one host task
    Submit {
        run_id: String,
        task_id: String,
        /// succeeded, failed, or blocked (the output is then the question
        /// for the person)
        #[arg(long)]
        status: String,
        /// File holding the subagent's final reply, verbatim (`-` reads
        /// stdin)
        #[arg(long = "output-file", value_name = "FILE")]
        output_file: PathBuf,
        /// A short note for the journal
        #[arg(long)]
        note: Option<String>,
        /// Input tokens the reply consumed, when known
        #[arg(long)]
        input_tokens: Option<u64>,
        /// Output tokens the reply consumed, when known
        #[arg(long)]
        output_tokens: Option<u64>,
    },
}

/// Every pending task of `run_id`, or of every run when `None` (a child
/// run's task listed under its parent is shown once).
fn collect(root: &Path, run_id: Option<&str>) -> Result<Vec<PendingHostTask>, String> {
    if let Some(id) = run_id {
        return host_task::pending_for_run(root, id).map_err(|e| e.to_string());
    }
    let runs = root.join(".apb/runs");
    let mut ids: Vec<String> = match std::fs::read_dir(&runs) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    ids.sort();
    let mut out: Vec<PendingHostTask> = Vec::new();
    for id in ids {
        if let Ok(tasks) = host_task::pending_for_run(root, &id) {
            for t in tasks {
                if !out
                    .iter()
                    .any(|o| o.run_id == t.run_id && o.task_id == t.task_id)
                {
                    out.push(t);
                }
            }
        }
    }
    Ok(out)
}

fn first_line(text: &str) -> String {
    one_line(text.lines().find(|l| !l.trim().is_empty()).unwrap_or(""))
}

/// Text for the terminal, whole: newlines and tabs stay, every other control
/// character (escape sequences start with one) and the bidirectional
/// overrides are dropped. A prompt embeds upstream agent output, so it must
/// not drive the terminal. `--json` prints the raw text.
fn for_terminal(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            c == '\n'
                || c == '\t'
                || !(c.is_control()
                    || ('\u{202a}'..='\u{202e}').contains(&c)
                    || ('\u{2066}'..='\u{2069}').contains(&c))
        })
        .collect()
}

/// [`for_terminal`] on one line: newlines and tabs become spaces.
fn one_line(text: &str) -> String {
    for_terminal(text).replace(['\n', '\t'], " ")
}

pub(crate) fn tasks_cmd(
    root: &Path,
    action: Option<TasksAction>,
    run_id: Option<String>,
    full: bool,
    json: bool,
) -> ExitCode {
    match action {
        Some(TasksAction::Submit {
            run_id,
            task_id,
            status,
            output_file,
            note,
            input_tokens,
            output_tokens,
        }) => submit(
            root,
            &run_id,
            &task_id,
            &status,
            &output_file,
            note,
            input_tokens,
            output_tokens,
        ),
        None => list(root, run_id.as_deref(), full, json),
    }
}

fn list(root: &Path, run_id: Option<&str>, full: bool, json: bool) -> ExitCode {
    let tasks = match collect(root, run_id) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("tasks failed: {e}");
            return ExitCode::from(2);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&tasks).unwrap_or_else(|_| "[]".into())
        );
        return ExitCode::SUCCESS;
    }
    if tasks.is_empty() {
        println!("no pending host tasks");
        return ExitCode::SUCCESS;
    }
    for t in &tasks {
        println!(
            "{}  {}  node {}  attempt {}{}",
            one_line(&t.run_id),
            one_line(&t.task_id),
            one_line(&t.node),
            t.attempt,
            t.model_hint
                .as_deref()
                .map(|m| format!("  model hint {}", one_line(m)))
                .unwrap_or_default()
        );
        println!("  workdir: {}", one_line(&t.workdir));
        if let Some(d) = t.deadline {
            let left = (d as i128 - apb_core::clock::now_ms() as i128) / 1000;
            println!("  deadline: in {}s", left.max(0));
        }
        for s in &t.skills {
            println!("  skill: {}", one_line(s));
        }
        if full {
            if let Some(role) = &t.role_prompt {
                println!("  role prompt:\n{}\n", for_terminal(role));
            }
            println!("  prompt:\n{}\n", for_terminal(&t.prompt));
        } else {
            println!("  prompt: {}", first_line(&t.prompt));
        }
        println!(
            "  submit: apb tasks submit {} {} --status succeeded --output-file <reply>",
            one_line(&t.run_id),
            one_line(&t.task_id)
        );
    }
    ExitCode::SUCCESS
}

#[allow(clippy::too_many_arguments)]
fn submit(
    root: &Path,
    run_id: &str,
    task_id: &str,
    status: &str,
    output_file: &Path,
    note: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> ExitCode {
    let Some(status) = SubmitStatus::parse(status) else {
        eprintln!("submit failed: --status must be succeeded, failed or blocked");
        return ExitCode::from(2);
    };
    let output = if output_file == Path::new("-") {
        let mut s = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut s) {
            eprintln!("submit failed: cannot read stdin: {e}");
            return ExitCode::from(2);
        }
        s
    } else {
        match std::fs::read_to_string(output_file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "submit failed: cannot read `{}`: {e}",
                    output_file.display()
                );
                return ExitCode::from(2);
            }
        }
    };
    let usage = (input_tokens.is_some() || output_tokens.is_some()).then(|| SubmittedUsage {
        input_tokens: input_tokens.unwrap_or(0),
        output_tokens: output_tokens.unwrap_or(0),
        ..Default::default()
    });
    match host_task::submit_to_run(
        root,
        run_id,
        SubmitRequest {
            task_id: task_id.to_string(),
            status,
            output,
            usage,
            note,
            submitted_by: "cli".to_string(),
            client: None,
        },
    ) {
        Ok(r) => {
            println!(
                "submitted {} (node {}): {}",
                one_line(&r.task_id),
                one_line(&r.node),
                r.status.as_str()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("submit failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// Prints each host task of `run_id` once as it appears, until `stop` is
/// set: a foreground `apb run --execution host` blocks on its first agent
/// step, and this is how its caller learns where the task waits.
pub(crate) fn announce_tasks(
    root: PathBuf,
    run_id: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let mut seen: Vec<String> = Vec::new();
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        if let Ok(tasks) = host_task::pending_for_run(&root, &run_id) {
            for t in tasks {
                if !seen.contains(&t.task_id) {
                    eprintln!(
                        "host task {} (node {}) waits: `apb tasks {}` shows it, `apb tasks submit {} {} --status succeeded --output-file <reply>` answers it",
                        one_line(&t.task_id),
                        one_line(&t.node),
                        one_line(&run_id),
                        one_line(&t.run_id),
                        one_line(&t.task_id)
                    );
                    seen.push(t.task_id);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_text_keeps_lines_and_drops_escapes() {
        let raw = "line one\x1b]0;pwn\x07\n\tline \x1b[31mtwo\u{202e}\r";
        let clean = for_terminal(raw);
        assert_eq!(clean, "line one]0;pwn\n\tline [31mtwo");
        assert_eq!(one_line(raw), "line one]0;pwn  line [31mtwo");
    }
}
