//! What `apb setup plan` reports for information only: the work trackers a
//! project uses and the optional review helpers a person may install. apb
//! depends on none of them and setup never installs anything: the process
//! needs only the project's own checks and review by agents, and a tool the
//! project declares among its own checks is that project's requirement.
//! Detection reads files and PATH; the one program it starts is a review
//! helper found on PATH, asked for its version the way agent detection
//! probes an agent (`apb_core::detect::probe_output`: sanitized
//! environment, a time limit, an output limit).

use std::path::{Path, PathBuf};

use serde::Serialize;

/// One work tracker seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrackerSeen {
    /// `beads`, `github` or `connector`.
    pub kind: String,
    /// `in_project` when the project itself shows it (a `.beads/`
    /// directory, a git remote, a connector account), `available` when it
    /// is only a program on this machine (`bd` on PATH), which says nothing
    /// about this project.
    pub status: &'static str,
    /// What was seen.
    pub evidence: String,
}

/// [`TrackerSeen::status`] of a tracker the project shows.
pub const IN_PROJECT: &str = "in_project";
/// [`TrackerSeen::status`] of a tracker program on this machine only.
pub const AVAILABLE: &str = "available";

/// One optional helper: what it is for and how to tell it is there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Suggestion {
    /// The role it plays in review.
    pub role: &'static str,
    pub why: &'static str,
    /// How to check that one is installed.
    pub check: &'static str,
    /// Tools that fill the role, named only as examples.
    pub examples: &'static [&'static str],
    /// The examples found on PATH that answered their version check.
    pub found: Vec<String>,
}

/// The helpers worth knowing about: the plan marks one found on PATH and
/// answering its version check as required for review; a missing one, or
/// one that does not answer as itself, stays optional.
const HELPERS: &[(&str, &str, &str, &[&str])] = &[
    (
        "code_review_cli",
        "a second, independent reading of a diff beside the agents' review",
        "the tool's --version answers, and it reviews a local diff without uploading the repository unless the person agrees",
        &["coderabbit", "ocr"],
    ),
    (
        "structural_checker",
        "flags dependency cycles, complexity and structural rules a review may miss",
        "the tool's --version answers, and it runs on the project from a terminal",
        &["code-ranker", "semgrep", "ast-grep"],
    ),
];

/// Helper names other programs share, with the text the helper's own
/// `--version` answer holds. Any other helper answers its version check by
/// running it successfully.
const SIGNATURES: &[(&str, &str)] = &[("ocr", "open-code-review")];

/// Whether `name` is an executable in one of the directories of `path`.
fn on_path(path: Option<&std::ffi::OsStr>, name: &str) -> bool {
    find_on_path(path, name, &[]).is_some()
}

/// The executable `name` in the first directory of `path` that has one,
/// canonical. A relative entry (an empty one or `.`, which a shell reads as
/// the current directory) is skipped, and so is a program whose canonical
/// path lies inside one of `untrusted` (the checkout and the project,
/// [`untrusted_dirs`]), as agent detection skips one inside the working
/// directory: a checkout's own program is never taken for a helper.
fn find_on_path(
    path: Option<&std::ffi::OsStr>,
    name: &str,
    untrusted: &[PathBuf],
) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .filter(|p| is_executable(p))
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .find(|p| !untrusted.iter().any(|d| p.starts_with(d)))
}

