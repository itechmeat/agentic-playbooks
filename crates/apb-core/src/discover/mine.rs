//! The repeated step sequences of a set of sessions (miner `ngram/1`).
//!
//! 1. Tokens are interned.
//! 2. Per session, the distinct contiguous n-grams for n from `min_n` to
//!    `max_n` (repeated steps were already collapsed into one).
//! 3. An n-gram is kept when it appears in at least `min_sessions`
//!    sessions written on at least `min_days` days.
//! 4. Only maximal patterns stay: an n-gram is dropped when a longer kept
//!    one contains it and appears in the same sessions.
//! 5. A pattern needs at least two distinct tokens and one that acts
//!    ([`super::token::is_acting`]).
//! 6. The score is the number of sessions times `n - 1`; ties go by slug.
//!
//! Deterministic: the same sessions always give the same patterns in the
//! same order. Memory is bounded by the steps read (the walk's
//! `max_total_steps`): one entry per distinct n-gram, holding a short list
//! of the sessions it appears in.

use std::collections::{BTreeSet, HashMap};

use super::session::Session;
use super::token;
use super::types::MineParams;

/// Where a pattern first appears in one session: step indexes, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    pub session: usize,
    pub start: usize,
    pub end: usize,
}

/// One repeated step sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    pub tokens: Vec<String>,
    /// `seq-` and 10 hex characters of the tokens.
    pub slug: String,
    /// The sessions it appears in, by index, ascending.
    pub sessions: Vec<usize>,
    pub days: u32,
    pub score: u32,
    /// Its first occurrence in each of its sessions.
    pub occurrences: Vec<Occurrence>,
}

/// The slug of a token sequence.
pub fn slug_of(tokens: &[String]) -> String {
    let digest = crate::content::sha256_hex(tokens.join(" ").as_bytes());
    let hex = digest.trim_start_matches("sha256:");
    format!("seq-{}", &hex[..10])
}

/// What one n-gram was seen as.
#[derive(Default)]
struct Seen {
    /// Each session it appears in, ascending, with its first start there.
    first: Vec<(usize, usize)>,
}

impl Seen {
    fn sessions(&self) -> impl Iterator<Item = usize> + '_ {
        self.first.iter().map(|(s, _)| *s)
    }
}

/// The repeated patterns of `sessions`, best first. Every pattern is
/// returned; the caller keeps the top ones it can offer.
pub fn mine(sessions: &[Session], p: &MineParams) -> Vec<Pattern> {
    let mut ids: HashMap<&str, u32> = HashMap::new();
    let mut names: Vec<&str> = Vec::new();
    let seqs: Vec<Vec<u32>> = sessions
        .iter()
        .map(|s| {
            s.steps
                .iter()
                .map(|st| {
                    *ids.entry(st.token.as_str()).or_insert_with(|| {
                        names.push(st.token.as_str());
                        names.len() as u32 - 1
                    })
                })
                .collect()
        })
        .collect();
    let acting: Vec<bool> = names.iter().map(|t| token::is_acting(t)).collect();
    let mut grams: HashMap<&[u32], Seen> = HashMap::new();
    // Sessions are visited in order, so a session already listed for a
    // gram is its last entry.
    for (si, seq) in seqs.iter().enumerate() {
        for n in p.min_n..=p.max_n.min(seq.len()) {
            for start in 0..=seq.len() - n {
                let seen = grams.entry(&seq[start..start + n]).or_default();
                if seen.first.last().is_none_or(|(s, _)| *s != si) {
                    seen.first.push((si, start));
                }
            }
        }
    }
    let kept: HashMap<&[u32], &Seen> = grams
        .iter()
        .filter(|(gram, seen)| {
            // The cheap checks first: most grams appear in one session.
            // Two distinct tokens: consecutive repeats are collapsed, so
            // any change between neighbours is enough.
            if seen.first.len() < p.min_sessions
                || !gram.windows(2).any(|w| w[0] != w[1])
                || !gram.iter().any(|t| acting[*t as usize])
            {
                return false;
            }
            let days: BTreeSet<i64> = seen.sessions().map(|s| sessions[s].day).collect();
            days.len() >= p.min_days
        })
        .map(|(g, s)| (*g, s))
        .collect();
    // A kept n-gram covers its shorter sub-grams that appear in the same
    // sessions: those are not maximal.
    let mut covered: BTreeSet<&[u32]> = BTreeSet::new();
    for (gram, seen) in &kept {
        for n in p.min_n..gram.len() {
            for start in 0..=gram.len() - n {
                let sub = &gram[start..start + n];
                if kept
                    .get(sub)
                    .is_some_and(|s| s.sessions().eq(seen.sessions()))
                {
                    covered.insert(sub);
                }
            }
        }
    }
    let mut out: Vec<Pattern> = kept
        .iter()
        .filter(|(gram, _)| !covered.contains(*gram))
        .map(|(gram, seen)| {
            let tokens: Vec<String> = gram
                .iter()
                .map(|t| names[*t as usize].to_string())
                .collect();
            let days: BTreeSet<i64> = seen.sessions().map(|s| sessions[s].day).collect();
            Pattern {
                slug: slug_of(&tokens),
                score: (seen.first.len() * (gram.len() - 1)) as u32,
                sessions: seen.sessions().collect(),
                days: days.len() as u32,
                occurrences: seen
                    .first
                    .iter()
                    .map(|(s, start)| Occurrence {
                        session: *s,
                        start: *start,
                        end: start + gram.len() - 1,
                    })
                    .collect(),
                tokens,
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.slug.cmp(&b.slug)));
    out
}
