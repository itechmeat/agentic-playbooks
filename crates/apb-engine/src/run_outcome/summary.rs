//! The human outcome of a run: what it did, what the engine itself checked,
//! what nobody checked, what the attempts reported for a person to look
//! at, and the baseline as problem, change and goal. One reading for every
//! surface: the JSON shape for MCP and the dashboard, [`OutcomeSummary::lines`]
//! for a terminal and a report.
//!
//! The rule it keeps: a claim is "verified" only when the engine checked it
//! (a goal criterion, a node's `success_check`, a baseline comparison). An
//! agent's own report, a manual criterion, a call whose effect is not known
//! and a cut output are listed as not verified, never folded into a pass.

use std::path::Path;

use apb_core::baseline::Verdict;
use apb_core::schema::{NodeKind, Playbook};
use serde::Serialize;

use super::baseline::{RunBaseline, observation_line};
use super::calls::{CallClass, ClassifiedCall, classified_calls};
pub use super::claims::{Evidence, EvidenceSource, Gap, GapKind};
use super::intents::{RunIntent, intents_from};
use super::lists::{ListOutcome, list_evidence, list_gaps, list_outcomes};
use super::notes::{RunFinding, RunNotes, RunWarning, finding_line, warning_line};
use super::{RunGoal, inbox_gap_lines, inbox_gaps_from};
use crate::event::{Event, EventPayload};
use crate::list::RunList;
use crate::run_view::RunView;
use crate::state::{NodeStatus, RunStatus};

/// The baseline told as a story: the problem the before snapshot found,
/// the change the after snapshot shows, and whether the goal (never the
/// baseline) says it was reached.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BaselineOutcome {
    pub problem: Vec<String>,
    pub change: Vec<String>,
    /// From the goal criteria only: every checked criterion passed and no
    /// manual one waits. Absent when that is not known.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub goal_reached: Option<bool>,
}

/// Why a run needs a person's look.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsLookReason {
    Regression,
    Findings,
    Warnings,
    /// Sends nobody can tell went out: a person resolves each.
    UnknownSends,
    /// List items that need a look.
    ListItems,
}

/// A run with a baseline regression, findings or warnings its attempts
/// reported, sends whose outcome is not known, or list items that need a
/// look. Nothing else sets it: a
/// failed run says so in its status.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NeedsLook {
    pub reasons: Vec<NeedsLookReason>,
    pub regressions: u32,
    pub findings: u32,
    pub warnings: u32,
    /// Sends whose outcome is not known and that no replay settled.
    #[serde(skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub unknown_sends: u32,
    /// List items that need a look.
    #[serde(skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub list_items: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl NeedsLook {
    /// For the run listing: the current comparisons as
    /// [`BaselineState`](crate::baseline::BaselineState) reads them, the
    /// notes and the send ledger. The playbook snapshot is read only when the journal holds a
    /// comparison, so a listing stays cheap; `None` when nothing calls for
    /// a look.
    pub fn for_run(run_dir: &Path, events: &[Event]) -> Option<Self> {
        let compared = events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::BaselineCompared { .. }));
        let playbook = compared
            .then(|| crate::legacy_snapshot::load_run_playbook(run_dir))
            .flatten();
        Self::from_events(playbook.as_ref(), events)
    }

    /// From the journal and the run's playbook snapshot, when it reads.
    pub fn from_events(playbook: Option<&Playbook>, events: &[Event]) -> Option<Self> {
        let notes = RunNotes::from_events(events);
        let intents = intents_from(events, apb_core::clock::now_ms_u64());
        Self::of(
            current_regressions(playbook, events),
            &notes,
            &intents,
            crate::list::run_lists(playbook, events)
                .iter()
                .map(|l| l.needs_look)
                .sum(),
        )
    }

    fn of(
        regressions: u32,
        notes: &RunNotes,
        intents: &[RunIntent],
        list_items: u32,
    ) -> Option<Self> {
        let findings = notes.findings.len() as u32;
        let warnings = notes.warnings.len() as u32;
        let unknown_sends = intents.iter().filter(|i| i.is_open_unknown()).count() as u32;
        let reasons: Vec<NeedsLookReason> = [
            (regressions, NeedsLookReason::Regression),
            (findings, NeedsLookReason::Findings),
            (warnings, NeedsLookReason::Warnings),
            (unknown_sends, NeedsLookReason::UnknownSends),
            (list_items, NeedsLookReason::ListItems),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(_, r)| r)
        .collect();
        (!reasons.is_empty()).then_some(NeedsLook {
            reasons,
            regressions,
            findings,
            warnings,
            unknown_sends,
            list_items,
        })
    }

    /// `1 regression, 2 findings, 1 warning, 1 unknown send, 2 list items`.
    pub fn line(&self) -> String {
        [
            (self.regressions, "regression", "regressions"),
            (self.findings, "finding", "findings"),
            (self.warnings, "warning", "warnings"),
            (self.unknown_sends, "unknown send", "unknown sends"),
            (self.list_items, "list item", "list items"),
        ]
        .into_iter()
        .filter(|(n, ..)| *n > 0)
        .map(|(n, one, many)| format!("{n} {}", if n == 1 { one } else { many }))
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// The current comparisons whose after value is worse than the before
/// one: the one reading of which comparisons are current is
/// [`BaselineState`](crate::baseline::BaselineState).
fn current_regressions(playbook: Option<&Playbook>, events: &[Event]) -> u32 {
    crate::baseline::BaselineState::fold_with(playbook, events).regressions() as u32
}

/// The human outcome of one run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutcomeSummary {
    pub headline: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub needs_look: Option<NeedsLook>,
    /// What the run did: the steps that succeeded, its commits, the
    /// connector writes that went through.
    pub done: Vec<String>,
    /// What the engine checked.
    pub verified: Vec<Evidence>,
    /// What nobody checked.
    pub unverified: Vec<Gap>,
    pub findings: Vec<RunFinding>,
    pub warnings: Vec<RunWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub baseline: Option<BaselineOutcome>,
    /// Each list node's counts; empty for a run without a list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<ListOutcome>>", optional))]
    pub lists: Vec<ListOutcome>,
    /// The finish node's answer: the agent's own account, not checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub agent_summary: Option<String>,
}

