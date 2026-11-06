//! The stall exit (spec node-contracts D5; not the attempt-silence watch
//! of [`crate::stall`]): with `stall_after` set, a node
//! stops retrying once the same rejection (by its normalized fingerprint)
//! repeats that many times in a row inside its effect scope. The streak is
//! read from the journal, so a deferred retry or a resume keeps counting.

use crate::error::EngineError;
use crate::event;
use crate::exec_key::ExecKey;
use std::path::Path;

pub(crate) mod streak;
pub(crate) use streak::streak;

pub(crate) struct Assessed {
    pub fingerprint: String,
    pub stalled_at: Option<u32>,
}

/// Reads the journal, computes the fingerprint of `rejected_for` and the
/// trailing streak, and decides whether this rejection is the
/// `stall_after`-th identical one. An unreadable journal is an error, never
/// a silent streak of 0.
pub(crate) fn assess(
    run_dir: &Path,
    run_id: &str,
    key: ExecKey<'_>,
    rejected_for: &str,
    limit: Option<u32>,
) -> Result<Assessed, EngineError> {
    let events = event::read_all(run_dir)?;
    let fingerprint =
        apb_core::rejection::fingerprint(rejected_for, run_id, &run_dir.display().to_string());
    let count = streak(&events, key, &fingerprint);
    let stalled_at = limit.and_then(|limit| (count + 1 >= limit).then_some(count + 1));
    Ok(Assessed {
        fingerprint,
        stalled_at,
    })
}

/// The journal fields of one non-fatal rejection and the stall output. A
/// fatal rejection carries neither field and never stalls; it never calls
/// [`rejection`].
pub(crate) struct RejectionFields {
    /// The `rejected_for` text: the reason redacted and bounded.
    pub rejected_for: String,
    pub fingerprint: String,
    pub stalled: bool,
    /// The node output when this rejection reached `stall_after`.
    pub output: Option<String>,
}

/// One rejection at any of the four sites: `reason` is the reason the
/// attempt was rejected for, redacted into `rejected_for` here
/// ([`rejected_for_text`] over `workdir`); the stall output
/// names the raw reason.
pub(crate) fn rejection(
    reason: &str,
    run_dir: &Path,
    workdir: &Path,
    run_id: &str,
    key: ExecKey<'_>,
    limit: Option<u32>,
) -> Result<RejectionFields, EngineError> {
    let text = rejected_for_text(run_dir, workdir, reason);
    let assessed = assess(run_dir, run_id, key, &text, limit)?;
    Ok(RejectionFields {
        output: assessed.stalled_at.map(|n| output(n, reason)),
        stalled: assessed.stalled_at.is_some(),
        fingerprint: assessed.fingerprint,
        rejected_for: text,
    })
}

/// Why a rejected report failed its attempt, as `attempt_finished.rejected_for`
/// carries it and the retry briefing quotes it: scrubbed of the secret
/// values of the run's tree and the workspace's connectors, token shapes and
/// the workspace's and attempt directory's paths (the run redactor's text
/// that leaves the machine, since the retry prompt goes to a model
/// provider), then cut to 500 characters by `chars`, never a byte slice (a
/// cut in the middle of a code point would panic). The scrub comes before
/// the bound, so a cut never leaves a fragment of a value behind.
fn rejected_for_text(run_dir: &Path, workdir: &Path, reason: &str) -> String {
    let run = crate::run_redact::RunRedactor::for_run_dir(run_dir).redact_text(reason);
    apb_core::scrub::Scrubber::for_workspace(workdir, Vec::new())
        .scrub(&run)
        .chars()
        .take(500)
        .collect()
}

/// The node output of a stall: the reason follows the count.
pub(crate) fn output(count: u32, reason: &str) -> String {
    format!(
        "stalled: the same rejection came back {count} times in a row, so apb stopped retrying: {reason}"
    )
}
