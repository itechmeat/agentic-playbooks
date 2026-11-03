//! The check of an attempt's claims against its evidence session, at the
//! end of a succeeded attempt and before the node's success decision.
//!
//! The agent writes `outputs.claims` in its status file:
//! `[{"id", "text", "cites": [{"ref", "span"?}], "absence": {"kinds"}?}]`.
//! Spans are character offsets into the record text as served. Each claim
//! gets one verdict through the shared citation rules (`crate::citation`):
//!
//! - `structurally_valid`: every cited record is a record of this session,
//!   of an allowed kind, returned whole to this attempt, and every span is
//!   non-empty and inside its record;
//! - `absence_ok`: a claim of absence whose views (every view of its kinds)
//!   are complete and were read to their end;
//! - `not_enough_data`: such a claim over a view that is not complete;
//! - `unsupported`: anything else, with its problems.
//!
//! A structurally valid claim may still be weak: deterministic flags
//! ([`Flag`]) mark what is worth a look. What is stored is references,
//! counts, verdicts, problems and flags; never source or claim text.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use apb_core::quality::grade::Cite;
use apb_core::records;
use apb_core::result_store::Citation;
use apb_core::schema::{ClaimsRequirement, Evidence, OnInvalidClaims};
use serde_json::Value;

use super::session::{self, ClaimVerdict, ClaimsOutcome, Flag, SessionFile, Verdict};
use super::sources::Built;
use crate::citation::{self, Checked, Claim};
use crate::error::EngineError;
use crate::run_redact::RunRedactor;

/// A cited fragment is the span, or this many characters of the record
/// without one.
pub const FRAGMENT_CHARS: usize = 300;
/// A span shorter than this is flagged `short_span`.
pub const SHORT_SPAN_CHARS: u64 = 16;
/// A record cited by more claims than this is flagged `shared_record`.
pub const SHARED_RECORD_CLAIMS: usize = 5;
/// The shortest word `no_shared_words` compares.
pub const MIN_WORD_LETTERS: usize = 4;
/// The most claims one attempt's check reads.
pub const MAX_CLAIMS: usize = 200;
/// The most cites one claim may carry; more are a claim problem.
pub const MAX_CITES_PER_CLAIM: usize = 32;
/// The most kinds one claim's absence may name.
pub const MAX_ABSENCE_KINDS: usize = 16;
/// The most problems one claim's verdict keeps.
pub const MAX_PROBLEMS_PER_CLAIM: usize = 16;
/// The longest claim id kept.
const MAX_ID_CHARS: usize = 64;
/// The longest reference a problem repeats.
const MAX_REF_CHARS: usize = 300;

