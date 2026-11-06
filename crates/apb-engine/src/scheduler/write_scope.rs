//! The declared write scope (spec node-contracts D3): one resolution per
//! execution, the attempt prompt's note, and the after-attempt check that
//! reuses the read-only tree comparison. A violation, or a comparison a
//! limit cut short, fails the node at once, with no retry.

use super::read_only::Before;
use crate::error::EngineError;
use apb_core::fingerprint::{ChangeKind, TreeChange};
use apb_core::schema::{Playbook, WriteScope};
use apb_core::write_scope::RenderProblem;
use std::path::Path;

pub(crate) const WRITE_SCOPE: &str = "write_scope";
/// The most changed paths a message or the evidence names, after the count.
pub(crate) const MAX_LISTED: usize = 5;

/// The node's scope for one execution.
#[derive(Debug)]
pub(crate) struct ResolvedScope {
    /// The globs the attempt may change; `None` when only
    /// `max_changed_files` is declared (every path counts).
    pub globs: Option<Vec<String>>,
    pub max: Option<u32>,
    set: Option<globset::GlobSet>,
}

impl ResolvedScope {
    /// Whether `path` (tree-relative) is in scope.
    fn matches(&self, path: &str) -> bool {
        match &self.set {
            None => true,
            Some(set) => set.is_match(path),
        }
    }
}

/// The node's scope for this execution, `Ok(Err(message))` when the node
/// must not start, `Ok(Ok(None))` when it declares neither key.
pub(crate) fn resolve(
    playbook: &Playbook,
    node_id: &str,
    render: impl FnOnce(&str) -> Result<String, EngineError>,
) -> Result<Result<Option<ResolvedScope>, String>, EngineError> {
    let Some(node) = playbook.node(node_id) else {
        return Ok(Ok(None));
    };
    let max = node.max_changed_files;
    let Some(scope) = &node.writes else {
        return Ok(Ok(max.map(|max| ResolvedScope {
            globs: None,
            max: Some(max),
            set: None,
        })));
    };
    let refused = |what: String| {
        Ok(Err(format!(
            "node `{node_id}` writes {what}, so the node was not started"
        )))
    };
    match scope {
        // The globs form: validation (V170) refuses a bad entry; this is the
        // engine's own refusal when validation was skipped.
        WriteScope::Globs(globs) => {
            if globs.is_empty() {
                return refused("an empty list".to_string());
            }
            if let Some(problem) = apb_core::write_scope::first_entry_problem(globs) {
                return refused(entry_words(&problem, None));
            }
            resolved(globs.clone(), max).map_or_else(refused, |s| Ok(Ok(Some(s))))
        }
        WriteScope::Template(template) => {
            if apb_core::write_scope::template_ref(template).is_none() {
                return refused(format!(
                    "`{template}` is not exactly one `{{{{...}}}}` reference"
                ));
            }
            let rendered = render(template)?;
            match apb_core::write_scope::parse_rendered(&rendered) {
                Ok(globs) => resolved(globs, max).map_or_else(refused, |s| Ok(Ok(Some(s)))),
                Err(problem) => refused(entry_words(&problem, Some(template))),
            }
        }
    }
}

/// What a refusal says about the scope: `problem` names the entry or the
/// rendered value; `template` is the template the entries came from.
fn entry_words(problem: &RenderProblem, template: Option<&str>) -> String {
    let from = template
        .map(|t| format!(" (from `{t}`)"))
        .unwrap_or_default();
    let named = template.map(|t| format!("`{t}` ")).unwrap_or_default();
    match problem {
        RenderProblem::Empty => {
            format!("{named}rendered empty: the value it reads was never published")
        }
        RenderProblem::EmptyList => {
            format!("{named}rendered an empty list: a scope that names no file allows no change")
        }
        RenderProblem::Whole(why) => format!("{named}cannot be read as a scope: {why}"),
        RenderProblem::Entry {
            entry,
            outside: true,
            ..
        } => format!("entry `{entry}`{from} is not a path inside the node's directory"),
        RenderProblem::Entry {
            entry,
            problem,
            outside: false,
        } => format!("entry `{entry}`{from} is {problem}"),
    }
}

