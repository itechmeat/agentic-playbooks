//! `apb notify test`: one live message through the machine's notify target,
//! outside any run, so a person can check the setup before a run depends
//! on it. The target goes through the same check a start runs (it loads,
//! it can carry notifications, the connector and the account are
//! approved), and the message is filled through the function's `people:`
//! block like any notification. Nothing is journaled: no run owns it.

use std::path::Path;

use apb_core::notify::{NotifyRequest, NotifyTarget};

use super::notice::Notice;
use super::send::args_for;
use crate::connector::call::LiveTarget;

/// The subject of the test message.
pub const TRIAL_SUBJECT: &str = "apb test notification";

/// The body of the test message.
pub const TRIAL_TEXT: &str = "This is a test message from apb. Notifications about your runs will arrive here: a review or a question that waits for you, a pause you did not make, the end of a run.";

/// Sends the test message through the target the workspace at `root`
/// would notify a run through (the machine's, unless `target` names
/// another). The target it went to, or why it could not be sent.
pub fn send_trial(root: &Path, target: Option<NotifyTarget>) -> Result<NotifyTarget, String> {
    let request = NotifyRequest::On { target, on: None };
    let check = super::check(root, Some(&request));
    if let Some(why) = check.refusal {
        return Err(why);
    }
    let plan = check
        .plan
        .ok_or_else(|| "notifications are off for this workspace".to_string())?;
    let record = plan.target;
    let live = LiveTarget::load(
        root,
        &record.connector,
        Some(&record.account),
        &record.function,
    )
    .map_err(|e| e.message)?;
    let notice = Notice {
        subject: TRIAL_SUBJECT.to_string(),
        text: TRIAL_TEXT.to_string(),
    };
    let args = args_for(&live, &record, &notice)
        .ok_or_else(|| "the function does not name the args a notification fills".to_string())?;
    let (value, ok) = crate::connector::call::play_call(
        root,
        &record.connector,
        Some(&record.account),
        &record.function,
        &args,
        false,
        false,
    );
    if ok {
        return Ok(record.target());
    }
    Err(value["error"]["message"]
        .as_str()
        .unwrap_or("the connector call failed")
        .to_string())
}
