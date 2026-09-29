//! The text form of the decisions report.

use std::fmt::Write;

use super::{DecisionsReport, GroupReport, SliceMetrics};

fn pct(x: Option<f64>) -> String {
    x.map_or_else(|| "n/a".to_string(), |v| format!("{:.1}%", v * 100.0))
}

fn ci(x: Option<(f64, f64)>) -> String {
    x.map_or_else(String::new, |(lo, hi)| {
        format!(" (95% CI {:.1}-{:.1}%)", lo * 100.0, hi * 100.0)
    })
}

fn secs(ms: u64) -> String {
    if ms >= 60_000 {
        format!("{:.1} min", ms as f64 / 60_000.0)
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

fn slice_lines(out: &mut String, s: &SliceMetrics, threshold: f64) {
    let _ = writeln!(
        out,
        "  accuracy at {threshold:.2}: {}   majority class ({}): {}   today ({}): {}   regex baseline: {} ({} items)",
        pct(s.accuracy),
        s.majority_label,
        pct(s.majority_accuracy),
        s.today_behaviour,
        pct(s.today_accuracy),
        pct(s.regex.accuracy),
        s.regex.items,
    );
    let _ = writeln!(
        out,
        "  recall {}   false actions {}/{}: {}{}",
        pct(s.recall),
        s.false_actions.count,
        s.false_actions.of,
        pct(s.false_actions.rate),
        ci(s.false_actions.ci95),
    );
}

fn group(out: &mut String, g: &GroupReport) {
    let kind = g
        .provider_kind
        .as_deref()
        .map(|k| format!(", {k}"))
        .unwrap_or_default();
    let cal = if g.calibrated {
        "calibrated"
    } else {
        "uncalibrated"
    };
    let _ = writeln!(
        out,
        "{}  {}/{}  ({cal}{kind})",
        g.use_site, g.provider, g.model
    );
    let _ = writeln!(
        out,
        "  decisions {} ({} {}), labelled {} of {} answered ({}): {} act, {} keep",
        g.decisions,
        g.errors,
        if g.errors == 1 { "error" } else { "errors" },
        g.labelled,
        g.answered,
        pct(g.label_coverage),
        g.act_labels,
        g.keep_labels,
    );
    let _ = writeln!(out, "  labels: {}", g.label_source);
    for (why, n) in &g.unlabelled {
        let _ = writeln!(out, "  unlabelled: {n} ({why})");
    }
    if let Some(sh) = &g.shown_to_reviewer {
        let _ = writeln!(
            out,
            "  shown to the reviewer (advise, kept out of the figures): {} labelled, {} agreed ({})",
            sh.labelled,
            sh.agreed,
            pct(sh.agreement),
        );
    }
    if g.labelled == 0 {
        let _ = writeln!(
            out,
            "  eligible for enforce: no ({})",
            g.eligibility.join("; ")
        );
        return;
    }
    let _ = writeln!(
        out,
        "  threshold {:.2} ({})",
        g.threshold, g.threshold_source
    );
    slice_lines(out, &g.all, g.threshold);
    if let Some(w) = &g.would_change {
        let _ = writeln!(
            out,
            "  would_change accuracy: {} ({} items)",
            pct(w.accuracy),
            w.items
        );
    }
    let _ = writeln!(
        out,
        "  calibration: Brier {}, ECE {} (10 bins)",
        g.brier.map_or("n/a".into(), |b| format!("{b:.4}")),
        g.ece.map_or("n/a".into(), |e| format!("{e:.4}")),
    );
    let _ = writeln!(
        out,
        "  long outputs (>= {} chars): {} labelled{}",
        super::LONG_OUTPUT_CHARS,
        g.long_outputs.labelled,
        if g.output_length_unknown > 0 {
            format!(", {} without a journaled length", g.output_length_unknown)
        } else {
            String::new()
        }
    );
    if g.long_outputs.labelled > 0 {
        slice_lines(out, &g.long_outputs, g.threshold);
    }
    let _ = writeln!(
        out,
        "  threshold  coverage  accuracy  recall   false-action rate"
    );
    for r in &g.table {
        let _ = writeln!(
            out,
            "  {:<9.2}  {:<8}  {:<8}  {:<7}  {}{}",
            r.threshold,
            pct(r.coverage),
            pct(r.accuracy),
            pct(r.recall),
            pct(r.false_action_rate),
            ci(r.false_action_ci95),
        );
    }
    match g.suggested_threshold {
        Some(t) => {
            let _ = writeln!(
                out,
                "  best row under the {:.1}% false-action target: {t:.2}",
                g.false_action_target * 100.0
            );
        }
        None => {
            let _ = writeln!(
                out,
                "  no row under the {:.1}% false-action target",
                g.false_action_target * 100.0
            );
        }
    }
    if let Some(s) = &g.savings {
        let _ = writeln!(
            out,
            "  savings estimate at {:.2} (labelled, not measured): {} avoidable attempts x median attempt {} = {}; {} false actions cost {}; decisions ${:.4} and {} latency",
            s.threshold,
            s.avoidable_attempts,
            secs(s.median_attempt_ms),
            secs(s.saved_ms),
            s.false_actions,
            secs(s.lost_ms),
            s.decision_cost_usd,
            secs(s.decision_latency_ms),
        );
    }
    for n in &g.notes {
        let _ = writeln!(out, "  note: {n}");
    }
    let verdict = if g.eligible { "yes" } else { "no" };
    let _ = writeln!(
        out,
        "  eligible for enforce: {verdict} ({})",
        g.eligibility.join("; ")
    );
}

/// The report as text, one block per group; emulation groups last under
/// their own heading.
pub fn render_text(r: &DecisionsReport) -> String {
    let mut out = String::new();
    if let Some(note) = &r.note {
        let _ = writeln!(out, "{note}");
        return out;
    }
    let _ = writeln!(
        out,
        "{} decisions over {} runs; median agent attempt {}",
        r.decisions,
        r.runs,
        r.median_attempt_ms.map_or("n/a".into(), secs),
    );
    let mut emulation_heading = false;
    for g in &r.groups {
        if g.emulation && !emulation_heading {
            let _ = writeln!(out, "\nemulation (uncalibrated, reported on its own)");
            emulation_heading = true;
        }
        out.push('\n');
        group(&mut out, g);
    }
    out
}