/// What [`OutcomeSummary::build`] reads, gathered once.
struct Inputs<'a> {
    playbook: Option<&'a Playbook>,
    view: &'a RunView,
    goal: Option<&'a RunGoal>,
    baseline: Option<&'a RunBaseline>,
    notes: &'a RunNotes,
    calls: Vec<ClassifiedCall>,
    intents: Vec<RunIntent>,
    lists: &'a [RunList],
    list_outcomes: Vec<ListOutcome>,
    /// The run's id: the run directory's name.
    run_id: String,
}

impl OutcomeSummary {
    /// The outcome of the run at `run_dir`, from its journal, its playbook
    /// snapshot and its connector snapshots.
    pub fn build(view: &RunView, run_dir: &Path) -> Self {
        let readout = view.readout(run_dir);
        let answer = crate::progress::run_answer(run_dir, &view.events);
        Self::from_readout(view, run_dir, &readout, answer)
    }

    /// [`OutcomeSummary::build`] over a readout and a finish answer the
    /// caller already holds, so nothing is read twice.
    pub fn from_readout(
        view: &RunView,
        run_dir: &Path,
        readout: &super::RunReadout,
        answer: Option<String>,
    ) -> Self {
        let inputs = Inputs {
            playbook: readout.playbook.as_ref(),
            view,
            goal: readout.goal.as_ref(),
            baseline: readout.baseline.as_ref(),
            notes: &readout.notes,
            calls: classified_calls(run_dir, &view.events),
            intents: intents_from(&view.events, apb_core::clock::now_ms_u64()),
            lists: &readout.lists,
            list_outcomes: list_outcomes(readout.playbook.as_ref(), &readout.lists),
            run_id: run_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let mut summary = inputs.summarize();
        summary.agent_summary = answer;
        summary
    }

    /// The one text rendering: a headline, then a block per non-empty
    /// section, items indented by two spaces.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![self.headline.clone()];
        out.extend(self.lists.iter().map(ListOutcome::line));
        let mut section = |title: &str, items: Vec<String>| {
            if !items.is_empty() {
                out.push(format!("{title}:"));
                out.extend(items.into_iter().map(|i| format!("  {i}")));
            }
        };
        section("Done", self.done.clone());
        section(
            "Verified",
            self.verified
                .iter()
                .map(|e| {
                    let detail = e
                        .detail
                        .as_deref()
                        .map(|d| format!(" ({d})"))
                        .unwrap_or_default();
                    format!("[{}] {}: {}{detail}", e.result, e.source.label(), e.subject)
                })
                .collect(),
        );
        section(
            "Not verified",
            self.unverified
                .iter()
                .map(|g| {
                    let detail = g
                        .detail
                        .as_deref()
                        .map(|d| format!(": {d}"))
                        .unwrap_or_default();
                    format!("[{}] {}{detail}", g.kind.label(), g.subject)
                })
                .collect(),
        );
        section("Findings", self.findings.iter().map(finding_line).collect());
        section("Warnings", self.warnings.iter().map(warning_line).collect());
        if let Some(b) = &self.baseline {
            let mut items: Vec<String> = b.problem.iter().map(|p| format!("before: {p}")).collect();
            items.extend(b.change.iter().map(|c| format!("change: {c}")));
            items.push(format!(
                "goal reached: {}",
                match b.goal_reached {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "not known",
                }
            ));
            section("Baseline", items);
        }
        if let Some(a) = &self.agent_summary {
            section("Agent's own summary, not checked", vec![a.clone()]);
        }
        out
    }
}

