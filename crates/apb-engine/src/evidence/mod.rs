//! Evidence for node outputs: an `agent_task` with an `evidence:` block
//! reads the latest result snapshots of its `from` nodes through a reading
//! session the engine opens per attempt, and backs its output with claims
//! that cite the records it read (see `docs/RESULTS.md`).
//!
//! - [`sources`]: the bound snapshots and their views;
//! - [`session`]: the session file, its reads, its close;
//! - [`check`]: the claims of a succeeded attempt, their verdicts and
//!   flags.
//!
//! The session reads through the same bounded reader and the same
//! citation rules as the run grader (`crate::citation`). Guaranteed for a
//! `structurally_valid` claim: every cited record exists in a stored
//! snapshot this run's journal names, was returned whole to this attempt
//! through the engine's reader, is of a kind the declaration allows, and
//! every span lies inside it; for an absence, every view of the named kinds
//! was complete and read to its end. Not promised: that the record supports
//! the claim or is true, that the agent read nothing past the reader, or
//! that a package is complete relative to the outside world.

pub mod check;
pub mod session;
pub mod sources;

use std::path::Path;

use serde::Serialize;

pub use session::{ClaimVerdict, ClaimsOutcome, Flag, SessionFile, Verdict, is_session_id};

/// The variable that carries an attempt's evidence session to its process.
pub const SESSION_ENV: &str = "APB_EVIDENCE_SESSION";

/// The wording every surface shows next to a structurally valid claim.
pub const STRUCTURALLY_VALID_WORDING: &str = "Structurally valid: the cited record exists, was returned whole to this attempt, is of an allowed kind, and the span lies inside it. Whether it supports the claim was not checked.";
/// The wording every surface shows next to an unsupported claim.
pub const UNSUPPORTED_WORDING: &str = "Not supported by a cited record (not shown as a fact).";
/// The heading of the flags of a structurally valid claim.
pub const FLAGS_WORDING: &str = "Worth a look";
/// How a package's own completeness statement is shown.
pub const COLLECTOR_COMPLETE_WORDING: &str = "the collector says the package is complete";

/// One claim as a surface shows it: the verdict and flags from the
/// session file, the claim text from the attempt's status file and the
/// cited fragments from the stored snapshots, through the run redactor.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimRow {
    pub id: String,
    pub verdict: Verdict,
    /// The fixed wording of the verdict.
    pub wording: String,
    pub text: String,
    pub problems: Vec<String>,
    pub flags: Vec<Flag>,
    pub cites: Vec<CitedFragment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub absence: Option<Vec<String>>,
}

/// One accepted citation and the fragment it points at, so a person can
/// compare it with the claim.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CitedFragment {
    #[serde(rename = "ref")]
    pub r#ref: String,
    pub fragment: String,
}

/// The claims of one attempt's evidence session.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceRow {
    pub node: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<String>,
    pub attempt_number: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub attempt: Option<u64>,
    pub session: String,
    pub closed: bool,
    pub read_chars: u64,
    pub budget_chars: u64,
    /// The bound snapshots.
    pub snapshots: Vec<String>,
    /// The session's views, without text.
    pub views: Vec<ViewSummary>,
    /// `false` until the attempt's claims were checked.
    pub checked: bool,
    pub output_unsupported: bool,
    pub problems: Vec<String>,
    pub claims: Vec<ClaimRow>,
}

/// One view of a session as a surface shows it. `complete` is the
/// engine's word; `collector_complete` a package's own statement, shown as
/// [`COLLECTOR_COMPLETE_WORDING`].
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ViewSummary {
    pub id: String,
    pub kind: String,
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub collector_complete: Option<bool>,
    pub records: u32,
}

