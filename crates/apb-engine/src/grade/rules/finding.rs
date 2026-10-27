//! What a rule concludes, and the facts several rules share: the
//! attempts whose tool calls cannot be seen, and the session records
//! (every one, or the main session's alone).

use apb_core::quality::grade::{Cite, Level};

use crate::grade::evidence::Proposal;
use crate::grade::sources::RunSources;

/// What one rule concluded.
pub(super) struct Finding {
    pub level: Level,
    pub reason_code: &'static str,
    pub why: String,
    pub cites: Vec<String>,
    /// The level rests on something not having happened.
    pub absence: bool,
}

impl Finding {
    pub fn new(level: Level, reason_code: &'static str, why: impl Into<String>) -> Self {
        Finding {
            level,
            reason_code,
            why: why.into(),
            cites: Vec::new(),
            absence: false,
        }
    }

    pub fn citing(mut self, cites: Vec<String>) -> Self {
        self.cites = cites;
        self
    }

    /// Nothing of the kind happened: `good`, resting on every view the
    /// criterion reads.
    pub fn none_seen(why: impl Into<String>) -> Self {
        Finding {
            absence: true,
            ..Finding::new(Level::Good, "no_signal", why)
        }
    }

    pub fn proposal(self, id: &str) -> Proposal {
        Proposal {
            criterion: id.to_string(),
            level: self.level,
            reason_code: self.reason_code.to_string(),
            why: self.why,
            cites: self.cites.into_iter().map(Cite::to).collect(),
            absence: self.absence,
        }
    }
}

/// The journal citation of event `seq`.
pub(super) fn j(seq: u64) -> String {
    format!("j:{seq}")
}

/// The attempts whose agent keeps no transcript apb can read for tool
/// calls (every agent but claude): what they did is not visible.
pub(super) fn unstructured(sources: &RunSources) -> usize {
    let structured: std::collections::BTreeSet<&str> = sources
        .transcripts
        .iter()
        .filter(|t| t.source.structured())
        .map(|t| t.source.stem.as_str())
        .collect();
    let all: std::collections::BTreeSet<&str> = sources
        .transcripts
        .iter()
        .map(|t| t.source.stem.as_str())
        .collect();
    all.difference(&structured).count()
}

/// Nothing of the kind was seen: `good` resting on every view, unless an
/// attempt's tool calls cannot be seen at all.
pub(super) fn nothing_seen(sources: &RunSources, why: &str) -> Finding {
    match unstructured(sources) {
        0 => Finding::none_seen(why),
        n => Finding::new(
            Level::NotEnoughData,
            "unstructured_transcript",
            format!("{n} attempt(s) left no transcript apb can read tool calls from"),
        ),
    }
}

/// Every session record, sub-agents' included: what a sub-agent did is
/// what the run did (`scope`, `consent`).
pub(super) fn session_records(
    sources: &RunSources,
) -> impl Iterator<Item = (&crate::transcript::View, &crate::transcript::Record)> {
    sources
        .transcripts
        .iter()
        .filter(|t| t.source.structured())
        .flat_map(|t| t.records.iter().map(move |r| (t, r)))
}

/// The main session's records, sub-agents left out: separate sub-agents
/// may make the same call each without going in circles (`loops`).
pub(super) fn main_session_records(
    sources: &RunSources,
) -> impl Iterator<Item = (&crate::transcript::View, &crate::transcript::Record)> {
    session_records(sources).filter(|(_, r)| !r.sidechain)
}
