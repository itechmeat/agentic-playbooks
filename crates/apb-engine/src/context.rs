use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::error::EngineError;
use crate::event::{Event, EventPayload};
use crate::state::ReviewDecision;

/// A context section (heading + body) with the seq of the event that
/// produced it. Needed to split the context into "old" (subject to
/// compaction) and "recent tail" (raw).
struct Section {
    seq: u64,
    text: String,
}

/// Parses the event log into ordered context sections. The single source for
/// the full context, the tail, and compaction ranges - the section format is
/// defined here once.
fn sections(events: &[Event]) -> Vec<Section> {
    let mut out = Vec::new();
    for e in events {
        match &e.payload {
            EventPayload::NodeFinished {
                node,
                status,
                attempt,
                output,
                ..
            } => {
                let mut text = String::new();
                let _ = write!(
                    text,
                    "## {node} ({status}, attempt {attempt})\n\n{output}\n\n"
                );
                out.push(Section { seq: e.seq, text });
            }
            // Supervisor notes (ContextAppend), injected into the event log
            // - rendered in order of appearance interleaved with node
            // sections, so {{run.context}} in subsequent prompts sees the
            // note right after it was applied.
            EventPayload::SupervisorAction { action, detail, .. } if action == "context_append" => {
                let mut text = String::new();
                let _ = write!(text, "## note (supervisor)\n\n{detail}\n\n");
                out.push(Section { seq: e.seq, text });
            }
            _ => {}
        }
    }
    out
}

/// The full context (all sections), the materialized view for context.md and the compaction threshold.
pub fn build_context(events: &[Event]) -> String {
    sections(events).into_iter().map(|s| s.text).collect()
}

/// Sections with seq strictly greater than `after_seq` - the uncompacted tail on top of the summary.
pub fn build_context_tail(events: &[Event], after_seq: u64) -> String {
    sections(events)
        .into_iter()
        .filter(|s| s.seq > after_seq)
        .map(|s| s.text)
        .collect()
}

/// Text of sections with seq in the range (after_seq, up_to_seq] - what still
/// needs to be folded into the next compaction on top of the previous summary.
pub fn sections_between(events: &[Event], after_seq: u64, up_to_seq: u64) -> String {
    sections(events)
        .into_iter()
        .filter(|s| s.seq > after_seq && s.seq <= up_to_seq)
        .map(|s| s.text)
        .collect()
}

/// Compaction boundary: a seq such that sections with seq > boundary (the
/// recent tail) together do not exceed `keep_budget` bytes, and everything
/// older is subject to compaction. Returns the seq of the newest of the OLD
/// sections. None if all sections fit in the tail (nothing to compact). A
/// single section that alone exceeds the budget stays in the tail
/// (termination guarantee).
pub fn compaction_boundary(events: &[Event], keep_budget: usize) -> Option<u64> {
    let secs = sections(events);
    let mut kept = 0usize;
    // Index of the first (oldest) section that made it into the tail.
    let mut first_kept = secs.len();
    for i in (0..secs.len()).rev() {
        let len = secs[i].text.len();
        if kept + len > keep_budget && kept > 0 {
            break;
        }
        kept += len;
        first_kept = i;
    }
    if first_kept == 0 {
        return None;
    }
    Some(secs[first_kept - 1].seq)
}

/// The latest compaction in the log: (summary file, up_to_seq). None if there was no compaction.
pub fn latest_compaction(events: &[Event]) -> Option<(String, u64)> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ContextCompacted {
            compact_file,
            up_to_seq,
            ..
        } => Some((compact_file.clone(), *up_to_seq)),
        _ => None,
    })
}

/// Renders the leading `## run instruction` section for a non-empty run
/// instruction, or an empty string when there is none. Shared by
/// `build_context_for_render` (what `{{run.context}}` actually resolves to in
/// every rendered node prompt) and `rebuild_context_md` (the context.md
/// materialized view) so a run-level instruction reaches every node, not only
/// ones whose author remembered to reference `{{run.instruction}}` explicitly
/// (a summarizing first node otherwise silently drops it downstream). Blank
/// after trimming counts as absent, so a run with no real instruction renders
/// byte-identical to before this section existed.
pub(crate) fn instruction_section(instruction: Option<&str>) -> String {
    match instruction.map(str::trim) {
        Some(text) if !text.is_empty() => format!("## run instruction\n\n{text}\n\n"),
        _ => String::new(),
    }
}

