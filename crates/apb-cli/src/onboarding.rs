use std::io;
use std::io::IsTerminal;
use std::path::Path;

const FEEDBACK_BLOCK: &str = include_str!("../assets/feedback-loop.md");
const PLAYBOOK_BLOCK: &str = include_str!("../assets/playbook-instructions.md");
const TARGET_FILES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

/// One standing-instruction block the questionnaire can append to the host
/// project's agent instruction files. Every block shares the same consent,
/// append and idempotency mechanics, so blocks differ only in this data.
struct StandingBlock {
    /// Heading whose presence means the block is already there: the whole
    /// idempotency of the append rests on it.
    marker: &'static str,
    body: &'static str,
    /// Section name used in the per-file log lines.
    section: &'static str,
    /// Sentence subject of the "already configured" line.
    subject: &'static str,
    /// Consent question asked before anything is written.
    question: &'static str,
    /// Line logged when consent is declined and nothing is written.
    skip_note: &'static str,
}

const PLAYBOOKS: StandingBlock = StandingBlock {
    marker: "## apb playbooks",
    body: PLAYBOOK_BLOCK,
    section: "apb playbooks",
    subject: "Playbook instructions",
    question: "Add a standing instruction to CLAUDE.md/AGENTS.md so coding agents \
               discover saved playbooks and offer to save repeatable actions?",
    skip_note: "Skipped. Nothing written; run apb init again to add it later",
};

const FEEDBACK: StandingBlock = StandingBlock {
    marker: "## apb feedback loop",
    body: FEEDBACK_BLOCK,
    section: "apb feedback loop",
    subject: "Feedback loop",
    question: "Allow coding agents to report apb errors after playbook runs? \
               Anonymized, consolidated issues are filed transparently at \
               https://github.com/itechmeat/agentic-playbooks (no secrets, no private prompts)",
    skip_note: "Skipped. You can add the section later from the README",
};

enum BlockAction {
    Created,
    Appended,
    AlreadyConfigured,
}

/// Appends `block` to every target file, creating a file that does not exist.
/// Idempotent through the block marker, so several blocks coexist in one file
/// in any order and a re-run never duplicates one.
fn apply_block(dir: &Path, block: &StandingBlock) -> io::Result<Vec<(String, BlockAction)>> {
    let mut out = Vec::new();
    for name in TARGET_FILES {
        let path = dir.join(name);
        let action = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            if text.contains(block.marker) {
                BlockAction::AlreadyConfigured
            } else {
                let sep = if text.ends_with("\n\n") {
                    ""
                } else if text.ends_with('\n') {
                    "\n"
                } else {
                    "\n\n"
                };
                std::fs::write(&path, format!("{text}{sep}{body}", body = block.body))?;
                BlockAction::Appended
            }
        } else {
            std::fs::write(&path, block.body)?;
            BlockAction::Created
        };
        out.push((name.to_string(), action));
    }
    Ok(out)
}

