//! Record packages (`format: records`) and the `@record` form of a result
//! reference.

use apb_core::records::{MAX_RECORDS, is_kind, is_record_id, parse};
use apb_core::result_store::{Citation, CitationError, EntryId, ResultScope};

const HEADER: &str = r#"{"apb_records": 1, "complete": true, "source": "desk inbox"}"#;

fn package(lines: &[&str]) -> String {
    let mut text = String::from(HEADER);
    for l in lines {
        text.push('\n');
        text.push_str(l);
    }
    text.push('\n');
    text
}

#[test]
fn a_package_parses_its_header_and_records() {
    let p = parse(&package(&[
        r#"{"id": "m-1", "kind": "email", "text": "the invoice is late"}"#,
        r#"{"id": "m.2:b_c", "kind": "email", "text": "paid", "source": "imap", "version": "3", "at": "2026-11-03T10:00:00Z"}"#,
    ]))
    .unwrap();
    assert!(p.header.complete);
    assert_eq!(p.header.source, "desk inbox");
    assert_eq!(p.records.len(), 2);
    assert_eq!(p.records[1].version.as_deref(), Some("3"));
    // A header alone is an empty package.
    assert!(parse(HEADER).unwrap().records.is_empty());
}

#[test]
fn the_first_problem_names_its_line_and_never_the_text() {
    let cases = [
        (String::from("secret words\n"), "line 1: header"),
        (
            r#"{"apb_records": 2, "complete": true, "source": "x"}"#.to_string(),
            "line 1: apb_records is 2",
        ),
        (
            package(&[r#"{"id": "bad id", "kind": "email", "text": "secret words"}"#]),
            "line 2: the id",
        ),
        (
            package(&[r#"{"id": "a", "kind": "Email", "text": "secret words"}"#]),
            "line 2: the kind",
        ),
        (
            package(&[
                r#"{"id": "a", "kind": "email", "text": "secret words"}"#,
                r#"{"id": "a", "kind": "email", "text": "secret words"}"#,
            ]),
            "line 3: the id repeats",
        ),
        (
            package(&[r#"{"id": "a", "kind": "email", "text": "secret words", "extra": 1}"#]),
            "line 2: record",
        ),
        (package(&["", "x"]), "line 2: record"),
    ];
    for (text, want) in cases {
        let problem = parse(&text).unwrap_err();
        assert!(problem.starts_with(want), "{problem:?} for {text:?}");
        assert!(!problem.contains("secret"), "{problem}");
    }
}

#[test]
fn a_package_holds_at_most_ten_thousand_records() {
    let record = |i: usize| format!(r#"{{"id": "r{i}", "kind": "row", "text": "t"}}"#);
    let lines: Vec<String> = (0..MAX_RECORDS).map(record).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert_eq!(parse(&package(&refs)).unwrap().records.len(), MAX_RECORDS);
    let mut more = lines.clone();
    more.push(record(MAX_RECORDS));
    let refs: Vec<&str> = more.iter().map(String::as_str).collect();
    assert!(
        parse(&package(&refs))
            .unwrap_err()
            .contains("more than 10000 records")
    );
}

#[test]
fn ids_and_kinds_follow_their_grammars() {
    assert!(is_record_id("A-z.0_9:x"));
    assert!(is_record_id(&"a".repeat(128)));
    assert!(!is_record_id(&"a".repeat(129)));
    assert!(!is_record_id(""));
    assert!(!is_record_id("a/b"));
    assert!(is_kind("email"));
    assert!(is_kind("support-ticket_2"));
    assert!(!is_kind("-email"));
    assert!(!is_kind("Email"));
    assert!(!is_kind(""));
}

#[test]
fn a_record_reference_names_its_record_percent_encoded() {
    let snap = "a".repeat(64);
    let text = format!("r:{snap}/file:workspace:mail.jsonl@m%3A1#[3,9)");
    let c: Citation = text.parse().unwrap();
    assert_eq!(
        c.entry,
        EntryId::File {
            scope: ResultScope::Workspace,
            path: "mail.jsonl".into()
        }
    );
    assert_eq!(c.record.as_deref(), Some("m:1"));
    assert_eq!(c.range, Some((3, 9)));
    assert_eq!(c.to_string(), text);
    // A path's own `@` stays encoded, so the record is the last one.
    let at = format!("r:{snap}/file:run:a%40b.jsonl@x");
    let c: Citation = at.parse().unwrap();
    assert_eq!(c.record.as_deref(), Some("x"));
    assert_eq!(c.to_string(), at);
    // An entry reference without a record is unchanged.
    let plain: Citation = format!("r:{snap}/output").parse().unwrap();
    assert_eq!(plain.record, None);
    assert_eq!(
        format!("r:{snap}/output@a%20b").parse::<Citation>(),
        Err(CitationError::Record)
    );
    assert_eq!(
        format!("r:{snap}/output@").parse::<Citation>(),
        Err(CitationError::Record)
    );
}
