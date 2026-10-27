//! `apb stats` as text for a terminal.

use super::digests::DigestStats;
use super::{PerRun, StatsReport, Waits, findings};

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

fn waits_text(w: &Waits) -> String {
    match w.median_ms {
        Some(m) => format!("median {} over {}", dur(m), w.count),
        None => "none".to_string(),
    }
}

fn per_run_text(p: &PerRun) -> String {
    match p.per_run {
        Some(x) => format!("{x:.2} per run ({} over {} runs)", p.total, p.runs),
        None => "0".to_string(),
    }
}

/// The report as text for a terminal.
pub fn render_text(r: &StatsReport) -> String {
    let mut out = String::new();
    if let Some(note) = &r.note {
        out.push_str(note);
        out.push('\n');
        return out;
    }
    out.push_str(&format!("{} runs\n", r.runs));
    for v in &r.versions {
        out.push_str(&format!(
            "\n{} {}: {} runs\n",
            v.playbook, v.version, v.runs
        ));
        if let Some(note) = &v.note {
            out.push_str(&format!("  note: {note}\n"));
        }
        if v.candidate_trials > 0 {
            out.push_str(&format!(
                "  candidate trials: {} of {} runs\n",
                v.candidate_trials, v.runs
            ));
        }
        let o = &v.outcomes;
        out.push_str(&format!(
            "  outcome: {} succeeded ({} succeeded, {} failed, {} aborted, {} other)\n",
            v.success.text(),
            o.succeeded,
            o.failed,
            o.aborted,
            o.other
        ));
        out.push_str(&format!("  first pass: {}\n", v.first_pass.text()));
        out.push_str(&format!("  retries: {}\n", per_run_text(&v.retries)));
        out.push_str(&format!("  fallbacks: {}\n", per_run_text(&v.fallbacks)));
        out.push_str(&format!(
            "  loop traversals: {}\n",
            per_run_text(&v.loop_traversals)
        ));
        out.push_str(&format!("  gate wait: {}\n", waits_text(&v.gate_wait)));
        out.push_str(&format!(
            "  question wait: {}\n",
            waits_text(&v.question_wait)
        ));
        out.push_str(&format!("  run duration: {}\n", waits_text(&v.duration)));
        if let Some(m) = v.model_use.text() {
            out.push_str(&format!("  models: {m}\n"));
        }
        let s = &v.spend;
        if s.runs_with_usage > 0 {
            let mut line = format!("  tokens: {}", per_run_text(&s.tokens));
            if let Some(c) = s.cost_per_run_usd {
                line.push_str(&format!(
                    ", ${c:.4} per run reported by {} of {} runs",
                    s.runs_with_cost, v.runs
                ));
            }
            if s.cost_known.of > 0 {
                line.push_str(&format!(
                    ", attempts with a known cost {}",
                    s.cost_known.text()
                ));
            }
            out.push_str(&line);
            out.push('\n');
        }
        if v.deliverable_missing + v.output_fields_missing > 0 {
            out.push_str(&format!(
                "  missing: {} deliverables, {} output field sets\n",
                v.deliverable_missing, v.output_fields_missing
            ));
        }
        for g in &v.goal {
            let result = if g.check == "manual" {
                format!("manual in {} runs", g.manual)
            } else {
                let mut t = format!("passed {}", g.passed.text());
                if g.errors > 0 {
                    t.push_str(&format!(", {} could not run", g.errors));
                }
                t
            };
            out.push_str(&format!(
                "  goal {}: {} ({}): {result}\n",
                g.index + 1,
                g.description,
                g.check
            ));
        }
        for line in findings::lines(&v.findings, &v.baseline) {
            out.push_str(&format!("  {line}\n"));
        }
        for n in &v.nodes {
            let mut line = format!(
                "  node {}: {} runs, first pass {}",
                n.node,
                n.runs,
                n.first_pass.text()
            );
            if n.retries + n.fallbacks + n.reentries > 0 {
                line.push_str(&format!(
                    ", {} retries, {} fallbacks, {} re-entries",
                    n.retries, n.fallbacks, n.reentries
                ));
            }
            if let Some(m) = n.duration.median_ms {
                line.push_str(&format!(", median {}", dur(m)));
            }
            if let (Some(e), Some(over)) = (n.expected_s, &n.over_expected) {
                line.push_str(&format!(
                    ", over the expected {} in {}",
                    dur(e * 1000),
                    over.text()
                ));
            }
            if let Some(m) = n.model_use.text() {
                line.push_str(&format!(", models {m}"));
            }
            out.push_str(&line);
            out.push('\n');
            for d in &n.digests {
                out.push_str(&digest_line(d));
            }
        }
    }
    if let Some(c) = &r.compare {
        out.push_str(&format!("\ncompare {} {}", c.playbook, c.base));
        let base_ran = r
            .versions
            .iter()
            .any(|v| v.playbook == c.playbook && v.version == c.base);
        match &c.against {
            _ if !base_ran => out.push_str(&format!(": no runs of {}\n", c.base)),
            None => out.push_str(": no other version has runs\n"),
            Some(a) => {
                out.push_str(&format!(" -> {a}\n"));
                let pct = |d: Option<f64>| {
                    d.map_or("n/a".to_string(), |x| format!("{:+.0} points", x * 100.0))
                };
                let num = |d: Option<f64>| d.map_or("n/a".to_string(), |x| format!("{x:+.2}"));
                out.push_str(&format!("  success: {}\n", pct(c.success_delta)));
                out.push_str(&format!("  first pass: {}\n", pct(c.first_pass_delta)));
                out.push_str(&format!(
                    "  retries per run: {}\n",
                    num(c.retries_per_run_delta)
                ));
                out.push_str(&format!(
                    "  loop traversals per run: {}\n",
                    num(c.loops_per_run_delta)
                ));
                out.push_str(&format!(
                    "  median run duration: {}\n",
                    c.median_duration_delta_ms
                        .map_or("n/a".to_string(), |d| format!("{d:+} ms"))
                ));
                out.push_str(&format!(
                    "  cost per run: {}\n",
                    c.cost_per_run_delta_usd
                        .map_or("n/a".to_string(), |d| format!("{d:+.4} USD"))
                ));
                for x in &c.executor_changes {
                    out.push_str(&format!(
                        "  executor of {}: profile {} -> {}, bundle {} -> {}\n",
                        x.node, x.base_profile, x.against_profile, x.base_bundle, x.against_bundle
                    ));
                }
            }
        }
    }
    out
}

/// One executor group of a node.
fn digest_line(d: &DigestStats) -> String {
    let mut line = format!(
        "    profile {} (bundle {}): {} runs, first pass {}",
        d.profile,
        d.bundle,
        d.runs,
        d.first_pass.text()
    );
    if let Some(m) = d.duration.median_ms {
        line.push_str(&format!(", median {}", dur(m)));
    }
    if let Some(t) = d.tokens_per_visit {
        line.push_str(&format!(", {t} tokens per visit"));
    }
    line + "\n"
}
