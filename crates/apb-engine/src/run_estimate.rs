//! The run estimate: how long a run of a playbook version takes, and how
//! many tokens and dollars it spends, from comparable past runs on this
//! machine.
//!
//! Comparable means the same playbook id and version and the same
//! execution mode (`cli` or `host`); a node's spend samples come only from
//! runs where that node was bound to the same profile digest (its
//! `profile.yaml` plus SOUL, so its model and run overrides). Time comes
//! from succeeded runs only, spend from every finished visit (a failed
//! visit costs money too). A visit enters the token samples only when every
//! attempt in it reported usage, and the cost samples only when every
//! attempt reported a cost: a partial sum is not a sample, it is counted.
//!
//! The run figures are sums over the gated nodes (see
//! [`apb_core::schema::NodeKind::budget_gated`]): the p50s add up (an approximation of the
//! p50 of the sum), and so do the lows and the highs, which assumes the
//! nodes move together and keeps the range deliberately wide. A list node
//! counts its per-item spend times its item count; a node in a loop counts
//! its per-run total, which covers every traversal.
//!
//! [`snapshot`] keeps the estimate a run started with.

pub mod compose;
mod history;
pub mod sample;
pub mod snapshot;
mod types;

use std::path::{Path, PathBuf};

use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::registry::Registry;
use apb_core::schema::Playbook;
use apb_core::store::ResolvedPlaybook;

use crate::error::EngineError;
use crate::manifest::RunExecutionManifest;
use sample::RunSample;
pub use types::*;

impl EstimateTarget {
    /// The target of a started run: its manifest's bindings (run overrides
    /// included) and execution mode.
    pub fn from_manifest(
        playbook: &Playbook,
        manifest: Option<&RunExecutionManifest>,
        run_id: &str,
    ) -> Self {
        EstimateTarget {
            playbook: playbook.clone(),
            version: playbook.version.clone(),
            execution: manifest
                .and_then(|m| m.execution_mode().ok())
                .unwrap_or_default()
                .as_str()
                .to_string(),
            node_profiles: manifest
                .map(|m| {
                    m.node_digests()
                        .into_iter()
                        .map(|(n, d)| (n, d.profile))
                        .collect()
                })
                .unwrap_or_default(),
            exclude_run: Some(run_id.to_string()),
        }
    }

    /// The target before a start: playbook `id` (`version`, else current)
    /// of the project at `root`, its nodes' profiles resolved the way run
    /// start resolves them. Run overrides are not applied.
    pub fn resolve(
        root: &Path,
        id: &str,
        version: Option<&str>,
        execution: &str,
    ) -> Result<Self, EngineError> {
        Self::resolve_from(
            &root.join(".apb"),
            root,
            PlaybookOrigin::Project,
            id,
            version,
            execution,
        )
    }

    /// The target before a start of a resolved playbook (a project's or a
    /// global one, run in its execution root).
    pub fn for_resolved(r: &ResolvedPlaybook, execution: &str) -> Result<Self, EngineError> {
        Self::resolve_from(
            &r.definition_parent,
            &r.execution_root,
            r.profile_origin(),
            &r.id,
            Some(&r.version),
            execution,
        )
    }

