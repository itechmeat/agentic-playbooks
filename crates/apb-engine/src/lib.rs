pub mod adapter;
mod agent_home;
pub mod connector;
pub mod context;
pub mod control;
pub mod decision;
pub mod driver;
pub mod error;
pub mod event;
pub mod failure_class;
pub mod gate;
pub mod hooks;
pub mod inspect;
pub mod invocation;
pub mod legacy_snapshot;
pub mod liveness;
// host execution mode (0.23.0)
pub mod host_task;
pub mod manifest;
pub mod parallel;
pub mod proc;
pub mod progress;
pub mod question;
pub mod review;
pub mod run_config;
pub mod run_doctor;
mod run_lineage;
pub mod run_view;
pub mod run_wait;
pub mod scheduler;
pub mod script;
pub mod signals;
mod stall;
pub mod state;
pub mod stop;
pub mod workdir;
pub mod zcode_ui_sync;

// Unit tests run with an isolated global config dir (issue #165 P1 open item
// 5), set before any test thread starts; see `tests/main.rs` for the same
// guard on the integration suite.
#[cfg(test)]
#[used]
#[cfg_attr(
    any(target_os = "linux", target_os = "android", target_os = "freebsd"),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func")
)]
static ISOLATE_TEST_CONFIG: extern "C" fn() = isolate_test_config;

#[cfg(test)]
extern "C" fn isolate_test_config() {
    let dir = std::env::temp_dir().join(format!("apb-engine-unit-xdg-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    // SAFETY: runs before `main`, so no other thread exists yet.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

pub use error::EngineError;
pub use hooks::{generate_hooks, hook_path, read_hooks};
pub use inspect::{
    PersistedSession, WakeEvent, find_session_by_token, heartbeat_age_ms, mint_supervisor_token,
    read_supervisor_report, run_inspect, should_declare_lost, supervisor_report_or_summary,
    supervisor_silence_ms, supervisor_token_fingerprint, touch_heartbeat, wait_wake,
    write_supervisor_report, write_supervisor_session,
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
    prepare_supervised_background, record_run_error, resume, resume_detached, resume_detached_with,
    resume_with, run, run_background, run_background_resolved, run_cancel, run_profile_origin,
    run_resolved, spawn_supervisor_agent, start_detached, start_detached_resolved,
};
pub use signals::{SignalCommand, SignalEntry, post_signal, read_signals_after};
pub use stop::{StopOutcome, stop_run};
