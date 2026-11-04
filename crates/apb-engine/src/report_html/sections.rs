//! The report's sections, each built from the model with fixed tags and
//! redacted, escaped text. An empty section says so in one line instead of
//! disappearing, so a reader can tell "none" from "not shown".

use super::markup::{Markup, SafeUrl};
use super::model::{ANSWER_MAX_BYTES, JOURNAL_LINE_MAX_BYTES, Report, Step};
use crate::event::Event;
use crate::notify::RunNotify;
use crate::redact::{Redactor, clip};
use crate::run_outcome::{AGENT_SUMMARY_TITLE, OutcomeSummary, RunGoal, RunIntent};
use crate::state::RunStatus;

/// Turns run text into markup: redacted, then escaped.
pub(super) struct Writer {
    redactor: Redactor,
}

impl Writer {
    pub(super) fn new(redactor: Redactor) -> Self {
        Writer { redactor }
    }

    pub(super) fn text(&self, text: &str) -> Markup {
        Markup::text(&self.redactor.redact(text))
    }

    /// `text` redacted whole, then cut to at most `max` chars, `...`
    /// appended when cut. The graph's labels use it: the cut never splits
    /// an escape, because escaping happens after the cut, in
    /// [`Markup::text`].
    pub(super) fn short(&self, text: &str, max: usize) -> String {
        let redacted = self.redactor.redact(text);
        let mut out: String = redacted.chars().take(max).collect();
        if redacted.chars().count() > max {
            out.push_str("...");
        }
        out
    }

    pub(super) fn el(&self, tag: &'static str, class: Option<&'static str>, text: &str) -> Markup {
        Markup::el(tag, class, self.text(text))
    }

    /// [`Writer::el`] over `text` cut to `head` and `tail` bytes. The whole
    /// text is redacted before the cut, so a secret the cut runs through
    /// is never left half shown.
    fn clipped(&self, tag: &'static str, text: &str, head: usize, tail: usize) -> Markup {
        Markup::el(
            tag,
            None,
            Markup::text(&clip(&self.redactor.redact(text), head, tail)),
        )
    }

    /// One journal line: every string of the event redacted as a value,
    /// before serialising escapes it, then the line cut.
    fn journal_line(&self, event: &Event) -> String {
        let line = serde_json::to_value(event)
            .map(|v| self.redactor.redact_value(v).to_string())
            .unwrap_or_else(|_| format!("event {} could not be shown", event.seq));
        clip(&line, JOURNAL_LINE_MAX_BYTES - 1024, 1024).replace('\n', " ")
    }

    pub(super) fn list(&self, items: &[String]) -> Markup {
        Markup::el(
            "ul",
            None,
            Markup::concat(items.iter().map(|i| self.el("li", None, i))),
        )
    }

    /// A table with a header row and one row per entry.
    pub(super) fn table<C: AsRef<str>>(&self, head: &[&str], rows: Vec<Vec<C>>) -> Markup {
        let th = Markup::concat(head.iter().map(|h| self.el("th", None, h)));
        let body = rows.into_iter().map(|r| {
            Markup::el(
                "tr",
                None,
                Markup::concat(r.iter().map(|c| self.el("td", None, c.as_ref()))),
            )
        });
        Markup::el(
            "div",
            Some("table"),
            Markup::el(
                "table",
                None,
                Markup::concat([
                    Markup::el("thead", None, Markup::el("tr", None, th)),
                    Markup::el("tbody", None, Markup::concat(body)),
                ]),
            ),
        )
    }
}

pub(super) fn section(title: &'static str, body: Markup) -> Markup {
    Markup::el(
        "section",
        None,
        Markup::concat([Markup::el("h2", None, Markup::text(title)), body]),
    )
}

pub(super) fn empty(text: &'static str) -> Markup {
    Markup::el("p", Some("empty"), Markup::text(text))
}

fn status_class(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Succeeded => "verdict status-succeeded",
        RunStatus::Failed | RunStatus::Interrupted => "verdict status-failed",
        RunStatus::Aborted => "verdict status-aborted",
        RunStatus::Paused => "verdict status-paused",
        RunStatus::Created | RunStatus::Running => "verdict status-running",
    }
}

