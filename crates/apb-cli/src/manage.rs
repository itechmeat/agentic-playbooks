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
            Ok(OnboardingState::Uninitialized) => {
                if let Err(e) = subscriptions_survey_step()
                    && e.kind() != std::io::ErrorKind::Interrupted
                {
                    eprintln!("subscriptions survey failed: {e}");
                }
                return ExitCode::SUCCESS;
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

/// Parses one `agent[:plan[:coverage]]` entry and appends it to `subs`.
/// Extracted verbatim from the old survey loop body so every survey front end
/// (the cliclack prompt below, and any future caller) stores entries through
/// exactly the same parse/validate path. It never writes state itself: the
/// resolved batch is committed once by the caller via `subscriptions_set`.
fn record_survey_entry(
    subs: &mut Vec<apb_core::models_table::Subscription>,
    entry: &str,
) -> Result<(), String> {
    subs.push(parse_subscription(entry)?);
    Ok(())
}

/// Composes an `agent[:plan[:coverage]]` entry from separately-collected
/// fields, preserving the positional grammar: a coverage still needs the plan
/// slot present (`agent::coverage` when the plan is blank).
fn compose_entry(agent: &str, plan: &str, coverage: &str) -> String {
    let agent = agent.trim();
    let plan = plan.trim();
    let coverage = coverage.trim();
    if !coverage.is_empty() {
        format!("{agent}:{plan}:{coverage}")
    } else if !plan.is_empty() {
        format!("{agent}:{plan}")
    } else {
        agent.to_string()
    }
}

/// Interactive subscriptions survey, re-skinned with cliclack. Self-gates:
/// only runs on a real terminal with the onboarding state still
/// `Uninitialized` (mirroring the offer test in `subscriptions_cmd`), and
/// returns `Ok(())` immediately otherwise. Cancellation (Esc/Ctrl-C) surfaces
/// as `io::ErrorKind::Interrupted` for the caller to handle. Stored data and
/// its format are identical to the old line-based survey.
pub(crate) fn subscriptions_survey_step() -> std::io::Result<()> {
    use apb_core::models_table::{self, OnboardingState};
    use std::io::IsTerminal;

    if !std::io::stdin().is_terminal() {
        return Ok(());
    }
    match models_table::onboarding::read() {
        Ok(OnboardingState::Uninitialized) => {}
        // Already completed/declined, or a broken state: do not offer it and do
        // not pretend it is Uninitialized (matches the old offer behaviour).
        _ => return Ok(()),
    }

    // Detected agents become the multiselect items. Installed agents with a
    // usable auth hint are pre-selected, matching the old "suggested" prefill.
    let mut items: Vec<(String, String, String)> = Vec::new();
    let mut preselected: Vec<String> = Vec::new();
    if let Ok(v) = apb_mcp::advisory_tools::agents_detect(false)
        && let Some(agents) = v["agents"].as_array()
    {
        for a in agents {
            let installed = a["installed"].as_bool().unwrap_or(false);
            let name = a["agent"].as_str().unwrap_or("?").to_string();
            let auth = a["auth"]["kind"].as_str();
            let hint = match (installed, auth) {
                (true, Some("oauth")) => "installed, auth: oauth".to_string(),
                (true, Some("api_key")) => "installed, auth: api-key".to_string(),
                (true, _) => "installed".to_string(),
                (false, _) => "not detected".to_string(),
            };
            if installed && matches!(auth, Some("oauth") | Some("api_key")) {
                preselected.push(name.clone());
            }
            items.push((name.clone(), name, hint));
        }
    }
    if items.is_empty() {
        cliclack::log::info("No coding agents detected; leaving survey uninitialized")?;
        return Ok(());
    }

    let mut selector = cliclack::multiselect(
        "Which agent subscriptions do you have? (space to toggle, enter to confirm)",
    )
    .items(&items)
    .required(false);
    if !preselected.is_empty() {
        selector = selector.initial_values(preselected);
    }
    let selected: Vec<String> = selector.interact()?;
    if selected.is_empty() {
        cliclack::log::info("No subscriptions selected; leaving survey uninitialized")?;
        return Ok(());
    }

    let mut subs = Vec::new();
    for agent in &selected {
        let plan: String = cliclack::input(format!("Plan for {agent} (optional)"))
            .placeholder("e.g. pro, max, team")
            .required(false)
            .interact()?;
        let coverage: String = cliclack::input(format!("Coverage for {agent} (optional)"))
            .placeholder("full | partial | unknown")
            .required(false)
            .interact()?;
        let entry = compose_entry(agent, &plan, &coverage);
        if let Err(e) = record_survey_entry(&mut subs, &entry) {
            cliclack::log::warning(format!("skipped {agent}: {e}"))?;
        }
    }
    if subs.is_empty() {
        cliclack::log::info("No valid subscriptions entered; leaving survey uninitialized")?;
        return Ok(());
    }
    match apb_mcp::advisory_tools::subscriptions_set(subs, false) {
        Ok(_) => cliclack::log::success("Subscriptions recorded")?,
        Err(e) => cliclack::log::error(format!("subscriptions error: {e}"))?,
    }
    Ok(())
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

/// Best-effort offer of the subscriptions survey from a command that already
/// printed its JSON result. The survey self-gates (terminal + `Uninitialized`),
/// a cancellation (Esc/Ctrl-C) is silent, and any other error is reported
/// without changing the caller's exit code.
pub(crate) fn offer_subscriptions_survey() {
    if let Err(e) = subscriptions_survey_step()
        && e.kind() != std::io::ErrorKind::Interrupted
    {
        eprintln!("subscriptions survey failed: {e}");
    }
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
