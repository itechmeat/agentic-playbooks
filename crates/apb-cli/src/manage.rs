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
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
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

pub(crate) fn detect_cmd(refresh: bool) -> ExitCode {
    match apb_mcp::advisory_tools::agents_detect(refresh) {
        Ok(v) => {
            print_json(&v);
            offer_onboarding_if_tty();
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("detect error: {e}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn adopt_cmd(root: &Path, id: Option<&str>) -> ExitCode {
    match apb_mcp::advisory_tools::playbook_adopt_report(root, id) {
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

pub(crate) fn subscriptions_cmd(set: Vec<String>, decline: bool) -> ExitCode {
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

pub(crate) fn run_init(root: &Path) -> ExitCode {
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
            for e in entries {
                let state = match e.state {
                    apb_core::projects::State::Active => "active",
                    apb_core::projects::State::Unreachable { .. } => "unreachable",
                    apb_core::projects::State::Tombstoned { .. } => "tombstoned",
                };
                println!(
                    "{}  {}  [{}]  {} playbooks  {}",
                    e.workspace_id, e.name, state, e.playbook_count, e.path
                );
            }
            ExitCode::SUCCESS
        }
        ProjectsAction::Remove { workspace_id } => {
            if apb_core::projects::remove(&workspace_id) {
                println!("removed {workspace_id}");
                ExitCode::SUCCESS
            } else {
                eprintln!("no such workspace: {workspace_id}");
                ExitCode::from(2)
            }
        }
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
                    "exported {} @ {} -> {}",
                    bundle.id,
                    bundle.version,
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

pub(crate) fn import_cmd(root: &Path, file: &Path, make_current: bool) -> ExitCode {
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
    match apb_core::bundle::import_bundle(root, &bundle, make_current) {
        Ok(version) => {
            println!("imported {} as version {version}", bundle.id);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("import failed: {e}");
            ExitCode::from(2)
        }
    }
}
