//! The retrospective as compact text: what `{{run.retro}}` renders into a
//! prompt. Bounded ([`MAX_BYTES`]): a long run's tail is cut at a line and a
//! note points at the full report (MCP `run_retro_context`).

use std::path::Path;

use super::{AttemptRetro, DEFAULT_COMPARE_LAST, NodeRetro, RetroReport, RetroRun};

/// The most bytes `{{run.retro}}` renders.
pub const MAX_BYTES: usize = 8 * 1024;

const TRUNCATED: &str = "(truncated: the full report is MCP run_retro_context with this run id)\n";

/// `12 s`, `3.4 min`, `1.2 h`.
fn dur(ms: u64) -> String {
    let s = ms as f64 / 1000.0;
    if s < 60.0 {
        format!("{s:.0} s")
    } else if s < 3600.0 {
        format!("{:.1} min", s / 60.0)
    } else {
        format!("{:.1} h", s / 3600.0)
    }
}

fn signed_dur(delta: i64) -> String {
    let sign = if delta < 0 { "-" } else { "+" };
    format!("{sign}{}", dur(delta.unsigned_abs()))
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

fn header(r: &RetroReport) -> String {
    let mut line = format!(
        "Run {} of {} {}: {}",
        r.run_id,
        r.playbook,
        r.version,
        r.outcome.as_deref().unwrap_or("still running")
    );
    if let Some(x) = &r.execution {
        line.push_str(&format!(", execution {x}"));
    }
    if let Some(d) = r.duration_ms {
        line.push_str(&format!(", {}", dur(d)));
    }
    if let Some(t) = r.tokens {
        line.push_str(&format!(", {t} tokens"));
    }
    if let Some(c) = r.cost_usd {
        line.push_str(&format!(", ${c:.4}"));
    }
    line.push('\n');
    line
}

fn baseline_line(r: &RetroReport) -> String {
    let Some(b) = &r.baseline else {
        return "Baseline: no earlier finished run of this version\n".to_string();
    };
    let mut line = format!(
        "Baseline: last {} of this version, {} succeeded",
        plural(b.run_ids.len(), "run"),
        b.success.text()
    );
    if let Some(m) = b.median_duration_ms {
        line.push_str(&format!(", median {}", dur(m)));
        if let Some(d) = b.duration_delta_ms {
            line.push_str(&format!(" (this run {})", signed_dur(d)));
        }
    }
    if let Some(t) = b.median_tokens {
        line.push_str(&format!(", median {t} tokens"));
    }
    if let Some(c) = b.median_cost_usd {
        line.push_str(&format!(", median ${c:.4}"));
    }
    line.push('\n');
    line
}

fn goal_lines(r: &RetroReport) -> String {
    let Some(g) = &r.goal else {
        return String::new();
    };
    let mut out = format!("Goal: {} ({})\n", g.statement, g.line());
    for c in &g.criteria {
        out.push_str(&format!("  [{}] {}", c.status, c.description));
        if let Some(d) = &c.detail {
            out.push_str(&format!(": {d}"));
        }
        out.push('\n');
    }
    out
}

fn node_line(n: &NodeRetro) -> String {
    let mut line = format!(
        "- {}: {}, {}",
        n.node,
        n.status.as_deref().unwrap_or("running"),
        dur(n.duration_ms)
    );
    if let Some(e) = n.expected_s {
        let verdict = if n.over_expected == Some(true) {
            "over"
        } else {
            "within"
        };
        line.push_str(&format!(" ({verdict} the expected {})", dur(e * 1000)));
    }
    if let Some(m) = n.baseline_median_ms {
        line.push_str(&format!(", baseline median {}", dur(m)));
    }
    line.push_str(&format!(
        ", {}, {}",
        plural(n.executions, "execution"),
        plural(n.attempts.len(), "attempt")
    ));
    if n.retries + n.fallbacks + n.reentries > 0 {
        line.push_str(&format!(
            ", {} retries, {} fallbacks, {} re-entries",
            n.retries, n.fallbacks, n.reentries
        ));
    }
    if let Some(t) = n.tokens {
        line.push_str(&format!(", {t} tokens"));
    }
    if let Some(c) = n.cost_usd {
        line.push_str(&format!(", ${c:.4}"));
    }
    if !n.models.is_empty() {
        line.push_str(&format!(", model {}", n.models.join(", ")));
    }
    if let Some(w) = n.host_wait_ms {
        line.push_str(&format!(", host wait {}", dur(w)));
    }
    line.push('\n');
    line
}

fn attempt_line(a: &AttemptRetro) -> String {
    let mut line = format!(
        "  attempt {}: {}",
        a.attempt,
        a.status.as_deref().unwrap_or("running")
    );
    if let Some(d) = a.duration_ms {
        line.push_str(&format!(", {}", dur(d)));
    }
    match (&a.model, a.model_source.as_str()) {
        (Some(m), src) => line.push_str(&format!(", model {m} ({src})")),
        (None, "unreported") => line.push_str(", model not reported by the host"),
        _ => {}
    }
    if let Some(v) = &a.verdict {
        line.push_str(&format!(", verdict {v}"));
    }
    if let Some(k) = &a.failure_kind {
        line.push_str(&format!(", failure {k}"));
    }
    line.push('\n');
    line
}

/// The whole report as text.
pub fn render(r: &RetroReport) -> String {
    let mut out = header(r);
    out.push_str(&baseline_line(r));
    out.push_str(&goal_lines(r));
    out.push_str("Nodes:\n");
    for n in &r.nodes {
        out.push_str(&node_line(n));
        // A single clean attempt says nothing the node line does not.
        let plain = n.attempts.len() == 1
            && n.attempts[0].failure_kind.is_none()
            && n.attempts[0].verdict.as_deref() != Some("failure");
        if !plain {
            for a in &n.attempts {
                out.push_str(&attempt_line(a));
            }
        }
    }
    out
}

/// [`render`], cut at a line boundary to at most `max` bytes (the note
/// included).
pub fn render_bounded(r: &RetroReport, max: usize) -> String {
    let full = render(r);
    if full.len() <= max {
        return full;
    }
    let budget = max.saturating_sub(TRUNCATED.len());
    let mut out = String::new();
    for line in full.split_inclusive('\n') {
        if out.len() + line.len() > budget {
            break;
        }
        out.push_str(line);
    }
    out.push_str(TRUNCATED);
    out
}

/// What `{{run.retro}}` renders: the report of the run at `run_dir` as of
/// now (the moment the node's prompt is rendered), compared with the last
/// [`DEFAULT_COMPARE_LAST`] runs of its version, as text bounded by
/// [`MAX_BYTES`]. Empty when the journal does not read.
pub fn prompt_text(run_dir: &Path) -> String {
    let report = match super::project_root_of(run_dir) {
        Some(root) => super::retro(&root, run_dir, DEFAULT_COMPARE_LAST),
        None => RetroRun::load(run_dir).map(|r| super::build(&r, &[], 0)),
    };
    report
        .map(|r| render_bounded(&r, MAX_BYTES))
        .unwrap_or_default()
}
