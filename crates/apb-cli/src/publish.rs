//! `apb publish`: the publish gate of a run. `show` prints the exact plan a
//! person consents to (the URL, the branch, the remote's state, the commits
//! with their tags, the nonce); `approve` and `skip` are thin wrappers over
//! the review channel, so the nonce check, the one-decision rule and the
//! refusal inside a run step live in one place (`apb_engine::review`).

use std::path::Path;
use std::process::ExitCode;

use apb_core::publish::{shell_word, short_sha as short};
use apb_engine::run_outcome::publish::{PlanView, RunPublish, commit_count};
use apb_engine::run_view::RunView;
use clap::Subcommand;

use crate::util::{QUESTION_TEXT_MAX, sanitize_for_terminal};

#[derive(Subcommand)]
pub(crate) enum PublishAction {
    /// Show the plan of a run's publish node: what would be pushed, where,
    /// over what the remote holds now, and the nonce a consent carries
    Show {
        run_id: String,
        /// The publish node; omit when the run has one, or one waiting
        #[arg(long)]
        node: Option<String>,
        /// Print JSON (the same object as MCP run_status `publish`)
        #[arg(long)]
        json: bool,
    },
    /// Consent to push exactly the plan `show` printed: pass its nonce
    Approve {
        run_id: String,
        #[arg(long)]
        node: Option<String>,
        /// The nonce `apb publish show` printed for the plan you checked
        #[arg(long)]
        nonce: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Decline the push: the node finishes `skipped` and nothing leaves
    Skip {
        run_id: String,
        #[arg(long)]
        node: Option<String>,
        #[arg(long, default_value = "")]
        note: String,
    },
}

pub(crate) fn publish_cmd(root: &Path, action: PublishAction) -> ExitCode {
    match action {
        PublishAction::Show { run_id, node, json } => show(root, &run_id, node.as_deref(), json),
        PublishAction::Approve {
            run_id,
            node,
            nonce,
            note,
        } => decide(
            root,
            &run_id,
            node.as_deref(),
            Decision {
                command: "publish approve",
                decision: apb_core::publish::OPTION_PUBLISH,
                note: &note,
                nonce: Some(&nonce),
            },
        ),
        PublishAction::Skip { run_id, node, note } => decide(
            root,
            &run_id,
            node.as_deref(),
            Decision {
                command: "publish skip",
                decision: apb_core::publish::OPTION_SKIP,
                note: &note,
                nonce: None,
            },
        ),
    }
}

/// The run's publish nodes, or the reason there are none to act on.
fn load(root: &Path, run_id: &str) -> Result<Vec<RunPublish>, String> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err("invalid run id".into());
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(format!("run `{run_id}` not found"));
    }
    let view = RunView::load(&run_dir, run_id).map_err(|e| e.to_string())?;
    Ok(view.publish())
}

/// The node `--node` names, else the one publish node, else the one that
/// waits for a consent.
fn pick<'a>(nodes: &'a [RunPublish], want: Option<&str>) -> Result<&'a RunPublish, String> {
    let names = || {
        nodes
            .iter()
            .map(|p| p.node.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(want) = want {
        return nodes
            .iter()
            .find(|p| p.node == want)
            .ok_or_else(|| format!("the run has no publish node `{want}`"));
    }
    match nodes {
        [] => Err("the run has no publish node that planned or pinned a target".into()),
        [one] => Ok(one),
        _ => {
            let mut waiting = nodes.iter().filter(|p| p.awaiting_consent);
            match (waiting.next(), waiting.next()) {
                (Some(p), None) => Ok(p),
                _ => Err(format!("pass --node, one of: {}", names())),
            }
        }
    }
}

fn show(root: &Path, run_id: &str, node: Option<&str>, json: bool) -> ExitCode {
    let picked = load(root, run_id).and_then(|nodes| pick(&nodes, node).cloned());
    let p = match picked {
        Ok(p) => p,
        Err(e) => return fail("publish show", &e),
    };
    if json {
        return match serde_json::to_value(&p) {
            Ok(v) => {
                crate::util::print_json(&v);
                ExitCode::SUCCESS
            }
            Err(e) => fail("publish show", &e.to_string()),
        };
    }
    for line in show_lines(run_id, &p) {
        // The indent is ours; only the text after it comes from the run.
        let text = line.trim_start();
        let indent = &line[..line.len() - text.len()];
        println!(
            "{indent}{}",
            sanitize_for_terminal(text, QUESTION_TEXT_MAX * 3)
        );
    }
    ExitCode::SUCCESS
}

/// The tags of one commit: whether this run made it, and its trailers.
pub(crate) fn commit_tags(c: &apb_engine::event::PublishCommit) -> Vec<String> {
    let mut tags = vec![
        if c.this_run {
            "this run"
        } else {
            "before the run"
        }
        .to_string(),
    ];
    for r in c.commit.apb_run.iter().flatten() {
        tags.push(format!("Apb-Run: {r}"));
    }
    tags
}

fn plan_lines(run_id: &str, node: &str, plan: &PlanView, waiting: bool) -> Vec<String> {
    let mut out = vec![
        format!("  push: {} to branch {}", plan.after, plan.branch),
        format!("  url: {}", plan.url),
        match &plan.remote_before {
            Some(b) => format!("  remote: branch {} is at {b}", plan.branch),
            None => format!("  remote: branch {} does not exist yet", plan.branch),
        },
        format!("  commits: {}", commit_count(plan)),
    ];
    for c in &plan.commits {
        out.push(format!(
            "    {} {} [{}]",
            short(&c.commit.sha),
            c.commit.subject,
            commit_tags(c).join("] [")
        ));
    }
    if plan.omitted > 0 {
        out.push(format!("    and {} more", plan.omitted));
    }
    if plan.outside_push_suspected {
        out.push(
            "  warning: commits of this run are already on the remote: something pushed them outside the publish step"
                .into(),
        );
    }
    if waiting {
        let (run_id, node) = (shell_word(run_id), shell_word(node));
        out.push(format!("  nonce: {}", plan.nonce));
        out.push(format!(
            "  approve: apb publish approve {run_id} --node {node} --nonce {}",
            plan.nonce
        ));
        out.push(format!("  skip: apb publish skip {run_id} --node {node}"));
    }
    out
}

/// The text `apb publish show` prints for one node.
fn show_lines(run_id: &str, p: &RunPublish) -> Vec<String> {
    let mut out = vec![format!("publish node `{}` of run {run_id}", p.node)];
    match (&p.url, &p.pin_refusal) {
        (_, Some(code)) => out.push(format!("  target: not pinned ({code})")),
        (Some(url), None) => out.push(format!("  target: remote {} ({url})", p.remote)),
        (None, None) => {}
    }
    match &p.plan {
        Some(plan) => out.extend(plan_lines(run_id, &p.node, plan, p.awaiting_consent)),
        None => out.push("  no plan yet".into()),
    }
    if let Some(push) = &p.push {
        let detail = push
            .detail
            .as_deref()
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        out.push(format!(
            "  pushed: {} of {} to branch {}: {}{detail}",
            push.intent, push.after, push.branch, push.state
        ));
        if p.needs_a_look() {
            out.push(format!(
                "  needs a look: check the remote, then: apb intent resolve {} {} --sent|--not-sent",
                shell_word(run_id),
                shell_word(&push.intent)
            ));
        }
    }
    if let Some(r) = &p.result {
        out.push(format!("  result: {r}"));
    }
    out
}

/// One decision of `approve` or `skip`.
struct Decision<'a> {
    /// The subcommand, for error messages.
    command: &'static str,
    decision: &'static str,
    note: &'a str,
    nonce: Option<&'a str>,
}