/// The directories a helper may not come from, canonical: the working
/// directory, `root`, the project, and the git checkout each sits in (its
/// root, and the main checkout of a linked worktree). A checkout rooted at
/// the home directory (a dotfiles repository) is the operator's own and
/// stays allowed.
fn untrusted_dirs(root: &Path, project: Option<&Path>) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").and_then(|h| std::fs::canonicalize(h).ok());
    let cwd = std::env::current_dir().ok();
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in [cwd.as_deref(), Some(root), project].into_iter().flatten() {
        let Ok(dir) = std::fs::canonicalize(dir) else {
            continue;
        };
        let checkout = dir
            .ancestors()
            .find(|a| a.join(".git").exists())
            .map(Path::to_path_buf);
        let main = checkout
            .as_deref()
            .and_then(git_config)
            .and_then(|c| std::fs::canonicalize(c.parent()?.parent()?).ok());
        for d in [Some(dir), checkout, main].into_iter().flatten() {
            if home.as_ref() != Some(&d) && !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

/// Whether helper `name` is on `path`, outside `untrusted`, and answers its
/// version check: it runs successfully, and for a shared name its answer
/// holds the helper's signature ([`SIGNATURES`]).
fn answers_as_helper(path: Option<&std::ffi::OsStr>, name: &str, untrusted: &[PathBuf]) -> bool {
    let Some(program) = find_on_path(path, name, untrusted) else {
        return false;
    };
    let Ok(answer) = apb_core::detect::probe_output(&program, &["--version".to_string()]) else {
        return false;
    };
    match SIGNATURES.iter().find(|(n, _)| *n == name) {
        Some((_, signature)) => answer.to_lowercase().contains(signature),
        None => true,
    }
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// The optional review helpers, with the examples found on PATH, outside
/// the checkout of `root` and of `project`, that answer their version
/// check.
pub fn suggestions(root: &Path, project: Option<&Path>) -> Vec<Suggestion> {
    suggestions_on(
        std::env::var_os("PATH").as_deref(),
        &untrusted_dirs(root, project),
    )
}

/// The optional review helpers, with the examples found in `path`, outside
/// `untrusted`, that answer their version check.
fn suggestions_on(path: Option<&std::ffi::OsStr>, untrusted: &[PathBuf]) -> Vec<Suggestion> {
    HELPERS
        .iter()
        .map(|(role, why, check, examples)| Suggestion {
            role,
            why,
            check,
            examples,
            found: examples
                .iter()
                .filter(|e| answers_as_helper(path, e, untrusted))
                .map(|e| e.to_string())
                .collect(),
        })
        .collect()
}

/// The git config of the repository at `dir`: `.git/config`, or, for a
/// linked worktree (`.git` is a file), the common directory's.
fn git_config(dir: &Path) -> Option<PathBuf> {
    let dot = dir.join(".git");
    if dot.is_dir() {
        return Some(dot.join("config"));
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let gitdir = dir.join(text.trim().strip_prefix("gitdir:")?.trim());
    let common = std::fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .map(|c| gitdir.join(c.trim()))
        .unwrap_or(gitdir);
    Some(common.join("config"))
}

/// The remote URLs of the git config `text` that point at GitHub.
fn github_remotes(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("url"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .map(str::trim)
        .filter(|u| u.contains("github.com"))
        .map(str::to_string)
        .collect()
}

/// An installed connector with an account and an issue function.
fn issue_connectors(root: &Path) -> Vec<TrackerSeen> {
    let mut out = Vec::new();
    for summary in apb_core::connector::store::list() {
        let Ok(loaded) = apb_core::connector::store::load(&summary.name) else {
            continue;
        };
        let issues = loaded
            .doc
            .functions
            .iter()
            .any(|f| f.name.contains("issue"));
        let accounts = apb_core::connector::config::load_merged(root, &summary.name)
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if issues && accounts {
            out.push(TrackerSeen {
                kind: "connector".to_string(),
                status: IN_PROJECT,
                evidence: format!(
                    "the connector {} has an account and issue functions",
                    summary.name
                ),
            });
        }
    }
    out
}

/// The work trackers seen for the project `project` (else `root`): a
/// `.beads/` directory, a GitHub remote, a configured issue connector, and
/// `bd` on PATH as only [`AVAILABLE`]. Information only.
pub fn trackers(root: &Path, project: Option<&Path>) -> Vec<TrackerSeen> {
    let dir = project.unwrap_or(root);
    let mut out = Vec::new();
    if dir.join(".beads").is_dir() {
        out.push(TrackerSeen {
            kind: "beads".to_string(),
            status: IN_PROJECT,
            evidence: format!("{} has a .beads directory", dir.display()),
        });
    } else if on_path(std::env::var_os("PATH").as_deref(), "bd") {
        out.push(TrackerSeen {
            kind: "beads".to_string(),
            status: AVAILABLE,
            evidence: "bd is on PATH; this project has no .beads directory".to_string(),
        });
    }
    if let Some(text) = git_config(dir).and_then(|p| std::fs::read_to_string(p).ok()) {
        for url in github_remotes(&text) {
            out.push(TrackerSeen {
                kind: "github".to_string(),
                status: IN_PROJECT,
                evidence: format!("a git remote points at GitHub: {url}"),
            });
        }
    }
    out.extend(issue_connectors(root));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_github_remote_is_reported_once_beside_beads() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join(".beads")).unwrap();
        std::fs::create_dir_all(t.path().join(".git")).unwrap();
        std::fs::write(
            t.path().join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:o/r.git\n[remote \"x\"]\n\turl = https://example.org/r\n",
        )
        .unwrap();
        let seen = trackers(t.path(), None);
        let kinds: Vec<&str> = seen.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"beads"), "{seen:?}");
        assert_eq!(
            kinds.iter().filter(|k| **k == "github").count(),
            1,
            "{seen:?}"
        );
    }

    #[test]
    fn a_linked_worktree_reads_the_common_config() {
        let t = tempfile::tempdir().unwrap();
        let main = t.path().join("main");
        let wt = t.path().join("wt");
        std::fs::create_dir_all(main.join(".git/worktrees/wt")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(main.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        let config = git_config(&wt).unwrap();
        assert_eq!(
            std::fs::canonicalize(config.parent().unwrap()).unwrap(),
            std::fs::canonicalize(main.join(".git")).unwrap()
        );
    }
    /// A helper counts as found (and the plan then requires it) only when it
    /// is an executable on PATH that answers its version check; a program of
    /// a generic name that answers as something else stays optional.
    #[cfg(unix)]
    #[test]
    fn a_found_helper_must_answer_its_version_check() {
        use std::os::unix::fs::PermissionsExt;
        let found_in = |tools: &[(&str, u32, &str)]| {
            let t = tempfile::tempdir().unwrap();
            for (name, mode, body) in tools {
                let p = t.path().join(name);
                std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(*mode)).unwrap();
            }
            let mut found: Vec<String> = suggestions_on(Some(t.path().as_os_str()), &[])
                .into_iter()
                .flat_map(|s| s.found)
                .collect();
            found.sort();
            found
        };
        assert_eq!(
            found_in(&[
                ("ocr", 0o755, "echo 'open-code-review v1.13.1'"),
                ("code-ranker", 0o755, "echo 'code-ranker 5.0.4'"),
            ]),
            ["code-ranker", "ocr"]
        );
        assert!(
            found_in(&[
                ("ocr", 0o755, "echo 'tesseract 5.3.4'"),
                ("code-ranker", 0o755, "exit 1"),
                ("semgrep", 0o644, "echo 1.0"),
            ])
            .is_empty()
        );
        assert!(suggestions_on(None, &[]).iter().all(|s| s.found.is_empty()));
    }
}