    /// [`EstimateTarget::resolve`] for a definition read from `def_parent`
    /// (a project's `.apb` or the global store) and run in `exec_root`.
    pub fn resolve_from(
        def_parent: &Path,
        exec_root: &Path,
        origin: PlaybookOrigin,
        id: &str,
        version: Option<&str>,
        execution: &str,
    ) -> Result<Self, EngineError> {
        let loaded = Registry::open_dir(def_parent)?.load(id, version)?;
        let playbook = loaded.playbook;
        // A profile that does not resolve is an error, not a node with no
        // history: the start would fail on it too.
        let node_profiles = playbook
            .nodes
            .iter()
            .filter_map(|n| Some((n, n.kind.effective_profile_ref(&playbook.defaults)?)))
            .map(|(n, r)| {
                profile_store::resolve_profile(exec_root, origin, &r)
                    .map(|p| (n.id.clone(), p.profile_digest))
                    .map_err(|e| {
                        EngineError::Invalid(format!("the profile of node `{}`: {e}", n.id))
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(EstimateTarget {
            version: loaded.version,
            playbook,
            execution: execution.to_string(),
            node_profiles,
            exclude_run: None,
        })
    }
}

/// The estimate for `target` from the runs under `roots`.
pub fn estimate(roots: &[PathBuf], target: &EstimateTarget) -> RunEstimate {
    let history = history::comparable(roots, target);
    let samples: Vec<RunSample> = history
        .iter()
        .map(|(dir, run)| RunSample::of(&run.run_id, &run.events, dir.parent()))
        .collect();
    let pairs: Vec<_> = samples
        .iter()
        .zip(&history)
        .map(|(s, (_, run))| (s, &run.node_profiles))
        .collect();
    let mut comparable = Comparable {
        runs: samples.len() as u32,
        window: ESTIMATE_WINDOW as u32,
        ..Comparable::default()
    };
    for s in &samples {
        let (n, no_usage, no_cost) = s.attempt_counts();
        comparable.succeeded += u32::from(s.outcome == Some("succeeded"));
        comparable.runs_without_usage += u32::from(n > 0 && no_usage == n);
        comparable.attempts_without_usage += no_usage;
        comparable.attempts_without_cost += no_cost;
    }
    let mut nodes = Vec::new();
    for node in target
        .playbook
        .nodes
        .iter()
        .filter(|n| n.kind.budget_gated())
    {
        let (runs, mismatch) = compose::matching(target, &node.id, &pairs);
        if mismatch > 0 {
            comparable
                .profile_mismatch
                .insert(node.id.clone(), mismatch);
        }
        nodes.push(compose::node_estimate(node, &runs));
    }
    let unestimated = nodes
        .iter()
        .filter(|n| compose::unestimated(n))
        .map(|n| n.node.clone())
        .collect();
    let (unestimated_tokens, unestimated_cost) = compose::unestimated_by_dimension(&nodes);
    let (tokens, cost_usd) = compose::run_spend(&nodes);
    let succeeded: Vec<&RunSample> = samples
        .iter()
        .filter(|s| s.outcome == Some("succeeded"))
        .collect();
    let note = (comparable.runs < MIN_COMPARABLE).then(|| {
        format!(
            "{} comparable run{}: indicative only",
            comparable.runs,
            if comparable.runs == 1 { "" } else { "s" }
        )
    });
    RunEstimate {
        playbook: target.playbook.id.clone(),
        version: target.version.clone(),
        execution: target.execution.clone(),
        time: compose::time(target, &succeeded),
        comparable,
        tokens,
        cost_usd,
        nodes,
        unestimated,
        unestimated_tokens,
        unestimated_cost,
        note,
    }
}

/// The estimate before a start, for playbook `id` of the project at `root`
/// from that project's runs.
pub fn for_playbook(
    root: &Path,
    id: &str,
    version: Option<&str>,
    execution: &str,
) -> Result<RunEstimate, EngineError> {
    let target = EstimateTarget::resolve(root, id, version, execution)?;
    Ok(estimate(&[root.to_path_buf()], &target))
}

/// One amount in a unit: one decimal below ten, whole above.
fn in_unit(x: f64) -> f64 {
    if x < 10.0 {
        (x * 10.0).round() / 10.0
    } else {
        x.round()
    }
}

/// `950`, `1.3k`, `182k`, `1.5m`: one decimal below ten of a unit, the unit
/// picked after rounding (999600 reads as `1m`, not `1000k`).
fn tokens_text(t: u64) -> String {
    let k = in_unit(t as f64 / 1000.0);
    match t {
        0..1_000 => t.to_string(),
        _ if k < 1000.0 => format!("{k}k"),
        _ => format!("{}m", in_unit(t as f64 / 1e6)),
    }
}

/// `40 s`, `14 min`; 59.6 s reads as `1 min`.
fn minutes_text(ms: u64) -> String {
    let s = (ms as f64 / 1000.0).round() as u64;
    if s < 60 {
        format!("{s} s")
    } else {
        format!("{} min", (ms as f64 / 60_000.0).round() as u64)
    }
}

fn spread_text<T: Copy>(s: &Spread<T>, f: impl Fn(T) -> String, unit: &str) -> String {
    let head = format!("~{}{unit}", f(s.p50));
    match s.kind {
        SpreadKind::Single => head,
        SpreadKind::Iqr => format!("{head} (p25-p75 {}-{}{unit})", f(s.low), f(s.high)),
        SpreadKind::MinMax => format!("{head} (min-max {}-{}{unit})", f(s.low), f(s.high)),
    }
}

/// The estimate as text lines, for the CLI and the MCP `summary`.
pub fn render_lines(e: &RunEstimate) -> Vec<String> {
    let c = &e.comparable;
    let mut head = format!(
        "estimate ({} comparable run{} of {} {}, {}",
        c.runs,
        if c.runs == 1 { "" } else { "s" },
        e.playbook,
        e.version,
        e.execution
    );
    if c.runs_without_usage + c.attempts_without_usage > 0 {
        head.push_str(&format!(
            "; {} without usage, {} attempts without usage",
            c.runs_without_usage, c.attempts_without_usage
        ));
    }
    head.push_str("):");
    let mut out = vec![head];
    let time = match (&e.time.spread, e.time.basis) {
        (Some(s), EstimateBasis::Declared) => {
            format!("~{} (declared expected durations)", minutes_text(s.p50))
        }
        (Some(s), _) => {
            let mut t = spread_text(s, minutes_text, "");
            if let Some(w) = e.time.human_wait_p50_ms.filter(|w| *w > 0) {
                t.push_str(&format!(", human waits ~{}", minutes_text(w)));
            }
            t
        }
        (None, _) => "unknown: no succeeded comparable run and no declared duration".into(),
    };
    out.push(format!("  time    {time}"));
    out.push(format!(
        "  tokens  {}",
        e.tokens.as_ref().map_or_else(
            || "unknown: no comparable attempt reported usage".to_string(),
            |t| spread_text(&t.spread, tokens_text, ""),
        )
    ));
    out.push(format!(
        "  cost    {}",
        e.cost_usd.as_ref().map_or_else(
            || "unknown: no comparable attempt reported a cost".to_string(),
            |c| spread_text(&c.spread, |x| format!("${x:.2}"), ""),
        )
    ));
    if !e.unestimated.is_empty() {
        let parts: Vec<String> = e
            .unestimated
            .iter()
            .map(|n| {
                if c.profile_mismatch.contains_key(n) {
                    format!("{n} (no history with this profile)")
                } else {
                    format!("{n} (no history)")
                }
            })
            .collect();
        out.push(format!("  not estimated: {}", parts.join(", ")));
    }
    if let Some(note) = &e.note {
        out.push(format!("  note: {note}"));
    }
    out
}

#[cfg(test)]
mod text_tests {
    #[test]
    fn units_are_picked_after_rounding() {
        assert_eq!(super::tokens_text(950), "950");
        assert_eq!(super::tokens_text(1_300), "1.3k");
        assert_eq!(super::tokens_text(999_600), "1m");
        assert_eq!(super::tokens_text(1_500_000), "1.5m");
        assert_eq!(super::minutes_text(59_600), "1 min");
        assert_eq!(super::minutes_text(40_000), "40 s");
    }
}
