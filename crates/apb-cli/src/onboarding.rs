// Not yet called from main.rs: a later task wires this into an interactive
// onboarding questionnaire. Only unit tests exercise it for now, so the
// non-test build sees these as dead code; `allow` (not `expect`) because the
// two build targets (test vs non-test) disagree on whether it fires.
#![allow(dead_code)]

use std::io;
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