/// Context for rendering a prompt with compaction taken into account:
/// summary from the compact file (if any) plus the uncompacted tail
/// (sections newer than up_to_seq). Without compaction - the full context. A
/// missing/unreadable compact file degrades to an empty summary (the tail is
/// kept), so an artifact failure does not bring down the run. `instruction` is
/// the run's `RunConfig.instruction` (the caller already has it in scope as
/// `cfg.instruction`) - prepended as a `## run instruction` section ahead of
/// everything else, see `instruction_section`.
pub fn build_context_for_render(
    run_dir: &Path,
    events: &[Event],
    instruction: Option<&str>,
) -> Result<String, EngineError> {
    let header = instruction_section(instruction);
    let Some((file, up_to)) = latest_compaction(events) else {
        return Ok(format!("{header}{}", build_context(events)));
    };
    let summary = std::fs::read_to_string(run_dir.join(&file)).unwrap_or_default();
    let tail = build_context_tail(events, up_to);
    let mut out = header;
    let summary = summary.trim();
    if !summary.is_empty() {
        out.push_str("## summary (compacted)\n\n");
        out.push_str(summary);
        out.push_str("\n\n");
    }
    out.push_str(&tail);
    Ok(out)
}

/// The terminal context: the run-instruction header followed by EVERY section
/// in the append-only event log, deliberately WITHOUT compaction. The terminal
/// finish-with-prompt node composes the run's final answer and must see every
/// completed node's raw output.
///
/// `build_context_for_render` is a lossy, budget-driven view for MID-RUN
/// prompts: once the accumulated context exceeds `context_max_bytes` (which the
/// re-run duplicate sections of repeated resume + patch-migration cycles push it
/// over), compaction replaces old sections with a cheap-model summary, and each
/// further compaction re-summarizes the previous summary until the earliest
/// nodes' substantive output is gone. That is correct for keeping a running
/// prompt inside a token budget, but wrong for the terminal answer - the run's
/// final deliverable would be composed from a summary that reports "no shipped
/// work" even though every output still lives verbatim in the log (issue #42
/// finding 5). The full record is always available here, so the terminal node
/// reads it directly.
pub fn build_terminal_context(events: &[Event], instruction: Option<&str>) -> String {
    format!(
        "{}{}",
        instruction_section(instruction),
        build_context(events)
    )
}

/// Manual scan for `{{ ... }}` without regex; substitutes known references, unknown ones -> "".
pub fn render(
    text: &str,
    params: &BTreeMap<String, String>,
    instruction: Option<&str>,
    outputs: &BTreeMap<String, String>,
    reviews: &BTreeMap<String, ReviewDecision>,
    hooks: &BTreeMap<String, String>,
    context: &str,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        if let Some(close) = after.find("}}") {
            let key = after[..close].trim();
            out.push_str(&resolve(
                key,
                params,
                instruction,
                outputs,
                reviews,
                hooks,
                context,
            ));
            rest = &after[close + 2..];
        } else {
            out.push_str(&rest[open..]);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

fn resolve(
    key: &str,
    params: &BTreeMap<String, String>,
    instruction: Option<&str>,
    outputs: &BTreeMap<String, String>,
    reviews: &BTreeMap<String, ReviewDecision>,
    hooks: &BTreeMap<String, String>,
    context: &str,
) -> String {
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["params", name] => params.get(*name).cloned().unwrap_or_default(),
        ["run", "instruction"] => instruction.unwrap_or("").to_string(),
        ["run", "context"] => context.to_string(),
        ["run", "hooks", key] => hooks.get(*key).cloned().unwrap_or_default(),
        ["nodes", id, "output"] | ["nodes", id, "report"] => {
            outputs.get(*id).cloned().unwrap_or_default()
        }
        ["nodes", id, "review_note"] => {
            reviews.get(*id).map(|r| r.note.clone()).unwrap_or_default()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `instruction_section` is `pub(crate)`, not reachable from the
    // integration-test binary, so it is covered here inline instead (project
    // convention: unit tests for non-pub items live next to the code).
    #[test]
    fn instruction_section_is_empty_for_none() {
        assert_eq!(instruction_section(None), "");
    }

    #[test]
    fn instruction_section_is_empty_for_blank_and_whitespace_only() {
        assert_eq!(instruction_section(Some("")), "");
        assert_eq!(instruction_section(Some("   ")), "");
        assert_eq!(instruction_section(Some("\n\t  \n")), "");
    }

    #[test]
    fn instruction_section_renders_trimmed_text_between_headings() {
        assert_eq!(
            instruction_section(Some("  stay within budget  ")),
            "## run instruction\n\nstay within budget\n\n"
        );
    }
}
