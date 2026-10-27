//! The recheck rule: whether a known problem still holds after its source
//! changed. Pure: whoever proposes (a decision model, a host, a person),
//! [`settle`] decides what the proposal counts as.
//!
//! `closed` is accepted only with a quote that is 12 to 400 characters, has
//! no hidden character, is found exactly in the new text (line endings
//! normalized) and is absent from the old one: a fix is shown by what the
//! new source says, never by what it no longer says. Anything short of that
//! is `undetermined`, for a person to look at. `holds` needs no quote. A
//! problem whose old text was too long to keep cannot show a quote is new:
//! only a person closes it, on their word, with text the new source has.
//!
//! A model answer counts by its mode: `shadow` changes nothing (the problem
//! stays undetermined), `advise` applies `holds` and turns `closed` into
//! `closed_proposed`, `enforce` applies both. A host's close is a proposal
//! too; only a person's close applies on its own.

use serde::{Deserialize, Serialize};

use super::{Quote, RecheckBasis, RecheckRecord, RecheckVerdict};
use crate::decisions::DecisionMode;

/// The shortest quote a close may rest on.
pub const QUOTE_MIN: usize = 12;
/// The longest quote a close may rest on.
pub const QUOTE_MAX: usize = 400;
/// The most changed passages offered to a model (one choice option each,
/// plus `none`).
pub const MAX_PASSAGES: usize = 254;
/// The most lines of a new text that are compared with the old one.
const MAX_DIFF_LINES: usize = 4000;

/// The reason code of a person's close whose old source text is unknown
/// (too long to keep): taken on the person's word.
pub const PERSON_WORD: &str = "person_word";

/// What a proposer says about a problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposedVerdict {
    Holds,
    Closed,
    Undetermined,
}

/// A proposal to settle: the verdict, the quote a close rests on, and a
/// short reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecheckProposal {
    pub verdict: ProposedVerdict,
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub why: String,
}

/// Who proposed, against which source digest, when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckMeta {
    pub digest: String,
    pub basis: RecheckBasis,
    /// A model answer's mode; `None` for a host or a person.
    pub mode: Option<DecisionMode>,
    pub by: String,
    pub at_ms: u64,
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// The 1-based first and last line of `needle` in `text`, both normalized.
fn lines_of(text: &str, needle: &str) -> Option<[u32; 2]> {
    let at = text.find(needle)?;
    let first = text[..at].matches('\n').count() as u32 + 1;
    let last = first + needle.trim_end_matches('\n').matches('\n').count() as u32;
    Some([first, last])
}

/// Why `quote` cannot be a close's quote by its shape alone, if it cannot:
/// `quote_length` outside [`QUOTE_MIN`] to [`QUOTE_MAX`] characters, or
/// `quote_hidden_character`.
pub fn quote_shape(quote: &str) -> Result<(), &'static str> {
    let n = quote.chars().count();
    if !(QUOTE_MIN..=QUOTE_MAX).contains(&n) {
        return Err("quote_length");
    }
    if crate::text::has_hidden(&quote.replace(['\n', '\t'], " ")) {
        return Err("quote_hidden_character");
    }
    Ok(())
}

/// Why `quote` cannot close the problem, if it cannot.
fn quote_problem(quote: &str, old: Option<&str>, new: &str) -> Result<[u32; 2], &'static str> {
    quote_shape(quote)?;
    let lines = lines_of(new, quote).ok_or("quote_not_found")?;
    match old {
        None => Err("old_source_unknown"),
        Some(old) if old.contains(quote) => Err("quote_not_new"),
        Some(_) => Ok(lines),
    }
}

/// Settles `p` against the old and the new source text.
pub fn settle(
    p: &RecheckProposal,
    old_excerpt: Option<&str>,
    new_text: &str,
    meta: CheckMeta,
) -> RecheckRecord {
    let (verdict, quote, reason_code) = match p.verdict {
        ProposedVerdict::Holds => (RecheckVerdict::Holds, None, "holds"),
        ProposedVerdict::Undetermined => (RecheckVerdict::Undetermined, None, "undetermined"),
        ProposedVerdict::Closed => {
            let new = normalize(new_text);
            let old = old_excerpt.map(normalize);
            let text = p.quote.as_deref().map(normalize).unwrap_or_default();
            match quote_problem(&text, old.as_deref(), &new) {
                Ok(lines) => (
                    RecheckVerdict::Closed,
                    Some(Quote { text, lines }),
                    "quote_new",
                ),
                // An old text too long to keep cannot show the quote is
                // new: only a person's close stands, on their word, and
                // still only on text the new source has.
                Err("old_source_unknown") if meta.basis == RecheckBasis::Person => {
                    match lines_of(&new, &text) {
                        Some(lines) => (
                            RecheckVerdict::Closed,
                            Some(Quote { text, lines }),
                            PERSON_WORD,
                        ),
                        None => (RecheckVerdict::Undetermined, None, "quote_not_found"),
                    }
                }
                Err(code) => (RecheckVerdict::Undetermined, None, code),
            }
        }
    };
    // A model answer without a mode counts as `shadow`: the safe default.
    let (verdict, reason_code) = match (verdict, meta.basis, meta.mode) {
        (_, RecheckBasis::Model, None | Some(DecisionMode::Shadow | DecisionMode::Off)) => {
            (RecheckVerdict::Undetermined, "shadow")
        }
        (RecheckVerdict::Closed, RecheckBasis::Model, Some(DecisionMode::Advise))
        | (RecheckVerdict::Closed, RecheckBasis::Host, _) => {
            (RecheckVerdict::ClosedProposed, reason_code)
        }
        (v, _, _) => (v, reason_code),
    };
    RecheckRecord {
        digest: meta.digest,
        verdict,
        basis: meta.basis,
        quote: quote.filter(|_| {
            matches!(
                verdict,
                RecheckVerdict::Closed | RecheckVerdict::ClosedProposed
            )
        }),
        reason_code: reason_code.to_string(),
        mode: meta.mode,
        at_ms: meta.at_ms,
        by: meta.by,
    }
}

