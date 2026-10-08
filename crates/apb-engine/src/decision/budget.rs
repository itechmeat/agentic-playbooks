//! The run's decision budget (`budget.max_requests_per_run`,
//! `budget.max_usd_per_run`) shared by every writer of the run's journal:
//! the drive's runner and any number of `decision_ask` / `apb decide` calls
//! from host tasks, each in its own process.
//!
//! Scheme: reserve, then call. Before a request goes out, the caller takes
//! a slot under the journal's append lock (`event::with_journal_lock`):
//! it re-reads the journal's `decision_made` events (every writer's sent
//! requests and their cost), adds the requests still in flight (one marker
//! file each under `decisions-inflight/`, any process), and takes a slot
//! only while the sum stays under the cap, writing its own marker before
//! the lock is released. The marker is removed once the decision is
//! journaled, so a request is always counted at least once (its marker or
//! its event; briefly both, which only errs on the safe side) and never
//! zero times. Check-call-append-recheck was rejected: a recheck after the
//! reply cannot unsend a request, so parallel callers would overshoot.
//!
//! A marker older than [`INFLIGHT_STALE`] belongs to a caller that died
//! mid-request; it no longer counts and is removed by the next reserve.
//! The cost cap is checked against what was journaled: a request in flight
//! has no cost yet.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::event::{Event, EventPayload};

/// Where the in-flight markers live, under the run directory.
const INFLIGHT_DIR: &str = "decisions-inflight";

/// How long an in-flight marker counts: well past any provider chain's
/// bounded retries, so only a dead caller's marker gets this old.
pub(crate) const INFLIGHT_STALE: Duration = Duration::from_secs(15 * 60);

/// Requests and cost a writer knows to be spent.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Spent {
    pub(crate) requests: u32,
    pub(crate) cost_usd: f64,
}

impl Spent {
    /// What the journal's decisions spent: every request a provider was
    /// actually asked (cache hits excluded), and its cost.
    pub(crate) fn of(events: &[Event]) -> Spent {
        let mut s = Spent::default();
        for e in events {
            if let EventPayload::DecisionMade {
                provider,
                cached,
                cost_usd,
                ..
            } = &e.payload
            {
                if provider.is_some() && !cached {
                    s.requests = s.requests.saturating_add(1);
                }
                s.cost_usd += cost_usd.unwrap_or(0.0);
            }
        }
        s
    }
}

/// A slot taken from the run's budget; dropping it removes its marker.
#[derive(Debug)]
pub(crate) struct Reservation {
    marker: Option<PathBuf>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(p) = self.marker.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Counts the live in-flight markers and removes the stale ones.
fn live_inflight(dir: &Path) -> u32 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut n = 0u32;
    for entry in entries.flatten() {
        let age = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok());
        match age {
            Some(a) if a >= INFLIGHT_STALE => {
                let _ = std::fs::remove_file(entry.path());
            }
            _ => n = n.saturating_add(1),
        }
    }
    n
}

/// Takes a slot from the budget of the run at `run_dir`, or `None` when it
/// is spent. `local` is what the caller itself counted (its completed
/// requests and their cost) and `local_inflight` its own requests in
/// flight: a caller whose journal is not the run's file still never
/// exceeds the cap on its own.
pub(crate) fn reserve(
    run_dir: &Path,
    max_requests: u32,
    max_usd: f64,
    local: Spent,
    local_inflight: u32,
) -> Option<Reservation> {
    let dir = run_dir.join(INFLIGHT_DIR);
    let take = || -> Option<Reservation> {
        let disk = crate::event::read_all_lossy_tail(run_dir)
            .map(|e| Spent::of(&e))
            .unwrap_or_default();
        let requests = disk
            .requests
            .max(local.requests)
            .saturating_add(live_inflight(&dir).max(local_inflight));
        let cost = disk.cost_usd.max(local.cost_usd);
        if requests >= max_requests || cost >= max_usd {
            return None;
        }
        let marker = std::fs::create_dir_all(&dir).ok().and_then(|()| {
            let p = dir.join(format!(
                "{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::write(&p, b"").ok().map(|()| p)
        });
        Some(Reservation { marker })
    };
    if !run_dir.is_dir() {
        // No run directory to share: the caller's own count decides.
        let requests = local.requests.saturating_add(local_inflight);
        return (requests < max_requests && local.cost_usd < max_usd)
            .then_some(Reservation { marker: None });
    }
    crate::event::with_journal_lock(run_dir, take)
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_of_other_callers_count_until_dropped() {
        let run = tempfile::tempdir().unwrap();
        let a = reserve(run.path(), 2, 10.0, Spent::default(), 0).expect("first slot");
        let b = reserve(run.path(), 2, 10.0, Spent::default(), 0).expect("second slot");
        assert!(reserve(run.path(), 2, 10.0, Spent::default(), 0).is_none());
        drop(a);
        let c = reserve(run.path(), 2, 10.0, Spent::default(), 0);
        assert!(c.is_some());
        drop((b, c));
    }

    #[test]
    fn journaled_requests_and_cost_count() {
        let run = tempfile::tempdir().unwrap();
        let mut log = crate::event::EventLog::create(run.path()).unwrap();
        log.append(made(Some(0.5))).unwrap();
        assert!(reserve(run.path(), 1, 10.0, Spent::default(), 0).is_none());
        assert!(reserve(run.path(), 5, 0.5, Spent::default(), 0).is_none());
        assert!(reserve(run.path(), 5, 1.0, Spent::default(), 0).is_some());
    }

    fn made(cost: Option<f64>) -> EventPayload {
        EventPayload::DecisionMade {
            use_site: "host_task".into(),
            node: None,
            attempt: None,
            provider: Some("fake".into()),
            model: Some("m".into()),
            calibrated: false,
            mode: "shadow".into(),
            questions_digest: "q".into(),
            state_digest: "s".into(),
            state_bytes: 0,
            output_chars: None,
            answers: Default::default(),
            applied: false,
            would_change: None,
            baseline: None,
            latency_ms: 1,
            input_tokens: None,
            cost_usd: cost,
            cost_estimated: false,
            cached: false,
            error: None,
            enforce_refused: None,
            join: Default::default(),
        }
    }
}
