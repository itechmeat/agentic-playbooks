//! The text a known problem is about, per source kind: what a run saw (from
//! its own snapshot, at observe time) and what is there now (for a
//! recheck). Both sides are read and digested the same way, so equal
//! digests mean the same text:
//!
//! - a connector function: its block of `connector.yaml` (the run's copy
//!   `runs/<id>/connectors/<c>.yaml`; now the installed one);
//! - a skill: its `SKILL.md` (the run's snapshot of the profile's skills,
//!   else the installed one when its tree digest is the manifest's; now the
//!   installed one);
//! - a project file: the file at the root, refusing a path that leaves the
//!   root or runs through a symlink, a private one (under `.git/` or
//!   `.apb/`, a dotenv file, a git-ignored file) and one over
//!   [`MAX_SOURCE_BYTES`];
//! - a playbook node: its prompt (the run's snapshot; now the playbook's
//!   current version).
//!
//! The digest is over the raw text; every text kept or compared is
//! scrubbed first.

use std::path::{Path, PathBuf};

use apb_core::known_problems::{EXCERPT_MAX, ProblemScope, ProblemSource, ScopeKind, SourceKind};
use apb_core::schema::{NodeKind, Playbook};
use apb_core::scope::Scope;
use apb_core::scrub::Scrubber;

use crate::manifest::RunExecutionManifest;

/// The largest project file read as a problem's source; a larger one
/// cannot be read (it never closes a problem).
pub const MAX_SOURCE_BYTES: u64 = 1024 * 1024;

/// A source's text now: its digest and its scrubbed text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub digest: String,
    pub text: String,
    /// The scrubbed text still looks like it holds a credential: it is
    /// compared by digest, but no passage of it is offered or sent.
    pub flagged: bool,
}

fn digest(text: &str) -> String {
    format!("sha256:{}", apb_core::content::sha256_hex(text.as_bytes()))
}

/// The block of `function` in a connector definition, as YAML.
fn function_block(yaml: &str, function: &str) -> Option<String> {
    let doc: yaml_serde::Value = yaml_serde::from_str(yaml).ok()?;
    let block = doc
        .get("functions")?
        .as_sequence()?
        .iter()
        .find(|f| f.get("name").and_then(|n| n.as_str()) == Some(function))?;
    yaml_serde::to_string(block).ok()
}

/// A path whose text never becomes a problem's source: under `.git/` or
/// `.apb/`, or a dotenv file (`.env`, `.env.<name>`, `<name>.env`).
/// Compared in any case: a case-insensitive file system resolves `.GIT`
/// or `.Env` to the same file.
fn is_private(rel: &str) -> bool {
    let parts: Vec<String> = rel
        .split(['/', '\\'])
        .map(str::to_ascii_lowercase)
        .collect();
    parts.iter().any(|p| p == ".git" || p == ".apb")
        || parts
            .last()
            .is_some_and(|f| f == ".env" || f.starts_with(".env.") || f.ends_with(".env"))
}

