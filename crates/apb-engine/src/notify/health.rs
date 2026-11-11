//! Sending a health notice (routine health D2): the machine's notify
//! target as it is at record time, pinned by its digests, and one call
//! whose outcome settles the notice ledger as the send ledger would.

use std::path::Path;

use apb_core::notify::{NotifyRequest, Transition};
use apb_core::policy_file::PlaybookKey;
use serde_json::Value;

use super::notice::Notice;
use super::record::NotifyTargetRecord;

/// What one send did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    NotSent(String),
    Unknown(String),
}

/// The target a health notice of `playbook` goes to: the plan of the
/// machine's target (the request a test message makes, so `notify.default`
/// does not matter), when its `on` holds `health`, the `notify.playbooks`
/// filters pass the playbook and the project's notify config is not
/// `enabled: false`. The record carries the digests that pin it.
pub fn health_target(root: &Path, playbook: &PlaybookKey<'_>) -> Option<NotifyTargetRecord> {
    let request = NotifyRequest::On {
        target: None,
        on: None,
        finished: None,
    };
    let plan = super::check(root, Some(playbook), Some(&request)).plan?;
    if !plan.on.contains(&Transition::Health) {
        return None;
    }
    let policies = apb_core::policy_file::Policies::load(root).ok()?;
    if !policies.covers(playbook) {
        return None;
    }
    let project = apb_core::config::project_notify(root).ok()?;
    if project.enabled == Some(false) {
        return None;
    }
    Some(plan.target)
}

/// Sends `notice` to `target`, refusing a target that changed since the
/// notice was recorded.
pub fn send(root: &Path, target: &NotifyTargetRecord, notice: &Notice) -> SendOutcome {
    let live = match crate::connector::call::LiveTarget::load(
        root,
        &target.connector,
        Some(&target.account),
        &target.function,
    ) {
        Ok(live) => live,
        Err(e) => return SendOutcome::NotSent(e.message),
    };
    let Some(args) = super::send::args_for(&live, target, notice) else {
        return SendOutcome::NotSent(
            "the function does not name the args a notification fills".into(),
        );
    };
    let pins = crate::connector::call::TargetPins {
        connector: target.connector_digest.clone(),
        account: target.account_digest.clone(),
    };
    let (value, ok) = crate::connector::call::play_call_pinned(
        root,
        &target.connector,
        &target.account,
        &target.function,
        &args,
        &pins,
        super::send::SEND_TIMEOUT_SECS,
    );
    classify(&value, ok)
}

/// The outcome of one call's answer, as the send ledger settles it.
pub(crate) fn classify(value: &Value, ok: bool) -> SendOutcome {
    use crate::connector::effect::{CallClass, CallFacts};
    let code = value["error"]["code"].as_str().unwrap_or("error");
    let outcome = if ok { "ok" } else { code };
    let http_status = value["status"]
        .as_u64()
        .or_else(|| value["error"]["http_status"].as_u64())
        .and_then(|s| u16::try_from(s).ok());
    let truncated = value["truncated"].as_bool() == Some(true)
        || code == crate::connector::result::CallErrorCode::IncompleteResponse.as_str();
    let facts = CallFacts {
        outcome,
        http_status,
        response_complete: truncated.then_some(false),
    };
    let why = || {
        value["error"]["message"]
            .as_str()
            .unwrap_or(code)
            .chars()
            .take(200)
            .collect::<String>()
    };
    if !ok && code == crate::connector::result::CallErrorCode::Permission.as_str() {
        return SendOutcome::NotSent(why());
    }
    match crate::connector::effect::classify(facts, false) {
        CallClass::WriteOk | CallClass::WriteIncomplete => SendOutcome::Sent,
        CallClass::WriteUnknown => SendOutcome::Unknown(why()),
        _ => SendOutcome::NotSent(why()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_maps_each_outcome() {
        let cases = [
            (json!({"ok": true, "status": 200}), true, "sent"),
            (
                json!({"ok": true, "status": 200, "truncated": true}),
                true,
                "sent",
            ),
            (
                json!({"ok": false, "error": {"code": "timeout", "message": "t"}}),
                false,
                "unknown",
            ),
            (
                json!({"ok": false, "error": {"code": "service", "http_status": 500, "message": "down"}}),
                false,
                "unknown",
            ),
            (
                json!({"ok": false, "error": {"code": "service", "http_status": 400, "message": "bad"}}),
                false,
                "not_sent",
            ),
            (
                json!({"ok": false, "error": {"code": "permission", "message": "no"}}),
                false,
                "not_sent",
            ),
        ];
        for (value, ok, want) in cases {
            let got = match classify(&value, ok) {
                SendOutcome::Sent => "sent",
                SendOutcome::NotSent(_) => "not_sent",
                SendOutcome::Unknown(_) => "unknown",
            };
            assert_eq!(got, want, "{value}");
        }
    }

    #[test]
    fn the_notice_names_the_clear_command_when_held() {
        let n = super::super::notice::health_notice(
            "digest",
            "consecutive_failures",
            "trip",
            "3 failed in a row, latest digest-9",
            true,
            "digest",
        );
        assert_eq!(n.subject, "digest: consecutive_failures tripped");
        assert!(n.text.contains("3 failed in a row"), "{}", n.text);
        assert!(
            n.text.contains("on hold: apb health clear digest"),
            "{}",
            n.text
        );
        assert!(n.text.contains("apb runs --playbook digest"), "{}", n.text);
        let r = super::super::notice::health_notice(
            "digest", "overdue", "recovery", "ok", false, "digest",
        );
        assert_eq!(r.subject, "digest: overdue recovered");
        assert!(!r.text.contains("on hold"));
    }
}
