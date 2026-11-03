//! Record packages (`format: records`) and the `@record` reference form.

use apb_core::records::{self, MAX_RECORDS};
use apb_core::result_store::{Citation, EntryId};

const HEADER: &str = r#"{"apb_records": 1, "complete": true, "source": "inbox"}"#;

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
fn a_package_parses_with_its_header_and_records_in_order() {
    let text = package(&[
        r#"{"id": "m1", "kind": "mail", "text": "hello", "at": "2026-11-01"}"#,
        "",
        r#"{"id": "m:2", "kind": "mail", "text": "again", "source": "imap", "version": "3"}"#,
    ]);
    let p = records::parse(&text).unwrap();
    assert!(p.header.complete);
    assert_eq!(p.header.source, "inbox");
    let ids: Vec<&str> = p.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["m1", "m:2"]);
    assert_eq!(p.records[1].source.as_deref(), Some("imap"));
}

/// Each broken rule names its first problem by line and rule, never the
/// record's text.
#[test]
fn a_text_that_breaks_a_rule_names_the_first_problem() {
    let ok = r#"{"id": "a", "kind": "k", "text": "secret-ish words"}"#;
    let cases: Vec<(String, &str)> = vec![
        (String::new(), "empty"),
        ("not json\n".into(), "line 1 is not the header"),
        (
            r#"{"apb_records": 2, "complete": true, "source": "s"}"#.into(),
            "format 2",
        ),
        (package(&[ok, ok]), "line 3: the id repeats"),
        (
            package(&[r#"{"id": "a b", "kind": "k", "text": "t"}"#]),
            "line 2: the id is not",
        ),
        (
            package(&[r#"{"id": "a", "kind": "Mail", "text": "t"}"#]),
            "line 2: the kind is not a slug",
        ),
        (
            package(&[r#"{"id": "a", "kind": "k", "text": "t", "extra": 1}"#]),
            "line 2 is not a record",
        ),
        (
            package(&[r#"{"id": "a", "kind": "k"}"#]),
            "line 2 is not a record",
        ),
    ];
    for (text, want) in cases {
        let err = records::parse(&text).unwrap_err();
        assert!(err.contains(want), "{text:?}: {err}");
        assert!(!err.contains("secret-ish"), "{err}");
    }
}

#[test]
fn a_package_holds_at_most_the_record_cap() {
    let line = |i: usize| format!(r#"{{"id": "r{i}", "kind": "k", "text": "t"}}"#);
    let full: Vec<String> = (0..MAX_RECORDS).map(line).collect();
    let refs: Vec<&str> = full.iter().map(String::as_str).collect();
    assert_eq!(
        records::parse(&package(&refs)).unwrap().records.len(),
        MAX_RECORDS
    );
    let mut over = full.clone();
    over.push(line(MAX_RECORDS));
    let refs: Vec<&str> = over.iter().map(String::as_str).collect();
    assert!(
        records::parse(&package(&refs))
            .unwrap_err()
            .contains("more than 10000 records")
    );
}

#[test]
fn ids_and_kinds_follow_their_grammar() {
    assert!(records::is_record_id("A-z.0_9:x"));
    assert!(!records::is_record_id(""));
    assert!(!records::is_record_id(&"a".repeat(129)));
    assert!(!records::is_record_id("a/b"));
    assert!(records::is_kind("mail_thread-2"));
    assert!(!records::is_kind("-mail"));
    assert!(!records::is_kind("Mail"));
    assert!(!records::is_kind(""));
}

/// `r:<snapshot>/<entry>@<record id, percent-encoded>` names one record of
/// a package, with or without a byte range, and prints back the same.
#[test]
fn a_record_reference_round_trips() {
    let sid = "a".repeat(64);
    for text in [
        format!("r:{sid}/file:run:mail.jsonl@m%3A1"),
        format!("r:{sid}/file:run:mail.jsonl@m%3A1#[0,4)"),
        format!("r:{sid}/output"),
    ] {
        let c: Citation = text.parse().unwrap();
        assert_eq!(c.to_string(), text);
    }
    let c: Citation = format!("r:{sid}/file:run:mail.jsonl@m%3A1#[2,5)")
        .parse()
        .unwrap();
    assert_eq!(c.record.as_deref(), Some("m:1"));
    assert_eq!(c.range, Some((2, 5)));
    assert_eq!(
        c.entry,
        EntryId::File {
            scope: apb_core::result_store::ResultScope::Run,
            path: "mail.jsonl".into()
        }
    );
    // A record part that does not decode to a record id is refused.
    assert!(format!("r:{sid}/output@a%2Fb").parse::<Citation>().is_err());
    assert!(format!("r:{sid}/output@").parse::<Citation>().is_err());
}
