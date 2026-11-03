//! The check of an attempt's claims against what its evidence session
//! read, through the shared citation rules ([`crate::citation`]).
//!
//! An attempt with `evidence` reports `outputs.claims` in its status file:
//!
//! ```json
//! [{ "id": "c1", "text": "...", "cites": [{ "ref": "r:...", "span": [a, b] }],
//!    "absence": { "kinds": ["email"] } }]
//! ```
//!
//! A span counts characters of the record's text as served. Each claim gets
//! one verdict: `structurally_valid` (every citation names a record of this
//! session, of an allowed kind, returned whole to this attempt, the span
//! inside it), `absence_ok` (every view of the named kinds complete and
//! read to its end), `not_enough_data` (an absence over a view that is not
//! complete) or `unsupported`, with its problems. A valid claim may carry
//! "worth a look" flags ([`CitationFlag`]); none of them is an error. Nothing here
//! judges whether a record supports a claim.
//!
//! The outcome keeps references, spans in byte form, verdicts, problems
//! and flags: no source text and no claim text (it stays in the node
//! output). Every problem passes the run redactor.

use std::collections::{BTreeMap, BTreeSet};

use apb_core::quality::grade::{Cite, ViewEnd};
use apb_core::result_store::Citation;
use serde::{Deserialize, Serialize};

use super::sources::BoundSource;
use crate::citation::ReadState;
use crate::citation::{self, Claim, ViewSet};

/// A span under this many characters is flagged `short_span`.
pub const SHORT_SPAN_CHARS: u64 = 16;
/// A record cited by more claims than this is flagged `shared_record`.
pub const SHARED_RECORD_CLAIMS: usize = 5;
/// The shortest word `no_shared_words` compares.
pub const MIN_WORD_CHARS: usize = 4;
/// What a citation without a span shows of its record.
pub const FRAGMENT_CHARS: usize = 300;

/// A claim's verdict.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimVerdict {
    StructurallyValid,
    Unsupported,
    AbsenceOk,
    NotEnoughData,
}

impl ClaimVerdict {
    /// Whether the claim stands (structurally): valid or a valid absence.
    pub fn stands(self) -> bool {
        matches!(
            self,
            ClaimVerdict::StructurallyValid | ClaimVerdict::AbsenceOk
        )
    }
}

/// A deterministic "worth a look" mark on a claim that stands.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationFlag {
    /// A citation names a whole record, no span.
    WholeRecord,
    /// A span is under [`SHORT_SPAN_CHARS`] characters.
    ShortSpan,
    /// A cited record is of the consumer node's own earlier execution.
    SelfCitation,
    /// A cited record is cited by more than [`SHARED_RECORD_CLAIMS`] claims.
    SharedRecord,
    /// No word of [`MIN_WORD_CHARS`] or more letters is shared between the
    /// claim's text and a cited fragment.
    NoSharedWords,
}

/// One citation as checked.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedCite {
    /// The record reference as cited (no range).
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// The character span as cited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub span: Option<[u64; 2]>,
    /// The accepted citation in byte form (`...#[a,b)` over the record's
    /// stored UTF-8 text); absent when the citation was not accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub stored: Option<String>,
}

/// One claim's result.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimResult {
    pub id: String,
    pub verdict: ClaimVerdict,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cites: Vec<CheckedCite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<CitationFlag>,
    /// For an absence: the kinds it names and the views it rests on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub absence_kinds: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<ViewEnd>>", optional))]
    pub absence_basis: Vec<ViewEnd>,
}

/// The check of one attempt.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Outcome {
    pub claims: Vec<ClaimResult>,
    /// Problems of the claims as a whole (`claims: required` and none, a
    /// list that does not parse).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

impl Outcome {
    /// Every claim stands and nothing is wrong with the list.
    pub fn valid(&self) -> bool {
        self.problems.is_empty() && self.claims.iter().all(|c| c.verdict.stands())
    }

    /// The rejection note of an `on_invalid: fail` attempt: every problem,
    /// by claim, references and counts only.
    pub fn rejection_note(&self) -> String {
        let mut parts: Vec<String> = self.problems.clone();
        for c in self.claims.iter().filter(|c| !c.verdict.stands()) {
            parts.push(format!(
                "claim `{}` is {}: {}",
                c.id,
                match c.verdict {
                    ClaimVerdict::NotEnoughData => "not_enough_data",
                    _ => "unsupported",
                },
                c.problems.join("; ")
            ));
        }
        format!(
            "evidence check rejected the claims: {}. Read every record you cite whole through the evidence reader (`read` or `page`), keep spans inside it, and cite only kinds the node allows",
            parts.join(". ")
        )
    }
}