/// The title, the verdict and where to look further.
pub(super) fn header(w: &Writer, title: &str, report: &Report) -> Markup {
    let link = report
        .link
        .as_deref()
        .and_then(SafeUrl::parse)
        .map(|u| {
            Markup::el(
                "p",
                Some("meta"),
                Markup::link(&u, "Open the run in the dashboard"),
            )
        })
        .unwrap_or_default();
    Markup::el(
        "header",
        None,
        Markup::concat([
            w.el("h1", None, title),
            w.el(
                "p",
                Some(status_class(report.status)),
                &format!("Run {}", report.status.as_str()),
            ),
            w.el("p", None, &report.outcome.headline),
            link,
        ]),
    )
}

/// The route: each node with its executions, every edge with what the
/// journal established, the judge answers, the rows that resolve to no
/// edge (not established) and why each node did not run, with its class.
pub(super) fn route(w: &Writer, g: &crate::run_outcome::RunGraph) -> Markup {
    use crate::run_outcome::EdgeState;
    if g.shown_segment.is_none() {
        return section(
            "Route",
            empty("The run's definition is not established, so its route is not shown."),
        );
    }
    let nodes = w.table(
        &["Node", "Executions", "Open", "Last status"],
        g.nodes
            .iter()
            .map(|n| {
                vec![
                    n.node.clone(),
                    n.executions.to_string(),
                    n.open.to_string(),
                    n.last_status.clone().unwrap_or_else(|| "none".into()),
                ]
            })
            .collect(),
    );
    let edges = w.table(
        &["Edge", "Segment", "State"],
        g.edges
            .iter()
            .map(|e| {
                let state = match &e.state {
                    EdgeState::Taken { count } => format!("taken {count}"),
                    EdgeState::NotTaken => "not taken".to_string(),
                    EdgeState::NotEstablished { reason } => {
                        format!("not established ({reason})")
                    }
                };
                vec![e.rendered.clone(), e.segment.to_string(), state]
            })
            .collect(),
    );
    let mut lines: Vec<String> = g
        .traversals
        .iter()
        .filter(|t| !t.resolved)
        .map(|t| {
            format!(
                "hop {} -> {} at seq {}: not established ({})",
                t.from,
                t.to,
                t.seq,
                t.reason.as_deref().unwrap_or("cannot_tell")
            )
        })
        .collect();
    lines.extend(
        g.lines().into_iter().filter(|l| {
            l.starts_with("judge ") || l.starts_with("not run ") || l.starts_with("  ")
        }),
    );
    Markup::concat([
        section("Route", Markup::concat([nodes, edges])),
        match lines.is_empty() {
            true => Markup::default(),
            false => section("Route detail", w.list(&lines)),
        },
    ])
}

/// What the run did, what was checked and what was not.
pub(super) fn outcome(w: &Writer, o: &OutcomeSummary) -> Markup {
    let parts: Vec<Markup> = o
        .sections()
        .into_iter()
        .filter(|(title, _)| *title != AGENT_SUMMARY_TITLE)
        .map(|(title, items)| Markup::concat([w.el("h3", None, title), w.list(&items)]))
        .collect();
    if parts.is_empty() {
        return section("Outcome", empty("Nothing to report."));
    }
    section("Outcome", Markup::concat(parts))
}

/// The goal and each criterion's result.
pub(super) fn goal(w: &Writer, goal: Option<&RunGoal>) -> Markup {
    let Some(g) = goal else {
        return section("Goal", empty("The playbook declares no goal."));
    };
    let numbers: Vec<String> = g
        .criteria
        .iter()
        .map(|c| (c.index + 1).to_string())
        .collect();
    let rows = g
        .criteria
        .iter()
        .zip(&numbers)
        .map(|(c, n)| {
            vec![
                n.as_str(),
                c.description.as_str(),
                c.check.as_str(),
                c.status.as_str(),
                c.detail.as_deref().unwrap_or_default(),
            ]
        })
        .collect();
    section(
        "Goal",
        Markup::concat([
            w.el("p", None, &g.statement),
            w.table(&["#", "Criterion", "Check", "Result", "Detail"], rows),
        ]),
    )
}

