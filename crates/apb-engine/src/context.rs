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

/// Applied supervisor context notes, oldest first (issue #45 finding 2).
/// Source of truth is the event log (`SupervisorAction{action: context_append}`),
/// not the control channel: only notes the drive has already applied are
/// returned, so a note posted mid-attempt is not visible until the next
/// attempt boundary.
pub fn applied_supervisor_notes(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SupervisorAction { action, detail, .. } if action == "context_append" => {
                Some(detail.clone())
            }
            _ => None,
        })
        .collect()
}

/// Header line for the trailing supervisor-notes block. States that applied
/// notes override both the node template and the run instruction on conflict
/// (issue #56 finding 4).
const SUPERVISOR_NOTES_HEADER: &str =
    "Supervisor notes (these override the node template and the run instruction on conflict):";

/// Delimited block injected into agent attempt prompts so notes reach the
/// executor even when the playbook template does not reference
/// `{{run.context}}`. Empty string when there are no applied notes.
///
/// Delivery scope (also documented on the MCP tool): every NEW agent_task
/// (and finish-with-prompt) attempt that starts after a note was applied sees
/// all notes applied so far, most recent last. Notes never enter script nodes
/// and are never written into the immutable run manifest. The header states
/// that applied notes override the node template and the run instruction on
/// conflict (issue #56 finding 4).
pub fn supervisor_notes_section(events: &[Event]) -> String {
    let notes = applied_supervisor_notes(events);
    if notes.is_empty() {
        return String::new();
    }
    let mut out = format!("\n\n{SUPERVISOR_NOTES_HEADER}\n");
    for note in &notes {
        let _ = writeln!(out, "- {note}");
    }
    out
}

/// Appends [`supervisor_notes_section`] to `prompt` when any notes are present.
pub fn with_supervisor_notes(prompt: &str, events: &[Event]) -> String {
    let section = supervisor_notes_section(events);
    if section.is_empty() {
        prompt.to_string()
    } else {
        format!("{prompt}{section}")
    }
}

/// Trailing run-instruction block appended to agent attempt prompts (issue #56
/// finding 4). Mirrors [`supervisor_notes_section`]: an explicit trailing
/// section, NOT a placeholder substitution, so the run instruction reaches the
/// executor even when the node template references neither `{{run.context}}`
/// nor `{{run.instruction}}`. Before this the instruction was delivered only
/// through those placeholders, so a template that omitted both dropped it
/// silently. Empty string when the instruction is absent or blank after
/// trimming (byte-stable no-op). When the template DOES reference
/// `{{run.context}}` the instruction also appears in that context header; the
/// duplication is idempotent and accepted, exactly as for supervisor notes.
pub fn run_instruction_section(instruction: Option<&str>) -> String {
    match instruction.map(str::trim) {
        Some(text) if !text.is_empty() => format!("\n\n## run instruction\n\n{text}\n"),
        _ => String::new(),
    }
}

/// Appends [`run_instruction_section`] to `prompt` when a non-empty run
/// instruction is present.
pub fn with_run_instruction(prompt: &str, instruction: Option<&str>) -> String {
    let section = run_instruction_section(instruction);
    if section.is_empty() {
        prompt.to_string()
    } else {
        format!("{prompt}{section}")
    }
}

/// Trailing precedence frame for agent attempt prompts (issue #56 finding 4).
///
/// When a non-empty run instruction and/or any applied supervisor note is
/// present, returns a short English block that states the on-conflict order:
/// supervisor notes > run instruction > node template. Empty (byte-stable
/// no-op) when both the instruction is absent/blank and there are no notes.
/// Does not re-embed the full instruction text; the instruction body itself is
/// delivered by [`run_instruction_section`] as a `## run instruction` section.
pub fn precedence_frame(instruction: Option<&str>, events: &[Event]) -> String {
    let has_instruction = instruction.map(str::trim).is_some_and(|t| !t.is_empty());
    let has_notes = !applied_supervisor_notes(events).is_empty();
    if !has_instruction && !has_notes {
        return String::new();
    }
    // Short pointer only: names the three sources and the order. Does not
    // re-paste the run instruction body or note text (those are delivered
    // elsewhere: `## run instruction` section / supervisor notes block). The
    // wording says the instruction "appears in a `## run instruction` section"
    // rather than "the leading" one, because `run_instruction_section` now
    // guarantees that section on every attempt whose run carries an
    // instruction, regardless of whether the template references
    // `{{run.context}}` (issue #56 finding 4).
    String::from(
        "\n\n## instruction precedence\n\n\
On conflict, higher-priority sources override lower ones: supervisor notes override the run instruction, and the run instruction overrides this node template's boilerplate. The run instruction (when present) appears in a `## run instruction` section; supervisor notes (when present) appear in the trailing supervisor notes block.\n",
    )
}