/// The scope of `globs`, or why they do not build one: a glob the matcher
/// refuses must never turn the scope off.
fn resolved(globs: Vec<String>, max: Option<u32>) -> Result<ResolvedScope, String> {
    let set = apb_core::validate::build_globset(&globs)
        .map_err(|e| format!("globs that do not build a matcher ({e})"))?;
    Ok(ResolvedScope {
        globs: Some(globs),
        max,
        set: Some(set),
    })
}

/// The sentence the attempt prompt gets on its first turn; the parts are
/// present only when declared.
pub(crate) fn note(scope: &ResolvedScope) -> String {
    let mut what = String::new();
    if let Some(globs) = &scope.globs {
        let named: Vec<String> = globs.iter().map(|g| format!("`{g}`")).collect();
        what.push_str(&format!("change only files matching {}", named.join(", ")));
    }
    if let Some(max) = scope.max {
        if !what.is_empty() {
            what.push_str(", and ");
        }
        what.push_str(&format!("at most {max} files in all"));
    }
    format!(
        "Write scope: {what}. apb compares the tree after the attempt; a change outside the scope, or more files, fails the step without a retry."
    )
}

/// The file changes the scope counts: every kind but the repository state,
/// which is not a file and a commit of an in-scope file is in scope.
fn file_changes(changes: &[TreeChange]) -> Vec<&TreeChange> {
    changes
        .iter()
        .filter(|c| {
            !matches!(
                c.kind,
                ChangeKind::GitHead
                    | ChangeKind::GitRefs
                    | ChangeKind::GitIndex
                    | ChangeKind::GitControl
            )
        })
        .collect()
}

/// The after-attempt check: what the tree says about the scope. Returns the
/// violation message when the node must fail; the evidence file is written
/// either way. The `Before` is taken with `peers: None`: a scoped node
/// never shares a concurrent batch with another writer.
pub(crate) fn finish(
    before: &Before,
    scope: &ResolvedScope,
    attempt: u32,
    run_dir: &Path,
    project: &Path,
) -> Result<Option<String>, EngineError> {
    let compared = before.compare();
    let partial = compared.partial();
    let changes = file_changes(&compared.changes);
    let (own, outside): (Vec<&TreeChange>, Vec<&TreeChange>) =
        changes.into_iter().partition(|c| scope.matches(&c.path));

    let mut violations: Vec<String> = Vec::new();
    if scope.globs.is_some() && !outside.is_empty() {
        violations.push(format!(
            "changed {} file{} outside its write scope: {}",
            outside.len(),
            if outside.len() == 1 { "" } else { "s" },
            first_and_count(&outside)
        ));
    }
    if let Some(max) = scope.max
        && own.len() > max as usize
    {
        violations.push(format!(
            "changed {} file{}, more than its max_changed_files {max}: {}",
            own.len(),
            if own.len() == 1 { "" } else { "s" },
            first_and_count(&own)
        ));
    }
    write_evidence(
        before, scope, attempt, run_dir, project, &compared, partial, &own, &outside,
    )?;
    let node = &before.node;
    let message = if let Some(limit) = partial {
        format!(
            "{WRITE_SCOPE}: node `{node}`'s tree is too large to compare whole ({}), so its write scope cannot be shown to hold",
            super::read_only::limit_words(limit)
        )
    } else if violations.is_empty() {
        return Ok(None);
    } else {
        let (head, tail) = violations.split_at(1);
        format!(
            "{WRITE_SCOPE}: node `{node}` {}{}",
            head[0],
            tail.iter().map(|v| format!("; {v}")).collect::<String>()
        )
    };
    Ok(Some(message))
}

/// The first changed path and the count of the others: `a.rs modified
/// (and 2 more)`.
fn first_and_count(changes: &[&TreeChange]) -> String {
    let first = changes[0];
    let more = match changes.len() {
        1 => String::new(),
        n => format!(" (and {} more)", n - 1),
    };
    format!("{} {}{more}", first.path, first.kind.as_str())
}

