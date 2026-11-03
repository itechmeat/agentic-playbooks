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
    let mut states: Vec<(String, InstallState)> = bundled::playbooks()
        .iter()
        .map(|p| (p.id.clone(), bundled::playbook_state(p)))
        .collect();
    states.extend(
        bundled::skills()
            .iter()
            .map(|s| (s.name.clone(), bundled::skill_state(s))),
    );
    let installed = states.iter().filter(|(_, s)| s.is_installed()).count();
    let updates: Vec<&str> = states
        .iter()
        .filter(|(_, s)| {
            matches!(
                s,
                InstallState::Pristine { .. } | InstallState::Modified { .. }
            )
        })
        .map(|(n, _)| n.as_str())
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
    ) && main == reviewer
    {
        out.push((
            true,
            "reviewer",
            format!(
                "{ROLE_REVIEWER} uses the same agent as {ROLE_MAIN} ({main}): reviews run in a fresh session but are less independent; install another agent and run apb setup plan"
            ),
        ));
    }
    out
}
