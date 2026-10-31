//! The message a person receives about one transition. Code-generated from
//! the journal and the run's outcome, never from what an agent or a prompt
//! wrote: the playbook, what happened, what the person is asked to do, and
//! a link. The question text, the review prompt and agent output stay in
//! the run. Everything is redacted like any text that leaves the machine,
//! and a review title, the one string a playbook author writes, is reduced
//! to plain text so a chat target renders no markup from it.

use std::path::Path;

use apb_core::notify::Transition;

use super::transitions::TransitionId;
use crate::event::{Event, EventPayload};
use crate::pause::kind::PauseBy;
use crate::redact::Redactor;

/// One notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// A short line a mail subject or a chat topic shows.
    pub subject: String,
    /// The body: the conclusion first, then the action, then the link.
    pub text: String,
}

impl Notice {
    /// The notice of transition `t` of run `run_id`, whose journal is
    /// `events`, in the workspace at `root`.
    pub fn build(
        root: &Path,
        run_dir: &Path,
        run_id: &str,
        events: &[Event],
        t: &TransitionId,
    ) -> Self {
        let playbook = playbook_name(events);
        let opened = events.iter().find(|e| e.seq == t.seq).map(|e| &e.payload);
        let (subject, mut lines) = match t.kind {
            Transition::Review => review(&playbook, run_id, opened),
            Transition::Question => question(&playbook, run_id, opened),
            Transition::Paused => paused(&playbook, run_id, opened),
            Transition::Finished => finished(&playbook, run_id, run_dir, opened),
        };
        lines.push(match crate::people::link::run_link(root, run_id) {
            Some(url) => format!("Open the run: {url}"),
            None => format!("Details: `apb runs {run_id}`."),
        });
        let redactor = Redactor::for_workspace(root, Vec::new());
        Notice {
            subject: redactor.redact(&subject),
            text: redactor.redact(&lines.join("\n")),
        }
    }

    /// The whole notice on one line, for a function that sends one line of
    /// plain text.
    pub fn one_line(&self) -> String {
        let body = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("{}. {body}", self.subject)
    }
}

/// The playbook the run started, from its `run_started`.
fn playbook_name(events: &[Event]) -> String {
    events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunStarted { playbook, .. } => Some(playbook.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "a playbook".to_string())
}

fn review(playbook: &str, run: &str, opened: Option<&EventPayload>) -> (String, Vec<String>) {
    let (node, title) = match opened {
        Some(EventPayload::ReviewRequested { node, title, .. }) => {
            (node.as_str(), title.as_deref())
        }
        _ => ("", None),
    };
    let gate = title
        .map(plain)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| node.to_string());
    (
        format!("{playbook} needs your review"),
        vec![
            format!(
                "Run {run} of {playbook} is waiting for your decision at the review gate \"{gate}\"."
            ),
            format!(
                "Decide in the dashboard, or run `apb review {run} {node} --decision <option>`."
            ),
        ],
    )
}

/// A playbook author's title as plain text: the characters a chat service
/// reads as markup (emphasis, code, links, mentions, headings) become
/// spaces, so the title reads the same through every target.
fn plain(title: &str) -> String {
    title
        .chars()
        .map(|c| {
            if "*_~`[]()<>|#\\!@".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn question(playbook: &str, run: &str, opened: Option<&EventPayload>) -> (String, Vec<String>) {
    let node = match opened {
        Some(EventPayload::QuestionAsked { node, .. }) => node.as_str(),
        _ => "",
    };
    (
        format!("{playbook} asked you a question"),
        vec![
            format!("Run {run} of {playbook} is waiting for your answer to a question."),
            match node {
                "" => format!(
                    "Read and answer it in the dashboard, or run `apb answer {run} \"<answer>\"`."
                ),
                node => format!(
                    "Read and answer it in the dashboard, or run `apb answer {run} --node {node} \"<answer>\"`."
                ),
            },
        ],
    )
}

fn paused(playbook: &str, run: &str, opened: Option<&EventPayload>) -> (String, Vec<String>) {
    let (by, until) = match opened {
        Some(EventPayload::RunPaused { by, until, .. }) => {
            (by.unwrap_or(PauseBy::Supervisor), *until)
        }
        _ => (PauseBy::Engine, None),
    };
    let who = match by {
        PauseBy::Supervisor => "its supervisor",
        PauseBy::Engine | PauseBy::Operator => "apb",
    };
    let mut lines = vec![format!("Run {run} of {playbook} was paused by {who}.")];
    lines.push(match until {
        Some(_) => format!(
            "It resumes on its own when its wait is over; to resume it now, run `apb resume {run}`."
        ),
        None => format!("Resume it in the dashboard, or run `apb resume {run}`."),
    });
    (format!("{playbook} paused"), lines)
}

fn finished(
    playbook: &str,
    run: &str,
    run_dir: &Path,
    opened: Option<&EventPayload>,
) -> (String, Vec<String>) {
    let ended = match opened {
        Some(EventPayload::RunFinished { outcome }) => outcome.clone(),
        _ => "aborted".to_string(),
    };
    let headline = crate::run_view::RunView::load(run_dir, run)
        .map(|view| crate::run_outcome::OutcomeSummary::build(&view, run_dir).headline)
        .ok();
    let mut lines = vec![format!("Run {run} of {playbook} has ended: {ended}.")];
    lines.extend(headline.map(|h| format!("{h}.")));
    (format!("{playbook} {ended}"), lines)
}