/// A run of new lines the old text does not have, numbered from 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passage {
    pub index: usize,
    /// 1-based, inclusive, in the new text.
    pub lines: [u32; 2],
    pub text: String,
}

/// The passages of `new` that `old` does not have: a line diff (longest
/// common subsequence), the added lines grouped into consecutive runs, each
/// cut to at most [`QUOTE_MAX`] characters (a cut passage is still a
/// substring of the new text), at most [`MAX_PASSAGES`]. Empty without an
/// old text: nothing can be shown to be new.
pub fn changed_passages(old: Option<&str>, new: &str) -> Vec<Passage> {
    let Some(old) = old else {
        return Vec::new();
    };
    let old = normalize(old);
    let new = normalize(new);
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().take(MAX_DIFF_LINES).collect();
    let added = added_lines(&a, &b);
    let mut out: Vec<Passage> = Vec::new();
    let mut i = 0;
    while i < b.len() && out.len() < MAX_PASSAGES {
        if !added[i] {
            i += 1;
            continue;
        }
        let start = i;
        let mut text = String::new();
        while i < b.len() && added[i] {
            let next = if text.is_empty() {
                b[i].to_string()
            } else {
                format!("{text}\n{}", b[i])
            };
            if next.chars().count() > QUOTE_MAX {
                if text.is_empty() {
                    text = b[i].chars().take(QUOTE_MAX).collect();
                    i += 1;
                }
                break;
            }
            text = next;
            i += 1;
        }
        if text.trim().is_empty() {
            continue;
        }
        out.push(Passage {
            index: out.len() + 1,
            lines: [start as u32 + 1, i as u32],
            text,
        });
    }
    out
}

