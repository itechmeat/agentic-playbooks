//! The message a person receives about one transition. Code-generated from
//! the journal and the run's outcome, never from what an agent or a prompt
//! wrote: the playbook, what happened, what the person is asked to do, and
//! a link. The question text, the review prompt, a host task's prompt, a
//! wake's detail and agent output stay in the run. Everything is redacted
//! like any text that leaves the machine, and a review title, the one
//! string a playbook author writes, is reduced to plain text so a chat
//! target renders no markup from it.
//!
//! A command line in a notice comes only from the next-action renderer
//! ([`crate::next_action::command::render`]), for the notified run and the
//! very request the transition opened: the line carries the request seq,
//! a publish plan's nonce and the shell quoting every surface shares. A
//! notice whose request moved on (a review decided and another opened, a
//! notice sent late) prints no command, so it never answers a request it
//! was not about. The supervisor's actions have no command line.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::notify::Transition;

use super::transitions::TransitionId;
use crate::event::{Event, EventPayload};
use crate::next_action::command::{CliCommand, render};
use crate::next_action::{ActionCode, NextAction};
use crate::pause::kind::PauseBy;

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
        let actions = crate::next_action::observation::Observation::load(run_dir, run_id)
            .map(|obs| crate::next_action::next_actions(&obs).actions)
            .unwrap_or_default();
        Self::compose(root, run_dir, run_id, events, t, &actions)
    }

    /// [`Notice::build`] with the run's next actions as they read now.
    pub fn compose(
        root: &Path,
        run_dir: &Path,
        run_id: &str,
        events: &[Event],
        t: &TransitionId,
        actions: &[NextAction],
    ) -> Self {
        let playbook = playbook_name(events);
        let opened = events.iter().find(|e| e.seq == t.seq).map(|e| &e.payload);
        let command = command_for(actions, run_id, t, opened).map(|c| c.line());
        let (subject, mut lines) = match t.kind {
            Transition::Review => review(&playbook, run_id, opened, command),
            Transition::Question => question(&playbook, run_id, command),
            Transition::Paused => paused(&playbook, run_id, opened, command),
            Transition::Finished => finished(&playbook, run_id, run_dir, opened),
            Transition::HostTask => host_task(&playbook, run_id, opened, command),
            Transition::Supervisor => supervisor(&playbook, run_id, opened),
            Transition::Stalled => stalled(&playbook, run_id, events, t),
        };
        lines.push(match crate::people::link::run_link(root, run_id) {
            Some(url) => format!("Open the run: {url}"),
            None => format!("Details: `apb runs {run_id}`."),
        });
        let redactor = crate::run_redact::RunRedactor::for_run(root, run_dir);
        Notice {
            subject: redactor.redact_text(&subject),
            text: redactor.redact_text(&lines.join("\n")),
        }
    }

    /// The whole notice on one line, for a function that sends one line of
    /// plain text.
    pub fn one_line(&self) -> String {
        let body = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("{}. {body}", self.subject)
    }
}

/// A stalled node: which step stopped retrying, how many times the same
/// rejection came back and the rejection itself (the attempt's
/// `rejected_for`, first 200 characters).
fn stalled(
    playbook: &str,
    run_id: &str,
    events: &[Event],
    t: &TransitionId,
) -> (String, Vec<String>) {
    let opened = events.iter().find(|e| e.seq == t.seq).map(|e| &e.payload);
    let (node, fingerprint, reason) = match opened {
        Some(EventPayload::AttemptFinished {
            node,
            rejection_fingerprint,
            rejected_for,
            ..
        }) => (
            node.as_str(),
            rejection_fingerprint.clone(),
            rejected_for
                .as_deref()
                .unwrap_or("the same rejection")
                .chars()
                .take(200)
                .collect::<String>(),
        ),
        _ => ("?", None, "the same rejection".to_string()),
    };
    // The stall exit's own fold, over the journal through the transition's
    // event: what came later (a retry that succeeded) never changes the
    // count this notice reports.
    let through = events.partition_point(|e| e.seq <= t.seq);
    let count = fingerprint.as_deref().map_or(0, |fp| {
        crate::scheduler::stall_exit::streak::streak(
            &events[..through],
            crate::exec_key::ExecKey { node, item: None },
            fp,
        )
    });
    (
        format!("{playbook} stalled at {node}"),
        vec![
            format!(
                "Step {node} of run {run_id} of {playbook} stopped retrying: the same rejection came back {count} times in a row."
            ),
            format!("Reason: {reason}"),
        ],
    )
}

