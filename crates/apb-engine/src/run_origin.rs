//! The definition origin a run was started from, read from its journal's
//! `run_provenance`: what a child's `scope: auto`, the run gate's tree walk
//! of a resume and the launch check of a run never authorized resolve
//! against. Reads only the journal, so the gate and the scheduler both use
//! it without depending on each other.

use std::path::Path;

use crate::event::{Event, EventPayload, read_all};

/// The definition scope a run executes, as its journal tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunScope {
    /// Labelled `project`, or a legacy journal that reads and carries no
    /// label.
    Project,
    /// Labelled `global`.
    Global,
    /// The journal does not read (absent, not a file, a torn or corrupt
    /// line), so the scope is unknown: no evidence for either side.
    Unreadable,
}

/// The scope the events name: the first `run_provenance` origin label,
/// `Project` without one. The one parse of that label.
pub fn origin_of_events(events: &[Event]) -> RunScope {
    match first_label(events) {
        Some(label) => match scope_of_label(label) {
            apb_core::scope::Scope::Global => RunScope::Global,
            apb_core::scope::Scope::Project => RunScope::Project,
        },
        None => RunScope::Project,
    }
}

/// The first `run_provenance` origin label of the events, if any: the
/// label a run's scope is read from.
pub fn first_label(events: &[Event]) -> Option<&str> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::RunProvenance {
            origin: Some(label),
            ..
        } => Some(label.as_str()),
        _ => None,
    })
}

/// The scope one origin label names: `global` is a global run, any other
/// label a project run.
pub fn scope_of_label(label: &str) -> apb_core::scope::Scope {
    if label == "global" {
        apb_core::scope::Scope::Global
    } else {
        apb_core::scope::Scope::Project
    }
}

/// [`origin_of_events`] as a definition scope: `Global` for a global run,
/// `Project` otherwise. What a reader that keys a store or a slot by scope
/// takes from the events.
pub fn scope_of_events(events: &[Event]) -> apb_core::scope::Scope {
    match origin_of_events(events) {
        RunScope::Global => apb_core::scope::Scope::Global,
        RunScope::Project | RunScope::Unreadable => apb_core::scope::Scope::Project,
    }
}

/// The scope of the run at `run_dir`: [`origin_of_events`] over a journal
/// read whole (every line parses; an event type this binary does not know
/// is skipped, since it cannot carry the label), `Unreadable` otherwise.
/// A run path whose `events.jsonl` is absent or is not a regular file (the
/// path itself missing or not a directory included) is `Unreadable` too:
/// the journal reader would read it as empty, and an empty journal is no
/// evidence that the run is a project run.
pub fn run_scope(run_dir: &Path) -> RunScope {
    if !run_dir.join("events.jsonl").is_file() {
        return RunScope::Unreadable;
    }
    match crate::event::read_journal_whole(run_dir) {
        Ok(journal) => origin_of_events(&journal.events),
        Err(_) => RunScope::Unreadable,
    }
}

/// The parent run's definition origin (from its RunProvenance event), used to
/// resolve a child's `scope: auto` the same way the policy gate does (parent
/// origin first, then global). Read through the engine's strict reader; a
/// journal it refuses resolves as Project, the scope a resume of it is
/// checked against (the resume itself refuses such a journal).
pub fn parent_run_origin(run_dir: &Path) -> apb_core::scope::Origin {
    use apb_core::scope::Origin;
    match origin_of_events(&read_all(run_dir).unwrap_or_default()) {
        RunScope::Global => Origin::Global,
        RunScope::Project | RunScope::Unreadable => Origin::Project { workspace_id: None },
    }
}

/// Why a run of `scope` cannot be patched as the project playbook of its
/// id: a global run has no project definition, and a run whose scope is
/// unknown is refused before any project definition is read. `None` for a
/// project run. The one refusal the three patch guards share.
pub fn patch_refusal(scope: RunScope) -> Option<(&'static str, &'static str)> {
    match scope {
        RunScope::Project => None,
        RunScope::Global => Some((
            "patch_global_unsupported",
            "a run of a global playbook cannot be patched",
        )),
        RunScope::Unreadable => Some((
            "patch_origin_unreadable",
            "the run's journal does not read, so its definition scope is unknown",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run directory whose journal is absent, or is no file that opens,
    /// has an unknown scope: it is no project run.
    #[test]
    fn a_run_directory_without_a_journal_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        std::fs::create_dir_all(&missing).unwrap();
        assert_eq!(run_scope(&missing), RunScope::Unreadable);
        let not_a_file = dir.path().join("dir-journal");
        std::fs::create_dir_all(not_a_file.join("events.jsonl")).unwrap();
        assert_eq!(run_scope(&not_a_file), RunScope::Unreadable);
        // A run path that does not exist, or is a file, holds no journal.
        assert_eq!(run_scope(&dir.path().join("absent")), RunScope::Unreadable);
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(run_scope(&file), RunScope::Unreadable);
    }

    /// A journal that reads and carries no label is a project run.
    #[test]
    fn a_legacy_journal_without_a_label_is_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            concat!(
                r#"{"seq":1,"ts":1,"type":"run_started","playbook":"dup","version":"1.0.0"}"#,
                "\n"
            ),
        )
        .unwrap();
        assert_eq!(run_scope(dir.path()), RunScope::Project);
    }
}
