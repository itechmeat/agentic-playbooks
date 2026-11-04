//! One wording for what an upgrade reconcile did, shared by every place
//! that runs one: the dashboard start (connectors, default playbooks and
//! skills), `apb self-update` and the `apb mcp` start (default playbooks
//! and skills). Each caller adds its own prefix and picks the stream.

use super::Reconciled;

/// Where a shipped copy that was not moved on its own points the person:
/// the plan that replaces it with the shipped content, followed by its
/// name.
pub const DEFAULTS_HINT: &str = "apb setup plan --replace-modified";

/// [`DEFAULTS_HINT`] for the default `name`.
fn defaults_hint(name: &str) -> String {
    format!("{DEFAULTS_HINT} {name}")
}

/// One line about one reconciled item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileNote {
    /// The item could not be updated (an error, not a report).
    pub failed: bool,
    pub text: String,
}

/// The lines for one kind of item (`connector`, `default playbook`,
/// `skill`). `hint` names the command that offers the update of a changed
/// copy.
pub fn describe(
    kind: &str,
    hint: impl Fn(&str) -> String,
    outcome: Vec<Reconciled>,
) -> Vec<ReconcileNote> {
    describe_changes(kind, hint, |_| None, outcome)
}

/// [`describe`] with what the shipped content changed: `changes` gives the
/// labelled one-line summary for the item `name` an update is available
/// for (`shipped changes: <summary>` or `differs from the shipped version:
/// <summary>`, see [`super::DiffBase::line`]), added to its line after `; `.
pub(crate) fn describe_changes(
    kind: &str,
    hint: impl Fn(&str) -> String,
    changes: impl Fn(&str) -> Option<String>,
    outcome: Vec<Reconciled>,
) -> Vec<ReconcileNote> {
    outcome
        .into_iter()
        .map(|r| match r {
            Reconciled::Updated {
                name,
                from_version,
                to_version,
            } => ReconcileNote {
                failed: false,
                text: format!(
                    "{kind} `{name}` updated to the built-in copy ({from_version} -> {to_version})"
                ),
            },
            Reconciled::UpdateAvailable {
                name,
                installed_version,
                embedded_version,
            } => ReconcileNote {
                failed: false,
                text: format!(
                    "{kind} `{name}` stays at {installed_version}, which was changed or chosen locally; the built-in {embedded_version} is available ({}){}",
                    hint(&name),
                    changes(&name)
                        .map(|c| format!("; {c}"))
                        .unwrap_or_default()
                ),
            },
            Reconciled::NewerInstalled {
                name,
                installed_version,
                embedded_version,
            } => ReconcileNote {
                failed: false,
                text: format!(
                    "{kind} `{name}`: newer defaults installed, leaving them ({installed_version} installed, this apb ships {embedded_version})"
                ),
            },
            Reconciled::Failed { name, error } => ReconcileNote {
                failed: true,
                text: format!("cannot update {kind} `{name}`: {error}"),
            },
        })
        .collect()
}

/// Brings the installed default playbooks and skills in line with the
/// running binary and describes what changed. Nothing installed means
/// nothing to say. It only tries the lock of shipped content: when another
/// process holds it (a setup apply, another server starting), this one
/// skips, since that process brings the same content in line. Role profiles
/// setup wrote keep their approval across a skill update
/// ([`super::role_trust`]).
pub fn reconcile_defaults() -> Vec<ReconcileNote> {
    let _lock = match super::try_hold() {
        Ok(Some(lock)) => lock,
        Ok(None) => return Vec::new(),
        Err(e) => {
            return vec![ReconcileNote {
                failed: true,
                text: format!("default playbooks and skills not checked: {e}"),
            }];
        }
    };
    let mut notes = match super::playbook_store() {
        Some(parent) => playbook_notes(&parent, &super::playbooks()),
        None => Vec::new(),
    };
    if let Some(dirs) = super::SkillDirs::global() {
        let held = super::role_trust::snapshot();
        let (skill_notes, updated) = skill_notes(&dirs, &super::skills());
        notes.extend(skill_notes);
        notes.extend(
            super::role_trust::carry(held, &updated)
                .into_iter()
                .map(|text| ReconcileNote { failed: true, text }),
        );
    }
    notes
}

/// Reconciles the shipped playbooks `shipped` in the store `parent` and
/// describes it, with the shipped changes of each copy left alone.
pub(crate) fn playbook_notes(
    parent: &std::path::Path,
    shipped: &[super::BundledPlaybook],
) -> Vec<ReconcileNote> {
    let outcome = super::playbooks::reconcile_playbooks_in(parent, shipped);
    let changes = |name: &str| {
        let p = shipped.iter().find(|p| p.id == name)?;
        super::update_diff::playbook_update_diff_in(parent, p)
            .map(|(base, d)| base.line(&super::update_diff::diff_summary(&d)))
    };
    describe_changes("default playbook", defaults_hint, changes, outcome)
}

/// Reconciles the shipped skills `skills` in `dirs` and describes it, with
/// the shipped changes of each copy left alone; also the skills it updated.
pub(crate) fn skill_notes(
    dirs: &super::SkillDirs,
    skills: &[super::BundledSkill],
) -> (Vec<ReconcileNote>, Vec<String>) {
    let outcome = super::skills::reconcile_skills_in(dirs, skills);
    let updated: Vec<String> = outcome
        .iter()
        .filter_map(|r| match r {
            Reconciled::Updated { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    let changes = |name: &str| {
        let s = skills.iter().find(|s| s.name == name)?;
        super::update_diff::skill_update_diff_in(&dirs.skills, s)
            .map(|d| super::SkillDiff::BASE.line(&d.summary()))
    };
    (
        describe_changes("skill", defaults_hint, changes, outcome),
        updated,
    )
}

/// Prints the notes, each behind `prefix`. A failure goes to stderr; a
/// report goes to stdout unless `stderr_only` (a stdio server's stdout is
/// its protocol).
pub fn print_notes(prefix: &str, notes: &[ReconcileNote], stderr_only: bool) {
    for n in notes {
        if n.failed || stderr_only {
            eprintln!("{prefix}{}", n.text);
        } else {
            println!("{prefix}{}", n.text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_outcome_has_its_line_and_only_a_failure_is_an_error() {
        let notes = describe(
            "skill",
            |name| format!("fix {name}"),
            vec![
                Reconciled::Updated {
                    name: "a".into(),
                    from_version: "1.0.0".into(),
                    to_version: "1.1.0".into(),
                },
                Reconciled::UpdateAvailable {
                    name: "b".into(),
                    installed_version: "1.0.0".into(),
                    embedded_version: "1.1.0".into(),
                },
                Reconciled::Failed {
                    name: "c".into(),
                    error: "disk full".into(),
                },
            ],
        );
        assert_eq!(
            notes,
            vec![
                ReconcileNote {
                    failed: false,
                    text: "skill `a` updated to the built-in copy (1.0.0 -> 1.1.0)".into(),
                },
                ReconcileNote {
                    failed: false,
                    text: "skill `b` stays at 1.0.0, which was changed or chosen locally; the built-in 1.1.0 is available (fix b)".into(),
                },
                ReconcileNote {
                    failed: true,
                    text: "cannot update skill `c`: disk full".into(),
                },
            ]
        );
    }
}