fn block_fully_configured(dir: &Path, block: &StandingBlock) -> bool {
    TARGET_FILES.iter().all(|name| {
        std::fs::read_to_string(dir.join(name))
            .map(|t| t.contains(block.marker))
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
    // Behaviour guidance first, error reporting second: how agents work with
    // playbooks is more fundamental than how they report apb bugs.
    offer_block(root, &PLAYBOOKS)?;
    offer_block(root, &FEEDBACK)?;
    subscriptions_survey_step()?;
    cliclack::outro("Project ready. Try: apb --help")?;
    Ok(())
}

/// Asks for consent once and, only when it is granted, writes `block` into the
/// target files. A declined step writes nothing.
fn offer_block(root: &Path, block: &StandingBlock) -> io::Result<()> {
    if block_fully_configured(root, block) {
        cliclack::log::success(format!(
            "{subject} already configured in CLAUDE.md and AGENTS.md",
            subject = block.subject
        ))?;
        return Ok(());
    }
    let consent = cliclack::confirm(block.question)
        .initial_value(true)
        .interact()?;
    if !consent {
        cliclack::log::info(block.skip_note)?;
        return Ok(());
    }
    for (name, action) in apply_block(root, block)? {
        let section = block.section;
        match action {
            BlockAction::Created => {
                cliclack::log::success(format!("{name} created with the {section} section"))?
            }
            BlockAction::Appended => {
                cliclack::log::success(format!("{name} updated with the {section} section"))?
            }
            BlockAction::AlreadyConfigured => {
                cliclack::log::info(format!("{name} already configured"))?
            }
        }
    }
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
        let out = apply_block(dir.path(), &FEEDBACK).unwrap();
        assert_eq!(out.len(), 2);
        for (name, action) in &out {
            assert!(matches!(action, BlockAction::Created), "{name}");
            let text = fs::read_to_string(dir.path().join(name)).unwrap();
            assert_eq!(text, FEEDBACK_BLOCK);
        }
    }

    #[test]
    fn appends_to_existing_file_with_separator() {
        let dir = tmp();
        fs::write(dir.path().join("CLAUDE.md"), "# My project\n").unwrap();
        let out = apply_block(dir.path(), &FEEDBACK).unwrap();
        let claude = out.iter().find(|(n, _)| n == "CLAUDE.md").unwrap();
        assert!(matches!(claude.1, BlockAction::Appended));
        let text = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(text.starts_with("# My project\n"));
        assert!(text.contains("\n\n## apb feedback loop (standing instruction)"));
    }

    #[test]
    fn rerun_is_idempotent() {
        let dir = tmp();
        apply_block(dir.path(), &FEEDBACK).unwrap();
        let before = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        let out = apply_block(dir.path(), &FEEDBACK).unwrap();
        for (_, action) in &out {
            assert!(matches!(action, BlockAction::AlreadyConfigured));
        }
        let after = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn fully_configured_detection() {
        let dir = tmp();
        assert!(!block_fully_configured(dir.path(), &FEEDBACK));
        apply_block(dir.path(), &FEEDBACK).unwrap();
        assert!(block_fully_configured(dir.path(), &FEEDBACK));
    }

    #[test]
    fn blocks_coexist_in_any_order_and_stay_idempotent() {
        for order in [[&PLAYBOOKS, &FEEDBACK], [&FEEDBACK, &PLAYBOOKS]] {
            let dir = tmp();
            for block in order {
                apply_block(dir.path(), block).unwrap();
            }
            let before = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
            assert!(before.contains(PLAYBOOKS.marker));
            assert!(before.contains(FEEDBACK.marker));
            for block in order {
                apply_block(dir.path(), block).unwrap();
            }
            let after = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
            assert_eq!(before, after);
        }
    }

    #[test]
    fn playbook_block_carries_the_discovery_capture_and_decline_duties() {
        assert!(PLAYBOOK_BLOCK.starts_with(PLAYBOOKS.marker));
        for tool in [
            "playbook_catalog",
            "playbook_capture",
            "suggestion_dismiss",
            "playbook_interview",
        ] {
            assert!(
                PLAYBOOK_BLOCK.contains(tool),
                "playbook block lost the reference to {tool}"
            );
        }
        for phrase in [
            "suppressed_suggestions",
            // Mirrors the tier-0 chit-chat guard: without it the agent calls
            // the catalog on every conversational turn.
            "Do not call it for chit-chat or clarifying replies.",
            "not by slug equality",
            "kind soft",
            "kind hard",
            "global scope only when",
            "offer a short interview",
        ] {
            assert!(
                PLAYBOOK_BLOCK.contains(phrase),
                "playbook block lost the phrase `{phrase}`"
            );
        }
        assert!(!PLAYBOOK_BLOCK.contains('\u{2014}'));
        assert!(!PLAYBOOK_BLOCK.contains('!'));
    }

    #[test]
    fn readme_and_asset_do_not_drift() {
        let readme = include_str!("../../../README.md");
        assert!(
            readme.contains(FEEDBACK_BLOCK),
            "README no longer contains the feedback-loop block verbatim"
        );
        assert!(
            FEEDBACK_BLOCK.contains("docs/FEEDBACK-LOOP.md"),
            "feedback block lost the link to the full reporting instruction"
        );
    }
}
