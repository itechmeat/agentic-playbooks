//! What a playbook needs from this machine before it can start: its
//! `requires` (files, commands) and a usable account for every connector it
//! binds. The run gate refuses on these (`apb_engine::gate`); `apb validate`
//! and `apb doctor` report them ahead of a run from the same functions.

use std::path::Path;

use crate::config::program_in_path;
use crate::schema::{Playbook, Requires};

/// A `requires` entry that is refused outright rather than checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequiresRefusal {
    /// A file path that is absolute or climbs out of the project.
    UnsafePath(String),
    /// A command given as a path instead of a name on PATH.
    UnsafeCommand(String),
}

/// A safe relative path name: not absolute and without `..` components.
/// Protection against `requires.files` serving as an existence oracle for
/// arbitrary files (especially in a foreign prepare_run before trust is
/// confirmed) - see spec 5.2.
pub fn is_safe_relative(p: &str) -> bool {
    let path = Path::new(p);
    if path.is_absolute() {
        return false;
    }
    path.components().all(|c| {
        !matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::Prefix(_)
                | std::path::Component::RootDir
        )
    })
}

/// The unmet `requires` entries in `root`, as `file:<path>` / `command:<name>`,
/// in declaration order; empty when everything is there.
pub fn requires_unmet(root: &Path, req: &Requires) -> Result<Vec<String>, RequiresRefusal> {
    let mut missing = Vec::new();
    for f in &req.files {
        if !is_safe_relative(f) {
            return Err(RequiresRefusal::UnsafePath(f.clone()));
        }
        if !root.join(f).exists() {
            missing.push(format!("file:{f}"));
        }
    }
    for c in &req.commands {
        if c.contains('/') || c.contains('\\') {
            return Err(RequiresRefusal::UnsafeCommand(c.clone()));
        }
        if !program_in_path(c) {
            missing.push(format!("command:{c}"));
        }
    }
    Ok(missing)
}

/// Why the connectors a playbook binds cannot serve a run on this machine,
/// one line per problem: a connector that does not resolve (not installed, a
/// broken manifest or account file, an unknown function), one with no
/// configured account (every call would fail), and secret env vars no account
/// can resolve. Trust is not judged here: approval is a deliberate user act
/// the run gate asks for.
pub fn connector_problems(root: &Path, playbook: &Playbook) -> Vec<String> {
    if !playbook.binds_connectors() {
        return Vec::new();
    }
    let resolution = match crate::connector::resolve::resolve_playbook(root, playbook) {
        Ok(r) => r,
        Err(errors) => return errors,
    };
    let mut out = Vec::new();
    for (name, resolved) in &resolution.connectors {
        if resolved.accounts.is_empty() {
            out.push(format!(
                "connector `{name}` has no configured account; add one to its connector-config"
            ));
        }
        let missing = crate::connector::secrets::missing_vars(root, &resolved.required_env);
        if !missing.is_empty() {
            out.push(format!(
                "connector `{name}` needs unset env vars: {}",
                missing.join(", ")
            ));
        }
    }
    out
}

/// Every reason [`requires_unmet`], [`connector_problems`] and
/// [`cold_handoffs`] and [`unmatched_protect_globs`] give for `playbook`, as
/// `(code, message)` pairs for a report: `requires_unmet`, `requires_unsafe`,
/// `connector_unconfigured`, `session_handoff_cold` and `V76`.
pub fn findings(root: &Path, playbook: &Playbook) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(req) = &playbook.requires {
        match requires_unmet(root, req) {
            Ok(missing) if missing.is_empty() => {}
            Ok(missing) => out.push((
                "requires_unmet",
                format!("this machine lacks {}", missing.join(", ")),
            )),
            Err(RequiresRefusal::UnsafePath(p)) => out.push((
                "requires_unsafe",
                format!("requires.files entry `{p}` must be a relative path inside the project"),
            )),
            Err(RequiresRefusal::UnsafeCommand(c)) => out.push((
                "requires_unsafe",
                format!("requires.commands entry `{c}` must be a program name, not a path"),
            )),
        }
    }
    for problem in connector_problems(root, playbook) {
        out.push(("connector_unconfigured", problem));
    }
    for problem in cold_handoffs(root, playbook) {
        out.push(("session_handoff_cold", problem));
    }
    for problem in unmatched_protect_globs(root, playbook) {
        out.push(("V76", problem));
    }
    out
}

