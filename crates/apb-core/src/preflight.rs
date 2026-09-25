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

/// Every reason [`requires_unmet`] and [`connector_problems`] give for
/// `playbook`, as `(code, message)` pairs for a report: `requires_unmet`,
/// `requires_unsafe` and `connector_unconfigured`.
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
    out
}
