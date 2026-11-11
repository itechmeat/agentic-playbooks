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
    /// What `run.previous_success.*` renders, as the start recorded it.
    pub(super) previous_success: Option<crate::run_view::PreviousSuccessView>,
    /// The deadline line (`45m, 12m left (ends ...)`); absent without one.
    pub(super) deadline: Option<String>,
    /// The concurrency line (`key "<display>" (queue_latest)`); absent
    /// without a slot.
    pub(super) concurrency: Option<String>,
    /// The start key's MAC prefix the run was started with.
    pub(super) start_key: Option<String>,
    /// The run step this run was started from, as a line.
    pub(super) started_by: Option<String>,
    /// The runs a step of this run started in this project, as a line.
    pub(super) started_runs: Option<String>,
    /// The nodes that render untrusted text, as the start recorded them.
    pub(super) untrusted: Vec<crate::event::UntrustedNode>,
    pub(super) journal: Vec<Event>,
    /// Events left out of the journal section.
    pub(super) journal_cut: usize,
    /// The latest trusted grade of each grader.
    pub(super) grades: Vec<apb_core::quality::grade::Grade>,
    /// Grade files left out as not signed for this run here.
    pub(super) grades_left_out: usize,
    pub(super) known_problems: Option<crate::known_problems::RunKnownProblems>,
    /// What the journal proves about every node and edge.
    pub(super) graph: crate::run_outcome::RunGraph,
    /// The definition the run actually executed: the shown segment's
    /// stored definition.
    pub(super) definition: Option<apb_core::schema::Playbook>,
    /// The run processes external data ([`crate::external_data`]).
    pub(super) external_data: bool,
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
        let (graph, definition) = view.graph_and_definition(run_dir);
        let previous_success = crate::run_view::previous_success(&view.events);
        let untrusted = crate::run_view::untrusted_nodes(&view.events);
        let deadline = crate::run_view::deadline(&view.events, u128::from(now))
            .map(|d| crate::run_view::deadline_line(&d));
        let concurrency = {
            let scope = view
                .events
                .iter()
                .find_map(|e| match &e.payload {
                    crate::event::EventPayload::RunProvenance { origin, .. } => origin.clone(),
                    _ => None,
                })
                .and_then(|o| apb_core::scope::Scope::parse(&o))
                .unwrap_or(apb_core::scope::Scope::Project);
            crate::run_view::concurrency(
                root,
                &run_id,
                scope,
                playbook.as_deref().unwrap_or(""),
                &view.events,
            )
            .as_ref()
            .map(crate::run_view::concurrency_line)
        };
        let start_key = crate::run_view::start_key(&view.events);
        let here = apb_core::projects::registered_id(root);
        let started_by = crate::run_view::started_by(run_dir)
            .map(|by| crate::run_view::started_by_line(&by, here.as_deref()));
        let started = crate::run_view::started_runs(root, &run_id);
        let started_runs = (!started.runs.is_empty() || started.truncated)
            .then(|| crate::run_view::started_runs_line(&started));
        let mut events = view.events;
        let journal = events.split_off(journal_cut);
        Ok(Report {
            playbook,
            status,
            sends,
            publish,
            notify,
            link: crate::people::link::run_link(root, &run_id),
            previous_success,
            deadline,
            concurrency,
            start_key,
            started_by,
            started_runs,
            untrusted,
            goal: readout.goal,
            outcome,
            steps,
            journal,
            journal_cut,
            grades: crate::grade::latest_by_grader(trusted),
            grades_left_out,
            known_problems,
            graph,
            definition,
            external_data: crate::external_data::is_flagged(run_dir),
            run_id,
        })
    }
}
