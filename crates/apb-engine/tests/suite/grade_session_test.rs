//! A host's grading session: the bounded reader (outline, children, read,
//! page, search; redaction; the budget) and the evidence validator every
//! submission goes through (citations read in full, absences over views
//! read to their end, one correction round). Neither the grade nor the
//! session file holds any text of the run.

use std::fs;
use std::path::{Path, PathBuf};

use apb_core::quality::grade::{Cite, Level};
use apb_engine::grade::evidence::Proposal;
use apb_engine::grade::reader::ReadRequest;
use apb_engine::grade::{SubmitOutcome, read, reopen, start_session, submit};

use crate::quality_common::{
    CLAUDE_SESSION, CRM_JOURNAL, agent_journal, agent_playbook, attempt_file, planted, run_dir,
};

const TONE: &str = "quality:\n  criteria:\n    - id: reply-tone\n      evidence: both\n      reads: [outputs, transcripts]\n      levels: { good: \"every reply is polite\", partial: \"one reply is curt\", poor: \"a reply is rude\" }\n";

/// The code fixture with the planted value in its session, graded on
/// `reply-tone`.
fn code_run(root: &Path) -> PathBuf {
    let dir = run_dir(
        root,
        "r1",
        &agent_journal("claude", &root.join("work"), "", "succeeded"),
        &agent_playbook(TONE),
    );
    attempt_file(
        &dir,
        "fix-1",
        "session.jsonl",
        &CLAUDE_SESSION.replace("PLANTED_VALUE", &planted()),
    );
    attempt_file(&dir, "fix-1", "stdout.log", "ok\n");
    attempt_file(&dir, "fix-1", "stderr.log", "");
    dir
}

fn proposal(level: Level, cites: &[&str], absence: bool) -> Vec<Proposal> {
    vec![Proposal {
        criterion: "reply-tone".into(),
        level,
        reason_code: "checked".into(),
        why: "checked the replies".into(),
        cites: cites.iter().map(|c| Cite::to(*c)).collect(),
        absence,
    }]
}

fn problems(outcome: SubmitOutcome) -> Vec<String> {
    match outcome {
        SubmitOutcome::Rejected { problems } => problems.into_iter().map(|p| p.problem).collect(),
        SubmitOutcome::Graded { .. } => panic!("expected a refusal"),
    }
}

fn graded(outcome: SubmitOutcome) -> apb_core::quality::grade::Grade {
    match outcome {
        SubmitOutcome::Graded { grade } => *grade,
        SubmitOutcome::Rejected { problems } => panic!("refused: {problems:?}"),
    }
}

fn op(dir: &Path, session: &str, req: ReadRequest) -> apb_engine::grade::reader::ReadResponse {
    read(dir, session, &req).unwrap()
}

#[test]
fn a_session_opens_once_and_lists_the_views_without_text() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let start = start_session(&dir, None).unwrap();
    assert_eq!(start.criteria.len(), 1);
    assert_eq!(start.criteria[0].id, "reply-tone");
    assert_eq!(start.criteria[0].reads, ["outputs", "transcripts"]);
    let views: Vec<&str> = start
        .outline
        .items
        .iter()
        .map(|i| i.r#ref.as_str())
        .collect();
    assert!(views.contains(&"n:journal") && views.contains(&"n:t:fix-1/session"));
    assert!(start.outline.items.iter().all(|i| !i.citable));
    // One session at a time.
    let again = start_session(&dir, None).unwrap_err().to_string();
    assert!(again.contains("is open until"), "{again}");
}

#[test]
fn an_outline_entry_never_proves() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    let refused =
        problems(submit(&dir, &s, &proposal(Level::Good, &["n:outputs"], false)).unwrap());
    assert!(refused[0].contains("outline"), "{refused:?}");
    assert!(
        read(
            &dir,
            &s,
            &ReadRequest::Read {
                r#ref: "n:outputs".into(),
                span: None,
            }
        )
        .is_err()
    );
}