impl Inputs<'_> {
    fn summarize(self) -> OutcomeSummary {
        let notes = self.notes;
        let needs_look = NeedsLook::of(
            current_regressions(self.playbook, &self.view.events),
            notes,
            &self.intents,
            self.list_outcomes.iter().map(|l| l.needs_look).sum(),
        );
        let verified = self.verified();
        let unverified = self.unverified();
        let headline = headline(
            self.view.run_status,
            &verified,
            unverified.len(),
            needs_look.as_ref(),
        );
        OutcomeSummary {
            headline,
            needs_look,
            done: self.done(),
            verified,
            unverified,
            findings: notes.findings.clone(),
            warnings: notes.warnings.clone(),
            baseline: self.baseline.map(|b| baseline_story(b, self.goal)),
            lists: self.list_outcomes,
            agent_summary: None,
        }
    }

    fn node_kind(&self, id: &str) -> Option<&NodeKind> {
        self.playbook?.node(id).map(|n| &n.kind)
    }

    /// The nodes that succeeded, start and finish aside.
    fn succeeded_nodes(&self) -> impl Iterator<Item = &str> {
        self.view
            .state
            .nodes
            .iter()
            .filter(|(_, s)| **s == NodeStatus::Succeeded)
            .map(|(id, _)| id.as_str())
            .filter(|id| {
                !matches!(
                    self.node_kind(id),
                    Some(NodeKind::Start | NodeKind::Finish { .. })
                )
            })
    }

    fn done(&self) -> Vec<String> {
        let mut out = Vec::new();
        let succeeded: Vec<&str> = self.succeeded_nodes().collect();
        if !succeeded.is_empty() {
            out.push(format!("succeeded: {}", succeeded.join(", ")));
        }
        for c in super::commits_from_events(&self.view.events) {
            out.push(super::commits::done_line(&c, &self.run_id));
        }
        for (target, n) in count_by(&self.calls, |c| {
            (c.class == CallClass::WriteOk).then(|| c.target())
        }) {
            out.push(format!(
                "{target}: {n} {} went through",
                plural(n, "write", "writes")
            ));
        }
        out.extend(self.intents.iter().filter_map(RunIntent::done_line));
        out
    }

    fn verified(&self) -> Vec<Evidence> {
        let mut out = Vec::new();
        if let Some(goal) = self.goal {
            out.extend(
                goal.criteria
                    .iter()
                    .filter(|c| matches!(c.status.as_str(), "passed" | "failed" | "error"))
                    .map(|c| Evidence {
                        source: EvidenceSource::Goal,
                        subject: c.description.clone(),
                        result: c.status.clone(),
                        detail: c.detail.clone(),
                    }),
            );
        }
        for id in self.succeeded_nodes() {
            let check = self
                .playbook
                .and_then(|p| p.node(id))
                .and_then(|n| n.success_check.as_ref());
            if let Some(check) = check {
                out.push(Evidence {
                    source: EvidenceSource::SuccessCheck,
                    subject: id.to_string(),
                    result: "passed".into(),
                    detail: Some(match check.script_path() {
                        Some(path) => format!("script {path}"),
                        None => "marker".to_string(),
                    }),
                });
            }
        }
        out.extend(list_evidence(&self.list_outcomes));
        if let Some(b) = self.baseline {
            out.extend(
                b.observations()
                    .filter(|(_, o)| {
                        o.verdict == Verdict::Met.as_str() || o.verdict == Verdict::NotMet.as_str()
                    })
                    .map(|(c, o)| Evidence {
                        source: EvidenceSource::Baseline,
                        subject: format!("{}/{}", c.id, o.id),
                        result: o.verdict.clone(),
                        detail: Some(observation_detail(o)),
                    }),
            );
        }
        out
    }

    fn unverified(&self) -> Vec<Gap> {
        let ended = !matches!(
            self.view.run_status,
            RunStatus::Created | RunStatus::Running | RunStatus::Paused
        );
        let mut out = Vec::new();
        if let Some(goal) = self.goal {
            out.extend(
                goal.criteria
                    .iter()
                    .filter(|c| c.check == "manual")
                    .map(|c| Gap {
                        kind: GapKind::ManualCriterion,
                        subject: c.description.clone(),
                        detail: None,
                    }),
            );
            let checkable = goal.criteria.iter().any(|c| c.check != "manual");
            if ended && checkable && !goal.checked {
                out.push(Gap {
                    kind: GapKind::GoalNotChecked,
                    subject: goal.statement.clone(),
                    detail: Some("the run ended before a finish node checked the goal".into()),
                });
            }
        }
        for id in self.succeeded_nodes() {
            let node = self.playbook.and_then(|p| p.node(id));
            // A list node's items are told one by one below.
            if let Some(n) = node.filter(|n| matches!(n.kind, NodeKind::AgentTask { .. }))
                && n.success_check.is_none()
                && n.list().is_none()
            {
                out.push(Gap {
                    kind: GapKind::SelfReportedNode,
                    subject: id.to_string(),
                    detail: Some("the agent's own report is the only evidence".into()),
                });
            }
        }
        out.extend(list_gaps(&self.list_outcomes, self.lists));
        out.extend(self.call_gaps());
        out.extend(self.intent_gaps());
        // The line names the inbox, the lost events and the node that found
        // them, so it is the subject whole.
        out.extend(
            inbox_gap_lines(&inbox_gaps_from(&self.view.events))
                .into_iter()
                .map(|line| Gap {
                    kind: GapKind::InboxGap,
                    subject: line,
                    detail: None,
                }),
        );
        out.extend(self.view.output_losses().iter().map(|l| Gap {
            kind: GapKind::OutputTruncated,
            subject: format!("{} attempt {}", l.node, l.attempt),
            detail: Some(l.line()),
        }));
        if let Some(b) = self.baseline {
            out.extend(baseline_gaps(b, ended));
        }
        out
    }

    /// One gap per send intent nobody can tell went out, including one
    /// whose sender vanished before any call was journaled.
    fn intent_gaps(&self) -> Vec<Gap> {
        self.intents
            .iter()
            .filter(|i| i.is_open_unknown())
            .map(|i| Gap {
                kind: GapKind::UnknownCall,
                subject: i.target(),
                detail: Some(i.gap_detail(&self.run_id)),
            })
            .collect()
    }

    /// One gap per node and function whose calls left the effect or the
    /// response unknown. A call that carried a send intent is left to the
    /// send ledger: its intent tells whether it went out, also after a
    /// later lookup, replay or a person settled it.
    fn call_gaps(&self) -> Vec<Gap> {
        let mut out = Vec::new();
        for (kind, what) in [
            (
                GapKind::UnknownCall,
                "whether it changed anything is not known",
            ),
            (GapKind::IncompleteResponse, "the response was cut short"),
        ] {
            let matches = |c: &ClassifiedCall| match kind {
                GapKind::UnknownCall => c.class == CallClass::WriteUnknown && c.intent.is_none(),
                _ => matches!(c.class, CallClass::ReadUnknown | CallClass::WriteIncomplete),
            };
            for ((node, target), n) in count_by(&self.calls, |c| {
                matches(c).then(|| (c.node.clone(), c.target()))
            }) {
                out.push(Gap {
                    kind,
                    subject: target,
                    detail: Some(format!(
                        "{n} {} by {node}: {what}",
                        plural(n, "call", "calls")
                    )),
                });
            }
        }
        out
    }
}

