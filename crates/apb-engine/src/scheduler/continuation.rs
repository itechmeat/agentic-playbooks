//! The prompt that continues an agent session after its attempt did not
//! finish (issue #136 items 2 and 3): the session already holds the step's
//! prompt, so only what happened is said.

/// How much of the last failure a continuation prompt quotes.
const CONTINUATION_REASON_MAX_BYTES: usize = 2048;

/// Why a session is being continued (issue #136 items 2 and 3).
pub(super) enum Continuation<'a> {
    /// The last attempt was stopped at its deadline.
    Deadline,
    /// The last attempt was cut short by a `--now` pause (issue #200).
    Paused,
    /// An attempt ended without recording a required verdict.
    Interrupted,
    /// The last attempt failed with this text (clipped when quoted).
    Failed(&'a str),
}

/// The prompt that continues a session after its attempt did not finish: the
/// session already holds the node prompt, so only what happened is said.
/// `verdict_file` adds the reminder to record the verdict in the new
/// attempt's status file (the interruption note already carries it).
pub(super) fn continuation_prompt(why: Continuation<'_>, verdict_file: bool) -> String {
    let mut out = match why {
        Continuation::Deadline => {
            "Your previous turn was stopped at its time limit before it finished. \
             Continue where you stopped; do not redo work that is already done."
                .to_string()
        }
        Continuation::Paused => "Your previous turn was stopped by a pause before it finished. \
             Continue where you stopped; do not redo work that is already done."
            .to_string(),
        Continuation::Interrupted => return super::status_file::INTERRUPTION_NOTE.to_string(),
        Continuation::Failed(reason) => {
            let reason = reason.trim();
            let mut cut = reason.len().min(CONTINUATION_REASON_MAX_BYTES);
            while !reason.is_char_boundary(cut) {
                cut -= 1;
            }
            format!(
                "Your previous attempt at this task did not succeed:\n\n{}\n\n\
                 Continue in this session: fix what is wrong and finish the task. \
                 Do not redo work that is already done.",
                &reason[..cut]
            )
        }
    };
    if verdict_file {
        out.push_str(
            " Record your final verdict in the status file named by APB_STATUS_FILE \
             (a new file for this attempt) before your turn ends.",
        );
    }
    out
}
