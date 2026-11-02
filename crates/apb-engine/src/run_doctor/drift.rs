//! Journal format drift of one run: which apb binaries drove it, a drive by
//! an older one, records this binary does not know, lines it cannot decode,
//! a torn last line, and the open deferred retries with their guard.
//!
//! Built on [`crate::event::survey`], which classifies every line and never
//! fails, so a journal no engine reader accepts is still described line by
//! line. Reads only.

use std::collections::BTreeMap;
use std::path::Path;

use super::check::{FAIL, OK, RunCheck, WARN};
use crate::event::{JournalSurvey, survey};
use crate::run_binaries::{RunBinaries, label};
use crate::state::RunState;

/// The seqs listed per unknown type before the rest is counted.
const SEQS_SHOWN: usize = 8;

/// The unknown record types listed before the rest is counted.
const KINDS_SHOWN: usize = 8;

/// The undecodable lines listed one per row before the rest is counted.
const BAD_LINES_SHOWN: usize = 8;

/// The longest journal text (a record type, a decode error) a row shows.
const TEXT_MAX: usize = 120;

/// Text read from the journal as a row shows it: control characters
/// escaped (no terminal sequence gets through) and at most [`TEXT_MAX`]
/// characters. The journal is a plain file an agent step without isolation
/// can write.
pub(super) fn shown(text: &str) -> String {
    let mut out = String::new();
    for (i, c) in text.chars().enumerate() {
        if i == TEXT_MAX {
            out.push_str(" (cut)");
            break;
        }
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// The journal of `run_dir` as the survey reads it (an absent journal reads
/// empty); an error when it is there but cannot be read, so no row reports
/// a journal nobody read as fine.
pub(super) fn read(run_dir: &Path) -> std::io::Result<JournalSurvey> {
    match std::fs::read(run_dir.join("events.jsonl")) {
        Ok(bytes) => Ok(survey(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(JournalSurvey::default()),
        Err(e) => Err(e),
    }
}

/// Every drift row of `journal`, in a fixed order: binaries, downgrades,
/// format, deferred retries.
pub(super) fn checks(journal: &JournalSurvey) -> Vec<RunCheck> {
    let mut out = binaries_checks(&RunBinaries::from_events(&journal.events));
    out.extend(format_checks(journal));
    out.extend(deferral_checks(journal));
    out
}

fn binaries_checks(binaries: &RunBinaries) -> Vec<RunCheck> {
    let mut out = vec![match binaries.line() {
        Some(line) => RunCheck::new(OK, "binaries", line),
        None => RunCheck::new(
            OK,
            "binaries",
            "no drive recorded its apb version (an older apb drove the run)",
        ),
    }];
    out.extend(binaries.downgrades().into_iter().map(|d| {
        RunCheck::new(
            WARN,
            "downgrade",
            format!(
                "the resume at seq {} ran on {}, older than apb {} that drove the run before it; \
                 it skipped what it did not know",
                d.seq,
                label(d.to.as_deref()),
                d.from
            ),
        )
    }));
    out
}

fn format_checks(journal: &JournalSurvey) -> Vec<RunCheck> {
    let mut out = Vec::new();
    if !journal.unknown.is_empty() {
        let mut by_kind: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
        for u in &journal.unknown {
            by_kind.entry(u.kind.as_str()).or_default().push(u.seq);
        }
        let total_kinds = by_kind.len();
        let mut kinds: Vec<String> = by_kind
            .into_iter()
            .take(KINDS_SHOWN)
            .map(|(kind, seqs)| {
                let mut shown: Vec<String> =
                    seqs.iter().take(SEQS_SHOWN).map(u64::to_string).collect();
                if seqs.len() > SEQS_SHOWN {
                    shown.push(format!("{} more", seqs.len() - SEQS_SHOWN));
                }
                format!(
                    "`{}` x{} (seq {})",
                    self::shown(kind),
                    seqs.len(),
                    shown.join(", ")
                )
            })
            .collect();
        if total_kinds > KINDS_SHOWN {
            kinds.push(format!("{} more type(s)", total_kinds - KINDS_SHOWN));
        }
        out.push(RunCheck::new(
            WARN,
            "unknown records",
            format!(
                "{} record(s) of a type this apb {} does not know, written by a newer apb: {}",
                journal.unknown.len(),
                env!("CARGO_PKG_VERSION"),
                kinds.join("; ")
            ),
        ));
    }
    for bad in journal.undecodable.iter().take(BAD_LINES_SHOWN) {
        let what = match (&bad.kind, bad.seq) {
            (Some(kind), Some(seq)) => format!("seq {seq}, `{}`", shown(kind)),
            _ => "not an event".to_string(),
        };
        out.push(RunCheck::new(
            FAIL,
            "journal line",
            format!(
                "line {} ({what}) does not decode: {}; no apb continues the run past it",
                bad.line,
                shown(&bad.error)
            ),
        ));
    }
    if journal.undecodable.len() > BAD_LINES_SHOWN {
        out.push(RunCheck::new(
            FAIL,
            "journal line",
            format!(
                "{} more line(s) do not decode",
                journal.undecodable.len() - BAD_LINES_SHOWN
            ),
        ));
    }
    if let Some(torn) = &journal.torn_tail {
        out.push(RunCheck::new(
            FAIL,
            "torn tail",
            format!(
                "line {} is a write that did not finish; read-only views skip it, and no apb \
                 resumes the run while it is there",
                torn.line
            ),
        ));
    }
    if out.is_empty() {
        out.push(RunCheck::new(
            OK,
            "journal format",
            "every line decodes and every record type is known",
        ));
    }
    out
}

fn deferral_checks(journal: &JournalSurvey) -> Vec<RunCheck> {
    let guard = journal.deferrals();
    if guard.open.is_empty() {
        return Vec::new();
    }
    let state = RunState::fold(&journal.events);
    guard
        .open
        .iter()
        .map(|node| {
            let what = match state.deferred.get(node) {
                Some(d) => format!(
                    "retry {} of `{}` deferred at seq {} for {} s",
                    d.retry,
                    shown(node),
                    d.seq,
                    d.delay_ms.div_ceil(1000)
                ),
                None => format!("a retry of `{}` is deferred", shown(node)),
            };
            match guard.unguarded_checkpoint {
                None => RunCheck::new(
                    OK,
                    "deferred retry",
                    format!("{what}; guarded: an apb that does not know deferrals refuses to continue the run"),
                ),
                Some(seq) => RunCheck::new(
                    WARN,
                    "deferred retry",
                    format!(
                        "{what}; the checkpoint at seq {seq} has no guard after it: an apb that \
                         does not know deferrals would re-run `{}` at once",
                        shown(node)
                    ),
                ),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str) -> Vec<RunCheck> {
        checks(&survey(text.as_bytes()))
    }

    /// Journal text never reaches a terminal as written: a version, a
    /// record type and a decode error lose their control characters and
    /// are cut to length, and a journal of junk lines prints a bounded
    /// number of rows.
    #[test]
    fn hostile_journal_text_is_escaped_clipped_and_counted() {
        let esc = "\u{1b}]52;c;Zm9v\u{7}\u{1b}[2J";
        let long = "x".repeat(5000);
        let mut text = serde_json::json!({
            "seq": 0, "ts": 1, "type": "run_provenance", "apb": format!("1.0.0{esc}{long}")
        })
        .to_string();
        text.push('\n');
        text.push_str(
            &serde_json::json!({"seq": 1, "ts": 1, "type": "node_finished", "node": format!("{esc}{long}"), "status": 7})
                .to_string(),
        );
        text.push('\n');
        text.push_str(
            &serde_json::json!({"seq": 2, "ts": 1, "type": "retry_deferred", "node": format!("{esc}{long}"),
                "after_attempt": 0, "retry": 1, "step": 0, "binding": "b", "delay_ms": 1000})
            .to_string(),
        );
        text.push('\n');
        for i in 0..300 {
            text.push_str(&format!(
                "{{\"seq\":{},\"ts\":1,\"type\":\"future_kind_{i}\"}}\n",
                3 + i
            ));
        }
        for i in 0..500 {
            text.push_str(&format!("junk {i}\n"));
        }
        let all = rows(&text);
        let printed: String = all.iter().map(|r| format!("{r:?}")).collect();
        assert!(!printed.contains('\u{1b}'), "no escape byte survives");
        assert!(!printed.contains('\u{7}'));
        let binaries = all.iter().find(|r| r.subject == "binaries").unwrap();
        assert!(binaries.detail.len() < 200, "{}", binaries.detail);
        let deferral = all.iter().find(|r| r.subject == "deferred retry").unwrap();
        assert!(deferral.detail.len() < 600, "{}", deferral.detail);
        let unknown = all.iter().find(|r| r.subject == "unknown records").unwrap();
        assert!(unknown.detail.len() < 1200, "{}", unknown.detail);
        assert!(
            unknown.detail.contains("292 more type(s)"),
            "{}",
            unknown.detail
        );
        let bad: Vec<&RunCheck> = all.iter().filter(|r| r.subject == "journal line").collect();
        assert_eq!(bad.len(), BAD_LINES_SHOWN + 1);
        assert!(bad.last().unwrap().detail.starts_with("493 more line(s)"));
        assert!(bad.iter().all(|r| r.detail.len() < 400));
    }
}
