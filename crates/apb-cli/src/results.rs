//! `apb results`: a run's result snapshots. Without a subcommand, every
//! snapshot of the run by node, execution and item (`--json`: the object
//! MCP `run_results` answers); `show` prints one entry's stored text,
//! re-verified on this read and redacted; `clean` removes what no stamp
//! and no open transaction names (`--dry-run` writes nothing). The lines
//! `apb runs <id>` prints about snapshots come from here too.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::result_store::SnapshotOmission;
use apb_engine::result_view::{EntryView, ResultRow, ResultsView, Verified};
use apb_mcp::tools::ToolError;
use apb_mcp::tools::results::{parse_ref, run_result_raw, run_results, run_results_clean};
use clap::Subcommand;

use crate::util::{QUESTION_TEXT_MAX, sanitize_for_terminal};

#[derive(Subcommand)]
pub(crate) enum ResultsAction {
    /// Print one entry's stored text (`r:<snapshot>/<entry>`), checked
    /// against its digest on this read
    Show {
        run_id: String,
        /// The entry, as `apb results <run>` lists it; `#[a,b)` after it
        /// shows only those bytes
        reference: String,
        /// Write the text to this file instead of standard output
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Remove what no stamp of the journal and no open transaction names,
    /// resolving transactions whose writer ended
    Clean {
        run_id: String,
        /// Only report what would be removed; write nothing
        #[arg(long)]
        dry_run: bool,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn results_cmd(
    root: &Path,
    action: Option<ResultsAction>,
    run_id: Option<String>,
    node: Option<String>,
    json: bool,
) -> ExitCode {
    match action {
        Some(ResultsAction::Show {
            run_id,
            reference,
            out,
        }) => show(root, &run_id, &reference, out.as_deref()),
        Some(ResultsAction::Clean {
            run_id,
            dry_run,
            json,
        }) => clean(root, &run_id, dry_run, json),
        None => match run_id {
            Some(run_id) => list(root, &run_id, node.as_deref(), json),
            None => {
                eprintln!("results: name a run: apb results <run>");
                ExitCode::from(2)
            }
        },
    }
}

fn fail(what: &str, e: &ToolError) -> ExitCode {
    eprintln!(
        "{what}: {}",
        sanitize_for_terminal(&e.to_string(), QUESTION_TEXT_MAX * 2)
    );
    match e {
        ToolError::NotFound(_) => ExitCode::from(2),
        _ => ExitCode::from(1),
    }
}

fn list(root: &Path, run_id: &str, node: Option<&str>, json: bool) -> ExitCode {
    if json {
        return match run_results(root, run_id, node) {
            Ok(v) => {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                ExitCode::SUCCESS
            }
            Err(e) => fail("results", &e),
        };
    }
    // The same lookup and refusal as the JSON answer.
    if let Err(e) = run_results(root, run_id, node) {
        return fail("results", &e);
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    let mut view = match ResultsView::load(&run_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("results: {e}");
            return ExitCode::from(1);
        }
    };
    if let Some(node) = node {
        view.rows.retain(|r| r.execution.node == node);
        view.legacy.retain(|r| r.execution.node == node);
        view.not_stored.retain(|r| r.node == node);
    }
    let redactor = apb_engine::run_redact::WorkspaceRedaction::new(root).run_dir(&run_dir);
    crate::util::with_run_redaction(redactor, || list_text(run_id, &view))
}

fn list_text(run_id: &str, view: &ResultsView) -> ExitCode {
    if view.rows.is_empty() && view.legacy.is_empty() && view.not_stored.is_empty() {
        say!("{run_id}: no result snapshots");
        return ExitCode::SUCCESS;
    }
    for row in &view.rows {
        say!("{}", row_line(row));
        for e in &row.entries {
            // An entry id percent-encodes every byte of a path that is
            // not unreserved, so the line holds no control character.
            say!("    {}", entry_line(&row.snapshot_id, e));
        }
        let omitted = omitted_count(row);
        if omitted > 0 {
            say!("    omitted: {omitted}");
        }
    }
    for f in &view.not_stored {
        let item = f
            .item
            .as_deref()
            .map(|i| format!(" item {}", sanitize_for_terminal(i, QUESTION_TEXT_MAX)))
            .unwrap_or_default();
        say!(
            "{} #{}{item}: {}",
            f.node,
            f.execution,
            sanitize_for_terminal(&f.reason, 320)
        );
    }
    if !view.legacy.is_empty() {
        say!("recorded digests (no stored bytes):");
        for l in &view.legacy {
            say!(
                "  {} #{}  {}:{}  {}",
                l.execution.node,
                l.execution.ordinal,
                l.scope,
                sanitize_for_terminal(&l.path, QUESTION_TEXT_MAX),
                short_digest(&l.digest)
            );
        }
    }
    ExitCode::SUCCESS
}

fn short_digest(d: &str) -> String {
    let hex = d.strip_prefix("sha256:").unwrap_or(d);
    format!("sha256:{}", clip12(hex))
}

/// The first 12 characters of a journal or manifest value, made safe for a
/// terminal: a value this binary did not write may hold anything, and
/// slicing it by bytes could cut a character.
fn clip12(s: &str) -> String {
    sanitize_for_terminal(&s.chars().take(12).collect::<String>(), QUESTION_TEXT_MAX)
}

fn item_part(r: &ResultRow) -> String {
    r.item
        .as_ref()
        .map(|i| format!(" item {}", sanitize_for_terminal(&i.id, QUESTION_TEXT_MAX)))
        .unwrap_or_default()
}

/// The words after a row: what is not plain about it.
fn flags(r: &ResultRow) -> String {
    let mut flags = Vec::new();
    match r.verified {
        Verified::Ok => {}
        Verified::Missing => flags.push("missing".to_string()),
        Verified::Corrupt => flags.push("corrupt".to_string()),
    }
    if r.entries.iter().any(|e| e.redacted || e.redacted_on_read) {
        flags.push("redacted".into());
    }
    if let Some(why) = r.incomplete {
        flags.push(format!("incomplete: {}", why.as_str()));
    }
    if let Some(why) = r.output_omitted {
        flags.push(format!("output not stored: {}", why.as_str()));
    }
    let omitted = omitted_count(r);
    if omitted > 0 {
        flags.push(format!("omitted {omitted}"));
    }
    if r.provenance.cache_hit {
        flags.push("cache hit".into());
    }
    if let Some(head) = &r.provenance.head {
        flags.push(format!("at {}", clip12(head)));
    }
    if flags.is_empty() {
        String::new()
    } else {
        format!(" ({})", flags.join(", "))
    }
}

fn row_line(r: &ResultRow) -> String {
    format!(
        "{} #{}{}  {}  {}{}",
        r.execution.node,
        r.execution.ordinal,
        item_part(r),
        sanitize_for_terminal(&r.status, QUESTION_TEXT_MAX),
        sanitize_for_terminal(&r.snapshot_id, QUESTION_TEXT_MAX),
        flags(r)
    )
}

fn entry_line(snapshot_id: &str, e: &EntryView) -> String {
    let redacted = if e.redacted || e.redacted_on_read {
        ", redacted"
    } else {
        ""
    };
    let snapshot_id = sanitize_for_terminal(snapshot_id, QUESTION_TEXT_MAX);
    let stored = match (e.size, &e.digest) {
        (Some(size), Some(digest)) => format!("{size} bytes  {}", short_digest(digest)),
        _ => "unverified".to_string(),
    };
    let entry = sanitize_for_terminal(&e.entry, QUESTION_TEXT_MAX);
    format!("r:{snapshot_id}/{entry}  {stored}{redacted}")
}

fn omitted_count(r: &ResultRow) -> u64 {
    r.omitted
        .iter()
        .map(|o| match o {
            SnapshotOmission::Item(_) => 1,
            SnapshotOmission::More(m) => m.more,
        })
        .sum()
}

/// The text of one entry, exactly as stored and redacted: to `out`, or to
/// standard output (with control characters other than line breaks shown
/// as `?` on a terminal).
fn show(root: &Path, run_id: &str, reference: &str, out: Option<&Path>) -> ExitCode {
    let c = match parse_ref(reference) {
        Ok(c) => c,
        Err(e) => return fail("results show", &e),
    };
    let content = match run_result_raw(root, run_id, &c.snapshot_id, &c.entry, c.range) {
        Ok(v) => v,
        Err(e) => return fail("results show", &e),
    };
    if content.redacted_on_read {
        eprintln!("results show: secret values known to the run were replaced on this read");
    }
    if let Some(path) = out {
        return match std::fs::write(path, content.text.as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("results show: {}: {e}", path.display());
                ExitCode::from(1)
            }
        };
    }
    let mut stdout = std::io::stdout().lock();
    let text = if stdout.is_terminal() {
        content
            .text
            .split('\n')
            .map(|l| apb_core::text::replace_hidden(l, '?'))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        content.text
    };
    match stdout.write_all(text.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}

fn clean(root: &Path, run_id: &str, dry_run: bool, json: bool) -> ExitCode {
    let r = match run_results_clean(root, run_id, dry_run) {
        Ok(v) => v,
        Err(e) => return fail("results clean", &e),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
        return ExitCode::SUCCESS;
    }
    let n = |k: &str| r[k].as_u64().unwrap_or(0);
    let removed = r["removed_snapshots"].as_array().map_or(0, Vec::len);
    let dead = r["dead"].as_array().map_or(0, Vec::len);
    let verb = if dry_run { "would remove" } else { "removed" };
    println!(
        "{run_id}: {verb} {removed} snapshot(s) and {} object(s), {} bytes; {dead} transaction(s) whose writer ended; {} open; {} snapshot(s) kept",
        n("removed_objects"),
        n("removed_bytes"),
        n("open"),
        n("kept_snapshots"),
    );
    if r["objects_unverified"].as_bool().unwrap_or(false) {
        println!("  a kept snapshot did not verify, so no object was removed");
    }
    ExitCode::SUCCESS
}

/// The lines `apb runs <id>` prints about the run's snapshots: none when
/// the run kept none. Printed inside the run's redaction.
pub(crate) fn run_lines(run_dir: &Path, run_id: &str) -> Vec<String> {
    let Ok(view) = ResultsView::load(run_dir) else {
        return vec![format!("results: unreadable; see apb results {run_id}")];
    };
    if view.rows.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "results: {} snapshot(s); apb results {run_id}",
        view.rows.len()
    )];
    lines.extend(view.rows.iter().map(summary_line));
    lines
}

fn summary_line(r: &ResultRow) -> String {
    let bytes: u64 = r.entries.iter().filter_map(|e| e.size).sum();
    let n = r.entries.len();
    format!(
        "result {} #{}{}: {n} entr{}, {bytes} bytes, {}{}",
        r.execution.node,
        r.execution.ordinal,
        item_part(r),
        if n == 1 { "y" } else { "ies" },
        clip12(&r.snapshot_id),
        flags(r)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_engine::identity::ExecutionRef;
    use apb_engine::result_view::Verified;

    /// A row whose journal and manifest values this binary did not write
    /// (a multi-byte character across byte 12) prints, never panics.
    #[test]
    fn values_with_wide_characters_print_without_a_panic() {
        let mut row = ResultRow {
            execution: ExecutionRef {
                node: "work".into(),
                ordinal: 1,
                ended_seq: None,
            },
            item: None,
            settlement_seq: 2,
            produced_by_attempt: None,
            attempt_number: None,
            status: "succeeded".into(),
            snapshot_id: "abcdefghijk\u{e9}xyz".into(),
            entries: Vec::new(),
            omitted: Vec::new(),
            incomplete: None,
            output_omitted: None,
            provenance: Default::default(),
            verified: Verified::Corrupt,
        };
        row.provenance.head = Some("0123456789a\u{e9}\u{e9}".into());
        assert!(flags(&row).contains("at 0123456789a\u{e9}"));
        assert!(summary_line(&row).contains("abcdefghijk\u{e9}"));
        assert_eq!(
            short_digest("sha256:\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"),
            "sha256:\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"
        );
    }

    /// An entry name comes from a manifest this binary may not have
    /// written: it reaches the terminal without control characters, and
    /// an entry nothing verifies prints no size or digest.
    #[test]
    fn an_entry_line_prints_no_control_characters() {
        let e = EntryView {
            entry: "file:workspace:a\u{1b}[2Jb".into(),
            part: apb_core::result_store::EntryPart::File,
            scope: None,
            path: None,
            size: None,
            digest: None,
            redacted: false,
            redacted_on_read: false,
            format: apb_core::result_store::ResultFormat::Text,
        };
        let line = entry_line("abc", &e);
        assert!(!line.contains('\u{1b}'), "{line:?}");
        assert!(line.ends_with("unverified"), "{line:?}");
    }
}