/// The baseline observations that could not be compared, and the checks
/// that never got an after snapshot in a run that ended.
fn baseline_gaps(b: &RunBaseline, ended: bool) -> Vec<Gap> {
    let mut out: Vec<Gap> = b
        .observations()
        .filter(|(_, o)| o.verdict == Verdict::Unknown.as_str())
        .map(|(c, o)| Gap {
            kind: GapKind::BaselineUnknown,
            subject: format!("{}/{}", c.id, o.id),
            detail: o.detail.clone(),
        })
        .collect();
    if ended {
        out.extend(
            b.checks
                .iter()
                .filter(|c| c.after_status.is_none())
                .map(|c| Gap {
                    kind: GapKind::BaselineUnknown,
                    subject: c.id.clone(),
                    detail: Some("no after snapshot: the run ended before a finish node".into()),
                }),
        );
    }
    out
}

/// `37 -> 0, equals 0`, plus the comparison's own words.
fn observation_detail(o: &super::baseline::ObservationView) -> String {
    let mut d = o.change.clone();
    if !o.rule.is_empty() {
        d.push_str(&format!(", {}", o.rule));
    }
    if let Some(extra) = o.detail.as_deref().filter(|x| !x.is_empty()) {
        d.push_str(&format!("; {extra}"));
    }
    d
}