#[test]
fn a_search_hit_is_not_citable_until_read() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    let hits = op(
        &dir,
        &s,
        ReadRequest::Search {
            query: "empty input".into(),
            view: Some("t:fix-1/session".into()),
        },
    );
    assert_eq!(hits.items[0].r#ref, "t:fix-1/session#12");
    assert!(!hits.items[0].citable);
    let refused = problems(
        submit(
            &dir,
            &s,
            &proposal(Level::Good, &["t:fix-1/session#12"], false),
        )
        .unwrap(),
    );
    assert!(refused[0].contains("search"), "{refused:?}");
    // The correction round reads the record and cites it: accepted.
    let page = op(
        &dir,
        &s,
        ReadRequest::Read {
            r#ref: "t:fix-1/session#12".into(),
            span: None,
        },
    );
    assert!(page.items[0].citable);
    let g = graded(
        submit(
            &dir,
            &s,
            &proposal(Level::Good, &["t:fix-1/session#12"], false),
        )
        .unwrap(),
    );
    assert_eq!(g.criteria[0].level, Level::Good);
    assert_eq!(g.grader.to_string(), "host/1");
    // The session is closed.
    assert!(submit(&dir, &s, &proposal(Level::Good, &[], true)).is_err());
}

#[test]
fn an_absence_over_a_view_not_read_to_its_end_is_refused_then_not_enough_data() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    op(
        &dir,
        &s,
        ReadRequest::Page {
            view: "outputs".into(),
            from: 0,
            count: None,
        },
    );
    let refused = problems(submit(&dir, &s, &proposal(Level::Good, &[], true)).unwrap());
    assert!(
        refused.iter().any(|p| p.contains("t:fix-1/session")),
        "{refused:?}"
    );
    // The correction submits the same claim without reading more: the one
    // correction round is spent and the level cannot stand.
    let g = graded(submit(&dir, &s, &proposal(Level::Good, &[], true)).unwrap());
    assert_eq!(g.criteria[0].level, Level::NotEnoughData);
    assert_eq!(g.criteria[0].reason_code, "evidence_rejected");
}

#[test]
fn reading_every_view_supports_an_absence() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    for view in [
        "outputs",
        "t:fix-1/session",
        "t:fix-1/stdout",
        "t:fix-1/stderr",
    ] {
        op(
            &dir,
            &s,
            ReadRequest::Page {
                view: view.into(),
                from: 0,
                count: None,
            },
        );
    }
    let g = graded(submit(&dir, &s, &proposal(Level::Good, &[], true)).unwrap());
    let c = &g.criteria[0];
    assert_eq!(c.level, Level::Good);
    assert_eq!(c.absence_basis.len(), 4);
    assert!(c.absence_basis.iter().all(|v| v.complete && v.ended));
}

#[test]
fn a_level_changed_without_new_reading_is_not_enough_data() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    op(
        &dir,
        &s,
        ReadRequest::Read {
            r#ref: "o:fix".into(),
            span: None,
        },
    );
    // First round: a cite of a record that was never read.
    problems(
        submit(
            &dir,
            &s,
            &proposal(Level::Good, &["t:fix-1/session#3"], false),
        )
        .unwrap(),
    );
    // Second round: another level on what was already read.
    let g = graded(submit(&dir, &s, &proposal(Level::Poor, &["o:fix"], false)).unwrap());
    assert_eq!(g.criteria[0].level, Level::NotEnoughData);
    assert_eq!(g.criteria[0].reason_code, "changed_without_reading");
}