/// One claim as an attempt reports it.
#[derive(Debug, Clone, Default, Deserialize)]
struct ClaimIn {
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    cites: Vec<CiteIn>,
    #[serde(default)]
    absence: Option<AbsenceIn>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CiteIn {
    #[serde(rename = "ref", default)]
    r#ref: String,
    #[serde(default)]
    span: Option<[u64; 2]>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AbsenceIn {
    #[serde(default)]
    kinds: Vec<String>,
}

/// A claim id as kept: the agent's own when it is short and plain, else
/// its position.
fn claim_id(id: Option<&serde_json::Value>, at: usize) -> String {
    match id.and_then(serde_json::Value::as_str) {
        Some(s) if apb_core::records::is_record_id(s) && s.len() <= 64 => s.to_string(),
        _ => format!("#{}", at + 1),
    }
}

/// The lowercase words of `text` of at least [`MIN_WORD_CHARS`] letters.
fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|w| w.chars().count() >= MIN_WORD_CHARS)
        .map(|w| w.chars().flat_map(char::to_lowercase).collect())
        .collect()
}

/// What a citation shows of its record: the span, or the first
/// [`FRAGMENT_CHARS`] characters without one.
pub fn fragment(text: &str, span: Option<[u64; 2]>) -> String {
    match span {
        Some([a, b]) if a < b => text
            .chars()
            .skip(a as usize)
            .take((b - a) as usize)
            .collect(),
        _ => text.chars().take(FRAGMENT_CHARS).collect(),
    }
}

/// The byte form of an accepted citation: its character span as a byte
/// range of the record's text.
fn byte_form(r: &str, text: &str, span: Option<[u64; 2]>) -> Option<String> {
    let mut c: Citation = r.parse().ok()?;
    if let Some([a, b]) = span {
        let at = |n: u64| {
            text.char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(text.len()))
                .nth(n as usize)
        };
        c.range = Some((at(a)? as u64, at(b)? as u64));
    }
    Some(c.to_string())
}

/// What a check reads of a closed session: the consumer node, the kinds
/// its declaration allows, the snapshots it bound and what it read.
#[derive(Debug, Clone, Copy)]
pub struct Reading<'a> {
    pub node: &'a str,
    pub kinds: Option<&'a [String]>,
    pub sources: &'a [BoundSource],
    pub state: &'a ReadState,
}

/// The kinds a citation may name: the declaration's, else every kind the
/// session's views have.
fn allowed(file: &Reading<'_>, views: &ViewSet<String>) -> Vec<String> {
    match file.kinds {
        Some(k) => k.to_vec(),
        None => {
            let mut k: Vec<String> = views.all().iter().map(|v| v.kind.clone()).collect();
            k.sort();
            k.dedup();
            k
        }
    }
}