/// The playbook's nodes in order, with how each one ended and how long it
/// took against the estimate.
pub(super) fn steps(w: &Writer, steps: &[Step]) -> Markup {
    if steps.is_empty() {
        return section("Steps", empty("The run has no playbook snapshot."));
    }
    let rows = steps
        .iter()
        .map(|s| {
            vec![
                s.row.node.clone(),
                s.row.kind.to_string(),
                s.status.clone().unwrap_or_else(|| "not run".to_string()),
                format!("{} s", s.row.expected_seconds),
                s.row
                    .measured_seconds
                    .map_or_else(String::new, |m| format!("{m} s")),
            ]
        })
        .collect();
    section(
        "Steps",
        w.table(&["Node", "Kind", "Status", "Expected", "Measured"], rows),
    )
}

/// Each list node's counts.
pub(super) fn lists(w: &Writer, o: &OutcomeSummary) -> Markup {
    if o.lists.is_empty() {
        return section("Lists", empty("The run has no list node."));
    }
    let lines: Vec<String> = o.lists.iter().map(|l| l.line()).collect();
    section("Lists", w.list(&lines))
}

/// The run's own sends and what became of each.
pub(super) fn sends(w: &Writer, sends: &[RunIntent]) -> Markup {
    if sends.is_empty() {
        return section("Sends", empty("No send went through the send ledger."));
    }
    let lines: Vec<String> = sends.iter().map(RunIntent::line).collect();
    section("Sends", w.list(&lines))
}

/// The run's publish nodes, when it has any.
pub(super) fn publish(w: &Writer, publish: &[crate::run_outcome::RunPublish]) -> Markup {
    if publish.is_empty() {
        return Markup::concat([]);
    }
    let lines: Vec<String> = publish
        .iter()
        .flat_map(crate::run_outcome::publish::lines)
        .collect();
    section("Publish", w.list(&lines))
}

/// The notifications a person subscribed to, and what was sent.
pub(super) fn notifications(w: &Writer, notify: Option<&RunNotify>) -> Markup {
    let Some(n) = notify else {
        return section("Notifications", empty("Nobody subscribed to this run."));
    };
    let state = match &n.target {
        Some(t) if n.active => format!(
            "On, through {}.{} on account {}, about: {}.",
            t.connector,
            t.function,
            t.account,
            n.on.iter()
                .map(|o| o.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => "Off.".to_string(),
    };
    let sent: Vec<String> = n
        .sent
        .iter()
        .map(|s| format!("{}: {}", s.transition, s.state))
        .collect();
    let problems = &n.problems;
    section(
        "Notifications",
        Markup::concat([
            w.el("p", None, &state),
            if sent.is_empty() {
                Markup::default()
            } else {
                Markup::concat([w.el("h3", None, "Sent"), w.list(&sent)])
            },
            if problems.is_empty() {
                Markup::default()
            } else {
                Markup::concat([w.el("h3", None, "Problems"), w.list(problems)])
            },
        ]),
    )
}

/// The finish node's answer, the agent's own account, as written.
pub(super) fn answer(w: &Writer, answer: Option<&str>) -> Markup {
    let Some(text) = answer else {
        return section(
            "Closing answer",
            empty("The run has no composed closing answer."),
        );
    };
    section(
        "Closing answer",
        Markup::concat([
            w.el("p", Some("meta"), AGENT_SUMMARY_TITLE),
            w.clipped("pre", text, ANSWER_MAX_BYTES, 0),
        ]),
    )
}

/// The journal, one JSON line per event, collapsed.
pub(super) fn journal(w: &Writer, events: &[Event], cut: usize) -> Markup {
    let lines: String = events
        .iter()
        .map(|e| w.journal_line(e))
        .collect::<Vec<_>>()
        .join("\n");
    let summary = if cut > 0 {
        format!(
            "Journal: the last {} events ({cut} earlier ones left out)",
            events.len()
        )
    } else {
        format!("Journal: {} events", events.len())
    };
    Markup::el(
        "section",
        None,
        Markup::el(
            "details",
            None,
            Markup::concat([
                w.el("summary", None, &summary),
                Markup::el("pre", None, Markup::text(&lines)),
            ]),
        ),
    )
}
