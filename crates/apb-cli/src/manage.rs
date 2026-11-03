use std::path::Path;
use std::process::ExitCode;

use apb_core::fsutil::atomic_write;
use apb_core::registry::init_project;

use clap::Subcommand;

use crate::onboarding::{offer_onboarding_if_tty, parse_subscription, subscriptions_survey_step};
use crate::util::print_json;

#[derive(Subcommand)]
pub(crate) enum ProjectsAction {
    /// List registered workspaces
    List,
    /// Remove a workspace registry entry by its workspace_id
    Remove { workspace_id: String },
    /// Remove every registry entry whose directory is gone (paths that only
    /// fail the reachability check are kept), printing each one
    Prune {
        /// Print what would be removed and change nothing
        #[arg(long)]
        dry_run: bool,
    },
}

pub(crate) fn migrate_cmd(root: &Path, apply: bool) -> ExitCode {
    let plan = match apb_core::schema_migrate::plan(root) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("migrate plan failed: {e}");
            return ExitCode::from(2);
        }
    };
    if plan.is_empty() {
        println!("nothing to migrate (already schema 2)");
        return ExitCode::SUCCESS;
    }
    println!("Migration plan:");
    for p in &plan.new_profiles {
        println!("  + profile `{}` ({}) from {}", p.name, p.scope, p.from);
    }
    for u in &plan.playbook_updates {
        println!(
            "  ~ playbook `{}` {} -> {}",
            u.id, u.from_version, u.new_version
        );
    }
    for d in &plan.diagnostics {
        println!("  note: {d}");
    }
    if !apply {
        println!(
            "\ndry-run: pass --apply to write. SOUL.md files are created empty - fill role text after."
        );
        return ExitCode::SUCCESS;
    }
    let ts = apb_core::clock::now_secs();
    match apb_core::schema_migrate::apply(root, &plan, ts) {
        Ok(()) => {
            println!("\napplied. backup in .apb/backup-{ts}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("migrate apply failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// `apb detect`: the detection JSON (`{ agents, options_by_agent }`); with
/// `json` nothing else is printed, not even the survey hint.
pub(crate) fn detect_cmd(refresh: bool, json: bool) -> ExitCode {
    match apb_mcp::advisory_tools::agents_detect(refresh) {
        Ok(v) => {
            print_json(&v);
            if !json {
                offer_onboarding_if_tty();
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("detect error: {e}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn adopt_cmd(root: &Path, id: Option<&str>, scope: Option<&str>) -> ExitCode {
    match apb_mcp::advisory_tools::playbook_adopt_report(root, id, scope) {
        Ok(v) => {
            print_json(&v);
            offer_onboarding_if_tty();
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("adopt error: {e}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn subscriptions_cmd(set: Vec<String>, decline: bool, json: bool) -> ExitCode {
    use apb_core::models_table::{self, OnboardingState};
    use std::io::IsTerminal;

    if decline {
        return match apb_mcp::advisory_tools::subscriptions_set(Vec::new(), true) {
            Ok(_) => {
                println!("subscriptions survey declined; will not be offered again");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("subscriptions error: {e}");
                ExitCode::from(2)
            }
        };
    }
    if json && set.is_empty() {
        return subscriptions_json();
    }
    if !set.is_empty() {
        let subs: Vec<_> = match set.iter().map(|s| parse_subscription(s)).collect() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("subscriptions error: {e}");
                return ExitCode::from(2);
            }
        };
        return match apb_mcp::advisory_tools::subscriptions_set(subs, false) {
            Ok(v) => {
                print_json(&v);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("subscriptions error: {e}");
                ExitCode::from(2)
            }
        };
    }
    let table = match models_table::load_merged() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("models table error: {e}");
            return ExitCode::from(2);
        }
    };
    if table.subscriptions.is_empty() {
        println!("no subscriptions declared");
    } else {
        println!("declared subscriptions:");
        for s in &table.subscriptions {
            let plan = s.plan.as_deref().unwrap_or("-");
            println!("  {} (plan: {plan}, coverage: {:?})", s.agent, s.coverage);
        }
    }
    // On a terminal without a completed survey, we offer the interactive one.
    // A broken state - we warn and do not offer it (we do not pretend it is
    // Uninitialized).
    // Only enter the blocking interactive survey when BOTH stdin and stdout are
    // terminals: with stdout piped (`apb subscriptions | tool`) we must not
    // stall on a prompt, so we fall through to the plain hint below.
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        match models_table::onboarding::read() {
            Ok(OnboardingState::Uninitialized) => {
                return match subscriptions_survey_step() {
                    Ok(()) => ExitCode::SUCCESS,
                    // Cancellation is a clean exit, like blanking out of the old
                    // survey; a store-write failure exits 2, like `--set`.
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("subscriptions error: {e}");
                        ExitCode::from(2)
                    }
                };
            }
            Ok(_) => {}
            Err(e) => eprintln!("onboarding state unreadable: {e}"),
        }
    }
    println!(
        "\nto declare: playbook subscriptions --set agent[:plan[:coverage]] ... (or --decline)"
    );
    ExitCode::SUCCESS
}

/// `apb subscriptions --json`: `{ subscriptions, onboarding }`, never a
/// survey.
fn subscriptions_json() -> ExitCode {
    use apb_core::models_table::{self, OnboardingState};
    let table = match models_table::load_merged() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("models table error: {e}");
            return ExitCode::from(2);
        }
    };
    let onboarding = match models_table::onboarding::read() {
        Ok(OnboardingState::Uninitialized) => "uninitialized",
        Ok(_) => "done",
        Err(_) => "unreadable",
    };
    print_json(&serde_json::json!({
        "subscriptions": table.subscriptions,
        "onboarding": onboarding,
    }));
    ExitCode::SUCCESS
}

/// The answers `apb init` takes as flags, for a run without a terminal.
pub(crate) struct InitFlags {
    pub standing_block: Option<bool>,
    pub feedback_loop: Option<bool>,
    pub subscriptions: Vec<String>,
    pub json: bool,
}

impl InitFlags {
    /// Any answer given as a flag: then no question is asked.
    fn answered(&self) -> bool {
        self.standing_block.is_some()
            || self.feedback_loop.is_some()
            || !self.subscriptions.is_empty()
    }
}

/// One answered block: the answer and what happened per file.
fn block_answer(
    root: &Path,
    answer: Option<bool>,
    feedback: bool,
) -> Result<serde_json::Value, String> {
    let Some(yes) = answer else {
        return Ok(serde_json::Value::Null);
    };
    if !yes {
        return Ok(serde_json::json!({ "answer": "no", "files": [] }));
    }
    let files: Vec<serde_json::Value> = crate::onboarding::write_block_by_flag(root, feedback)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(file, action)| {
            let action = match action {
                apb_mcp::setup::standing::BlockAction::Created => "created",
                apb_mcp::setup::standing::BlockAction::Appended => "appended",
                apb_mcp::setup::standing::BlockAction::AlreadyConfigured => "already_configured",
            };
            serde_json::json!({ "file": file, "action": action })
        })
        .collect();
    Ok(serde_json::json!({ "answer": "yes", "files": files }))
}

/// The flag answers of `apb init`, applied in the questionnaire's order.
fn apply_init_flags(root: &Path, flags: &InitFlags) -> Result<serde_json::Value, String> {
    let standing = block_answer(root, flags.standing_block, false)?;
    let feedback = block_answer(root, flags.feedback_loop, true)?;
    let subscriptions = if flags.subscriptions.is_empty() {
        serde_json::Value::Null
    } else {
        let subs = flags
            .subscriptions
            .iter()
            .map(|s| parse_subscription(s))
            .collect::<Result<Vec<_>, _>>()?;
        apb_mcp::advisory_tools::subscriptions_set(subs, false).map_err(|e| e.to_string())?
    };
    Ok(serde_json::json!({
        "initialized": root.join(".apb"),
        "standing_block": standing,
        "feedback_loop": feedback,
        "subscriptions": subscriptions,
    }))
}

/// The exit code of an `apb init` a person's decision is missing from.
const INIT_REFUSED: u8 = 3;

/// `--feedback-loop yes` is a person's answer: refused inside a run step
/// and without a terminal, and asked once more at a terminal. `Ok(false)`
/// when the person declines there.
fn feedback_consent() -> Result<bool, String> {
    if let Some(from) = apb_core::step_env::StepCaller::from_env().describe() {
        return Err(format!(
            "--feedback-loop yes is refused {from}: the feedback-loop block lets agents file issues about apb, and only the person turns it on, at a terminal"
        ));
    }
    if !crate::launch::at_terminal() {
        return Err(
            "--feedback-loop yes needs the person at a terminal: the feedback-loop block lets agents file issues about apb, so the person runs apb init there and answers the question (--feedback-loop no works anywhere)"
                .to_string(),
        );
    }
    crate::onboarding::confirm_feedback().map_err(|e| e.to_string())
}

pub(crate) fn run_init(root: &Path, flags: &InitFlags) -> ExitCode {
    let declined;
    let flags = match flags.feedback_loop {
        Some(true) => match feedback_consent() {
            Ok(true) => flags,
            Ok(false) => {
                declined = InitFlags {
                    feedback_loop: Some(false),
                    subscriptions: flags.subscriptions.clone(),
                    ..*flags
                };
                &declined
            }
            Err(why) => {
                if flags.json {
                    print_json(&serde_json::json!({
                        "refused": "feedback_loop_needs_person",
                        "message": why,
                    }));
                } else {
                    eprintln!("init refused: {why}");
                }
                return ExitCode::from(INIT_REFUSED);
            }
        },
        _ => flags,
    };
    if flags.answered() || flags.json {
        if let Err(e) = init_project(root) {
            eprintln!("init failed: {e}");
            return ExitCode::from(2);
        }
        return match apply_init_flags(root, flags) {
            Ok(v) if flags.json => {
                print_json(&v);
                ExitCode::SUCCESS
            }
            Ok(_) => {
                println!("initialized {}", root.join(".apb").display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("init failed: {e}");
                ExitCode::from(2)
            }
        };
    }
    match init_project(root) {
        Ok(()) => {
            println!("initialized {}", root.join(".apb").display());
            // Init dispatch: on success (and only then) offer the interactive
            // questionnaire. It self-gates on a real terminal and never turns a
            // successful init into a failure.
            crate::onboarding::run_init_questionnaire(root);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("init failed: {e}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn projects_cmd(action: Option<ProjectsAction>) -> ExitCode {
    match action.unwrap_or(ProjectsAction::List) {
        ProjectsAction::List => {
            let entries = apb_core::projects::list_active();
            if entries.is_empty() {
                println!("no registered workspaces");
                return ExitCode::SUCCESS;
            }
            // A git linked worktree is listed under its parent project.
            for (depth, e) in apb_core::projects::nest_worktrees(entries) {
                let indent = if depth > 0 { "  " } else { "" };
                println!(
                    "{indent}{}  {}  [{}]  {} playbooks  {}",
                    e.workspace_id,
                    e.name,
                    state_word(&e.state),
                    e.playbook_count,
                    e.path
                );
            }
            ExitCode::SUCCESS
        }
        ProjectsAction::Prune { dry_run } => match apb_core::projects::prune(dry_run) {
            Ok(removed) => {
                let verb = if dry_run { "would remove" } else { "removed" };
                for e in &removed {
                    println!(
                        "{verb} {}  {}  [{}]  {}",
                        e.workspace_id,
                        e.name,
                        state_word(&e.state),
                        e.path
                    );
                }
                if removed.is_empty() {
                    println!("nothing to prune");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("projects: {e}");
                ExitCode::from(1)
            }
        },
        ProjectsAction::Remove { workspace_id } => {
            match apb_core::projects::remove(&workspace_id) {
                Ok(true) => {
                    println!("removed {workspace_id}");
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    eprintln!("no such workspace: {workspace_id}");
                    ExitCode::from(2)
                }
                Err(e) => {
                    eprintln!("projects: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

fn state_word(state: &apb_core::projects::State) -> &'static str {
    match state {
        apb_core::projects::State::Active => "active",
        apb_core::projects::State::Unreachable { .. } => "unreachable",
        apb_core::projects::State::Tombstoned { .. } => "tombstoned",
    }
}

pub(crate) fn export_cmd(
    root: &Path,
    name: &str,
    version: Option<&str>,
    out: Option<&Path>,
) -> ExitCode {
    let bundle = match apb_core::bundle::export_bundle(root, name, version) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("export failed: {e}");
            return ExitCode::from(2);
        }
    };
    let json = match bundle.to_json() {
        Ok(j) => j,
        Err(e) => {
            eprintln!("export failed: {e}");
            return ExitCode::from(2);
        }
    };
    match out {
        Some(path) => match atomic_write(path, json.as_bytes()) {
            Ok(()) => {
                println!(
                    "exported {} @ {} ({} script file(s)) -> {}",
                    bundle.id,
                    bundle.version,
                    bundle.scripts.len(),
                    path.display()
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("export write failed: {e}");
                ExitCode::from(2)
            }
        },
        None => {
            println!("{json}");
            ExitCode::SUCCESS
        }
    }
}

/// How `apb import` was asked to proceed.
pub(crate) struct ImportFlags<'a> {
    pub make_current: bool,
    /// `--preview`: print the preview and stop.
    pub preview_only: bool,
    /// `--accept <bundle_digest>`: accept this exact file.
    pub accept: Option<&'a str>,
}

pub(crate) fn import_cmd(root: &Path, file: &Path, flags: &ImportFlags<'_>) -> ExitCode {
    let raw = match std::fs::read_to_string(file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("import read failed: {e}");
            return ExitCode::from(2);
        }
    };
    let bundle = match apb_core::bundle::PlaybookBundle::from_json(&raw) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("import parse failed: {e}");
            return ExitCode::from(2);
        }
    };
    let preview = match apb_core::bundle::preview(root, &bundle, raw.as_bytes()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("import refused: {e}");
            return ExitCode::from(2);
        }
    };
    crate::import_preview::print_preview(&preview);
    if flags.preview_only {
        return ExitCode::SUCCESS;
    }
    if preview.unchanged {
        println!("nothing to import");
        return ExitCode::SUCCESS;
    }
    // Nobody is asked to accept an import that would be refused anyway.
    if !preview.errors.is_empty() || preview.version_taken {
        eprintln!("import refused: see the preview above; nothing imported");
        return ExitCode::from(2);
    }
    match flags.accept {
        Some(d) if d == preview.bundle_digest => {}
        Some(_) => {
            eprintln!(
                "import refused: --accept does not name this file's bundle digest; nothing imported"
            );
            return ExitCode::from(2);
        }
        None => {
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() {
                eprintln!(
                    "nothing imported: review the preview, then accept this exact file with --accept {}",
                    preview.bundle_digest
                );
                return ExitCode::from(3);
            }
            if !crate::import_preview::confirm("import this bundle? [y/N] ") {
                println!("nothing imported");
                return ExitCode::SUCCESS;
            }
        }
    }
    let source = apb_core::bundle::ImportSource {
        raw: raw.as_bytes(),
        file_name: file.file_name().and_then(|n| n.to_str()),
    };
    match apb_core::bundle::import_bundle(root, &bundle, flags.make_current, Some(source)) {
        Ok(saved) => {
            // The id and version come from the bundle file: never print a
            // control character from it raw.
            let id = crate::util::sanitize_for_terminal(&bundle.id, 128);
            let version = crate::util::sanitize_for_terminal(&saved.version, 64);
            if saved.unchanged {
                println!("{id} @ {version} already holds this bundle; nothing imported");
                return ExitCode::SUCCESS;
            }
            println!(
                "imported {id} as version {version} ({} script file(s))",
                bundle.scripts.len()
            );
            crate::import_preview::print_approval_hint(root, &bundle.id, &saved.version);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("import failed: {e}");
            ExitCode::from(2)
        }
    }
}
