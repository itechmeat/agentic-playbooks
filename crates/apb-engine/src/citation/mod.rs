//! Citations: what a claim about a run may rest on, checked against what a
//! bounded reader actually read.
//!
//! A citation ([`Cite`]) names one record of a view read whole (`j:<seq>`,
//! `o:<node>`, `s:<node>-<attempt>`, `t:<stem>/<stream>#<record>`),
//! optionally narrowed to a character range `[from, to)` of the record as
//! the reader showed it. A claim of absence ("this did not happen") cites
//! nothing and rests instead on every view of the kinds it reads: each
//! must be complete and read to its end, and at least one must exist.
//!
//! - [`views`]: the views a citation may point into and the [`ReadState`]
//!   that proves a record was read whole;
//! - [`rules`]: the structural rules of one citation.
//!
//! [`check`] is the one entry: the run grader's evidence validator
//! ([`crate::grade::evidence`]) and the check of a node's claims
//! ([`crate::evidence::check`]) go through it; [`reader`] is the one
//! bounded reader both read through. A view's kind is the grader's
//! [`apb_core::quality::criteria::ReadKind`] or an evidence session's own. It keeps references and
//! counts only, never source text.

pub mod reader;
pub mod rules;
pub mod views;

use apb_core::quality::grade::{Cite, ViewEnd};

pub use views::{ReadState, ViewSet};

/// What a claim rests on.
#[derive(Debug, Clone, Copy)]
pub struct Claim<'a> {
    pub cites: &'a [Cite],
    /// Set to claim that something did not happen.
    pub absence: bool,
    /// Whether the claim may rest on an absence at all.
    pub absence_allowed: bool,
}

/// The outcome of [`check`]: the problems found, in order, and for an
/// absence the views it rests on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Checked {
    pub problems: Vec<String>,
    pub absence_basis: Vec<ViewEnd>,
}

/// Checks `claim` for a criterion that reads `kinds` against the `views`
/// and what `state` says was read: every citation by [`rules`], and an
/// absence against every view of those kinds (complete and read to its
/// end, at least one). An incomplete view is not a problem here: it is
/// in the basis, and the caller decides what an absence over it is worth.
pub fn check<K: PartialEq>(
    claim: &Claim<'_>,
    kinds: &[K],
    views: &ViewSet<K>,
    state: &ReadState,
) -> Checked {
    let mut problems: Vec<String> = claim
        .cites
        .iter()
        .filter_map(|c| rules::cite_problem(c, kinds, views, state))
        .collect();
    let mut absence_basis = Vec::new();
    if claim.absence {
        if !claim.absence_allowed {
            problems.push("this criterion rests on what happened, not on an absence".into());
        }
        for v in views.of_kinds(kinds) {
            let ended = state.ended(v);
            if v.complete && !ended {
                problems.push(format!("an absence needs `{}` read to its end", v.id));
            }
            absence_basis.push(ViewEnd {
                view: v.id.clone(),
                complete: v.complete,
                ended,
            });
        }
        if absence_basis.is_empty() {
            // Nothing of the kinds the criterion reads exists: an absence
            // resting on no view rests on nothing.
            problems.push(
                "the run has no view this criterion reads: an absence needs one read to its end"
                    .into(),
            );
        }
    }
    Checked {
        problems,
        absence_basis,
    }
}