/// One claim as the agent wrote it.
struct Written {
    id: String,
    text: String,
    cites: Vec<Cite>,
    absence: Option<Vec<String>>,
    problem: Option<String>,
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The claims of `outputs.claims`, or the problem of the list itself.
fn written(outputs: Option<&Value>) -> Result<Vec<Written>, String> {
    let Some(list) = outputs.and_then(|o| o.get("claims")) else {
        return Ok(Vec::new());
    };
    let Some(items) = list.as_array() else {
        return Err("`outputs.claims` is not a list".into());
    };
    if items.len() > MAX_CLAIMS {
        return Err(format!("more than {MAX_CLAIMS} claims"));
    }
    let mut claims: Vec<Written> = items
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let id = c
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(|s| clip(s, MAX_ID_CHARS))
                .unwrap_or_else(|| format!("#{}", i + 1));
            let text = c.get("text").and_then(Value::as_str).unwrap_or_default();
            let cites: Option<Vec<Cite>> = match c.get("cites") {
                None => Some(Vec::new()),
                Some(v) => serde_json::from_value(v.clone()).ok(),
            };
            let mut absence = match c.get("absence") {
                None | Some(Value::Null) => Ok(None),
                Some(a) => a
                    .get("kinds")
                    .and_then(|k| serde_json::from_value::<Vec<String>>(k.clone()).ok())
                    .map(Some)
                    .ok_or(()),
            };
            let mut problem = match (&cites, &absence) {
                (None, _) => Some("`cites` is not a list of {\"ref\", \"span\"?}".to_string()),
                (_, Err(())) => Some("`absence` is not {\"kinds\": [...]}".to_string()),
                _ if text.trim().is_empty() => Some("the claim has no text".to_string()),
                _ => None,
            };
            // The absence kinds are stored in the session file: each is
            // clipped to the kind grammar's length first, then only kept
            // when it is a slug — anything else is a claim problem, and no
            // raw value is ever stored or quoted.
            if let Ok(Some(kinds)) = &mut absence {
                for k in kinds.iter_mut() {
                    *k = clip(k, records::MAX_KIND_LEN);
                }
                if kinds.len() > MAX_ABSENCE_KINDS {
                    problem = Some(format!("more than {MAX_ABSENCE_KINDS} absence kinds"));
                } else if let Some(at) = kinds.iter().position(|k| !records::is_kind(k)) {
                    problem = Some(format!(
                        "absence kinds entry {} is not a slug ([a-z0-9][a-z0-9_-]*)",
                        at + 1
                    ));
                }
                if problem.is_some() {
                    // The kinds failed their check: the verdict keeps the
                    // problem, never the kinds themselves.
                    absence = Ok(None);
                }
            }
            if let Some(cites) = &cites
                && cites.len() > MAX_CITES_PER_CLAIM
            {
                problem = Some(format!("more than {MAX_CITES_PER_CLAIM} cites"));
            }
            Written {
                id,
                text: text.to_string(),
                cites: cites.unwrap_or_default(),
                absence: absence.unwrap_or(None),
                problem,
            }
        })
        .collect();
    // A repeated id (after clipping) stops being an address: the later
    // claims carrying it are unsupported, so no surface can show one
    // claim's text under another claim's verdict.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for c in &mut claims {
        if !seen.insert(c.id.clone()) && c.problem.is_none() {
            c.problem = Some(format!("duplicate claim id `{}`: ids must be unique", c.id));
        }
    }
    Ok(claims)
}

/// Lowercased words of at least [`MIN_WORD_LETTERS`] letters.
fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|w| w.chars().count() >= MIN_WORD_LETTERS)
        .map(|w| w.chars().flat_map(char::to_lowercase).collect())
        .collect()
}

/// The fragment a citation shows: its span, or the first
/// [`FRAGMENT_CHARS`] characters of the record.
pub fn fragment(text: &str, span: Option<[u64; 2]>) -> String {
    match span {
        Some([a, b]) => text
            .chars()
            .skip(a as usize)
            .take(b.saturating_sub(a) as usize)
            .collect(),
        None => text.chars().take(FRAGMENT_CHARS).collect(),
    }
}

/// `cite` in the byte form, its character span turned into a byte range of
/// the record's text.
fn byte_form(cite: &Cite, text: &str) -> String {
    let Ok(mut c) = cite.r#ref.parse::<Citation>() else {
        return cite.r#ref.clone();
    };
    c.range = cite.span.map(|[a, b]| {
        let at = |n: u64| {
            text.char_indices()
                .nth(n as usize)
                .map_or(text.len(), |(i, _)| i) as u64
        };
        (at(a), at(b))
    });
    c.to_string()
}

