//! `apb runs <id> --items`: one line per item of the run's lists, optionally
//! of one node and in one state. Item ids, notes and reasons come from the
//! journal (an agent or a check wrote them), so every field goes through the
//! terminal sanitizer.

use std::path::Path;
use std::process::ExitCode;

use apb_core::list::verdict::ItemState;
use apb_engine::list::{RunList, RunListItem};

use crate::util::{QUESTION_TEXT_MAX, sanitize_for_terminal};

/// Which items to print.
pub(crate) struct ItemFilter<'a> {
    pub node: Option<&'a str>,
    pub state: Option<&'a str>,
}

pub(crate) fn items_cmd(root: &Path, run_id: &str, filter: &ItemFilter<'_>) -> ExitCode {
    if !apb_core::registry::is_safe_segment(run_id) {
        eprintln!("runs: invalid run id `{run_id}`");
        return ExitCode::from(2);
    }
    if let Some(state) = filter.state.filter(|s| ItemState::parse(s).is_none()) {
        let known: Vec<&str> = ItemState::ALL.iter().map(|s| s.as_str()).collect();
        eprintln!(
            "runs: unknown item state `{}` (one of {})",
            sanitize_for_terminal(state, QUESTION_TEXT_MAX),
            known.join(", ")
        );
        return ExitCode::from(2);
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        eprintln!("runs: run `{run_id}` not found");
        return ExitCode::from(2);
    }
    let view = match apb_engine::run_view::RunView::load(&run_dir, run_id) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("runs failed: {e}");
            return ExitCode::from(2);
        }
    };
    let playbook = apb_engine::legacy_snapshot::load_run_playbook(&run_dir);
    let lists = apb_engine::list::run_lists(playbook.as_ref(), &view.events);
    if let Some(node) = filter.node
        && !lists.iter().any(|l| l.node == node)
    {
        eprintln!(
            "runs: no list node `{}` in run `{run_id}`",
            sanitize_for_terminal(node, QUESTION_TEXT_MAX)
        );
        return ExitCode::from(2);
    }
    if lists.is_empty() {
        println!("run `{run_id}` has no list");
        return ExitCode::SUCCESS;
    }
    for line in item_lines(&lists, filter) {
        println!("{line}");
    }
    ExitCode::SUCCESS
}

/// The printed lines: a header per list with its counts, then one line per
/// item that passes the filter.
fn item_lines(lists: &[RunList], filter: &ItemFilter<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for l in lists
        .iter()
        .filter(|l| filter.node.is_none_or(|n| n == l.node))
    {
        out.push(format!(
            "{}: {} of {} processed, {} passed, {} need a look",
            sanitize_for_terminal(&l.node, QUESTION_TEXT_MAX),
            l.processed,
            l.total,
            l.passed,
            l.needs_look
        ));
        out.extend(
            l.items
                .iter()
                .filter(|i| filter.state.is_none_or(|s| s == i.state))
                .map(|i| format!("  {}", item_line(i))),
        );
        // An item not admitted yet has no row; `pending` counts it.
        let listed = l
            .items
            .iter()
            .filter(|i| i.state == ItemState::Pending.as_str())
            .count();
        let unlisted = (l.pending as usize).saturating_sub(listed);
        if unlisted > 0
            && filter
                .state
                .is_none_or(|s| s == ItemState::Pending.as_str())
        {
            out.push(format!("  ({unlisted} pending not started yet)"));
        }
    }
    out
}

/// `id<TAB>state<TAB>round n<TAB>what it says`.
fn item_line(i: &RunListItem) -> String {
    let words: Vec<&str> = [i.verdict.as_deref(), i.reason.as_deref(), i.note.as_deref()]
        .into_iter()
        .flatten()
        .filter(|w| !w.is_empty())
        .collect();
    let mut line = format!(
        "{}\t{}\tround {}",
        sanitize_for_terminal(&i.id, QUESTION_TEXT_MAX),
        i.state,
        i.round
    );
    if !words.is_empty() {
        line.push('\t');
        line.push_str(&sanitize_for_terminal(&words.join(": "), QUESTION_TEXT_MAX));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, state: &str, note: Option<&str>) -> RunListItem {
        RunListItem {
            id: id.into(),
            state: state.into(),
            round: 1,
            verdict: None,
            note: note.map(str::to_string),
            reason: (state == "needs_look").then(|| "rejected".into()),
            attempts: 2,
            check_rejected: false,
        }
    }

    #[test]
    fn a_line_per_item_in_the_state_asked_with_control_characters_made_safe() {
        let list = RunList {
            node: "rows".into(),
            title: None,
            total: 2,
            processed: 2,
            passed: 1,
            passed_with_note: 0,
            needs_look: 1,
            pending: 0,
            running: 0,
            batch: 1,
            batches: 1,
            items: vec![
                item("a", "passed", None),
                item("b\x1b[2J", "needs_look", Some("bad\nline")),
            ],
        };
        let filter = ItemFilter {
            node: Some("rows"),
            state: Some("needs_look"),
        };
        let lines = item_lines(&[list], &filter);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "rows: 2 of 2 processed, 1 passed, 1 need a look");
        assert!(
            lines[1].starts_with("  b"),
            "the passed item is filtered out"
        );
        assert!(!lines[1].contains('\x1b') && !lines[1].contains('\n'));
        assert!(lines[1].contains("needs_look\tround 1\trejected: bad"));
    }

    /// The node id comes from the journal: a header made of it is made safe
    /// like an item line.
    #[test]
    fn a_list_header_makes_its_node_id_safe() {
        let list = RunList {
            node: "rows\x1b]0;x\x07".into(),
            title: None,
            total: 0,
            processed: 0,
            passed: 0,
            passed_with_note: 0,
            needs_look: 0,
            pending: 0,
            running: 0,
            batch: 0,
            batches: 0,
            items: Vec::new(),
        };
        let filter = ItemFilter {
            node: None,
            state: None,
        };
        let lines = item_lines(&[list], &filter);
        assert!(lines[0].starts_with("rows"));
        assert!(!lines[0].contains('\x1b') && !lines[0].contains('\x07'));
    }

    /// Items not admitted yet have no row: the pending ones are counted in
    /// one line instead, so `--state pending` accounts for all of them.
    #[test]
    fn pending_items_not_started_yet_are_counted() {
        let list = RunList {
            node: "rows".into(),
            title: None,
            total: 4,
            processed: 1,
            passed: 1,
            passed_with_note: 0,
            needs_look: 0,
            pending: 3,
            running: 0,
            batch: 1,
            batches: 2,
            items: vec![item("a", "passed", None), item("b", "pending", None)],
        };
        let pending = ItemFilter {
            node: None,
            state: Some("pending"),
        };
        let lines = item_lines(std::slice::from_ref(&list), &pending);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2], "  (2 pending not started yet)");
        let passed = ItemFilter {
            node: None,
            state: Some("passed"),
        };
        assert_eq!(item_lines(&[list], &passed).len(), 2);
    }
}