/// The command of the action of run `run_id` that answers exactly what
/// transition `t` opened (`opened` is its event): the review or publish
/// request of that seq, the question of that seq, the host task it
/// requested, the pause. `None` when the run has no such action any more,
/// and for the supervisor's actions and the run's end.
pub fn command_for(
    actions: &[NextAction],
    run_id: &str,
    t: &TransitionId,
    opened: Option<&EventPayload>,
) -> Option<CliCommand> {
    let request_seq = |a: &NextAction| a.target.request.as_ref().map(|r| r.seq);
    let task = match opened {
        Some(EventPayload::HostTaskRequested { task_id, .. }) => Some(task_id.as_str()),
        _ => None,
    };
    let answers = |a: &NextAction| match t.kind {
        Transition::Review => {
            matches!(a.code, ActionCode::DecideReview | ActionCode::DecidePublish)
                && request_seq(a) == Some(t.seq)
        }
        Transition::Question => {
            a.code == ActionCode::AnswerQuestion && request_seq(a) == Some(t.seq)
        }
        Transition::HostTask => {
            a.code == ActionCode::SubmitHostTask
                && task.is_some()
                && a.target.task_id.as_deref() == task
        }
        Transition::Paused => {
            matches!(a.code, ActionCode::ResumeRun | ActionCode::ResumeWithBudget)
        }
        Transition::Finished | Transition::Supervisor | Transition::Stalled => false,
    };
    actions
        .iter()
        .filter(|a| a.run_id == run_id)
        .find(|a| answers(a))
        .and_then(|a| render(a, &BTreeMap::new()))
}

/// `Decide in the dashboard, or run `<command>`.`, or the dashboard alone
/// when no command answers the request any more.
fn act_line(how: &str, command: Option<String>) -> String {
    match command {
        Some(cmd) => format!("{how} in the dashboard, or run `{cmd}`."),
        None => format!("{how} in the dashboard."),
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

fn review(
    playbook: &str,
    run: &str,
    opened: Option<&EventPayload>,
    command: Option<String>,
) -> (String, Vec<String>) {
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
            act_line("Decide", command),
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

fn question(playbook: &str, run: &str, command: Option<String>) -> (String, Vec<String>) {
    (
        format!("{playbook} asked you a question"),
        vec![
            format!("Run {run} of {playbook} is waiting for your answer to a question."),
            act_line("Read and answer it", command),
        ],
    )
}

fn paused(
    playbook: &str,
    run: &str,
    opened: Option<&EventPayload>,
    command: Option<String>,
) -> (String, Vec<String>) {
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
    lines.push(match (until, command) {
        (Some(_), Some(cmd)) => {
            format!("It resumes on its own when its wait is over; to resume it now, run `{cmd}`.")
        }
        (Some(_), None) => "It resumes on its own when its wait is over.".to_string(),
        (None, command) => act_line("Resume it", command),
    });
    (format!("{playbook} paused"), lines)
}

/// A host task the host has not submitted: the node it belongs to, never
/// its prompt.
fn host_task(
    playbook: &str,
    run: &str,
    opened: Option<&EventPayload>,
    command: Option<String>,
) -> (String, Vec<String>) {
    let node = match opened {
        Some(EventPayload::HostTaskRequested { node, .. }) => node.as_str(),
        _ => "",
    };
    let mut lines = vec![format!(
        "Run {run} of {playbook} is waiting for its host to carry out the task of step \"{node}\" and submit it."
    )];
    lines.push(match command {
        Some(cmd) => {
            format!("The host agent driving the run submits it; to submit it by hand, run `{cmd}`.")
        }
        None => "The host agent driving the run submits it.".to_string(),
    });
    (format!("{playbook} waits for its host"), lines)
}

/// A failed or timed-out node waiting for the run's supervisor: who is
/// expected, never the wake's detail. The supervisor's actions have no
/// command line.
fn supervisor(playbook: &str, run: &str, opened: Option<&EventPayload>) -> (String, Vec<String>) {
    let (node, what) = match opened {
        Some(EventPayload::WakeRaised { node, trigger, .. }) => (
            node.as_str(),
            match trigger {
                crate::event::WakeTrigger::NodeTimeout => "timed out",
                _ => "failed",
            },
        ),
        _ => ("", "failed"),
    };
    (
        format!("{playbook} waits for its supervisor"),
        vec![
            format!("Step \"{node}\" of run {run} of {playbook} {what}."),
            "The run waits for its supervisor to retry the step, continue from another one, or stop the run.".to_string(),
        ],
    )
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
