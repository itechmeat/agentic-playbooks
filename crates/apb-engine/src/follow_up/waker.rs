//! The follow-up waker's body: polls until the due time, drops its lease,
//! and lets `observe_due` take it again.

use std::path::Path;

/// Runs the follow-up waker of run `run_id` for the `run_finished` at
/// `seq`: holds the lease `follow-up-<seq>` while it polls the journal
/// (the state is `Pending` for `seq` and `now < due_at_ms`), so a second
/// arm or `apb resume --all` sees it held and spawns no other waker; then
/// drops the lease and calls `observe_due`, which takes it again and
/// re-checks the state under it. Holding the lease into `observe_due`
/// would make that call find it held and journal nothing.
pub fn run_follow_up_waker(root: &Path, run_id: &str, seq: u64) {
    let run_dir = root.join(".apb/runs").join(run_id);
    let wait = crate::waker::WakerFor::FollowUp(seq);
    let lease = match crate::waker::Lease::try_take(&run_dir, wait) {
        Ok(Some(lease)) => lease,
        // A live waker holds it, or the lease file cannot be opened.
        _ => return,
    };
    let poll = crate::pause::waker::poll();
    loop {
        let now = apb_core::clock::now_ms_u64();
        let events = match crate::event::read_journal(&run_dir) {
            Ok(j) => j.events,
            Err(_) => return,
        };
        let snapshot = crate::legacy_snapshot::load_run_playbook(&run_dir);
        let production =
            crate::manifest::RunOrigin::of_read(&crate::manifest::read(&run_dir)).runs_follow_up();
        match crate::follow_up::state(snapshot.as_ref(), &events, production) {
            crate::follow_up::FollowUpState::Pending {
                finished_seq,
                due_at_ms,
                ..
            } if finished_seq == seq => {
                if now >= due_at_ms {
                    break;
                }
            }
            // Observed, or pending for a later finish, or no longer
            // pending at all (a resume, a failed end): nothing to do.
            _ => return,
        }
        std::thread::sleep(poll);
    }
    // The time came: drop the lease so `observe_due` takes it again and
    // re-checks the state under it.
    drop(lease);
    if let Err(e) = crate::follow_up::read::observe_due(root, run_id, apb_core::clock::now_ms_u64())
    {
        eprintln!("apb: follow-up of run `{run_id}`: {e}");
    }
}