/// V76 (C6): one line per `protect` glob that matches no file in `root`
/// (the project tree; a node with its own `workdir` is checked against the
/// root too, as the closest tree known before a run). Such a glob protects
/// nothing, which is usually a typo. Invalid globs are V75's business.
pub fn unmatched_protect_globs(root: &Path, playbook: &Playbook) -> Vec<String> {
    let mut out = Vec::new();
    for node in &playbook.nodes {
        for g in node.kind.protect_globs() {
            if let Ok(files) = crate::fingerprint::matching_files(root, std::slice::from_ref(g))
                && files.is_empty()
            {
                out.push(format!(
                    "node `{}`: protect glob `{g}` matches no file in the project, so it protects nothing",
                    node.id
                ));
            }
        }
    }
    out
}

/// One line per `continue_session` node whose bound agent cannot continue a
/// session at all (issue #67 item 1): such a node always starts a fresh agent.
/// A profile that does not resolve here is left to the run-time resolver.
pub fn cold_handoffs(root: &Path, playbook: &Playbook) -> Vec<String> {
    use crate::schema::NodeKind;
    let mut out = Vec::new();
    for node in &playbook.nodes {
        let NodeKind::AgentTask {
            continue_session: Some(source),
            profile,
            ..
        } = &node.kind
        else {
            continue;
        };
        let Some(reference) = profile.as_ref().or(playbook.defaults.profile.as_ref()) else {
            continue;
        };
        let Ok(loaded) = crate::profile_store::resolve_profile(
            root,
            crate::profile_store::PlaybookOrigin::Project,
            reference,
        ) else {
            continue;
        };
        let agent = &loaded.doc.executor.agent;
        if !crate::detect::continues_sessions(agent) {
            out.push(format!(
                "node `{}` continues the session of `{source}`, but its agent `{agent}` cannot continue a session, so it starts a fresh agent",
                node.id
            ));
        }
    }
    out
}

/// The references a run of `playbook` would fail on at start because the
/// file they name is not there, as [`Issue`]s:
///
/// - V91: an agent-running node's profile (its own or `defaults.profile`)
///   resolves in no scope the run would look in (project, then global, for
///   a project playbook), or its files cannot be read. One issue per
///   distinct reference, on the first node that uses it. An error only when
///   the project alone could provide the profile: a project profile that
///   exists but cannot be loaded. When the global store was the last place
///   looked (`scope: global`, or `auto` falling through to it), it is a
///   warning, since the global store is machine-local and another machine
///   (CI, a fresh clone) may well have the profile. A missing
///   `scope: project` profile is V14's finding and is not repeated here.
/// - V92 (error): a `script` node's script, an `agent_task`'s
///   `success_check` script or a goal criterion's script is missing. A run
///   copies only the version's `scripts/` directory next to its snapshot and
///   runs every script from there, so a path counts as present only when it
///   is a regular file (not a symlink) under `scripts/` in `version_dir`. A
///   path V12 or V41 already refuses is left to them.
///
/// Not checked: the profiles and scripts of sub-playbook children (each is
/// validated on its own) and the supervisor profile of `apb run --supervise`
/// (chosen at run start).
///
/// The validator itself stays free of the filesystem, and a save does not
/// call this: the dashboard and MCP write the YAML first and add the
/// profile or the scripts afterwards. `apb validate` and MCP
/// `playbook_validate` report these with the severity above, the
/// dashboard's playbook view always as warnings.
pub fn missing_references(
    root: &Path,
    origin: crate::profile_store::PlaybookOrigin,
    version_dir: &Path,
    playbook: &Playbook,
) -> Vec<crate::validate::Issue> {
    let mut out = unresolved_profiles(root, origin, playbook);
    out.extend(missing_scripts(version_dir, playbook));
    out
}

fn reference_issue(
    code: &'static str,
    severity: crate::validate::Severity,
    node: &str,
    message: String,
) -> crate::validate::Issue {
    crate::validate::Issue {
        code,
        severity,
        message,
        node: Some(node.to_string()),
    }
}