/// Checks the claims of `output` (the node output: the status file's
/// `outputs` as JSON) of an attempt of `file`'s node against `views` and
/// what the session read. `required`: a missing or empty `claims` is a
/// problem. `redact` passes every problem on its way out.
pub fn check(
    file: &Reading<'_>,
    views: &ViewSet<String>,
    output: &str,
    required: bool,
    redact: &dyn Fn(&str) -> String,
) -> Outcome {
    let mut outcome = Outcome::default();
    let doc: Option<serde_json::Value> = serde_json::from_str(output).ok();
    let claims = doc.as_ref().and_then(|d| d.get("claims"));
    let list: Vec<ClaimIn> = match claims {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(v) => match serde_json::from_value::<Vec<ClaimIn>>(v.clone()) {
            Ok(l) => l,
            Err(_) => {
                outcome.problems.push(
                    "`outputs.claims` is not a list of { id, text, cites, absence? } claims".into(),
                );
                Vec::new()
            }
        },
    };
    if required && list.is_empty() && outcome.problems.is_empty() {
        outcome.problems.push(
            "claims are required: the status file's `outputs.claims` holds none (unsupported output)"
                .into(),
        );
    }
    let kinds = allowed(file, views);
    let own: BTreeSet<&str> = file
        .sources
        .iter()
        .filter(|s| s.node == file.node)
        .map(|s| s.snapshot_id.as_str())
        .collect();
    for (at, c) in list.iter().enumerate() {
        outcome
            .claims
            .push(one(c, at, file, views, &kinds, &own, redact));
    }
    // A record cited by many claims: worth a look on each of them.
    let mut cited_by: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &outcome.claims {
        let refs: BTreeSet<&str> = c.cites.iter().map(|x| x.r#ref.as_str()).collect();
        for r in refs {
            *cited_by.entry(r).or_default() += 1;
        }
    }
    let shared: BTreeSet<String> = cited_by
        .into_iter()
        .filter(|(_, n)| *n > SHARED_RECORD_CLAIMS)
        .map(|(r, _)| r.to_string())
        .collect();
    for c in &mut outcome.claims {
        if c.verdict == ClaimVerdict::StructurallyValid
            && c.cites.iter().any(|x| shared.contains(&x.r#ref))
        {
            c.flags.push(CitationFlag::SharedRecord);
            c.flags.sort();
        }
    }
    outcome.problems = outcome.problems.iter().map(|p| redact(p)).collect();
    outcome
}

fn one(
    c: &ClaimIn,
    at: usize,
    file: &Reading<'_>,
    views: &ViewSet<String>,
    kinds: &[String],
    own: &BTreeSet<&str>,
    redact: &dyn Fn(&str) -> String,
) -> ClaimResult {
    let id = claim_id(c.id.as_ref(), at);
    let mut problems = Vec::new();
    let mut cites = Vec::new();
    for (i, x) in c.cites.iter().enumerate() {
        // A reference that is no result reference is not echoed: it is
        // the agent's text, not a record's name.
        if x.r#ref.parse::<Citation>().is_err() {
            problems.push(format!(
                "citation {} is not a result reference r:<snapshot>/<entry>[@<record>]",
                i + 1
            ));
            continue;
        }
        let cite = Cite {
            r#ref: x.r#ref.clone(),
            span: x.span,
        };
        // A record of a kind the node does not allow, said in this node's
        // words; every other rule is the shared one.
        let problem = if views.record(&x.r#ref).is_some()
            && !views.holding(&x.r#ref).any(|v| kinds.contains(&v.kind))
        {
            Some(format!(
                "`{}` is of a kind this node's evidence does not allow",
                x.r#ref
            ))
        } else {
            citation::rules::cite_problem(&cite, kinds, views, file.state)
        };
        let stored = match (&problem, views.record(&x.r#ref)) {
            (None, Some(rec)) => byte_form(&x.r#ref, &rec.text, x.span),
            _ => None,
        };
        problems.extend(problem);
        cites.push(CheckedCite {
            r#ref: redact(&x.r#ref),
            span: x.span,
            stored,
        });
    }
    let mut absence_basis = Vec::new();
    let absence_kinds = c.absence.as_ref().map(|a| {
        if a.kinds.is_empty() {
            kinds.to_vec()
        } else {
            a.kinds.clone()
        }
    });
    if let Some(ak) = &absence_kinds {
        for k in ak.iter().filter(|k| !kinds.contains(k)) {
            problems.push(format!(
                "an absence of `{k}` names a kind this node does not allow"
            ));
        }
        let checked = citation::check(
            &Claim {
                cites: &[],
                absence: true,
                absence_allowed: true,
            },
            ak,
            views,
            file.state,
        );
        problems.extend(checked.problems);
        absence_basis = checked.absence_basis;
    } else if c.cites.is_empty() {
        problems.push("the claim cites no record and claims no absence".into());
    }
    let verdict = if !problems.is_empty() {
        ClaimVerdict::Unsupported
    } else if absence_kinds.is_some() {
        if let Some(v) = absence_basis.iter().find(|v| !v.complete) {
            problems.push(format!(
                "incomplete_view: `{}` is not complete, so an absence over it is not established",
                v.view
            ));
            ClaimVerdict::NotEnoughData
        } else {
            ClaimVerdict::AbsenceOk
        }
    } else {
        ClaimVerdict::StructurallyValid
    };
    let mut flags = BTreeSet::new();
    if verdict == ClaimVerdict::StructurallyValid {
        let claim_words = words(&c.text);
        for x in &c.cites {
            let Some(rec) = views.record(&x.r#ref) else {
                continue;
            };
            match x.span {
                None => {
                    flags.insert(CitationFlag::WholeRecord);
                }
                Some([a, b]) if b - a < SHORT_SPAN_CHARS => {
                    flags.insert(CitationFlag::ShortSpan);
                }
                Some(_) => {}
            }
            let snapshot = x.r#ref.parse::<Citation>().map(|c| c.snapshot_id).ok();
            if snapshot.is_some_and(|s| own.contains(s.as_str())) {
                flags.insert(CitationFlag::SelfCitation);
            }
            if words(&fragment(&rec.text, x.span))
                .intersection(&claim_words)
                .next()
                .is_none()
            {
                flags.insert(CitationFlag::NoSharedWords);
            }
        }
    }
    ClaimResult {
        id,
        verdict,
        problems: problems.iter().map(|p| redact(p)).collect(),
        cites,
        flags: flags.into_iter().collect(),
        absence_kinds,
        absence_basis,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::citation::views::{SourceRecord, SourceView};

    const OWN: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn record(snapshot: &str, id: &str, text: &str) -> SourceRecord {
        SourceRecord {
            r#ref: format!("r:{snapshot}/file:run:p.jsonl@{id}"),
            text: text.into(),
        }
    }

    /// A session of node `use` over two packages: one of `collect`, one of
    /// `use`'s own earlier execution, every record read in full.
    fn fixture() -> (Vec<BoundSource>, ReadState, ViewSet<String>) {
        let views = vec![
            SourceView {
                id: "collect.1/file:run:p.jsonl".into(),
                kind: "records".to_string(),
                complete: true,
                records: vec![record(OTHER, "a", "The October invoice is late again")],
            },
            SourceView {
                id: "use.1/file:run:p.jsonl".into(),
                kind: "records".to_string(),
                complete: true,
                records: vec![record(OWN, "b", "An earlier answer about invoices")],
            },
        ];
        let mut state = ReadState::new(1000);
        for v in &views {
            for r in &v.records {
                state.full.insert(r.r#ref.clone());
            }
        }
        let sources = vec![
            BoundSource {
                node: "collect".into(),
                execution: 1,
                item: None,
                snapshot_id: OTHER.into(),
            },
            BoundSource {
                node: "use".into(),
                execution: 1,
                item: None,
                snapshot_id: OWN.into(),
            },
        ];
        (sources, state, ViewSet::new(views))
    }

    fn checked(claims: serde_json::Value) -> Outcome {
        let (sources, state, views) = fixture();
        let reading = Reading {
            node: "use",
            kinds: None,
            sources: &sources,
            state: &state,
        };
        let output = serde_json::json!({ "claims": claims }).to_string();
        check(&reading, &views, &output, false, &|t: &str| t.to_string())
    }

    #[test]
    fn every_flag_is_raised_on_its_own_case_and_none_is_an_error() {
        let a = format!("r:{OTHER}/file:run:p.jsonl@a");
        let b = format!("r:{OWN}/file:run:p.jsonl@b");
        let o = checked(serde_json::json!([
            { "id": "plain", "text": "The invoice is late", "cites": [{ "ref": a, "span": [4, 33] }] },
            { "id": "whole", "text": "October invoice", "cites": [{ "ref": a }] },
            { "id": "short", "text": "October invoice", "cites": [{ "ref": a, "span": [4, 11] }] },
            { "id": "self", "text": "earlier answer", "cites": [{ "ref": b, "span": [0, 32] }] },
            { "id": "words", "text": "zz yy", "cites": [{ "ref": b, "span": [0, 32] }] }
        ]));
        let flags = |id: &str| {
            o.claims
                .iter()
                .find(|c| c.id == id)
                .map(|c| (c.verdict, c.flags.clone()))
                .unwrap()
        };
        assert!(o.valid(), "{o:?}");
        assert_eq!(flags("plain"), (ClaimVerdict::StructurallyValid, vec![]));
        assert_eq!(flags("whole").1, vec![CitationFlag::WholeRecord]);
        assert_eq!(flags("short").1, vec![CitationFlag::ShortSpan]);
        assert_eq!(flags("self").1, vec![CitationFlag::SelfCitation]);
        assert_eq!(
            flags("words").1,
            vec![CitationFlag::SelfCitation, CitationFlag::NoSharedWords]
        );
    }

    #[test]
    fn a_record_cited_by_more_than_five_claims_is_shared() {
        let a = format!("r:{OTHER}/file:run:p.jsonl@a");
        let claims: Vec<_> = (0..6)
            .map(|i| serde_json::json!({ "id": format!("c{i}"), "text": "invoice late", "cites": [{ "ref": a, "span": [4, 33] }] }))
            .collect();
        let o = checked(serde_json::Value::Array(claims.clone()));
        assert!(
            o.claims
                .iter()
                .all(|c| c.flags == vec![CitationFlag::SharedRecord])
        );
        let o = checked(serde_json::Value::Array(claims[..5].to_vec()));
        assert!(o.claims.iter().all(|c| c.flags.is_empty()));
    }

    #[test]
    fn a_reference_that_is_no_result_reference_is_not_echoed() {
        let o = checked(serde_json::json!([
            { "id": "x", "text": "t", "cites": [{ "ref": "sk-secretvalue" }] }
        ]));
        assert_eq!(o.claims[0].verdict, ClaimVerdict::Unsupported);
        assert!(!format!("{o:?}").contains("secretvalue"));
        // An absence of a kind the node does not allow.
        let o = checked(serde_json::json!([
            { "id": "y", "text": "t", "absence": { "kinds": ["email"] } }
        ]));
        assert!(o.claims[0].problems[0].contains("does not allow"));
    }

    #[test]
    fn a_character_span_is_kept_as_a_byte_range() {
        assert_eq!(
            byte_form(&format!("r:{OTHER}/output"), "aé b", Some([1, 3])),
            Some(format!("r:{OTHER}/output#[1,4)"))
        );
    }
}
