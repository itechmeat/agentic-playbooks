//! The `apb doctor` lines about setup: whether the defaults apb ships are
//! installed and current, and whether the reviewer role shares the main
//! role's agent.

use std::path::Path;

use apb_core::bundled::{self, InstallState, ROLE_MAIN, ROLE_REVIEWER};

use super::plan::existing_profile;

/// One doctor line: whether it warns, its subject and its detail.
pub type DoctorLine = (bool, &'static str, String);

/// The setup lines of `apb doctor`.
pub fn doctor_lines(root: &Path) -> Vec<DoctorLine> {
    // Each default with its state and, for a changed one, the labelled
    // summary of how it differs from the shipped content.
    let mut states: Vec<(String, InstallState, Option<String>)> = bundled::playbooks()
        .iter()
        .map(|p| {
            let state = bundled::playbook_state(p);
            let changes = matches!(state, InstallState::Modified { .. })
                .then(|| {
                    bundled::playbook_update_diff(p)
                        .map(|(base, d)| base.line(&bundled::diff_summary(&d)))
                })
                .flatten();
            (p.id.clone(), state, changes)
        })
        .collect();
    states.extend(bundled::skills().iter().map(|s| {
        let state = bundled::skill_state(s);
        let changes = matches!(state, InstallState::Modified { .. })
            .then(|| {
                bundled::skill_update_diff(s).map(|d| bundled::SkillDiff::BASE.line(&d.summary()))
            })
            .flatten();
        (s.name.clone(), state, changes)
    }));
    let installed = states.iter().filter(|(_, s, _)| s.is_installed()).count();
    let updates: Vec<String> = states
        .iter()
        .filter(|(_, s, _)| {
            matches!(
                s,
                InstallState::Pristine { .. } | InstallState::Modified { .. }
            )
        })
        .map(|(n, _, changes)| match changes {
            Some(c) => format!("{n} ({c})"),
            None => n.clone(),
        })
        .collect();
    let mut out = Vec::new();
    out.push(if installed == 0 {
        (
            false,
            "defaults",
            "not installed (apb setup plan shows what setup would install)".to_string(),
        )
    } else if !updates.is_empty() {
        (
            true,
            "defaults",
            format!(
                "installed, updates available: {} (an unmodified copy moves on apb setup apply or the next dashboard start; a changed one is replaced only on request: apb setup plan --replace-modified <name>)",
                updates.join(", ")
            ),
        )
    } else {
        (
            false,
            "defaults",
            format!("installed, {installed} of {} up to date", states.len()),
        )
    });
    if let (Some(main), Some(reviewer)) = (
        existing_profile(root, ROLE_MAIN),
        existing_profile(root, ROLE_REVIEWER),
    ) && bundled::same_agent_reviewer(&main, &reviewer)
    {
        out.push((
            true,
            "reviewer",
            format!(
                "{ROLE_REVIEWER} on {main}: {}; install another agent and run apb setup plan",
                bundled::SAME_AGENT_REVIEWER_DETAIL
            ),
        ));
    }
    out
}