/// Posts the decision to the run's publish node: `--node` too must name a
/// publish node of the run, so a `skip` never reaches a `human_review` gate
/// that happens to offer one.
fn decide(root: &Path, run_id: &str, node: Option<&str>, d: Decision<'_>) -> ExitCode {
    let node = match load(root, run_id).and_then(|nodes| pick(&nodes, node).map(|p| p.node.clone()))
    {
        Ok(n) => n,
        Err(e) => return fail(d.command, &e),
    };
    crate::run::review_cmd(root, run_id, &node, d.decision, d.note, d.nonce)
}

fn fail(what: &str, e: &str) -> ExitCode {
    eprintln!(
        "{what}: {}",
        sanitize_for_terminal(e, QUESTION_TEXT_MAX * 2)
    );
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_engine::event::{CommittedArtifact, PublishCommit};

    fn commit(sha: &str, this_run: bool, runs: &[&str]) -> PublishCommit {
        PublishCommit {
            commit: CommittedArtifact {
                sha: sha.into(),
                subject: format!("subject {sha}"),
                apb_run: Some(runs.iter().map(|r| r.to_string()).collect()),
            },
            this_run,
        }
    }

    fn view(waiting: bool) -> RunPublish {
        RunPublish {
            node: "publish".into(),
            remote: "origin".into(),
            url: Some("file:///srv/r.git".into()),
            pin_refusal: None,
            plan: Some(PlanView {
                url: "file:///srv/r.git".into(),
                branch: "apb/out".into(),
                after: "b".repeat(40),
                remote_before: Some("a".repeat(40)),
                commits: vec![commit("bbbb", true, &["r1"]), commit("cccc", false, &[])],
                omitted: 2,
                this_run: 3,
                outside_push_suspected: true,
                plan_digest: "sha256:x".into(),
                nonce: "publish-0123".into(),
            }),
            awaiting_consent: waiting,
            push: None,
            result: None,
        }
    }

    #[test]
    fn show_prints_the_exact_plan_with_tags_and_the_nonce_only_while_waiting() {
        let text = show_lines("r1", &view(true)).join("\n");
        for want in [
            "push: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb to branch apb/out",
            "url: file:///srv/r.git",
            "remote: branch apb/out is at aaaa",
            "commits: 4 commit(s), 3 made by this run",
            "bbbb subject bbbb [this run] [Apb-Run: r1]",
            "cccc subject cccc [before the run]",
            "and 2 more",
            "already on the remote",
            "approve: apb publish approve r1 --node publish --nonce publish-0123",
        ] {
            assert!(text.contains(want), "missing `{want}` in:\n{text}");
        }
        let done = show_lines("r1", &view(false)).join("\n");
        assert!(!done.contains("publish-0123"), "{done}");
        // A node id with a space or `;` stays one shell word.
        let mut odd = view(true);
        odd.node = "ship it;x".into();
        let text = show_lines("r1", &odd).join("\n");
        assert!(
            text.contains("apb publish approve r1 --node 'ship it;x' --nonce publish-0123"),
            "{text}"
        );
    }

    #[test]
    fn pick_takes_the_one_node_or_the_one_waiting() {
        let one = [view(false)];
        assert_eq!(pick(&one, None).unwrap().node, "publish");
        let mut other = view(false);
        other.node = "release".into();
        let two = [view(true), other.clone()];
        assert_eq!(pick(&two, None).unwrap().node, "publish");
        let none_waiting = [view(false), other];
        assert!(pick(&none_waiting, None).unwrap_err().contains("--node"));
        assert!(pick(&one, Some("ghost")).is_err());
    }
}