/// The text of each claim the status file of `file`'s attempt holds, in the
/// order `outputs.claims` lists them. Verdicts were stored in that same
/// order, so a row finds its text by position and keeps it only while the
/// text still hashes to the digest the check stored.
fn claim_texts(run_dir: &Path, file: &SessionFile) -> Vec<String> {
    let path = run_dir
        .join("agent-status")
        .join(format!("{}.json", file.stem));
    let Some(v) = std::fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
    else {
        return Vec::new();
    };
    v.pointer("/outputs/claims")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|c| {
            c.get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Every evidence session of the run at `run_dir`, oldest first, with its
/// claims as a surface shows them. Text goes through the run redactor.
pub fn rows(run_dir: &Path) -> Vec<EvidenceRow> {
    let redactor = crate::run_redact::RunRedactor::for_run_dir(run_dir);
    session::all(run_dir)
        .into_iter()
        .map(|f| {
            let texts = claim_texts(run_dir, &f);
            let built = session::rebuild(run_dir, &f).ok();
            let claims = f
                .claims
                .as_ref()
                .map(|o| {
                    o.claims
                        .iter()
                        .enumerate()
                        .map(|(i, c)| claim_row(c, i, &texts, built.as_ref(), &redactor))
                        .collect()
                })
                .unwrap_or_default();
            EvidenceRow {
                node: f.node.clone(),
                item: f.item.clone(),
                attempt_number: f.attempt_number,
                attempt: f.attempt,
                session: f.session.clone(),
                closed: f.closed_at.is_some(),
                read_chars: f.state.read_chars,
                budget_chars: f.state.budget_chars,
                snapshots: f.snapshots.iter().map(|s| s.snapshot_id.clone()).collect(),
                views: f
                    .views
                    .iter()
                    .map(|v| ViewSummary {
                        id: redactor.redact_str(&v.id),
                        kind: v.kind.clone(),
                        complete: v.complete,
                        collector_complete: v.collector_complete,
                        records: u32::try_from(v.records.len()).unwrap_or(u32::MAX),
                    })
                    .collect(),
                checked: f.claims.is_some(),
                output_unsupported: f.claims.as_ref().is_some_and(|c| c.output_unsupported),
                problems: f
                    .claims
                    .as_ref()
                    .map(|c| c.problems.clone())
                    .unwrap_or_default(),
                claims,
            }
        })
        .collect()
}

fn claim_row(
    c: &ClaimVerdict,
    at: usize,
    texts: &[String],
    built: Option<&sources::Built>,
    redactor: &crate::run_redact::RunRedactor,
) -> ClaimRow {
    // The text by position, only while the status file's text still hashes
    // to the digest the check stored for this verdict.
    let text = texts
        .get(at)
        .filter(|t| {
            c.text_digest
                .as_deref()
                .is_some_and(|d| apb_core::content::sha256_hex(t.as_bytes()) == d)
        })
        .map(|t| redactor.redact_str(t))
        .unwrap_or_default();
    let cites = c
        .cites
        .iter()
        .map(|r| CitedFragment {
            r#ref: r.clone(),
            fragment: built.and_then(|b| cited_fragment(b, r)).unwrap_or_default(),
        })
        .collect();
    ClaimRow {
        id: c.id.clone(),
        verdict: c.verdict,
        wording: match c.verdict {
            Verdict::StructurallyValid => STRUCTURALLY_VALID_WORDING.to_string(),
            Verdict::Unsupported => UNSUPPORTED_WORDING.to_string(),
            Verdict::AbsenceOk => {
                "Absence: every view of these kinds was complete and read to its end.".to_string()
            }
            Verdict::NotEnoughData => {
                "Not enough data: a view of these kinds is not complete.".to_string()
            }
        },
        text,
        problems: c.problems.clone(),
        flags: c.flags.clone(),
        cites,
        absence: c.absence.clone(),
    }
}

/// The served fragment a stored citation (byte form) points at: its byte
/// range of the record's text, or the record's first characters.
fn cited_fragment(built: &sources::Built, stored: &str) -> Option<String> {
    let mut c: apb_core::result_store::Citation = stored.parse().ok()?;
    let range = c.range.take();
    let text = &built.views.record(&c.to_string())?.text;
    Some(match range {
        Some((a, b)) => text.get(a as usize..b as usize)?.to_string(),
        None => check::fragment(text, None),
    })
}