/// Which lines of `b` are not in a longest common subsequence with `a`. The
/// common head and tail are matched first, so the table covers only the
/// changed middle (one flat row-major table).
fn added_lines(a: &[&str], b: &[&str]) -> Vec<bool> {
    let mut added = vec![true; b.len()];
    let head = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    for i in (0..head).chain(b.len() - tail..b.len()) {
        added[i] = false;
    }
    let (a, b_mid) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let (n, m) = (a.len(), b_mid.len());
    let w = m + 1;
    let mut lcs = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * w + j] = if a[i] == b_mid[j] {
                lcs[(i + 1) * w + j + 1] + 1
            } else {
                lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b_mid[j] {
            added[head + j] = false;
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * w + j] >= lcs[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "name: update_contact\ndescription: Update a contact\n";
    const NEW: &str = "name: update_contact\ndescription: Update a contact\nretry: { on: rate_limited, wait_seconds: 60 }\n";

    fn meta(basis: RecheckBasis, mode: Option<DecisionMode>) -> CheckMeta {
        CheckMeta {
            digest: "sha256:new".into(),
            basis,
            mode,
            by: "test".into(),
            at_ms: 1,
        }
    }

    fn close(quote: &str) -> RecheckProposal {
        RecheckProposal {
            verdict: ProposedVerdict::Closed,
            quote: Some(quote.into()),
            why: String::new(),
        }
    }

    #[test]
    fn a_new_quote_closes_and_names_its_lines() {
        let r = settle(
            &close("retry: { on: rate_limited"),
            Some(OLD),
            NEW,
            meta(RecheckBasis::Model, Some(DecisionMode::Enforce)),
        );
        assert_eq!(r.verdict, RecheckVerdict::Closed);
        assert_eq!(r.quote.unwrap().lines, [3, 3]);
    }

    #[test]
    fn a_close_without_a_new_found_quote_is_undetermined() {
        let m = || meta(RecheckBasis::Person, None);
        for (p, old, code) in [
            (close("wait_seconds: 90 here"), Some(OLD), "quote_not_found"),
            (close("Update a contact"), Some(OLD), "quote_not_new"),
            (close("retry"), Some(OLD), "quote_length"),
        ] {
            let r = settle(&p, old, NEW, m());
            assert_eq!(r.verdict, RecheckVerdict::Undetermined, "{code}");
            assert_eq!(r.reason_code, code);
            assert!(r.quote.is_none());
        }
    }

    #[test]
    fn an_unknown_old_text_closes_only_on_a_persons_word() {
        let q = close("retry: { on: rate_limited");
        for basis in [RecheckBasis::Model, RecheckBasis::Host] {
            let mode = (basis == RecheckBasis::Model).then_some(DecisionMode::Enforce);
            let r = settle(&q, None, NEW, meta(basis, mode));
            assert_eq!(r.verdict, RecheckVerdict::Undetermined);
            assert_eq!(r.reason_code, "old_source_unknown");
        }
        let person = settle(&q, None, NEW, meta(RecheckBasis::Person, None));
        assert_eq!(person.verdict, RecheckVerdict::Closed);
        assert_eq!(person.reason_code, PERSON_WORD);
        assert_eq!(person.quote.unwrap().lines, [3, 3]);
        // Still only on text the new source has.
        let missing = settle(
            &close("wait_seconds: 90 here"),
            None,
            NEW,
            meta(RecheckBasis::Person, None),
        );
        assert_eq!(
            (missing.verdict, missing.reason_code.as_str()),
            (RecheckVerdict::Undetermined, "quote_not_found")
        );
    }

    #[test]
    fn the_mode_decides_what_a_model_answer_counts_as() {
        let q = close("retry: { on: rate_limited");
        let advise = settle(
            &q,
            Some(OLD),
            NEW,
            meta(RecheckBasis::Model, Some(DecisionMode::Advise)),
        );
        assert_eq!(advise.verdict, RecheckVerdict::ClosedProposed);
        assert!(advise.quote.is_some());
        let shadow = settle(
            &q,
            Some(OLD),
            NEW,
            meta(RecheckBasis::Model, Some(DecisionMode::Shadow)),
        );
        assert_eq!(shadow.verdict, RecheckVerdict::Undetermined);
        let holds = RecheckProposal {
            verdict: ProposedVerdict::Holds,
            quote: None,
            why: String::new(),
        };
        let r = settle(
            &holds,
            Some(OLD),
            NEW,
            meta(RecheckBasis::Model, Some(DecisionMode::Advise)),
        );
        assert_eq!(r.verdict, RecheckVerdict::Holds);
        let host = settle(&q, Some(OLD), NEW, meta(RecheckBasis::Host, None));
        assert_eq!(host.verdict, RecheckVerdict::ClosedProposed);
        // A model answer without a mode changes nothing.
        let no_mode = settle(&q, Some(OLD), NEW, meta(RecheckBasis::Model, None));
        assert_eq!(
            (no_mode.verdict, no_mode.reason_code.as_str()),
            (RecheckVerdict::Undetermined, "shadow")
        );
    }

    #[test]
    fn quote_rules_hold_at_their_edges() {
        let person = || meta(RecheckBasis::Person, None);
        // A hidden character refuses the quote.
        let hidden = settle(
            &close("retry: { on:\u{202e} rate_limited"),
            Some(OLD),
            "retry: { on:\u{202e} rate_limited\n",
            person(),
        );
        assert_eq!(hidden.reason_code, "quote_hidden_character");
        // CRLF line endings on either side are the same text.
        let crlf = settle(
            &close("retry: { on: rate_limited"),
            Some(&OLD.replace('\n', "\r\n")),
            &NEW.replace('\n', "\r\n"),
            person(),
        );
        assert_eq!(crlf.verdict, RecheckVerdict::Closed);
        // 400 characters stand, 401 do not.
        let line = "y".repeat(QUOTE_MAX + 1);
        let new = format!("{OLD}{line}\n");
        let at_max = settle(&close(&line[..QUOTE_MAX]), Some(OLD), &new, person());
        assert_eq!(at_max.verdict, RecheckVerdict::Closed);
        let over = settle(&close(&line), Some(OLD), &new, person());
        assert_eq!(over.reason_code, "quote_length");
    }

    #[test]
    fn the_diff_matches_a_head_and_tail_and_finds_the_middle() {
        let old = "h1\nh2\nmid old\nt1\nt2\n";
        let new = "h1\nh2\nmid new\nmid more\nt1\nt2\n";
        let p = changed_passages(Some(old), new);
        assert_eq!(p.len(), 1);
        assert_eq!(
            (p[0].text.as_str(), p[0].lines),
            ("mid new\nmid more", [3, 4])
        );
        // Only removed lines: nothing new.
        assert!(changed_passages(Some(new), "h1\nh2\nt1\nt2\n").is_empty());
    }

    #[test]
    fn passages_are_the_added_runs_of_lines() {
        let old = "a line one\nb line two\nc line three\n";
        let new = "a line one\nnew first\nnew second\nb line two\nc line three\nnew tail\n";
        let p = changed_passages(Some(old), new);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].text, "new first\nnew second");
        assert_eq!(p[0].lines, [2, 3]);
        assert_eq!(p[1].text, "new tail");
        assert!(changed_passages(None, new).is_empty());
        let long = format!("{}\n", "x".repeat(QUOTE_MAX + 50));
        let cut = changed_passages(Some(old), &long);
        assert_eq!(cut[0].text.chars().count(), QUOTE_MAX);
        assert!(long.contains(&cut[0].text));
    }
}
