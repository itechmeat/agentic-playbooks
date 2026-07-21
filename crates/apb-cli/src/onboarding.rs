use std::io;
use std::io::IsTerminal;
use std::path::Path;

pub(crate) const FEEDBACK_BLOCK: &str = include_str!("../assets/feedback-loop.md");
const MARKER: &str = "## apb feedback loop";
const TARGET_FILES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

pub(crate) enum FeedbackAction {
    Created,
    Appended,
    AlreadyConfigured,
}

pub(crate) fn apply_feedback_loop(dir: &Path) -> io::Result<Vec<(String, FeedbackAction)>> {
    let mut out = Vec::new();
    for name in TARGET_FILES {
        let path = dir.join(name);
        let action = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            if text.contains(MARKER) {
                FeedbackAction::AlreadyConfigured
            } else {
                let sep = if text.ends_with("\n\n") {
                    ""
                } else if text.ends_with('\n') {
                    "\n"
                } else {
                    "\n\n"
                };
                std::fs::write(&path, format!("{text}{sep}{FEEDBACK_BLOCK}"))?;
                FeedbackAction::Appended
            }
        } else {
            std::fs::write(&path, FEEDBACK_BLOCK)?;
            FeedbackAction::Created
        };
        out.push((name.to_string(), action));
    }
    Ok(out)
}

pub(crate) fn feedback_loop_fully_configured(dir: &Path) -> bool {
    TARGET_FILES.iter().all(|name| {
        std::fs::read_to_string(dir.join(name))
            .map(|t| t.contains(MARKER))
            .unwrap_or(false)
    })
}

/// Runs the interactive `apb init` questionnaire on a real terminal only.
/// Never turns a successful init into a failure: a non-terminal returns
/// silently, a cancellation (Esc/Ctrl-C) exits with a cancel outro, and any
/// other error is reported without changing the caller's exit code.
pub(crate) fn run_init_questionnaire(root: &Path) {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return;
    }
    if let Err(e) = questionnaire(root) {
        if e.kind() == io::ErrorKind::Interrupted {
            let _ = cliclack::outro_cancel("Setup cancelled. Run apb init again anytime.");
        } else {
            eprintln!("init questionnaire failed: {e}");
        }
    }
}

fn questionnaire(root: &Path) -> io::Result<()> {
    cliclack::intro(" apb init ")?;
    if feedback_loop_fully_configured(root) {
        cliclack::log::success("Feedback loop already configured in CLAUDE.md and AGENTS.md")?;
    } else {
        let consent = cliclack::confirm(
            "Allow coding agents to report apb errors after playbook runs? \
             Anonymized, consolidated issues are filed transparently at \
             https://github.com/itechmeat/agentic-playbooks (no secrets, no private prompts)",
        )
        .initial_value(true)
        .interact()?;
        if consent {
            for (name, action) in apply_feedback_loop(root)? {
                match action {
                    FeedbackAction::Created => cliclack::log::success(format!(
                        "{name} created with the apb feedback loop section"
                    ))?,
                    FeedbackAction::Appended => cliclack::log::success(format!(
                        "{name} updated with the apb feedback loop section"
                    ))?,
                    FeedbackAction::AlreadyConfigured => {
                        cliclack::log::info(format!("{name} already configured"))?
                    }
                }
            }
        } else {
            cliclack::log::info("Skipped. You can add the section later from the README")?;
        }
    }
    subscriptions_survey_step()?;
    cliclack::outro("Project ready. Try: apb --help")?;
    Ok(())
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
/// only runs when BOTH stdin and stdout are real terminals (never a blocking
/// prompt when stdout is piped, e.g. `apb profile write | tool`) and the
/// onboarding state is still `Uninitialized`; returns `Ok(())` immediately
/// otherwise. Cancellation (Esc/Ctrl-C) surfaces as `io::ErrorKind::Interrupted`
/// and a failed store write surfaces as an `io::Error` for the caller to map to
/// its own exit code. Stored data and its format are identical to the old
/// line-based survey.
pub(crate) fn subscriptions_survey_step() -> std::io::Result<()> {
    use apb_core::models_table::{self, OnboardingState};

    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
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
        Ok(_) => {
            cliclack::log::success("Subscriptions recorded")?;
            Ok(())
        }
        // Surface the store failure so the caller can exit non-zero, matching
        // the old `interactive_survey` (exit 2) and the `--set` path. A store
        // failure is distinguishable from a cancellation (`Interrupted`).
        Err(e) => Err(std::io::Error::other(e.to_string())),
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
/// printed its JSON result. On a full terminal (stdin AND stdout) it runs the
/// interactive survey; a cancellation is silent and any other error is reported
/// without changing the caller's exit code. When stdin is a terminal but stdout
/// is piped (`apb profile write | tool`) it must not launch a blocking prompt,
/// so it falls back to the unobtrusive stderr hint that lived here before.
pub(crate) fn offer_subscriptions_survey() {
    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    if stdin_tty && stdout_tty {
        if let Err(e) = subscriptions_survey_step()
            && e.kind() != std::io::ErrorKind::Interrupted
        {
            eprintln!("subscriptions survey failed: {e}");
        }
    } else if stdin_tty {
        offer_onboarding_if_tty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn creates_both_files_when_missing() {
        let dir = tmp();
        let out = apply_feedback_loop(dir.path()).unwrap();
        assert_eq!(out.len(), 2);
        for (name, action) in &out {
            assert!(matches!(action, FeedbackAction::Created), "{name}");
            let text = fs::read_to_string(dir.path().join(name)).unwrap();
            assert_eq!(text, FEEDBACK_BLOCK);
        }
    }

    #[test]
    fn appends_to_existing_file_with_separator() {
        let dir = tmp();
        fs::write(dir.path().join("CLAUDE.md"), "# My project\n").unwrap();
        let out = apply_feedback_loop(dir.path()).unwrap();
        let claude = out.iter().find(|(n, _)| n == "CLAUDE.md").unwrap();
        assert!(matches!(claude.1, FeedbackAction::Appended));
        let text = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(text.starts_with("# My project\n"));
        assert!(text.contains("\n\n## apb feedback loop (standing instruction)"));
    }

    #[test]
    fn rerun_is_idempotent() {
        let dir = tmp();
        apply_feedback_loop(dir.path()).unwrap();
        let before = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        let out = apply_feedback_loop(dir.path()).unwrap();
        for (_, action) in &out {
            assert!(matches!(action, FeedbackAction::AlreadyConfigured));
        }
        let after = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn fully_configured_detection() {
        let dir = tmp();
        assert!(!feedback_loop_fully_configured(dir.path()));
        apply_feedback_loop(dir.path()).unwrap();
        assert!(feedback_loop_fully_configured(dir.path()));
    }

    #[test]
    fn readme_and_asset_do_not_drift() {
        let readme = include_str!("../../../README.md");
        assert!(
            readme.contains(FEEDBACK_BLOCK),
            "README no longer contains the feedback-loop block verbatim"
        );
    }
}