/// The evidence of one scope check: `runs/<id>/writes/<stem>/<n>.json`
/// (0600), the resolved scope and the scrubbed paths, never content.
#[allow(clippy::too_many_arguments)]
fn write_evidence(
    before: &Before,
    scope: &ResolvedScope,
    attempt: u32,
    run_dir: &Path,
    project: &Path,
    compared: &super::read_only::Comparison,
    partial: Option<crate::event::ReadOnlyLimit>,
    own: &[&TreeChange],
    outside: &[&TreeChange],
) -> Result<(), EngineError> {
    let scrubber = apb_core::scrub::Scrubber::for_workspace(project, Vec::new());
    let scrub = |changes: &[&TreeChange]| -> Vec<serde_json::Value> {
        changes
            .iter()
            .take(MAX_LISTED)
            .map(|c| serde_json::json!({ "path": scrubber.scrub(&c.path), "change": c.kind.as_str() }))
            .collect()
    };
    let body = serde_json::json!({
        "node": before.node,
        "attempt": attempt,
        "scope": scope.globs,
        "max": scope.max,
        "own": scrub(own),
        "outside": scrub(outside),
        "partial": partial,
        "after_partial": compared.after_partial,
        "clean": own.is_empty() && outside.is_empty() && partial.is_none(),
    });
    let dir = run_dir
        .join("writes")
        .join(super::read_only::evidence_stem(&before.node));
    apb_core::fsutil::create_dir_under(run_dir, &dir)?;
    let n = super::read_only::next_ordinal(&dir);
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(|e| EngineError::Invalid(format!("{WRITE_SCOPE} evidence: {e}")))?;
    let path = dir.join(format!("{n}.json"));
    apb_core::fsutil::atomic_write_private_under(run_dir, &path, &bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(globs: Vec<String>, max: Option<u32>) -> ResolvedScope {
        resolved(globs, max).unwrap()
    }

    fn playbook_with(node_extra: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: ws\nname: WS\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: hi{node_extra} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: done }}\n"
        ))
        .unwrap()
    }

    // --- resolve -----------------------------------------------------------

    #[test]
    fn resolve_gives_none_without_the_keys() {
        let pb = playbook_with("");
        let r = resolve(&pb, "w", |_| unreachable!()).unwrap();
        assert!(r.unwrap().is_none());
    }

    #[test]
    fn resolve_globs_and_the_max_only_form() {
        let pb = playbook_with(", writes: [\"src/**\"], max_changed_files: 5");
        let r = resolve(&pb, "w", |_| unreachable!())
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(r.globs.as_deref(), Some(&["src/**".to_string()][..]));
        assert_eq!(r.max, Some(5));
        let pb = playbook_with(", max_changed_files: 2");
        let r = resolve(&pb, "w", |_| unreachable!())
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(r.globs.is_none());
        assert_eq!(r.max, Some(2));
    }

    /// Every render the node must not start on is refused with its
    /// sentence, before any attempt.
    #[test]
    fn resolve_renders_a_template_once_and_refuses_bad_renders() {
        let pb = playbook_with(", writes: \"{{nodes.p.output.files}}\"");
        let good = resolve(&pb, "w", |_| Ok(r#"["src/a.rs"]"#.to_string()))
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(good.globs.as_deref(), Some(&["src/a.rs".to_string()][..]));
        let refused = |rendered: &str| {
            let rendered = rendered.to_string();
            resolve(&pb, "w", move |_| Ok(rendered))
                .unwrap()
                .unwrap_err()
        };
        assert_eq!(
            refused(""),
            "node `w` writes `{{nodes.p.output.files}}` rendered empty: the value it reads was never published, so the node was not started"
        );
        assert_eq!(
            refused("[]"),
            "node `w` writes `{{nodes.p.output.files}}` rendered an empty list: a scope that names no file allows no change, so the node was not started"
        );
        assert_eq!(
            refused("[\"src/a.rs\", \"../x\"]"),
            "node `w` writes entry `../x` (from `{{nodes.p.output.files}}`) is not a path inside the node's directory, so the node was not started"
        );
        assert_eq!(
            refused("[\"[\"]"),
            "node `w` writes entry `[` (from `{{nodes.p.output.files}}`) is not a valid glob, so the node was not started"
        );
    }

    /// With validation skipped, a glob-form scope the validator would
    /// refuse is refused here: a bad entry never turns the scope off.
    #[test]
    fn resolve_refuses_a_bad_glob_form_without_validation() {
        let refused = |extra: &str| {
            resolve(&playbook_with(extra), "w", |_| unreachable!())
                .unwrap()
                .unwrap_err()
        };
        assert_eq!(
            refused(", writes: [\"[\"]"),
            "node `w` writes entry `[` is not a valid glob, so the node was not started"
        );
        assert_eq!(
            refused(", writes: []"),
            "node `w` writes an empty list, so the node was not started"
        );
    }

    // --- note ----------------------------------------------------------------

    #[test]
    fn the_note_names_what_is_declared() {
        let both = note(&scope(vec!["src/**".into(), "tests/**".into()], Some(5)));
        assert!(
            both.contains(
                "change only files matching `src/**`, `tests/**`, and at most 5 files in all"
            ),
            "{both}"
        );
        assert!(both.contains("fails the step without a retry"));
        let globs_only = note(&scope(vec!["src/**".into()], None));
        assert!(!globs_only.contains("at most"));
        let max_only = note(&ResolvedScope {
            globs: None,
            max: Some(5),
            set: None,
        });
        assert!(max_only.contains("at most 5 files in all"), "{max_only}");
        assert!(!max_only.contains("matching"));
    }

    // --- finish ----------------------------------------------------------------

    /// A tree with `src/a.rs` and `docs/b.md`, its before state, then an
    /// in-scope change and two outside ones.
    fn changed_tree() -> (tempfile::TempDir, Before) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "a\n").unwrap();
        std::fs::write(dir.path().join("docs/b.md"), "b\n").unwrap();
        let before = Before::take("w", 1, dir.path(), None);
        std::fs::write(dir.path().join("src/a.rs"), "a2\n").unwrap();
        std::fs::write(dir.path().join("docs/b.md"), "b2\n").unwrap();
        std::fs::write(dir.path().join("docs/c.md"), "c\n").unwrap();
        (dir, before)
    }

    /// A change outside the globs and a count over the max fail with their
    /// sentences, joined; the evidence names the own and outside paths.
    #[test]
    fn finish_names_outside_changes_and_a_count_over_the_max() {
        let (dir, before) = changed_tree();
        let run = tempfile::tempdir().unwrap();
        let globs = scope(vec!["src/**".into()], None);
        assert_eq!(
            finish(&before, &globs, 1, run.path(), dir.path()).unwrap(),
            Some(
                "write_scope: node `w` changed 2 files outside its write scope: docs/b.md modified (and 1 more)"
                    .to_string()
            )
        );
        let evidence: serde_json::Value =
            serde_json::from_slice(&std::fs::read(run.path().join("writes/w/1.json")).unwrap())
                .unwrap();
        assert_eq!(evidence["own"][0]["path"], "src/a.rs");
        assert_eq!(evidence["outside"].as_array().unwrap().len(), 2);
        assert_eq!(evidence["clean"], false);

        let both = scope(vec!["src/**".into()], Some(0));
        let max_only = ResolvedScope {
            globs: None,
            max: Some(1),
            set: None,
        };
        assert_eq!(
            finish(&before, &max_only, 1, run.path(), dir.path()).unwrap(),
            Some(
                "write_scope: node `w` changed 3 files, more than its max_changed_files 1: docs/b.md modified (and 2 more)"
                    .to_string()
            )
        );
        assert_eq!(
            finish(&before, &both, 1, run.path(), dir.path()).unwrap(),
            Some(
                "write_scope: node `w` changed 2 files outside its write scope: docs/b.md modified (and 1 more); changed 1 file, more than its max_changed_files 0: src/a.rs modified"
                    .to_string()
            )
        );
        // Every check writes the next evidence file of the node.
        assert!(run.path().join("writes/w/3.json").exists());
    }

    /// Changes inside the scope and within the max pass.
    #[test]
    fn finish_passes_changes_inside_the_scope() {
        let (dir, before) = changed_tree();
        let run = tempfile::tempdir().unwrap();
        let wide = scope(vec!["src/**".into(), "docs/**".into()], Some(3));
        assert_eq!(
            finish(&before, &wide, 1, run.path(), dir.path()).unwrap(),
            None
        );
    }
}
