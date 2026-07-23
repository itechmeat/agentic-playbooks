pub mod adapter;
mod agent_home;
pub mod connector_call;
pub mod connector_imap;
pub mod connector_prompt;
pub mod connector_result;
pub mod connector_run;
pub mod connector_smtp;
pub mod connector_test;
pub mod context;
pub mod control;
pub mod driver;
pub mod error;
pub mod event;
pub mod hooks;
pub mod inspect;
pub mod invocation;
pub mod legacy_snapshot;
pub mod liveness;
pub mod manifest;
pub mod parallel;
pub mod proc;
pub mod progress;
pub mod question;
pub mod review;
pub mod run_config;
pub mod run_doctor;
mod run_lineage;
pub mod scheduler;
pub mod script;
pub mod signals;
mod stall;
pub mod state;
pub mod stop;
pub mod workdir;

pub use error::EngineError;
pub use hooks::{generate_hooks, hook_path, read_hooks};
pub use inspect::{
    PersistedSession, WakeEvent, find_session_by_token, heartbeat_age_ms, read_supervisor_report,
    run_inspect, should_declare_lost, supervisor_report_or_summary, supervisor_silence_ms,
    touch_heartbeat, wait_wake, write_supervisor_report, write_supervisor_session,
};
pub use liveness::{
    NodeTimes, driver_alive, lost_nodes, node_times, reported_node_statuses, reported_run_status,
};
pub use progress::{
    PendingSupervisor, ProgressSummary, compute as run_progress, node_durations_seconds,
    pending_supervisor_decision,
};
pub use question::{
    PostedAnswer, PostedQuestion, post_answer, post_question, read_answers_after,
    read_questions_after,
};
pub use review::{ReviewCommand, ReviewEntry, post_review, read_reviews_after};
pub use run_doctor::{RunCheck, diagnose_run};
pub use scheduler::{
    PreparedRun, ResumeDecision, ResumeReason, RunMode, RunOptions, RunResult, RunSummary,
    StartMode, drive_prepared, drive_run_from_dir, list_runs, plan_resume, post_supervisor_command,
    prepare_supervised_background, resume, resume_detached, resume_with, run, run_background,
    run_background_resolved, run_cancel, run_resolved, spawn_supervisor_agent, start_detached,
    start_detached_resolved,
};
pub use signals::{SignalCommand, SignalEntry, post_signal, read_signals_after};
pub use stop::{StopOutcome, stop_run};
