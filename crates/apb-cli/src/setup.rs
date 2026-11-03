//! `apb setup plan|apply|remove`: a thin surface over [`apb_mcp::setup`].
//!
//! Exit codes: `plan` 0 (1 when it cannot be computed); `apply` 0 done or
//! nothing to do, 1 failure, 2 the plan changed since it was read (nothing
//! written), 3 refused (no usable agent, no `--plan` without a terminal,
//! or a bare `--replace-modified` without a terminal); `remove` 0 (1 when the record cannot be read or written).
//! Inside a run step (`APB_RUN_ID` or `APB_HOST_TASK` set) `apply` and
//! `remove` are refused with 3: setup is the person's. `plan` only reads,
//! so a step may run it.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_mcp::setup::{
    self, ActionKind, ApplyOutcome, ApplyStatus, Plan, RemoveOutcome, ReplaceModified,
};
use clap::Subcommand;

use crate::util::print_json;

#[derive(Subcommand)]
pub(crate) enum SetupAction {
    /// What setup would do here, read only: agents, subscriptions, default
    /// playbooks and skills, role profiles, MCP hosts, the sandbox and,
    /// with --project, the project's standing block. Prints the actions and
    /// the plan digest
    Plan {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Also plan this project's standing block (CLAUDE.md, AGENTS.md)
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
        /// Replace changed defaults with the shipped content: every one, or
        /// only the named playbooks and skills. A playbook keeps the changed
        /// version in its history; a skill's changed copy is moved to a
        /// backup. Pass the same flag to apply (by name without a terminal)
        #[arg(long, value_name = "NAME", num_args = 0.., value_delimiter = ',')]
        replace_modified: Option<Vec<String>>,
    },
    /// Run exactly the plan with this digest, idempotently, and record what
    /// was done for `apb setup remove`. Exit codes: 0 done or nothing to
    /// do, 1 failure, 2 the plan changed (nothing written), 3 refused
    Apply {
        /// The digest `apb setup plan` printed; required without a terminal
        #[arg(long, value_name = "DIGEST")]
        plan: Option<String>,
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
        /// The --replace-modified the plan was read with; without a
        /// terminal only with names
        #[arg(long, value_name = "NAME", num_args = 0.., value_delimiter = ',')]
        replace_modified: Option<Vec<String>>,
        #[arg(long)]
        json: bool,
    },
    /// Undo what setup recorded: default playbooks to the trash, unchanged
    /// skills and role profiles removed, the MCP entries and standing block
    /// apb wrote taken back, and a replaced default given back to the
    /// person's copy. Anything changed since is kept and listed
    Remove {
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn setup_cmd(root: &Path, action: SetupAction) -> ExitCode {
    let json = match &action {
        SetupAction::Plan { json, .. }
        | SetupAction::Apply { json, .. }
        | SetupAction::Remove { json } => *json,
    };
    // A run step may read the plan; it never applies or removes.
    let writes = !matches!(action, SetupAction::Plan { .. });
    if let Some(why) = setup::step_refusal().filter(|_| writes) {
        if json {
            print_json(&serde_json::json!({ "refused": setup::RUN_STEP, "message": why }));
        } else {
            eprintln!("{why}");
        }
        return ExitCode::from(ApplyStatus::Refused.exit_code());
    }
    match action {
        SetupAction::Plan {
            json,
            project,
            replace_modified,
        } => plan_cmd(
            root,
            project.as_deref(),
            &ReplaceModified::from_flag(replace_modified),
            json,
        ),
        SetupAction::Apply {
            plan,
            project,
            replace_modified,
            json,
        } => apply_cmd(
            root,
            plan,
            project.as_deref(),
            &ReplaceModified::from_flag(replace_modified),
            json,
        ),
        SetupAction::Remove { json } => remove_cmd(root, json),
    }
}

fn plan_cmd(
    root: &Path,
    project: Option<&Path>,
    replace: &ReplaceModified,
    json: bool,
) -> ExitCode {
    match setup::plan(root, project, replace) {
        Ok(plan) => {
            if json {
                print_json(&serde_json::to_value(&plan).unwrap_or_default());
            } else {
                print_plan(&plan, project, replace);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("setup plan failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// The options of `apply` that reproduce the plan read with `project` and
/// `replace`. A bare `--replace-modified` is spelled out as the names it
/// replaces in `plan`: without a terminal, apply takes only named ones.
fn apply_options(plan: &Plan, project: Option<&Path>, replace: &ReplaceModified) -> String {
    let mut out = String::new();
    if let Some(p) = project {
        out.push_str(&format!(" --project {}", p.display()));
    }
    let names = match replace {
        ReplaceModified::None => Vec::new(),
        ReplaceModified::All => plan
            .actions
            .iter()
            .filter(|a| {
                matches!(
                    a.kind,
                    ActionKind::ReplaceModifiedPlaybook | ActionKind::ReplaceModifiedSkill
                )
            })
            .map(|a| a.target.clone())
            .collect(),
        ReplaceModified::Only(names) => names.clone(),
    };
    if !names.is_empty() {
        out.push_str(&format!(" --replace-modified {}", names.join(",")));
    }
    out
}

/// Whether the bare `--replace-modified` (every changed default) may be
/// applied: only by a person at a terminal. Without one the defaults to
/// replace are named, so an agent never replaces every changed default on
/// its own.
fn replace_all_refused(replace: &ReplaceModified) -> bool {
    matches!(replace, ReplaceModified::All) && !at_terminal()
}

fn at_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn print_plan(plan: &Plan, project: Option<&Path>, replace: &ReplaceModified) {
    let o = &plan.observed;
    let installed: Vec<&str> = o
        .agents
        .iter()
        .filter(|a| a.installed)
        .map(|a| a.agent.as_str())
        .collect();
    println!(
        "agents: {}",
        if installed.is_empty() {
            "none installed".to_string()
        } else {
            installed.join(", ")
        }
    );
    if let Some(ind) = o.role_profiles.independence {
        println!(
            "reviewer: {}",
            match ind {
                apb_core::bundled::Independence::SeparateAgent => "another agent",
                apb_core::bundled::Independence::SameAgent => "the same agent as the main profile",
            }
        );
    }
    let (ok, detail) = (o.sandbox.available, &o.sandbox.detail);
    println!(
        "sandbox: {} ({detail})",
        if ok { "available" } else { "not available" }
    );
    if plan.actions.is_empty() {
        println!("\nnothing to do");
    } else {
        println!("\nactions:");
        for (i, a) in plan.actions.iter().enumerate() {
            println!("  {}. {}", i + 1, a.detail);
        }
    }
    for w in &plan.warnings {
        println!("warning: {}", w.detail);
    }
    for m in &plan.manual {
        println!("by hand: {} ({}); add:\n{}", m.host, m.why, m.snippet);
    }
    for t in &o.trackers {
        let seen = if t.status == setup::TRACKER_AVAILABLE {
            "available on this machine"
        } else {
            "used here"
        };
        println!("tracker {seen}: {} ({})", t.kind, t.evidence);
    }
    if !plan.suggestions.is_empty() {
        println!(
            "\nreview helpers (one installed here is a required review step, the others are optional; setup never installs any):"
        );
        for s in &plan.suggestions {
            let found = if s.found.is_empty() {
                "none found".to_string()
            } else {
                format!("found: {}", s.found.join(", "))
            };
            let need = if s.required { "required" } else { "optional" };
            println!(
                "  - {} ({need}): {} (for example {}; {found})",
                s.role,
                s.why,
                s.examples.join(", ")
            );
        }
    }
    if let Some(r) = &plan.refusal {
        println!("refused: {}", r.message);
    }
    println!("\nleft to the person:");
    for n in &plan.needs_person {
        println!("  - {} ({})", n.detail, n.how);
    }
    println!("\ndigest: {}", plan.digest);
    if plan.ready && !plan.actions.is_empty() {
        println!(
            "apply with: apb setup apply --plan {}{}",
            plan.digest,
            apply_options(plan, project, replace)
        );
    }
}

fn apply_cmd(
    root: &Path,
    digest: Option<String>,
    project: Option<&Path>,
    replace: &ReplaceModified,
    json: bool,
) -> ExitCode {
    if replace_all_refused(replace) {
        let why = "setup apply: --replace-modified without names replaces every changed default and needs a person at a terminal; name the defaults to replace (apb setup plan prints them)";
        if json {
            print_json(&serde_json::json!({ "refused": "replace_needs_names", "message": why }));
        } else {
            eprintln!("{why}");
        }
        return ExitCode::from(ApplyStatus::Refused.exit_code());
    }
    let digest = match digest {
        Some(d) => d,
        None => match ask_at_terminal(root, project, replace) {
            Ok(Some(d)) => d,
            Ok(None) => return ExitCode::SUCCESS,
            Err(code) => return code,
        },
    };
    let out = setup::apply(root, project, replace, &digest);
    if json {
        print_json(&serde_json::to_value(&out).unwrap_or_default());
    } else {
        print_apply(&out);
    }
    ExitCode::from(out.status.exit_code())
}

/// At a terminal without `--plan`: shows the plan and asks once. `Ok(None)`
/// when the person declines or there is nothing to do.
fn ask_at_terminal(
    root: &Path,
    project: Option<&Path>,
    replace: &ReplaceModified,
) -> Result<Option<String>, ExitCode> {
    if !at_terminal() {
        eprintln!(
            "setup apply: --plan <digest> is required without a terminal (run apb setup plan --json first)"
        );
        return Err(ExitCode::from(ApplyStatus::Refused.exit_code()));
    }
    let plan = setup::plan(root, project, replace).map_err(|e| {
        eprintln!("setup plan failed: {e}");
        ExitCode::from(1)
    })?;
    print_plan(&plan, project, replace);
    if plan.actions.is_empty() || !plan.ready {
        return Ok(None);
    }
    match cliclack::confirm("Apply this plan?")
        .initial_value(true)
        .interact()
    {
        Ok(true) => Ok(Some(plan.digest)),
        _ => {
            println!("nothing applied");
            Ok(None)
        }
    }
}

fn print_apply(out: &ApplyOutcome) {
    match out.status {
        ApplyStatus::PlanChanged => {
            eprintln!(
                "setup apply: the plan changed since it was read (now {}); nothing written. Run apb setup plan again",
                out.digest
            );
            return;
        }
        ApplyStatus::Refused => {
            let why = out
                .refusal
                .as_ref()
                .map(|r| r.message.as_str())
                .unwrap_or("");
            eprintln!("setup apply refused: {why}");
            return;
        }
        ApplyStatus::NoOp => {
            println!("nothing to do: this plan is applied");
            return;
        }
        ApplyStatus::Applied | ApplyStatus::Failed => {}
    }
    for d in &out.done {
        println!("done: {}", d.detail);
        if let Some(b) = &d.backup {
            println!("  kept: {b}");
        }
    }
    for n in &out.notes {
        println!("note: {n}");
    }
    for f in &out.failed {
        eprintln!("failed: {} {}: {}", f.kind.as_str(), f.target, f.error);
    }
    if let Some(e) = &out.error {
        eprintln!("setup apply: {e}");
    }
    if let Some(r) = &out.record {
        println!("recorded in {}; undo with apb setup remove", r.display());
    }
}

fn remove_cmd(root: &Path, json: bool) -> ExitCode {
    let out = match setup::remove(root) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("setup remove failed: {e}");
            return ExitCode::from(1);
        }
    };
    if json {
        print_json(&serde_json::to_value(&out).unwrap_or_default());
    } else {
        print_remove(&out);
    }
    ExitCode::SUCCESS
}

fn print_remove(out: &RemoveOutcome) {
    if out.removed.is_empty() && out.restored.is_empty() && out.kept.is_empty() {
        println!("nothing recorded: setup has not written anything here");
        return;
    }
    for r in &out.removed {
        println!("removed: {} {}: {}", r.kind, r.target, r.detail);
    }
    for r in &out.restored {
        println!("restored: {} {}: {}", r.kind, r.target, r.detail);
    }
    for k in &out.kept {
        println!("kept: {} {}: {}", k.kind, k.target, k.reason);
    }
}
