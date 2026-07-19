pub mod adapter;
pub mod connector_call;
pub mod connector_prompt;
pub mod connector_result;
pub mod connector_run;
pub mod connector_smtp;
pub mod context;
pub mod control;
pub mod error;
pub mod event;
pub mod hooks;
pub mod inspect;
pub mod invocation;
pub mod legacy_snapshot;
pub mod manifest;
pub mod parallel;
pub mod proc;
pub mod progress;
pub mod review;
pub mod run_config;
pub mod scheduler;
pub mod script;
pub mod signals;
pub mod state;
pub mod workdir;

pub use error::EngineError;
pub use hooks::{generate_hooks, hook_path, read_hooks};
pub use inspect::{
    PersistedSession, WakeEvent, find_session_by_token, heartbeat_age_ms, read_supervisor_report,
    run_inspect, should_declare_lost, supervisor_report_or_summary, supervisor_silence_ms,
    touch_heartbeat, wait_wake, write_supervisor_report, write_supervisor_session,
};
pub use progress::{ProgressSummary, compute as run_progress, node_durations_seconds};
pub use review::{ReviewCommand, ReviewEntry, post_review, read_reviews_after};
pub use scheduler::{
    PreparedRun, RunMode, RunOptions, RunResult, RunSummary, drive_prepared, list_runs,
    post_supervisor_command, prepare_supervised_background, resume, resume_with, run,
    run_background, run_background_resolved, run_cancel, run_resolved, spawn_supervisor_agent,
};
pub use signals::{SignalCommand, SignalEntry, post_signal, read_signals_after};
