//! The run report as one HTML file a person can keep, mail or open from
//! disk: the same outcome model `run_report` reports, in a page that needs
//! no server, no network and runs nothing.
//!
//! The page carries no script, no event-handler attribute, no `style`
//! attribute and no external resource. Its one stylesheet is inline and
//! pinned by hash in a Content Security Policy that comes before it. Every
//! string from the run is redacted like any text that leaves the machine
//! (whole, before any cut; a journal event field by field, before it is
//! serialised), then escaped through [`markup::Markup`], the only way text
//! reaches the page. The journal keeps its last [`JOURNAL_MAX_EVENTS`] events, each line
//! cut at [`JOURNAL_LINE_MAX_BYTES`], and the finish answer is cut at
//! [`ANSWER_MAX_BYTES`].

pub mod markup;
mod model;
mod quality;
mod sections;

use std::path::Path;

use crate::error::EngineError;
use crate::redact::Redactor;
use markup::Markup;
use model::Report;

pub use model::{ANSWER_MAX_BYTES, JOURNAL_LINE_MAX_BYTES, JOURNAL_MAX_EVENTS};

/// The report's stylesheet, inline and pinned by hash.
pub const STYLESHEET: &str = include_str!("report.css");

/// The report of the run at `run_dir` of the workspace at `root`.
pub fn render(root: &Path, run_dir: &Path) -> Result<String, EngineError> {
    let report = Report::load(root, run_dir)?;
    let w = sections::Writer::new(Redactor::for_workspace(root, Vec::new()));
    let title = match &report.playbook {
        Some(p) => format!("{p} run {}", report.run_id),
        None => format!("Run {}", report.run_id),
    };
    let body = Markup::el(
        "main",
        None,
        Markup::concat([
            sections::header(&w, &title, &report),
            sections::outcome(&w, &report.outcome),
            sections::goal(&w, report.goal.as_ref()),
            sections::steps(&w, &report.steps),
            sections::lists(&w, &report.outcome),
            quality::grades(&w, &report.grades, report.grades_left_out),
            quality::known_problems(&w, report.known_problems.as_ref()),
            sections::sends(&w, &report.sends),
            sections::notifications(&w, report.notify.as_ref()),
            sections::answer(&w, report.outcome.agent_summary.as_deref()),
            sections::journal(&w, &report.journal, report.journal_cut),
        ]),
    );
    Ok(format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{}\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"referrer\" content=\"no-referrer\">\n<title>{}</title>\n\
         <style>{STYLESHEET}</style>\n</head>\n<body>\n{}\n</body>\n</html>\n",
        markup::escape(&markup::csp(STYLESHEET)),
        w.text(&title).into_string(),
        body.into_string(),
    ))
}