fn unresolved_profiles(
    root: &Path,
    origin: crate::profile_store::PlaybookOrigin,
    playbook: &Playbook,
) -> Vec<crate::validate::Issue> {
    use crate::profile::{ProfileError, ProfileScope};
    use crate::profile_store::PlaybookOrigin;
    use crate::validate::Severity;
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for node in &playbook.nodes {
        let Some(reference) = node.kind.effective_profile_ref(&playbook.defaults) else {
            continue;
        };
        if !seen.insert((
            crate::profile_store::scope_str(reference.scope),
            reference.name.clone(),
        )) {
            continue;
        }
        // Whether the project's own profile directory is what failed: then
        // only the project can fix it and the finding is an error.
        let project_only = reference.scope == ProfileScope::Project
            || (reference.scope == ProfileScope::Auto
                && origin == PlaybookOrigin::Project
                && crate::profile_store::project_dir(root)
                    .join(&reference.name)
                    .exists());
        let (severity, message) = match crate::profile_store::resolve_profile(
            root, origin, &reference,
        ) {
            Ok(_) | Err(ProfileError::ScopeForbidden(_)) => continue, // V14's finding
            // A missing project-scope profile is V14's finding too.
            Err(ProfileError::NotFound(_)) if reference.scope == ProfileScope::Project => {
                continue;
            }
            Err(ProfileError::NotFound(_)) => {
                let scopes = match (reference.scope, origin) {
                    (ProfileScope::Auto, PlaybookOrigin::Project) => {
                        "the project or the global store"
                    }
                    _ => "the global store",
                };
                (
                    Severity::Warning,
                    format!(
                        "profile `{}` is not found in {scopes}; a run on this machine fails at start until it exists (create it with `apb profile write` or MCP `profile_write`)",
                        reference.name
                    ),
                )
            }
            Err(e) => (
                if project_only {
                    Severity::Error
                } else {
                    Severity::Warning
                },
                format!(
                    "profile `{}` cannot be loaded: {e}; a run fails at start",
                    reference.name
                ),
            ),
        };
        out.push(reference_issue("V91", severity, &node.id, message));
    }
    out
}

/// Whether `file` (under `version_dir/scripts/`) is a script the run would
/// find in its copy of `scripts/`. The copy (`fsutil::copy_tree`) keeps a
/// symlink as a symlink with the same target, and the definition digest
/// refuses an absolute or escaping one, so a symlink counts only when it is
/// relative and resolves to a regular file inside `scripts/`.
fn script_in_run_copy(version_dir: &Path, file: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(file) else {
        return false;
    };
    if meta.is_file() {
        return true;
    }
    if !meta.file_type().is_symlink() || !std::fs::read_link(file).is_ok_and(|t| t.is_relative()) {
        return false;
    }
    let (Ok(target), Ok(scripts)) = (
        std::fs::canonicalize(file),
        std::fs::canonicalize(version_dir.join("scripts")),
    ) else {
        return false;
    };
    target.starts_with(&scripts) && target.is_file()
}

/// The V92 half of [`missing_references`]: script nodes whose file the
/// version directory `version_dir` lacks.
pub fn missing_scripts(version_dir: &Path, playbook: &Playbook) -> Vec<crate::validate::Issue> {
    use crate::schema::{GoalCheck, NodeKind, SuccessCheck};
    let mut out = Vec::new();
    let mut check = |node: &str, what: &str, path: &str| {
        if !is_safe_relative(path) {
            return; // V12 / V41
        }
        let in_scripts = Path::new(path)
            .components()
            .find(|c| !matches!(c, std::path::Component::CurDir))
            .is_some_and(|c| c.as_os_str() == "scripts");
        let file = version_dir.join(path);
        if in_scripts && script_in_run_copy(version_dir, &file) {
            return;
        }
        let message = if file.is_file() && !in_scripts {
            format!(
                "{what} `{path}` is outside `scripts/`; a run copies only the version's `scripts/` directory, so it cannot find the file (move it under `scripts/`)"
            )
        } else if file.is_file() {
            format!(
                "{what} `{path}` is a symlink that is absolute or leaves `scripts/`; a run copies the link, not its target, so it cannot find the file (replace it with the file)"
            )
        } else {
            format!(
                "{what} `{path}` does not exist in the version directory; a run fails on this node until it is added"
            )
        };
        out.push(reference_issue(
            "V92",
            crate::validate::Severity::Error,
            node,
            message,
        ));
    };
    for node in &playbook.nodes {
        if let NodeKind::Script { script, .. } = &node.kind {
            check(&node.id, "script", script);
        }
        if let Some(path) = node
            .success_check
            .as_ref()
            .and_then(SuccessCheck::script_path)
        {
            check(&node.id, "success_check script", path);
        }
    }
    if let Some(goal) = &playbook.goal {
        for criterion in &goal.criteria {
            if let GoalCheck::Script { path } = &criterion.check {
                check("goal", "goal criterion script", path);
            }
        }
    }
    out
}
