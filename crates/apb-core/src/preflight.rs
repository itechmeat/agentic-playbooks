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
    let binds = playbook
        .nodes
        .iter()
        .any(|n| !n.kind.connector_bindings().is_empty());
    if !binds {
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
