// The `testing` feature compiles test seams (the system root of agent files, the diagnostics writer's test hooks) that no person's
// apb may carry; like apb-core's, it is refused in any build without debug
// assertions, so a release or dist binary never holds one.
#[cfg(all(feature = "testing", not(debug_assertions)))]
compile_error!(
    "the apb-engine `testing` feature is for test builds only and is refused in a build without debug assertions (a release or dist profile)"
);

pub mod adapter;
pub mod agent_env;
mod agent_home;
pub mod agent_probe;
pub mod approval;
pub mod baseline;
pub mod budget;
// candidate trials (issue #192)
pub mod candidate;
pub mod capture;
pub(crate) mod citation;
pub mod connector;
// 0.24.0 irreversible consent
pub mod consent;
pub mod context;
pub mod control;
pub mod corrections;
pub mod decision;
pub mod decision_channel;
pub mod diagnostics;
pub mod driver;
mod env_duration;
pub mod error;
pub mod event;
pub mod exec_key;
pub mod execution_identity;
pub mod follow_up;
// --- 0.24.0 eval suites ---
pub mod eval;
// --- end 0.24.0 eval suites ---
pub mod evidence;
pub mod failure_class;
pub mod fit;
// fork branch-failure policies and join refusals on the run surfaces (issue #195)
pub mod fork_view;
// forward patches (issue #192)
pub mod forward_patch;
pub mod gate;
pub mod grade;
pub mod hooks;
pub mod identity;
pub mod inspect;
pub mod invocation;
pub mod judge_eval;
pub mod known_problems;
pub mod launch_handoff;
pub mod legacy_snapshot;
pub mod list;
pub mod liveness;
// host execution mode (0.23.0)
pub mod host_task;
// seen text of the no_new_items loop exit (spec loops-and-graphs)
pub mod loop_seen;
pub mod manifest;
pub mod next_action;
pub mod notify;
pub mod parallel;
pub mod previous_success;
// operator pause (issue #200)
pub mod pause;
pub mod people;
pub mod proc;
pub mod progress;
pub mod publish;
pub mod question;
pub mod read_only;
pub(crate) mod redact;
pub mod report_html;
pub mod result_clean;
pub mod result_ref;
pub mod result_view;
pub mod resume_kind;
pub mod resume_launch;
pub mod retry_wait;
pub mod review;
pub mod run_binaries;
pub mod run_config;
pub mod run_definitions;
pub mod run_doctor;
pub mod run_end;
pub mod run_estimate;
pub mod run_history;
mod run_lineage;
pub mod run_origin;
pub mod run_outcome;
pub mod run_outputs;
pub mod run_perms;
pub mod run_redact;
// --- run retrospective (#192 part 3) ---
pub mod run_retro;
// --- end of run retrospective ---
mod run_scripts;
mod run_store;
// --- 0.23.0 stats (C3) ---
pub mod run_stats;
// --- end of 0.23.0 stats ---
pub mod run_view;
pub mod run_wait;
pub mod sandbox;
pub mod scenario;
pub mod scheduler;
pub mod script;
pub mod signals;
mod stall;
pub mod state;
pub mod stop;
pub mod transcript;
pub mod waker;
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
    let created = own_test_dir(&std::env::temp_dir(), "apb-engine-unit-xdg");
    let dir = created.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("apb-engine-unit-xdg-{}", std::process::id()))
    });
    // Records the real config dir as forbidden, then redirects to `dir`.
    apb_core::config::test_isolation::isolate(&dir);
    // Every test process (nextest runs one per test) removes its own
    // directory when it exits, so none is left behind in the temp dir.
    let Some(dir) = created else { return };
    let _ = TEST_CONFIG_DIR.set(dir);
    // SAFETY: registers a plain function with no captured state.
    #[cfg(unix)]
    unsafe {
        libc::atexit(remove_test_config);
    }
}

/// A directory under `parent` this process created itself: named by the
/// pid and a random suffix and made with `create_dir`, a name taken already
/// tried again with another suffix, so the at-exit removal only ever
/// removes what this process made. `None` when none could be made.
#[cfg(test)]
fn own_test_dir(parent: &std::path::Path, prefix: &str) -> Option<std::path::PathBuf> {
    use std::hash::{BuildHasher, Hasher};
    for _ in 0..16 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos(),
        );
        let dir = parent.join(format!(
            "{prefix}-{}-{:016x}",
            std::process::id(),
            h.finish()
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Some(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

#[cfg(test)]
static TEST_CONFIG_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

#[cfg(all(test, unix))]
extern "C" fn remove_test_config() {
    if let Some(dir) = TEST_CONFIG_DIR.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

pub use error::EngineError;
pub use hooks::{RunHooks, generate_hooks, read_hooks};
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
    PreparedRun, ResumeDecision, ResumeReason, ResumeRequest, RunFilter, RunListing, RunMode,
    RunOptions, RunResult, RunSummary, StartMode, drive_prepared, drive_run_from_dir, list_runs,
    list_runs_filtered, plan_resume, post_supervisor_command, prepare_supervised_background,
    record_run_error, resume, resume_by, resume_detached, resume_detached_by, resume_detached_req,
    resume_detached_with, resume_req, resume_with, run, run_background, run_background_resolved,
    run_cancel, run_profile_origin, run_resolved, spawn_supervisor_agent, start_detached,
    start_detached_resolved,
};
pub use signals::{SignalCommand, SignalEntry, post_signal, read_signals_after};
pub use stop::{StopOutcome, stop_run, stop_run_by};