#[test]
fn a_planted_value_is_redacted_from_reads_and_absent_from_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let value = planted();
    let s = start_session(&dir, None).unwrap().session;
    let rec = op(
        &dir,
        &s,
        ReadRequest::Read {
            r#ref: "t:fix-1/session#11".into(),
            span: None,
        },
    );
    let text = &rec.items[0].text;
    assert!(text.contains("[redacted-token]"));
    assert!(!text.contains(&value));
    let hits = op(
        &dir,
        &s,
        ReadRequest::Search {
            query: "env file".into(),
            view: None,
        },
    );
    assert!(hits.items.iter().all(|i| !i.text.contains(&value)));
    graded(
        submit(
            &dir,
            &s,
            &proposal(Level::Partial, &["t:fix-1/session#11"], false),
        )
        .unwrap(),
    );
    // Neither the grade nor the session file holds the run's text.
    for sub in ["grades", "grading"] {
        for f in fs::read_dir(dir.join(sub)).unwrap() {
            let path = f.unwrap().path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let body = fs::read_to_string(&path).unwrap();
            assert!(!body.contains(&value));
            assert!(!body.contains("The env file holds"));
            assert!(!body.contains("cargo test -p parser"));
        }
    }
}

#[test]
fn the_budget_stops_a_read_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, Some(20)).unwrap().session;
    let page = op(
        &dir,
        &s,
        ReadRequest::Page {
            view: "journal".into(),
            from: 0,
            count: None,
        },
    );
    assert!(page.items.is_empty());
    assert!(page.note.unwrap().contains("budget"));
    assert!(page.read_chars <= 20);
}

#[test]
fn a_run_without_criteria_of_its_own_has_no_host_session() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = run_dir(tmp.path(), "r1", CRM_JOURNAL, &agent_playbook(""));
    let err = start_session(&dir, None).unwrap_err().to_string();
    assert!(err.contains("declares no criteria"), "{err}");
}

/// Reads `refs` whole in session `s`.
fn read_whole(dir: &Path, s: &str, refs: &[&str]) {
    for r in refs {
        op(
            dir,
            s,
            ReadRequest::Read {
                r#ref: (*r).into(),
                span: None,
            },
        );
    }
}

#[test]
fn a_routing_call_between_rounds_is_not_new_reading() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    read_whole(&dir, &s, &["o:fix"]);
    problems(
        submit(
            &dir,
            &s,
            &proposal(Level::Poor, &["t:fix-1/session#3"], false),
        )
        .unwrap(),
    );
    // An outline, a listing and a search cost nothing and read nothing in
    // full: the changed level still does not stand.
    op(&dir, &s, ReadRequest::Outline);
    op(
        &dir,
        &s,
        ReadRequest::Children {
            view: "outputs".into(),
            from: 0,
            count: None,
        },
    );
    let g = graded(submit(&dir, &s, &proposal(Level::Good, &["o:fix"], false)).unwrap());
    assert_eq!(g.criteria[0].reason_code, "changed_without_reading");
}

#[test]
fn a_run_takes_one_host_grade_until_a_person_reopens_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    // Nothing to reopen before a host grade.
    assert!(reopen(&dir, "cli").is_err());
    let s = start_session(&dir, None).unwrap().session;
    read_whole(&dir, &s, &["o:fix"]);
    let first = graded(submit(&dir, &s, &proposal(Level::Poor, &["o:fix"], false)).unwrap());
    // The session closed with a grade: a new one is refused, so a grader
    // cannot grade the run again until the level suits it.
    let refused = start_session(&dir, None).unwrap_err().to_string();
    assert!(refused.contains(&first.grade_id), "{refused}");
    assert!(refused.contains("--reopen"), "{refused}");
    reopen(&dir, "cli").unwrap();
    // Reopening again before the leave is spent changes nothing.
    reopen(&dir, "dashboard").unwrap();
    let s = start_session(&dir, None).unwrap().session;
    read_whole(&dir, &s, &["o:fix"]);
    graded(submit(&dir, &s, &proposal(Level::Good, &["o:fix"], false)).unwrap());
    // The leave is spent by the grade it allowed.
    assert!(start_session(&dir, None).is_err());
    // A host grade file that stops reading reopens nothing.
    reopen(&dir, "cli").unwrap();
    std::fs::write(
        dir.join("grades").join(format!("{}.json", first.grade_id)),
        "not json",
    )
    .unwrap();
    assert!(start_session(&dir, None).is_err());
}

