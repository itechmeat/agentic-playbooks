//! The report's quality sections: how the run was graded (the latest
//! trusted grade of each grader, one row per criterion) and the known
//! problems the run showed its agents and the ones it hid.

use crate::known_problems::RunKnownProblems;
use apb_core::quality::grade::{Cite, Grade};

use super::markup::Markup;
use super::sections::{Writer, empty, section};

/// Cites shown per criterion; the rest are counted.
pub const CITES_SHOWN: usize = 5;

/// A criterion's cites as plain refs: the first [`CITES_SHOWN`], then how
/// many more.
fn cites(cites: &[Cite]) -> String {
    let mut out = cites
        .iter()
        .take(CITES_SHOWN)
        .map(|c| c.r#ref.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if cites.len() > CITES_SHOWN {
        out.push_str(&format!(" and {} more", cites.len() - CITES_SHOWN));
    }
    out
}

/// One row per criterion of the latest trusted grade of each grader, and
/// how many grade files were left out as not signed for this run here.
pub(super) fn grades(w: &Writer, grades: &[Grade], left_out: usize) -> Markup {
    let note = if left_out > 0 {
        w.el(
            "p",
            Some("meta"),
            &format!(
                "{left_out} grade file{} not signed for this run on this machine left out.",
                if left_out == 1 { "" } else { "s" }
            ),
        )
    } else {
        Markup::default()
    };
    if grades.is_empty() {
        return section(
            "Quality",
            Markup::concat([empty("The run has no trusted grade."), note]),
        );
    }
    let rows = grades
        .iter()
        .flat_map(|g| {
            let grader = g.grader.to_string();
            g.criteria.iter().map(move |c| {
                vec![
                    c.id.clone(),
                    c.level.as_str().to_string(),
                    grader.clone(),
                    c.reason_code.clone(),
                    c.why.clone(),
                    cites(&c.cites),
                ]
            })
        })
        .collect();
    section(
        "Quality",
        Markup::concat([
            w.table(
                &["Criterion", "Level", "Grader", "Reason", "Why", "Cites"],
                rows,
            ),
            note,
        ]),
    )
}

/// The known problems the run showed its agents, and the hidden ones with
/// why.
pub(super) fn known_problems(w: &Writer, known: Option<&RunKnownProblems>) -> Markup {
    let Some(k) = known else {
        return section(
            "Known problems shown",
            empty("The run showed no known problem."),
        );
    };
    let shown = if k.shown.is_empty() {
        empty("No known problem was shown.")
    } else {
        let rows = k
            .shown
            .iter()
            .map(|p| {
                vec![
                    p.scope.clone(),
                    p.symptom.clone(),
                    p.workaround.clone().unwrap_or_default(),
                    if p.nodes.is_empty() {
                        "every agent node".to_string()
                    } else {
                        p.nodes.join(", ")
                    },
                    p.basis.clone(),
                ]
            })
            .collect();
        w.table(&["Scope", "Symptom", "Workaround", "Nodes", "Basis"], rows)
    };
    let hidden = if k.hidden.is_empty() {
        Markup::default()
    } else {
        let rows = k
            .hidden
            .iter()
            .map(|p| vec![p.id.as_str(), p.reason.as_str()])
            .collect();
        Markup::concat([
            w.el("h3", None, "Hidden"),
            w.table(&["Problem", "Why"], rows),
        ])
    };
    let unsigned = if k.unsigned > 0 {
        w.el(
            "p",
            Some("meta"),
            &format!(
                "{} problem{} from a file this installation did not write left out.",
                k.unsigned,
                if k.unsigned == 1 { "" } else { "s" }
            ),
        )
    } else {
        Markup::default()
    };
    section(
        "Known problems shown",
        Markup::concat([shown, hidden, unsigned]),
    )
}
