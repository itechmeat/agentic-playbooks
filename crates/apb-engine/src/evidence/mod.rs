//! Citations for node outputs: an `agent_task` with `evidence:` reads the
//! stored snapshots of its `from` nodes through the engine's bounded
//! reader, one session per attempt, and its claims are checked against
//! what that attempt read.
//!
//! - [`sources`]: the bound snapshots and their views (record packages
//!   included, [`apb_core::records`]);
//! - [`session`]: the session file, opened right after `attempt_started`,
//!   read through [`session::read`] and closed when the attempt ends;
//! - [`check`]: the claims of a succeeded attempt, one verdict each,
//!   through the shared citation rules ([`crate::citation`]).
//!
//! What a `structurally_valid` claim guarantees: the record exists in a
//! stored snapshot this run's journal names; it was returned whole to this
//! attempt through the reader; it is of a kind the declaration allows; the
//! span lies inside it; for an absence, every view of the named kinds was
//! complete and read to its end. Not promised: that the record supports the
//! claim, that it is true, that the agent read nothing past the reader, or
//! that a package is complete relative to the outside world.

pub mod check;
pub mod session;
pub mod sources;

use std::path::Path;

use apb_core::quality::grade::ReadTrace;
use serde::Serialize;

pub use check::{CheckedCite, CitationFlag, ClaimResult, ClaimVerdict, Outcome};
pub use session::{SESSION_ENV, SessionFile};
pub use sources::BoundSource;

use crate::error::EngineError;
use crate::run_redact::RunRedactor;

/// The failure kind of an attempt whose claims `on_invalid: fail` refused.
pub const CHECK_REJECTED: &str = "check_rejected";

/// The fixed wording of a `structurally_valid` claim on every surface.
pub const STRUCTURALLY_VALID: &str = "Structurally valid: the cited record exists, was returned whole to this attempt, is of an allowed kind, and the span lies inside it. Whether it supports the claim was not checked.";
/// The fixed wording of an `unsupported` claim.
pub const UNSUPPORTED: &str = "not supported by a cited record (not shown as a fact)";
/// The fixed wording of a valid absence.
pub const ABSENCE_OK: &str = "Absence structurally valid: every view of the named kinds was complete and read to its end. Whether the source holds nothing else was not checked.";
/// The fixed wording of an absence over a view that is not complete.
pub const NOT_ENOUGH_DATA: &str = "not enough data: a view the absence rests on is not complete";
/// How a collector's own statement of completeness is shown.
pub const COLLECTOR_COMPLETE: &str = "the collector says the package is complete";
/// How a collector's own statement of incompleteness is shown.
pub const COLLECTOR_INCOMPLETE: &str = "the collector says the package is not complete";
/// How the flags are introduced: never as errors.
pub const WORTH_A_LOOK: &str = "worth a look";

/// The wording of `verdict`.
pub fn wording(verdict: ClaimVerdict) -> &'static str {
    match verdict {
        ClaimVerdict::StructurallyValid => STRUCTURALLY_VALID,
        ClaimVerdict::Unsupported => UNSUPPORTED,
        ClaimVerdict::AbsenceOk => ABSENCE_OK,
        ClaimVerdict::NotEnoughData => NOT_ENOUGH_DATA,
    }
}

/// One citation as a surface shows it: the reference and the fragment it
/// cites (the span, or the first 300 characters of the record), redacted.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CiteShown {
    #[serde(rename = "ref")]
    pub r#ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub span: Option<[u64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub stored: Option<String>,
    /// Absent when the record is not in the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub fragment: Option<String>,
}

/// One claim as a surface shows it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClaimShown {
    pub id: String,
    /// The claim's text from the node output, redacted.
    pub text: String,
    pub verdict: ClaimVerdict,
    /// The fixed wording of the verdict.
    pub wording: String,
    pub problems: Vec<String>,
    pub flags: Vec<CitationFlag>,
    pub cites: Vec<CiteShown>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub absence_kinds: Option<Vec<String>>,
}

/// One package view as a surface shows it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageShown {
    pub view: String,
    pub records: u64,
    /// The engine's own view: the snapshot is whole and the collector says
    /// so.
    pub complete: bool,
    /// The collector's own statement, as its wording.
    pub collector: String,
}

/// One attempt's session as a surface shows it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionShown {
    pub node: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<String>,
    pub attempt: u32,
    pub attempt_id: u64,
    /// `open`, `closed` (the attempt ended, no check) or `checked`.
    pub state: String,
    pub sources: Vec<BoundSource>,
    pub packages: Vec<PackageShown>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub trace: Option<ReadTrace>,
    pub problems: Vec<String>,
    pub claims: Vec<ClaimShown>,
}

