//! The tolerant reader of the diagnostics sidecar, for read-only surfaces.
//! It never creates, repairs or backfills the file.

use std::path::Path;

use serde_json::Value;

use super::mismatch::DefinitionMismatch;
use super::route::RouteEvaluated;

/// One diagnostic record of a kind this apb knows.
#[derive(Debug, Clone, PartialEq)]
pub enum Diagnostic {
    Route(RouteEvaluated),
    Mismatch(DefinitionMismatch),
}

/// Every record of the sidecar at `run_dir` this apb can read, in file
/// order. A missing file, a line that does not parse, a torn last line and
/// a record of an unknown kind or format are skipped.
pub fn read(run_dir: &Path) -> Vec<Diagnostic> {
    super::index::read_raw(run_dir)
        .into_iter()
        .filter(|v| v.get("v").and_then(Value::as_u64) == Some(u64::from(super::VERSION)))
        .filter_map(|v| match v.get("kind").and_then(Value::as_str)? {
            super::route::KIND => serde_json::from_value(v).ok().map(Diagnostic::Route),
            super::mismatch::KIND => serde_json::from_value(v).ok().map(Diagnostic::Mismatch),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE: &str = r#"{"v":1,"kind":"route_evaluated","after_seq":4,"key":"a|1","node":"a","execution":1,"mode":"dead_end","checks":[],"chosen":[]}"#;
    const MISMATCH: &str = r#"{"v":1,"kind":"definition_mismatch","after_seq":7,"key":"2|abc","boundary_seq":2,"found":"abc","reason":"digest_differs"}"#;

    #[test]
    fn a_missing_file_is_no_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).is_empty());
        assert!(!dir.path().join(super::super::FILE).exists());
    }

    #[test]
    fn unknown_kinds_other_formats_bad_lines_and_a_torn_last_line_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let text = [
            ROUTE,
            r#"{"v":1,"kind":"from_the_future","after_seq":5,"key":"x"}"#,
            &ROUTE.replace(r#""v":1"#, r#""v":2"#),
            r#"{"v":1,"kind":"route_evaluated","after_seq":"five"}"#,
            "not json at all",
            "[1, 2]",
            MISMATCH,
            &ROUTE[..ROUTE.len() / 2],
        ]
        .join("\n");
        std::fs::write(dir.path().join(super::super::FILE), &text).unwrap();
        let found = read(dir.path());
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(matches!(&found[0], Diagnostic::Route(r) if r.node == "a" && r.after_seq == 4));
        assert!(matches!(&found[1], Diagnostic::Mismatch(m) if m.boundary_seq == 2));
        // Reading changed nothing.
        assert_eq!(
            std::fs::read_to_string(dir.path().join(super::super::FILE)).unwrap(),
            text
        );
    }

    #[test]
    fn past_the_cap_only_the_first_part_is_read_and_its_torn_line_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut text = format!("{ROUTE}\n");
        let filler = r#"{"v":1,"kind":"filler","after_seq":0,"key":"f"}"#;
        while text.len() < super::super::MAX_BYTES as usize + 1024 {
            text.push_str(filler);
            text.push('\n');
        }
        // A record that starts before the cap and ends after it, and one
        // wholly past it.
        let cut = super::super::MAX_BYTES as usize - 10;
        text.insert_str(cut, MISMATCH);
        text.push_str(MISMATCH);
        text.push('\n');
        std::fs::write(dir.path().join(super::super::FILE), &text).unwrap();
        let found = read(dir.path());
        assert_eq!(found.len(), 1, "only the route record before the cap");
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_sidecar_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere.jsonl");
        std::fs::write(&target, format!("{ROUTE}\n")).unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir(&run).unwrap();
        std::os::unix::fs::symlink(&target, run.join(super::super::FILE)).unwrap();
        assert!(read(&run).is_empty());
    }
}
