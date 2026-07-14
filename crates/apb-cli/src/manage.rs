use std::path::Path;
use std::process::ExitCode;

use apb_core::fsutil::atomic_write;
use apb_core::registry::init_project;

use clap::Subcommand;

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

/// Parses `agent[:plan[:coverage]]` into a subscription. An empty agent, an
/// unknown coverage, or an extra separator (beyond the three fields) is an
/// error, not a silent default: otherwise a typo would silently record a
/// garbage subscription.
pub(crate) fn parse_subscription(s: &str) -> Result<apb_core::models_table::Subscription, String> {
    use apb_core::models_table::{Coverage, Subscription};
    let mut parts = s.splitn(3, ':');
    let agent = parts.next().unwrap_or("").trim().to_string();
    if agent.is_empty() {
        return Err(format!("empty agent in `{s}`"));
    }
    let plan = parts
        .next()
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string());
    let coverage = match parts.next() {
        None => Coverage::Unknown,
        Some("full") => Coverage::Full,
        Some("partial") => Coverage::Partial,
        Some("unknown") => Coverage::Unknown,
        // Catches both an invalid value and an extra separator (`a:b:full:x` -> `full:x`).
        Some(other) => {
            return Err(format!(
                "invalid coverage `{other}` in `{s}` (use full|partial|unknown)"
            ));
        }
    };
    Ok(Subscription {
        agent,
        plan,
        coverage,
    })
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
    if std::io::stdin().is_terminal() {
        match models_table::onboarding::read() {
            Ok(OnboardingState::Uninitialized) => return interactive_survey(),
            Ok(_) => {}
            Err(e) => eprintln!("onboarding state unreadable: {e}"),
        }
    }
    println!(
        "\nto declare: playbook subscriptions --set agent[:plan[:coverage]] ... (or --decline)"
    );
    ExitCode::SUCCESS
}

/// Interactive subscriptions survey (terminal only). Pre-filled from detection;
/// reads lines of `agent[:plan[:coverage]]`, an empty line finishes it.
pub(crate) fn interactive_survey() -> ExitCode {
    use std::io::Write as _;
    println!("\nDetected agents:");
    // Pre-fill: for installed agents with a detected auth hint, we suggest a
    // ready-made subscription line (agent, coverage unknown) - it can be
    // entered as-is or refined with a plan.
    let mut prefill: Vec<String> = Vec::new();
    if let Ok(v) = apb_mcp::advisory_tools::agents_detect(false)
        && let Some(agents) = v["agents"].as_array()
    {
        for a in agents {
            let installed = a["installed"].as_bool().unwrap_or(false);
            let name = a["agent"].as_str().unwrap_or("?");
            let auth = a["auth"]["kind"].as_str();
            let auth_note = match auth {
                Some("oauth") => " [auth: oauth]",
                Some("api_key") => " [auth: api-key]",
                _ => "",
            };
            println!(
                "  {name} {}{auth_note}",
                if installed { "(installed)" } else { "" }
            );
            if installed && matches!(auth, Some("oauth") | Some("api_key")) {
                prefill.push(name.to_string());
            }
        }
    }
    if !prefill.is_empty() {
        println!("\nSuggested (from detected auth): {}", prefill.join(", "));
        println!("Enter each as `{{agent}}` or `{{agent}}:{{plan}}:{{coverage}}` to refine.");
    }
    println!(
        "\nEnter subscriptions as agent[:plan[:coverage]], one per line. Blank line finishes. Ctrl-C or blank to skip:"
    );
    let mut subs = Vec::new();
    let stdin = std::io::stdin();
    loop {
        print!("> ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        match parse_subscription(line) {
            Ok(sub) => subs.push(sub),
            Err(e) => eprintln!("skipped: {e}"),
        }
    }
    if subs.is_empty() {
        println!("no subscriptions entered; leaving survey uninitialized");
        return ExitCode::SUCCESS;
    }
    match apb_mcp::advisory_tools::subscriptions_set(subs, false) {
        Ok(v) => {
            print_json(&v);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("subscriptions error: {e}");
            ExitCode::from(2)
        }
    }
}

/// In an interactive session (stdin is a terminal) with a survey that hasn't
/// run yet, prints an unobtrusive hint to STDERR (stdout carries JSON - it
/// must not be cluttered). Non-interactively (piped/redirected stdin) we stay
/// silent and do NOT change state; a survey already completed/declined is
/// not re-offered; a broken state is silently skipped (we do not pretend it
/// is Uninitialized).
pub(crate) fn offer_onboarding_if_tty() {
    use apb_core::models_table::{self, OnboardingState};
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal()
        && matches!(
            models_table::onboarding::read(),
            Ok(OnboardingState::Uninitialized)
        )
    {
        eprintln!(
            "\nhint: run `apb subscriptions` to declare your agent subscriptions (improves profile advice)"
        );
    }
}

pub(crate) fn run_init(root: &Path) -> ExitCode {
    match init_project(root) {
        Ok(()) => {
            println!("initialized {}", root.join(".apb").display());
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
