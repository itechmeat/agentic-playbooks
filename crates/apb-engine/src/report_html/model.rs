//! What the report shows, gathered once from the run directory: the same
//! outcome model the JSON report reads, and the caps that keep the page a
//! sane size.

use std::path::Path;

use crate::error::EngineError;
use crate::event::Event;
use crate::notify::RunNotify;
use crate::progress::DurationRow;
use crate::run_outcome::{OutcomeSummary, RunGoal, RunIntent};
use crate::run_view::RunView;
use crate::state::RunStatus;

/// How many of the journal's latest events the report keeps.
pub const JOURNAL_MAX_EVENTS: usize = 5000;

/// The longest finish answer the report keeps, in bytes.
pub const ANSWER_MAX_BYTES: usize = 64 * 1024;

/// The longest journal line the report keeps, in bytes: a node's whole
/// output stays in the run, the report shows its start and its end.
pub const JOURNAL_LINE_MAX_BYTES: usize = 4096;

/// What the report shows, gathered once from the run directory.
pub(super) struct Report {
    pub(super) run_id: String,
    pub(super) playbook: Option<String>,
    pub(super) status: RunStatus,
    pub(super) outcome: OutcomeSummary,
    pub(super) goal: Option<RunGoal>,
    pub(super) steps: Vec<Step>,
    pub(super) sends: Vec<RunIntent>,
    pub(super) publish: Vec<crate::run_outcome::RunPublish>,
    pub(super) notify: Option<RunNotify>,
    pub(super) link: Option<String>,
    pub(super) journal: Vec<Event>,
    /// Events left out of the journal section.
    pub(super) journal_cut: usize,
    /// The latest trusted grade of each grader.
    pub(super) grades: Vec<apb_core::quality::grade::Grade>,
    /// Grade files left out as not signed for this run here.
    pub(super) grades_left_out: usize,
    pub(super) known_problems: Option<crate::known_problems::RunKnownProblems>,
}

/// One node of the playbook, in playbook order.
pub(super) struct Step {
    pub(super) row: DurationRow,
    pub(super) status: Option<String>,
}

impl Report {
    pub(super) fn load(root: &Path, run_dir: &Path) -> Result<Self, EngineError> {
        let run_id = run_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let view = RunView::load(run_dir, &run_id)?;
        let readout = view.readout(run_dir);
        let answer = crate::progress::run_answer(run_dir, &view.events);
        let outcome = OutcomeSummary::from_readout(&view, run_dir, &readout, answer);
        let statuses = view.nodes();
        let steps = readout
            .playbook
            .as_ref()
            .map(|pb| {
                let measured = crate::progress::node_durations_seconds(&view.events);
                crate::progress::build_duration_table_from(pb, &measured)
                    .into_iter()
                    .map(|row| Step {
                        status: statuses.get(&row.node).cloned(),
                        row,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let now = apb_core::clock::now_ms_u64();
        let playbook = view.events.iter().find_map(|e| match &e.payload {
            crate::event::EventPayload::RunStarted { playbook, .. } => Some(playbook.clone()),
            _ => None,
        });
        let journal_cut = view.events.len().saturating_sub(JOURNAL_MAX_EVENTS);
        let status = view.run_status;
        let sends = crate::run_outcome::outcome_intents(&view.events, now);
        let publish = crate::run_outcome::publish_from(&view.events, now);
        let notify = RunNotify::for_run(run_dir, &view.events, now);
        let known_problems = crate::known_problems::of_run(&view.events);
        let (trusted, grades_left_out) = crate::grade::split_grades_of(run_dir);
        let mut events = view.events;
        let journal = events.split_off(journal_cut);
        Ok(Report {
            playbook,
            status,
            sends,
            publish,
            notify,
            link: crate::people::link::run_link(root, &run_id),
            goal: readout.goal,
            outcome,
            steps,
            journal,
            journal_cut,
            grades: crate::grade::latest_by_grader(trusted),
            grades_left_out,
            known_problems,
            run_id,
        })
    }
}