/// Assembles the trailing instruction-source blocks appended to an agent
/// attempt prompt after the rendered template (issue #56 finding 4): the run
/// instruction, then the applied supervisor notes, then the precedence frame
/// naming the on-conflict order (supervisor notes > run instruction > node
/// template). Shared by `execute_node` and `execute_finish_answer` so the two
/// assembly sites cannot drift. The run instruction and supervisor notes are
/// appended as explicit trailing sections (not placeholder substitutions), so a
/// node template that references neither `{{run.context}}` nor
/// `{{run.instruction}}` still receives both. Byte-stable no-op (returns the
/// template unchanged) when the instruction is absent/blank and no notes apply,
/// so note-less, instruction-less prompts and their cache keys do not shift.
pub fn assemble_agent_prompt(
    template: &str,
    instruction: Option<&str>,
    events: &[Event],
) -> String {
    let mut text = with_run_instruction(template, instruction);
    text = with_supervisor_notes(&text, events);
    text.push_str(&precedence_frame(instruction, events));
    text
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

    fn ev(seq: u64, p: EventPayload) -> Event {
        Event {
            seq,
            ts: 0,
            payload: p,
        }
    }

    #[test]
    fn supervisor_notes_section_is_empty_without_notes() {
        let events = vec![ev(
            0,
            EventPayload::NodeFinished {
                node: "a".into(),
                status: "succeeded".into(),
                attempt: 1,
                output: "ok".into(),
                artifacts: Vec::new(),
            },
        )];
        assert_eq!(supervisor_notes_section(&events), "");
        assert_eq!(with_supervisor_notes("do work", &events), "do work");
    }

    #[test]
    fn supervisor_notes_section_lists_notes_oldest_first() {
        let events = vec![
            ev(
                0,
                EventPayload::SupervisorAction {
                    action: "context_append".into(),
                    node: None,
                    detail: "first note".into(),
                },
            ),
            ev(
                1,
                EventPayload::SupervisorAction {
                    action: "node_retry".into(),
                    node: Some("a".into()),
                    detail: String::new(),
                },
            ),
            ev(
                2,
                EventPayload::SupervisorAction {
                    action: "context_append".into(),
                    node: None,
                    detail: "second note".into(),
                },
            ),
        ];
        let section = supervisor_notes_section(&events);
        assert!(
            section.starts_with(
                "\n\nSupervisor notes (these override the node template and the run instruction on conflict):\n"
            ),
            "got: {section:?}"
        );
        let first = section.find("first note").expect("first note");
        let second = section.find("second note").expect("second note");
        assert!(first < second, "most recent last: {section}");
        let injected = with_supervisor_notes("do work", &events);
        assert!(
            injected.starts_with(
                "do work\n\nSupervisor notes (these override the node template and the run instruction on conflict):\n"
            ),
            "got: {injected}"
        );
        assert!(injected.contains("- first note"));
        assert!(injected.contains("- second note"));
    }

    // Issue #56 finding 4: when applied notes exist, the section must carry
    // an explicit on-conflict-override frame (notes beat template AND run
    // instruction). Header wording is the contract agents read.
    #[test]
    fn supervisor_notes_section_frames_override_on_conflict() {
        let events = vec![ev(
            0,
            EventPayload::SupervisorAction {
                action: "context_append".into(),
                node: None,
                detail: "prefer approach B".into(),
            },
        )];
        let section = supervisor_notes_section(&events);
        assert!(
            section.contains(
                "Supervisor notes (these override the node template and the run instruction on conflict):"
            ),
            "notes section must state override over template and run instruction, got: {section:?}"
        );
        assert!(section.contains("- prefer approach B"));
        let lower = section.to_ascii_lowercase();
        assert!(
            lower.contains("override")
                && lower.contains("node template")
                && lower.contains("run instruction"),
            "override framing must name both lower-priority sources, got: {section:?}"
        );
    }

    // Note-less prompts stay byte-stable: cache keys and many tests depend on
    // with_supervisor_notes being a pure no-op when nothing was applied.
    #[test]
    fn with_supervisor_notes_unchanged_without_notes_no_spurious_frame() {
        let prompt = "exact template body";
        assert_eq!(with_supervisor_notes(prompt, &[]), prompt);
        let events = vec![ev(
            0,
            EventPayload::NodeFinished {
                node: "a".into(),
                status: "succeeded".into(),
                attempt: 1,
                output: "ok".into(),
                artifacts: Vec::new(),
            },
        )];
        assert_eq!(with_supervisor_notes(prompt, &events), prompt);
        // Precedence frame is a separate helper; notes path must not invent it.
        assert!(!with_supervisor_notes(prompt, &events).contains("precedence"));
    }

    #[test]
    fn precedence_frame_empty_without_instruction_and_without_notes() {
        assert_eq!(precedence_frame(None, &[]), "");
        assert_eq!(precedence_frame(Some(""), &[]), "");
        assert_eq!(precedence_frame(Some("   "), &[]), "");
        assert_eq!(precedence_frame(Some("\n\t  \n"), &[]), "");
        let events = vec![ev(
            0,
            EventPayload::NodeFinished {
                node: "a".into(),
                status: "succeeded".into(),
                attempt: 1,
                output: "ok".into(),
                artifacts: Vec::new(),
            },
        )];
        assert_eq!(precedence_frame(None, &events), "");
        assert_eq!(precedence_frame(Some("  "), &events), "");
    }

    #[test]
    fn precedence_frame_states_order_when_instruction_present() {
        let frame = precedence_frame(Some("stay within budget"), &[]);
        assert!(
            !frame.is_empty(),
            "non-empty instruction must produce a precedence frame"
        );
        let lower = frame.to_ascii_lowercase();
        assert!(
            lower.contains("override") || lower.contains("precedence"),
            "frame must state override/precedence, got: {frame:?}"
        );
        assert!(
            lower.contains("run instruction") && lower.contains("template"),
            "frame must name run instruction vs template, got: {frame:?}"
        );
        assert!(
            lower.contains("supervisor"),
            "frame must place supervisor notes in the order, got: {frame:?}"
        );
        // Do not re-paste the full instruction body into the frame.
        assert!(
            !frame.contains("stay within budget"),
            "frame must reference precedence without re-embedding the instruction text, got: {frame:?}"
        );
    }

    #[test]
    fn precedence_frame_states_order_when_notes_present_without_instruction() {
        let events = vec![ev(
            0,
            EventPayload::SupervisorAction {
                action: "context_append".into(),
                node: None,
                detail: "use path A".into(),
            },
        )];
        let frame = precedence_frame(None, &events);
        assert!(
            !frame.is_empty(),
            "applied notes alone must still produce a precedence frame"
        );
        let lower = frame.to_ascii_lowercase();
        assert!(
            lower.contains("supervisor")
                && (lower.contains("override") || lower.contains("precedence")),
            "got: {frame:?}"
        );
        // Notes body is delivered by supervisor_notes_section, not re-listed here.
        assert!(
            !frame.contains("use path A"),
            "precedence frame must not re-list note bodies, got: {frame:?}"
        );
    }

    #[test]
    fn precedence_frame_does_not_duplicate_multi_kb_instruction() {
        let long = "x".repeat(4096);
        let frame = precedence_frame(Some(&long), &[]);
        assert!(!frame.is_empty());
        assert!(
            !frame.contains(&long),
            "frame must not re-embed multi-KB instruction text"
        );
        assert!(
            frame.len() < 1024,
            "frame should stay a short pointer, got {} bytes",
            frame.len()
        );
    }

    #[test]
    fn run_instruction_section_empty_for_none_and_blank() {
        assert_eq!(run_instruction_section(None), "");
        assert_eq!(run_instruction_section(Some("")), "");
        assert_eq!(run_instruction_section(Some("   ")), "");
        assert_eq!(run_instruction_section(Some("\n\t \n")), "");
    }

    #[test]
    fn run_instruction_section_is_a_trailing_section_with_trimmed_body() {
        assert_eq!(
            run_instruction_section(Some("  drop the docs/reviews artifacts  ")),
            "\n\n## run instruction\n\ndrop the docs/reviews artifacts\n"
        );
    }

    #[test]
    fn with_run_instruction_no_op_without_instruction() {
        let prompt = "exact template body";
        assert_eq!(with_run_instruction(prompt, None), prompt);
        assert_eq!(with_run_instruction(prompt, Some("   ")), prompt);
    }

    // The core of issue #56 finding 4: a node template that references NEITHER
    // `{{run.context}}` NOR `{{run.instruction}}` must still receive the run
    // instruction body. Before the fix, `execute_node`/`execute_finish_answer`
    // only appended the supervisor notes and the precedence frame, so such a
    // template dropped the instruction entirely while the frame still claimed a
    // `## run instruction` section existed. `assemble_agent_prompt` is the exact
    // shared assembly both call sites now delegate to.
    #[test]
    fn assemble_agent_prompt_delivers_instruction_without_placeholder() {
        let template = "Follow your SOUL and lock the branch scope.";
        let instruction = "Exclude the docs/reviews artifacts from the branch and the PR.";
        let assembled = assemble_agent_prompt(template, Some(instruction), &[]);

        assert!(assembled.starts_with(template), "template stays first");
        assert!(
            assembled.contains("## run instruction"),
            "instruction delivered as its own section, got:\n{assembled}"
        );
        assert!(
            assembled.contains(instruction),
            "run instruction body must be present without any placeholder, got:\n{assembled}"
        );
        // The precedence frame's claim must match the assembled prompt: it
        // points at a `## run instruction` section, which now genuinely exists.
        assert!(
            assembled.contains("## instruction precedence"),
            "precedence frame present, got:\n{assembled}"
        );
        let frame_ref_section = assembled.contains("appears in a `## run instruction` section");
        assert!(
            frame_ref_section && assembled.contains("## run instruction"),
            "frame claim (`appears in a ## run instruction section`) must match reality, got:\n{assembled}"
        );
    }

    #[test]
    fn assemble_agent_prompt_orders_instruction_then_notes_then_frame() {
        let template = "TEMPLATE BODY";
        let events = vec![ev(
            0,
            EventPayload::SupervisorAction {
                action: "context_append".into(),
                node: None,
                detail: "prefer approach B".into(),
            },
        )];
        let assembled = assemble_agent_prompt(template, Some("stay within budget"), &events);

        let tmpl = assembled.find("TEMPLATE BODY").expect("template");
        let instr = assembled.find("## run instruction").expect("instruction");
        let notes = assembled
            .find("Supervisor notes (these override")
            .expect("notes");
        let frame = assembled.find("## instruction precedence").expect("frame");
        assert!(
            tmpl < instr && instr < notes && notes < frame,
            "order must be template, run instruction, notes, frame, got:\n{assembled}"
        );
        assert!(assembled.contains("stay within budget"));
        assert!(assembled.contains("- prefer approach B"));
    }

    #[test]
    fn assemble_agent_prompt_byte_stable_without_instruction_or_notes() {
        let template = "exact template body, no placeholders";
        // No instruction, no notes: assembly must be a pure no-op so cache keys
        // and byte-for-byte prompt expectations do not shift.
        assert_eq!(assemble_agent_prompt(template, None, &[]), template);
        let events = vec![ev(
            0,
            EventPayload::NodeFinished {
                node: "a".into(),
                status: "succeeded".into(),
                attempt: 1,
                output: "ok".into(),
                artifacts: Vec::new(),
            },
        )];
        assert_eq!(assemble_agent_prompt(template, None, &events), template);
        assert_eq!(
            assemble_agent_prompt(template, Some("  "), &events),
            template
        );
    }
}