/// Checks the claims of `outputs` (the status file's `outputs` object)
/// against the session `file` and its views `built`; `redactor` scrubs what
/// is kept of the agent's own text (ids, references in problems).
pub fn check(
    file: &SessionFile,
    built: &Built,
    spec: &Evidence,
    outputs: Option<&Value>,
    redactor: &RunRedactor,
) -> ClaimsOutcome {
    let kept = |s: &str| redactor.redact_str(&clip(s, MAX_REF_CHARS));
    let mut out = ClaimsOutcome {
        output_unsupported: false,
        problems: Vec::new(),
        claims: Vec::new(),
    };
    let claims = match written(outputs) {
        Ok(c) => c,
        Err(p) => {
            out.problems.push(p);
            out.output_unsupported = true;
            return out;
        }
    };
    if claims.is_empty() && spec.claims == ClaimsRequirement::Required {
        out.output_unsupported = true;
        out.problems
            .push("claims are required and none were written in `outputs.claims`".into());
    }
    let allowed: Vec<String> = file
        .views
        .iter()
        .map(|v| v.kind.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut cited_by: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &claims {
        for r in c
            .cites
            .iter()
            .map(|c| c.r#ref.as_str())
            .collect::<BTreeSet<_>>()
        {
            *cited_by.entry(r).or_default() += 1;
        }
    }
    for c in &claims {
        let kinds = c.absence.clone().unwrap_or_else(|| allowed.clone());
        let Checked {
            mut problems,
            absence_basis,
        } = citation::check(
            &Claim {
                cites: &c.cites,
                absence: c.absence.is_some(),
                absence_allowed: true,
            },
            &kinds,
            &built.views,
            &file.state,
        );
        if let Some(p) = &c.problem {
            problems.insert(0, p.clone());
        }
        if c.absence.is_none() && c.cites.is_empty() && c.problem.is_none() {
            problems.push("the claim cites no record and claims no absence".into());
        }
        problems.dedup();
        problems.truncate(MAX_PROBLEMS_PER_CLAIM);
        let verdict = match (problems.is_empty(), c.absence.is_some()) {
            (false, _) => Verdict::Unsupported,
            (true, true) if absence_basis.iter().any(|v| !v.complete) => {
                problems.push("incomplete_view: a view of these kinds is not complete".into());
                Verdict::NotEnoughData
            }
            (true, true) => Verdict::AbsenceOk,
            (true, false) => Verdict::StructurallyValid,
        };
        let mut flags = BTreeSet::new();
        let mut cites = Vec::new();
        if verdict == Verdict::StructurallyValid {
            let claim_words = words(&c.text);
            for cite in &c.cites {
                let text = built
                    .views
                    .record(&cite.r#ref)
                    .map(|r| r.text.as_str())
                    .unwrap_or_default();
                match cite.span {
                    None => {
                        flags.insert(Flag::WholeRecord);
                    }
                    Some([a, b]) if b - a < SHORT_SPAN_CHARS => {
                        flags.insert(Flag::ShortSpan);
                    }
                    Some(_) => {}
                }
                if built.node_of(&cite.r#ref) == Some(file.node.as_str()) {
                    flags.insert(Flag::SelfCitation);
                }
                if cited_by.get(cite.r#ref.as_str()).copied().unwrap_or(0) > SHARED_RECORD_CLAIMS {
                    flags.insert(Flag::SharedRecord);
                }
                if claim_words.is_disjoint(&words(&fragment(text, cite.span))) {
                    flags.insert(Flag::NoSharedWords);
                }
                cites.push(byte_form(cite, text));
            }
        }
        out.claims.push(ClaimVerdict {
            id: redactor.redact_str(&c.id),
            verdict,
            problems: problems.iter().map(|p| kept(p)).collect(),
            cites,
            flags: flags.into_iter().collect(),
            // Only kinds that passed the grammar check (clipped, from
            // `written`) and survive the redactor unchanged are stored.
            absence: c
                .absence
                .as_ref()
                .map(|kinds| {
                    let kept: Vec<String> = kinds.iter().map(|k| redactor.redact_str(k)).collect();
                    (kept.iter().all(|k| records::is_kind(k))).then_some(kept)
                })
                .unwrap_or(None),
            // The digest of the claim text as checked: a surface shows the
            // status file's text only while it still hashes to this, since
            // the status file stays writable after the check.
            text_digest: Some(apb_core::content::sha256_hex(c.text.as_bytes())),
        });
    }
    out
}

/// The status file's `outputs` object of the attempt `file` is bound to.
fn status_outputs(run_dir: &Path, file: &SessionFile) -> Option<Value> {
    let path = run_dir
        .join("agent-status")
        .join(format!("{}.json", file.stem));
    let raw = std::fs::read(path).ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    v.get("outputs").cloned()
}

/// The check at the end of a succeeded attempt of session `session`: the
/// verdicts are stored in the session file, and under `on_invalid: fail`
/// an invalid outcome is the rejection note that fails the attempt.
pub fn at_success(
    run_dir: &Path,
    session: &str,
    spec: &Evidence,
) -> Result<Option<String>, EngineError> {
    let file = session::load(run_dir, session)?;
    let built = session::rebuild(run_dir, &file)?;
    let redactor = RunRedactor::for_run_dir(run_dir);
    let outcome = check(
        &file,
        &built,
        spec,
        status_outputs(run_dir, &file).as_ref(),
        &redactor,
    );
    let reject = (spec.on_invalid == OnInvalidClaims::Fail && outcome.invalid())
        .then(|| outcome.rejection_note());
    session::store_claims(run_dir, session, outcome)?;
    Ok(reject)
}