#[test]
fn a_hosts_not_applicable_does_not_count_for_promotion() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    let g = graded(submit(&dir, &s, &proposal(Level::NotApplicable, &[], false)).unwrap());
    assert_eq!(g.criteria[0].level, Level::NotApplicable);
    assert!(!g.criteria[0].counts_for_promotion);
}

#[test]
fn two_levels_for_one_criterion_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    read_whole(&dir, &s, &["o:fix"]);
    let mut both = proposal(Level::Good, &["o:fix"], false);
    both.extend(proposal(Level::Poor, &["o:fix"], false));
    let refused = problems(submit(&dir, &s, &both).unwrap());
    assert!(refused[0].contains("more than one level"), "{refused:?}");
}

#[test]
fn a_cite_outside_the_views_the_criterion_reads_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    // `reply-tone` reads outputs and transcripts, not the journal.
    read_whole(&dir, &s, &["j:4"]);
    let refused = problems(submit(&dir, &s, &proposal(Level::Good, &["j:4"], false)).unwrap());
    assert!(refused[0].contains("outside the views"), "{refused:?}");
}

#[test]
fn an_absence_over_no_view_of_its_kinds_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let only_transcripts = TONE.replace("[outputs, transcripts]", "[transcripts]");
    // A run with no agent attempt has no transcript view at all.
    let dir = run_dir(
        tmp.path(),
        "r1",
        CRM_JOURNAL,
        &agent_playbook(&only_transcripts),
    );
    let s = start_session(&dir, None).unwrap().session;
    let refused = problems(submit(&dir, &s, &proposal(Level::Good, &[], true)).unwrap());
    assert!(refused[0].contains("no view"), "{refused:?}");
}

#[test]
fn a_span_is_checked_against_the_text_the_reader_showed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    let shown = op(
        &dir,
        &s,
        ReadRequest::Read {
            r#ref: "t:fix-1/session#11".into(),
            span: None,
        },
    );
    let len = shown.items[0].text.chars().count() as u64;
    let cite = |to: u64| {
        vec![Proposal {
            cites: vec![Cite {
                r#ref: "t:fix-1/session#11".into(),
                span: Some([0, to]),
            }],
            ..proposal(Level::Partial, &[], false).remove(0)
        }]
    };
    // The redacted text is shorter than the run's own: a span past what
    // the reader showed is outside the record.
    let refused = problems(submit(&dir, &s, &cite(len + 10)).unwrap());
    assert!(refused[0].contains("outside the record"), "{refused:?}");
    graded(submit(&dir, &s, &cite(len)).unwrap());
}

#[test]
fn a_reason_code_is_a_short_slug_or_the_levels_name() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, None).unwrap().session;
    read_whole(&dir, &s, &["o:fix"]);
    let mut p = proposal(Level::Good, &["o:fix"], false);
    p[0].reason_code = "\u{1b}[31mcopied run text".into();
    let g = graded(submit(&dir, &s, &p).unwrap());
    assert_eq!(g.criteria[0].reason_code, "good");
}

#[test]
fn listings_and_searches_stop_at_the_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = code_run(tmp.path());
    let s = start_session(&dir, Some(100)).unwrap().session;
    let listing = op(
        &dir,
        &s,
        ReadRequest::Children {
            view: "journal".into(),
            from: 0,
            count: None,
        },
    );
    assert!(listing.read_chars <= 100, "{}", listing.read_chars);
    assert!(listing.note.unwrap().contains("budget"));
    let hits = op(
        &dir,
        &s,
        ReadRequest::Search {
            query: "fix".into(),
            view: None,
        },
    );
    assert!(hits.read_chars <= 100, "{}", hits.read_chars);
    assert!(hits.items.is_empty());
    assert!(hits.note.unwrap().contains("budget"));
}
