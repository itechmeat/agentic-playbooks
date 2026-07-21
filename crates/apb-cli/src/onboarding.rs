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
    crate::manage::subscriptions_survey_step()?;
    cliclack::outro("Project ready. Try: apb --help")?;
    Ok(())
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
        for line in FEEDBACK_BLOCK.lines() {
            assert!(
                readme.contains(line),
                "README lost a feedback-loop line: {line}"
            );
        }
    }
}