/// Whether git ignores `rel` in the checkout at `root` (a file kept out of
/// the repository is often a local secret). Not a checkout, or no git:
/// not ignored.
fn git_ignored(root: &Path, rel: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-q", "--", rel])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A file of the project at `root/rel`, refused when the path leaves the
/// root, any part of it is a symlink, it is private ([`is_private`], or
/// git-ignored) or it is over [`MAX_SOURCE_BYTES`].
fn project_file(root: &Path, rel: &str) -> Option<String> {
    if is_private(rel) {
        return None;
    }
    let mut at = root.to_path_buf();
    for part in Path::new(rel).components() {
        match part {
            std::path::Component::Normal(p) => at.push(p),
            _ => return None,
        }
        if std::fs::symlink_metadata(&at)
            .ok()?
            .file_type()
            .is_symlink()
        {
            return None;
        }
    }
    let canon_root = std::fs::canonicalize(root).ok()?;
    let canon = std::fs::canonicalize(&at).ok()?;
    if !canon.starts_with(&canon_root) || !canon.is_file() {
        return None;
    }
    if std::fs::metadata(&canon).ok()?.len() > MAX_SOURCE_BYTES || git_ignored(root, rel) {
        return None;
    }
    let bytes = std::fs::read(&canon).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The prompt of agent node `id` in `playbook`.
fn node_prompt(playbook: &Playbook, id: &str) -> Option<String> {
    match &playbook.node(id)?.kind {
        NodeKind::AgentTask { prompt, .. } => Some(prompt.clone()),
        _ => None,
    }
}

/// The run's snapshot of skill `name`'s `SKILL.md`, from any profile.
fn snapshot_skill(run_dir: &Path, name: &str) -> Option<String> {
    let profiles = run_dir.join("profiles");
    for scope in std::fs::read_dir(&profiles).ok()?.flatten() {
        for profile in std::fs::read_dir(scope.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let skills = profile.path().join("skills");
            for sscope in std::fs::read_dir(&skills).into_iter().flatten().flatten() {
                let file: PathBuf = sscope.path().join(name).join("SKILL.md");
                if let Ok(text) = std::fs::read_to_string(&file) {
                    return Some(text);
                }
            }
        }
    }
    None
}

/// The installed skill `name` as the project resolves it: its directory.
fn installed_skill(root: &Path, name: &str) -> Option<PathBuf> {
    let r = apb_core::profile::SkillRef {
        name: name.to_string(),
        scope: apb_core::profile::ProfileScope::Auto,
    };
    apb_core::skills::resolve_skill(root, apb_core::profile::ProfileScope::Project, &r)
        .ok()
        .map(|s| s.canonical_path)
}

/// The raw text of `scope`'s source as the run at `run_dir` saw it.
fn seen_text(
    root: &Path,
    run_dir: &Path,
    scope: &ProblemScope,
    manifest: Option<&RunExecutionManifest>,
    snapshot: Option<&Playbook>,
) -> Option<String> {
    match scope.kind {
        ScopeKind::Connector => {
            let (connector, function) = scope.connector_parts()?;
            let yaml = std::fs::read_to_string(
                run_dir.join("connectors").join(format!("{connector}.yaml")),
            )
            .ok()?;
            function_block(&yaml, function)
        }
        ScopeKind::Skill => snapshot_skill(run_dir, &scope.name).or_else(|| {
            // No snapshot: the installed skill, only while it is the one
            // the run was given.
            let dir = installed_skill(root, &scope.name)?;
            let recorded = manifest?
                .profiles
                .iter()
                .flat_map(|p| &p.skills)
                .find(|s| s.name == scope.name)?;
            let now =
                apb_core::content::tree_digest(&dir, &apb_core::content::TreeLimits::default())
                    .ok()?;
            (now == recorded.digest)
                .then(|| std::fs::read_to_string(dir.join("SKILL.md")).ok())
                .flatten()
        }),
        ScopeKind::Path => project_file(root, &scope.name),
        ScopeKind::Node => node_prompt(snapshot?, &scope.name),
        ScopeKind::McpTool | ScopeKind::Cli | ScopeKind::Other => None,
    }
}

/// The source of `scope` as the run at `run_dir` saw it, with the text kept
/// when it fits [`EXCERPT_MAX`]. `None` for a scope without a readable
/// source, or one the run's records no longer hold.
pub fn seen(
    root: &Path,
    run_dir: &Path,
    scope: &ProblemScope,
    manifest: Option<&RunExecutionManifest>,
    snapshot: Option<&Playbook>,
    scrubber: &Scrubber,
) -> Option<ProblemSource> {
    let kind = SourceKind::of(scope.kind)?;
    let text = seen_text(root, run_dir, scope, manifest, snapshot)?;
    let scrubbed = scrubber.scrub(&text);
    Some(ProblemSource {
        kind,
        name: scope.name.clone(),
        digest: digest(&text),
        excerpt: (scrubbed.chars().count() <= EXCERPT_MAX && scrubber.flags(&scrubbed).is_none())
            .then_some(scrubbed),
    })
}

/// The current version of playbook `id` of `scope` under `root`.
pub fn current_playbook(root: &Path, scope: Scope, id: &str) -> Option<Playbook> {
    let parent = match scope {
        Scope::Project => root.join(".apb"),
        Scope::Global => apb_core::store::global_playbooks_parent()?,
    };
    apb_core::registry::Registry::open_dir(&parent)
        .ok()?
        .load(id, None)
        .ok()
        .map(|l| l.playbook)
}

/// `scope`'s source now, scrubbed; `None` when it cannot be read (it may be
/// gone, which never closes a problem).
pub fn current(
    root: &Path,
    scope: &ProblemScope,
    playbook_scope: Scope,
    playbook_id: &str,
    scrubber: &Scrubber,
) -> Option<Current> {
    let text = match scope.kind {
        ScopeKind::Connector => {
            let (connector, function) = scope.connector_parts()?;
            let loaded = apb_core::connector::store::load(connector).ok()?;
            function_block(&loaded.yaml, function)?
        }
        ScopeKind::Skill => {
            std::fs::read_to_string(installed_skill(root, &scope.name)?.join("SKILL.md")).ok()?
        }
        ScopeKind::Path => project_file(root, &scope.name)?,
        ScopeKind::Node => node_prompt(
            &current_playbook(root, playbook_scope, playbook_id)?,
            &scope.name,
        )?,
        ScopeKind::McpTool | ScopeKind::Cli | ScopeKind::Other => return None,
    };
    let scrubbed = scrubber.scrub(&text);
    Some(Current {
        digest: digest(&text),
        flagged: scrubber.flags(&scrubbed).is_some(),
        text: scrubbed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_function_block_is_the_named_entry_only() {
        let yaml = "name: crm\nversion: 0.1.0\nfunctions:\n  - name: a\n    description: first\n  - name: b\n    description: second\n";
        let b = function_block(yaml, "b").unwrap();
        assert!(b.contains("second") && !b.contains("first"), "{b}");
        assert!(function_block(yaml, "c").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_project_file_never_leaves_the_root_or_follows_a_link() {
        // The root sits inside a parent that has `x.txt` next to it, so
        // `../x.txt` names a real file: only the guards refuse it.
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(parent.path().join("x.txt"), "outside").unwrap();
        std::fs::write(root.join("in.txt"), "inside").unwrap();
        std::os::unix::fs::symlink(parent.path().join("x.txt"), root.join("link.txt")).unwrap();
        assert_eq!(project_file(&root, "in.txt").as_deref(), Some("inside"));
        assert!(project_file(&root, "link.txt").is_none());
        assert!(root.join("../x.txt").is_file());
        assert!(project_file(&root, "../x.txt").is_none());
    }

    #[test]
    fn a_private_path_is_private_in_any_case() {
        for rel in [
            ".GIT/config",
            "x/.Env",
            ".APB/decisions.yaml",
            "deploy/Prod.ENV",
            "a\\.Git\\HEAD",
        ] {
            assert!(is_private(rel), "{rel}");
        }
        assert!(!is_private("scripts/build.sh"));
    }

    #[test]
    fn a_private_ignored_or_large_file_is_never_a_source() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        for (rel, text) in [
            (".env", "SESSION_KEY=abcdefghijklmnop"),
            ("config/.env.prod", "x"),
            ("deploy/prod.env", "x"),
            (".git/config", "x"),
            (".apb/decisions.yaml", "x"),
            ("big.txt", ""),
            ("ok.txt", "fine"),
        ] {
            let at = r.join(rel);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(&at, text).unwrap();
        }
        std::fs::write(r.join("big.txt"), vec![b'a'; MAX_SOURCE_BYTES as usize + 1]).unwrap();
        for rel in [
            ".env",
            "config/.env.prod",
            "deploy/prod.env",
            ".git/config",
            ".apb/decisions.yaml",
            "big.txt",
        ] {
            assert!(project_file(r, rel).is_none(), "{rel}");
        }
        assert_eq!(project_file(r, "ok.txt").as_deref(), Some("fine"));
        // A git-ignored file, in a checkout.
        let git = std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["init", "-q"])
            .status();
        if git.is_ok_and(|s| s.success()) {
            std::fs::write(r.join(".gitignore"), "local.txt\n").unwrap();
            std::fs::write(r.join("local.txt"), "kept out").unwrap();
            assert!(project_file(r, "local.txt").is_none());
            assert_eq!(project_file(r, "ok.txt").as_deref(), Some("fine"));
        }
    }
}
