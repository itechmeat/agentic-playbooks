use std::path::Path;
use std::process::ExitCode;

use apb_core::registry::Registry;

pub(crate) fn print_json(v: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    );
}

pub(crate) fn open_registry(root: &Path) -> Result<Registry, ExitCode> {
    Registry::open(root).map_err(|e| {
        eprintln!("no project here: {e} (run `apb init`)");
        ExitCode::from(2)
    })
}

pub(crate) fn resolve_port(flag: Option<u16>) -> u16 {
    flag.or_else(|| {
        apb_core::config::GlobalConfig::load()
            .ok()
            .and_then(|c| c.port)
    })
    .unwrap_or(apb_core::config::DEFAULT_PORT)
}

/// Bind address for the dashboard: the `--bind` flag, then `server.bind` from
/// the global config, then loopback. A malformed config or an unparseable
/// address is an error rather than a silent fallback, so an operator who
/// mistypes `0.0.0.0` never gets a loopback server they believe is public.
pub(crate) fn resolve_bind(flag: Option<&str>) -> Result<std::net::IpAddr, String> {
    let cfg = apb_core::config::GlobalConfig::load()?;
    cfg.server.resolve_bind(flag)
}

/// Prints left-aligned, space-padded columns with a two-space gutter. Rows are
/// ragged-tolerant: a short row simply has fewer columns. The first row is the
/// header by convention of the callers.
pub(crate) fn print_table(rows: &[Vec<String>]) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            line.push_str(&format!("{:<width$}", cell, width = widths[i]));
        }
        println!("{}", line.trim_end());
    }
}

/// Cap on a sanitized question's rendered length (spec 2026-07-20-interactive-
/// nodes, Security section, fix round 1): long enough to be useful on both
/// the `apb runs` table line and the `apb doctor --run` check line, short
/// enough that a maliciously long question cannot dominate either report.
pub(crate) const QUESTION_TEXT_MAX: usize = 160;

/// Renders agent-generated (untrusted) question text as safe, single-line
/// plain text for a terminal (spec 2026-07-20-interactive-nodes, Security
/// section, fix round 1): the node asking the question is under the
/// playbook author's control, but the question TEXT is model output, so it
/// must not be interpreted as anything other than literal characters.
///
/// Every control character - including the ESC byte that opens an ANSI CSI
/// sequence, embedded `\r`, and `\n` - and every invisible format character
/// (bidirectional overrides, zero-width characters) becomes a space, which both strips
/// the escape channel and guarantees the result cannot break the caller's
/// single-line indent (a raw `\n` would otherwise let the question text
/// forge extra report lines). Runs of whitespace then collapse to one
/// space, the result is trimmed, and it is capped at `max` chars
/// (appending "..." when cut) so one long question cannot dominate the
/// report it appears in. Shared by both print sites that render a pending
/// question's text (`print_waiting_on_question`, `print_pending_question_check`);
/// node ids are not routed through this - they are already validated safe
/// segments, not model output.
pub(crate) fn sanitize_for_terminal(s: &str, max: usize) -> String {
    let cleaned = apb_core::text::replace_hidden(s, ' ');
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        let truncated: String = trimmed.chars().take(max).collect();
        format!("{truncated}...")
    }
}

pub(crate) use apb_core::step_env::StepCaller;