/// The claims texts of an attempt's status file, by claim id.
fn claim_texts(run_dir: &Path, stem: &str) -> Vec<(Option<String>, String)> {
    let raw = std::fs::read(run_dir.join("agent-status").join(format!("{stem}.json")));
    let doc: Option<serde_json::Value> = raw.ok().and_then(|r| serde_json::from_slice(&r).ok());
    doc.as_ref()
        .and_then(|d| d.pointer("/outputs/claims"))
        .and_then(serde_json::Value::as_array)
        .map(|l| {
            l.iter()
                .map(|c| {
                    (
                        c.get("id").and_then(|v| v.as_str()).map(str::to_string),
                        c.get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every evidence session of the run at `run_dir` (only `node`'s, when
/// given), with each claim's verdict, wording, flags and the fragments it
/// cites, every text through the run redactor. Refused while the redactor
/// withholds the run's text.
pub fn view(run_dir: &Path, node: Option<&str>) -> Result<Vec<SessionShown>, EngineError> {
    let redactor = RunRedactor::for_run_dir(run_dir);
    if redactor.withholds() {
        return Err(crate::run_redact::SecretsUnresolved.into());
    }
    let mut out = Vec::new();
    for file in session::all(run_dir) {
        if node.is_some_and(|n| n != file.node) {
            continue;
        }
        let views = session::views_of(run_dir, &file).ok();
        let packages = file
            .views
            .iter()
            .filter_map(|v| {
                let said = v.collector_complete?;
                Some(PackageShown {
                    view: v.id.clone(),
                    records: v.records.len() as u64,
                    complete: v.complete,
                    collector: if said {
                        COLLECTOR_COMPLETE
                    } else {
                        COLLECTOR_INCOMPLETE
                    }
                    .to_string(),
                })
            })
            .collect();
        let texts = claim_texts(run_dir, &file.stem);
        let outcome = file.closed.as_ref().and_then(|c| c.outcome.clone());
        let claims = outcome
            .as_ref()
            .map(|o| {
                o.claims
                    .iter()
                    .enumerate()
                    .map(|(at, c)| {
                        let text = texts
                            .iter()
                            .find(|(id, _)| id.as_deref() == Some(c.id.as_str()))
                            .or_else(|| texts.get(at))
                            .map(|(_, t)| redactor.redact_str(t))
                            .unwrap_or_default();
                        ClaimShown {
                            id: c.id.clone(),
                            text,
                            verdict: c.verdict,
                            wording: wording(c.verdict).to_string(),
                            problems: c.problems.clone(),
                            flags: c.flags.clone(),
                            cites: c
                                .cites
                                .iter()
                                .map(|x| CiteShown {
                                    r#ref: x.r#ref.clone(),
                                    span: x.span,
                                    stored: x.stored.clone(),
                                    fragment: views
                                        .as_ref()
                                        .and_then(|v| v.set.record(&x.r#ref))
                                        .map(|r| check::fragment(&r.text, x.span)),
                                })
                                .collect(),
                            absence_kinds: c.absence_kinds.clone(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(SessionShown {
            state: match &file.closed {
                None => "open",
                Some(c) if c.checked => "checked",
                Some(_) => "closed",
            }
            .to_string(),
            node: file.node,
            item: file.item,
            attempt: file.attempt,
            attempt_id: file.attempt_id,
            sources: file.sources,
            packages,
            trace: file.closed.as_ref().map(|c| c.trace.clone()),
            problems: outcome.map(|o| o.problems).unwrap_or_default(),
            claims,
        });
    }
    Ok(out)
}

/// Checks the claims a succeeded attempt reported in `output` against its
/// closed session `file` and writes the outcome into the session file.
/// Returns the rejection note when the declaration says `on_invalid: fail`
/// and a claim does not stand; `None` keeps the attempt's status (`mark`).
pub fn check_attempt(
    run_dir: &Path,
    file: &SessionFile,
    decl: &apb_core::schema::EvidenceDecl,
    output: &str,
) -> Result<Option<String>, EngineError> {
    let redactor = RunRedactor::for_run_dir(run_dir);
    let redact = |t: &str| redactor.redact_str(t);
    let required = decl.claims == apb_core::schema::EvidenceClaims::Required;
    let outcome = match session::views_of(run_dir, file) {
        Ok(views) => check::check(&file.reading(), &views.set, output, required, &redact),
        Err(e) => Outcome {
            claims: Vec::new(),
            problems: vec![redact(&format!(
                "the session's sources no longer read, so no claim could be checked: {e}"
            ))],
        },
    };
    session::record(run_dir, file.attempt_id, &outcome)?;
    let fail = decl.on_invalid == apb_core::schema::EvidenceOnInvalid::Fail;
    Ok((fail && !outcome.valid()).then(|| outcome.rejection_note()))
}
