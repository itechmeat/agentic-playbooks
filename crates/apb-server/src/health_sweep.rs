//! The dashboard's routine health sweep (D2): a minute after start and
//! then every ten minutes, every watched playbook of every active project
//! is evaluated, so an `overdue` routine trips even when nothing runs.

use std::path::PathBuf;
use std::time::Duration;

/// The test-only override of the period, in milliseconds (debug builds
/// only, through `apb_engine::env_duration::test_override`).
pub(crate) const HEALTH_SWEEP_ENV: &str = "APB_HEALTH_SWEEP_MS";

/// The period between two sweeps: ten minutes.
pub(crate) fn period() -> Duration {
    apb_engine::env_duration::test_override(std::env::var(HEALTH_SWEEP_ENV).ok().as_deref())
        .unwrap_or(Duration::from_secs(600))
}

/// One sweep over `roots`; the number of playbooks evaluated. An error is
/// logged and the next playbook goes on.
pub(crate) fn sweep_once(roots: &[PathBuf], now: &dyn Fn() -> u64) -> usize {
    let mut n = 0;
    for root in roots {
        for (r, res) in apb_engine::health::sweep(root, now) {
            n += 1;
            if let Err(e) = res {
                eprintln!("apb dashboard: health of `{r}`: {e}");
            }
        }
    }
    n
}

/// Starts the task: a minute after start, then every [`period`].
pub(crate) fn spawn() {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60).min(period())).await;
        loop {
            let _ = tokio::task::spawn_blocking(|| {
                let roots: Vec<PathBuf> = apb_core::projects::list_active()
                    .into_iter()
                    .map(|e| PathBuf::from(e.path))
                    .collect();
                sweep_once(&roots, &apb_core::clock::now_ms_u64)
            })
            .await;
            tokio::time::sleep(period()).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_period_defaults_to_ten_minutes() {
        // SAFETY: the variable is only read by this module's own period.
        let saved = std::env::var_os(HEALTH_SWEEP_ENV);
        unsafe { std::env::remove_var(HEALTH_SWEEP_ENV) };
        assert_eq!(period(), Duration::from_secs(600));
        unsafe { std::env::set_var(HEALTH_SWEEP_ENV, "250") };
        let short = period();
        match saved {
            Some(v) => unsafe { std::env::set_var(HEALTH_SWEEP_ENV, v) },
            None => unsafe { std::env::remove_var(HEALTH_SWEEP_ENV) },
        }
        assert_eq!(
            short,
            if cfg!(debug_assertions) {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(600)
            }
        );
    }

    #[test]
    fn sweep_once_evaluates_a_watched_playbook() {
        apb_core::run_origin::ensure_key().unwrap();
        let t = tempfile::tempdir().unwrap();
        let root: &std::path::Path = t.path();
        apb_core::registry::init_project(root).unwrap();
        let vdir = root.join(".apb/playbooks/r/1.0.0");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(
            vdir.join("playbook.yaml"),
            "schema: 2\nid: r\nname: r\nversion: 1.0.0\nhealth:\n  overdue: 26h\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        )
        .unwrap();
        std::fs::write(root.join(".apb/playbooks/r/current"), "1.0.0").unwrap();
        let n = sweep_once(&[root.to_path_buf()], &apb_core::clock::now_ms_u64);
        assert_eq!(n, 1);
        assert!(
            apb_core::signed_store::file::<apb_engine::health::state::HealthFile>(
                root,
                apb_core::scope::Scope::Project,
                "r"
            )
            .exists()
        );
    }
}