fn baseline_story(b: &RunBaseline, goal: Option<&RunGoal>) -> BaselineOutcome {
    let problem = b
        .observations()
        .filter_map(|(c, o)| {
            let before = o.before.as_ref()?;
            let label = if c.description.is_empty() {
                c.id.as_str()
            } else {
                c.description.as_str()
            };
            let value = before
                .as_value()
                .map_or_else(|| "unknown".to_string(), |v| v.to_string());
            Some(format!("{label}: {} {value}", o.id))
        })
        .collect();
    let change = b
        .observations()
        .map(|(c, o)| observation_line(c, o))
        .collect();
    BaselineOutcome {
        problem,
        change,
        goal_reached: goal.and_then(goal_reached),
    }
}

/// Whether the goal was reached, from its criteria only: every criterion
/// was checked and passed, none waits for a person. `None` when not known,
/// including a run interrupted while its criteria were being checked.
pub fn goal_reached(goal: &RunGoal) -> Option<bool> {
    if !goal.checked || goal.criteria.is_empty() {
        return None;
    }
    if goal.failed > 0 {
        return Some(false);
    }
    (goal.manual == 0 && goal.passed == goal.criteria.len()).then_some(true)
}

fn headline(
    status: RunStatus,
    verified: &[Evidence],
    unverified: usize,
    needs_look: Option<&NeedsLook>,
) -> String {
    let mut parts = vec![format!("Run {}", status.as_str())];
    if !verified.is_empty() {
        let passed = verified.iter().filter(|e| e.passed()).count();
        parts.push(format!(
            "{passed} of {} {} passed",
            verified.len(),
            plural(verified.len(), "check", "checks")
        ));
    } else {
        parts.push("nothing checked by the engine".into());
    }
    if unverified > 0 {
        parts.push(format!("{unverified} not verified"));
    }
    if let Some(n) = needs_look {
        parts.push(format!("needs a look ({})", n.line()));
    }
    parts.join("; ")
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// Counts the calls `key` picks, in first-seen order.
fn count_by<K: PartialEq>(
    calls: &[ClassifiedCall],
    key: impl Fn(&ClassifiedCall) -> Option<K>,
) -> Vec<(K, usize)> {
    let mut out: Vec<(K, usize)> = Vec::new();
    for k in calls.iter().filter_map(key) {
        match out.iter_mut().find(|(e, _)| *e == k) {
            Some((_, n)) => *n += 1,
            None => out.push((k, 1)),
        }
    }
    out
}
