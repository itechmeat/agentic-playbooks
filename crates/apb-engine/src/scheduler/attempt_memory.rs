//! Attempt memory: what the earlier attempts of a step did, as apb recorded
//! it, told to the next attempt as a retry briefing.
//!
//! The briefing covers every attempt in the node's effect scope (everything
//! after its last success, see
//! [`effect_scope_floor`](crate::connector::intent::effect_scope_floor)), so
//! it reaches every kind of repeat: an agent or infrastructure retry, a
//! fallback step, a deadline continuation, a resume after a pause or a crash,
//! a supervisor retry and a failure route back into the node. A loop visit
//! after a success starts afresh and gets none.
//!
//! It reads the journal only: how each attempt ended, the send intents of
//! the send ledger and the connector writes that carry no intent. It never
//! claims more than that: what apb did not record is named as not known.

use std::fmt::Write as _;
use std::path::Path;

use apb_core::connector::idempotency::IdempotencyMode;

use crate::connector::intent::{
    ARGS_LABEL_PREFIX, IntentRecord, IntentState, Ledger, chain_verdict, effect_scope_floor,
};
use crate::event::{Event, EventPayload};
use crate::run_outcome::calls::{CallClass, classified_calls};

/// At most this many effect lines are listed; the rest is counted.
const MAX_EFFECT_LINES: usize = 30;

/// The longest failure line quoted from an attempt.
const MAX_FAILURE_CHARS: usize = 160;

/// What the earlier attempts of one node did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttemptMemory {
    attempts: Vec<AttemptRecall>,
}

/// One earlier attempt and its effects.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AttemptRecall {
    /// Its place among the attempts in scope, from 1. Attempt numbers
    /// restart with every execution of a node; this does not.
    ordinal: usize,
    /// The seq of its `attempt_started`.
    seq: u64,
    agent: String,
    model: Option<String>,
    ending: String,
    effects: Vec<Effect>,
}

/// What became of one send or write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EffectKind {
    Confirmed,
    Unknown,
    NotSent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Effect {
    kind: EffectKind,
    /// The line without its leading `- ` and attempt label.
    target: String,
    what: String,
}

/// The attempts of `node` in its effect scope, with their effects; `None`
/// when there is none (a first attempt, or a loop visit after a success).
/// `run_dir` holds the connector snapshots that tell a write from a read.
pub(crate) fn recall(run_dir: &Path, events: &[Event], node: &str) -> Option<AttemptMemory> {
    let floor = effect_scope_floor(events, node);
    let mut attempts = started_attempts(events, node, floor);
    if attempts.is_empty() {
        return None;
    }
    let now = apb_core::clock::now_ms_u64();
    let ledger = Ledger::fold(events);
    // One line per send: the latest intent of each chain, told by the
    // chain's verdict, so an unknown send a replay or a resend did not
    // settle stays in view.
    for tip in ledger
        .records()
        .iter()
        .filter(|r| r.seq >= floor && r.key.node == node && r.superseded_by.is_none())
    {
        let verdict = chain_verdict(&ledger, tip, now);
        let rec = verdict.member;
        let at = owner(&attempts, rec.attempt_seq, rec.seq);
        attempts[at]
            .effects
            .push(intent_effect(rec, &verdict.state));
    }
    for call in classified_calls(run_dir, events)
        .into_iter()
        .filter(|c| c.seq >= floor && c.node == node && c.intent.is_none())
    {
        if let Some(effect) = write_effect(&call.target(), &call.outcome, call.class) {
            let at = owner(&attempts, call.attempt_seq, call.seq);
            attempts[at].effects.push(effect);
        }
    }
    Some(AttemptMemory { attempts })
}

/// Every `attempt_started` of `node` from `floor` on, with how it ended.
fn started_attempts(events: &[Event], node: &str, floor: u64) -> Vec<AttemptRecall> {
    let starts: Vec<(usize, &Event)> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.seq >= floor
                && matches!(&e.payload, EventPayload::AttemptStarted { node: n, .. } if n == node)
        })
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(i, (at, e))| {
            let EventPayload::AttemptStarted {
                attempt,
                agent,
                model,
                ..
            } = &e.payload
            else {
                unreachable!("filtered to attempt_started above")
            };
            let until = starts.get(i + 1).map_or(events.len(), |(next, _)| *next);
            let finished = events[*at..until].iter().find_map(|f| match &f.payload {
                EventPayload::AttemptFinished {
                    node: n,
                    attempt: a,
                    status,
                    failure_kind,
                    summary,
                    partial_output,
                    ..
                } if n == node && a == attempt => Some(ending(
                    status,
                    failure_kind.as_deref(),
                    summary.as_deref().or(partial_output.as_deref()),
                )),
                _ => None,
            });
            AttemptRecall {
                ordinal: i + 1,
                seq: e.seq,
                agent: agent.clone(),
                model: model.clone(),
                ending: finished.unwrap_or_else(|| "ended with no result recorded".to_string()),
                effects: Vec::new(),
            }
        })
        .collect()
}

/// How an attempt ended, in words.
fn ending(status: &str, failure_kind: Option<&str>, said: Option<&str>) -> String {
    match (status, failure_kind) {
        ("succeeded", _) => "succeeded".to_string(),
        ("cancelled", _) => "was cancelled".to_string(),
        ("interrupted", Some("quota")) => "stopped at a usage limit and was paused".to_string(),
        ("interrupted", _) => {
            "was interrupted before it finished (a pause, a stop or a crash)".to_string()
        }
        ("failed", Some("transient")) => {
            "ended with a transient failure (network, service or process)".to_string()
        }
        ("failed", Some("quota")) => "failed at a usage limit".to_string(),
        ("failed", Some("auth")) => "failed to authenticate".to_string(),
        ("failed", Some("budget")) => "ran out of budget".to_string(),
        ("failed", _) => match said.and_then(first_line) {
            Some(line) => format!("failed: {line}"),
            None => "failed".to_string(),
        },
        (other, _) => format!("ended as {other}"),
    }
}

/// The first non-empty line of `text`, bounded.
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut out: String = line.chars().take(MAX_FAILURE_CHARS).collect();
    if line.chars().count() > MAX_FAILURE_CHARS {
        out.push_str("...");
    }
    Some(out)
}

/// The index of the attempt an effect belongs to: the one its
/// `attempt_seq` names, else the last attempt started before it (the first
/// one when none was).
fn owner(attempts: &[AttemptRecall], attempt_seq: Option<u64>, seq: u64) -> usize {
    attempt_seq
        .and_then(|s| attempts.iter().position(|a| a.seq == s))
        .or_else(|| attempts.iter().rposition(|a| a.seq < seq))
        .unwrap_or(0)
}

fn intent_effect(rec: &IntentRecord, state: &IntentState) -> Effect {
    let target = format!(
        "{}.{}, intent \"{}\"",
        rec.key.connector, rec.key.function, rec.key.label
    );
    let (kind, what) = match state {
        IntentState::Confirmed { source, effect_ref } => {
            let how = if source == "person" {
                "a person confirmed it went through"
            } else {
                "went through"
            };
            let at = effect_ref
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            (
                EffectKind::Confirmed,
                format!(
                    "{how}{at}. Do not send it again; {} returns the earlier result.",
                    repeat_of(rec)
                ),
            )
        }
        IntentState::NotSent { source } => {
            let why = match source.as_str() {
                "person" => "a person confirmed it did not go out",
                "gate" => "apb recorded it but never sent it",
                _ => "it did not go out (refused or failed before it could act)",
            };
            (
                EffectKind::NotSent,
                format!("{why}; you may send it again."),
            )
        }
        IntentState::Pending { .. } => (
            EffectKind::Unknown,
            "another process is still sending it; do not send it under another name.".to_string(),
        ),
        IntentState::Unknown { detail, .. } => {
            let why = detail.as_deref().unwrap_or("no answer was recorded");
            let next = match rec.mode {
                IdempotencyMode::Key => format!(
                    "Repeat it only as {}: apb replays it with the same key while the service still deduplicates it, and refuses after that.",
                    repeat_of(rec)
                ),
                IdempotencyMode::Marker => format!(
                    "Repeat it only as {}: apb first looks for it at the service, and refuses if it cannot find it.",
                    repeat_of(rec)
                ),
                IdempotencyMode::None => {
                    "apb refuses to send this intent again until a person resolves it.".to_string()
                }
            };
            (
                EffectKind::Unknown,
                format!(
                    "whether it was delivered is not known ({why}). {next} Do not send it under another name; report it as a finding; a person resolves it."
                ),
            )
        }
    };
    Effect { kind, target, what }
}

/// How the agent repeats the send of `rec`: with its `--intent`, or, for a
/// send made without one, as the same call word for word.
fn repeat_of(rec: &IntentRecord) -> String {
    if rec.key.label.starts_with(ARGS_LABEL_PREFIX) {
        "the same call with byte-identical arguments and no --intent".to_string()
    } else {
        format!("a call with --intent {}", rec.key.label)
    }
}

/// A connector write with no send intent; `None` for a read.
fn write_effect(target: &str, outcome: &str, class: CallClass) -> Option<Effect> {
    let (kind, what) = match class {
        CallClass::ReadOk | CallClass::ReadUnknown => return None,
        CallClass::WriteOk | CallClass::WriteIncomplete => (
            EffectKind::Confirmed,
            "went through. apb keeps no send-once record for this function, so calling it again acts again.".to_string(),
        ),
        CallClass::WriteUnknown => (
            EffectKind::Unknown,
            format!(
                "whether it changed anything is not known ({outcome}). apb keeps no send-once record for this function: check before calling it again."
            ),
        ),
        CallClass::Failed => (
            EffectKind::NotSent,
            format!("did not go through ({outcome}); you may call it again."),
        ),
    };
    Some(Effect {
        kind,
        target: target.to_string(),
        what,
    })
}

impl AttemptMemory {
    /// The briefing the next attempt gets, appended to its prompt.
    pub(crate) fn briefing(&self) -> String {
        let mut out = String::from("## What earlier attempts of this step did (apb's record)\n\n");
        for a in &self.attempts {
            let binding = match &a.model {
                Some(m) => format!("{}, {m}", a.agent),
                None => a.agent.clone(),
            };
            let _ = writeln!(out, "Attempt {} ({binding}): {}.", a.ordinal, a.ending);
        }
        let mut listed = 0usize;
        let mut total = 0usize;
        for (kind, title) in [
            (EffectKind::Confirmed, "Sends apb confirmed:"),
            (EffectKind::Unknown, "Sends with an unknown outcome:"),
            (EffectKind::NotSent, "Sends that did not go out:"),
        ] {
            let lines: Vec<String> = self
                .attempts
                .iter()
                .flat_map(|a| {
                    a.effects
                        .iter()
                        .filter(move |e| e.kind == kind)
                        .map(move |e| format!("- {} (attempt {}): {}", e.target, a.ordinal, e.what))
                })
                .collect();
            total += lines.len();
            let room = MAX_EFFECT_LINES.saturating_sub(listed);
            if lines.is_empty() || room == 0 {
                continue;
            }
            out.push('\n');
            out.push_str(title);
            out.push('\n');
            for line in lines.iter().take(room) {
                out.push_str(line);
                out.push('\n');
            }
            listed += lines.len().min(room);
        }
        if total > listed {
            let _ = writeln!(out, "({} more sends not listed.)", total - listed);
        }
        let quiet: Vec<String> = self
            .attempts
            .iter()
            .filter(|a| a.effects.is_empty())
            .map(|a| a.ordinal.to_string())
            .collect();
        if !quiet.is_empty() {
            let _ = writeln!(
                out,
                "\nAttempts with no connector writes recorded: {} {}.",
                if quiet.len() == 1 {
                    "attempt"
                } else {
                    "attempts"
                },
                quiet.join(", ")
            );
        }
        out.push_str(
            "\nNot recorded by apb, so the outcome is unknown: reconcile before sending again.\n\
             - anything done outside `apb connector call`: shell commands, gh or curl or other API calls, messages or emails sent another way, git pushes;\n\
             - file changes and commits in the working tree (check git status and git log).\n\
             A missing line above is not evidence that an action did not happen.",
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: 1_000,
            payload,
        }
    }

    fn started(seq: u64, attempt: u32, agent: &str, model: &str) -> Event {
        ev(
            seq,
            EventPayload::AttemptStarted {
                node: "send".into(),
                attempt,
                agent: agent.into(),
                soul_delivery: None,
                skills_mode: None,
                pid: None,
                spawn_ms: None,
                model: Some(model.into()),
                workdir: None,
                transcript: None,
                token: None,
            },
        )
    }

    fn finished(seq: u64, attempt: u32, status: &str, kind: Option<&str>) -> Event {
        ev(
            seq,
            EventPayload::AttemptFinished {
                node: "send".into(),
                attempt,
                status: status.into(),
                duration_ms: None,
                session: None,
                summary: Some("tests are red\nmore".into()),
                rejected_output: None,
                partial_output: None,
                failure_kind: kind.map(str::to_string),
                usage: None,
                output_truncated: Vec::new(),
                transcript_incomplete: false,
            },
        )
    }

    fn intent(seq: u64, id: &str, label: &str, mode: &str, attempt_seq: u64) -> Event {
        ev(
            seq,
            EventPayload::SendIntentRecorded {
                intent: id.into(),
                nonce: format!("n-{id}"),
                node: "send".into(),
                attempt: Some(1),
                attempt_seq: Some(attempt_seq),
                connector: "telegram".into(),
                function: "send_message".into(),
                account: "main".into(),
                mode: mode.into(),
                label: label.into(),
                pid: 0,
                deadline_ms: 0,
                replay_of: None,
                resend_of: None,
            },
        )
    }

    fn settled(seq: u64, id: &str, outcome: &str, detail: Option<&str>) -> Event {
        ev(
            seq,
            EventPayload::SendIntentSettled {
                intent: id.into(),
                outcome: outcome.into(),
                source: "response".into(),
                detail: detail.map(str::to_string),
                effect_ref: (outcome == "confirmed").then(|| "msg-1".to_string()),
                by: None,
            },
        )
    }

    fn node_finished(seq: u64, status: &str) -> Event {
        ev(
            seq,
            EventPayload::NodeFinished {
                node: "send".into(),
                status: status.into(),
                attempt: 1,
                output: String::new(),
                artifacts: Vec::new(),
            },
        )
    }

    fn briefing(events: &[Event]) -> Option<String> {
        let dir = tempfile::tempdir().unwrap();
        recall(dir.path(), events, "send").map(|m| m.briefing())
    }

    #[test]
    fn a_first_attempt_and_a_visit_after_a_success_get_no_briefing() {
        assert_eq!(briefing(&[]), None);
        let events = [
            started(1, 1, "claude", "sonnet"),
            finished(2, 1, "succeeded", None),
            node_finished(3, "succeeded"),
        ];
        assert_eq!(briefing(&events), None);
    }

    #[test]
    fn the_briefing_lists_each_attempt_and_its_sends_by_outcome() {
        let events = [
            started(1, 1, "claude", "sonnet"),
            intent(2, "si-a", "triage-summary", "marker", 1),
            settled(3, "si-a", "confirmed", None),
            intent(4, "si-b", "announce", "none", 1),
            settled(5, "si-b", "unknown", Some("timeout")),
            intent(6, "si-c", "args:3f2a", "none", 1),
            settled(7, "si-c", "not_sent", None),
            finished(8, 1, "failed", Some("transient")),
            started(9, 2, "codex", "gpt-5"),
            finished(10, 2, "failed", None),
            node_finished(11, "failed"),
        ];
        let text = briefing(&events).expect("two earlier attempts");
        let expected = [
            "## What earlier attempts of this step did (apb's record)",
            "Attempt 1 (claude, sonnet): ended with a transient failure (network, service or process).",
            "Attempt 2 (codex, gpt-5): failed: tests are red.",
            "Sends apb confirmed:\n- telegram.send_message, intent \"triage-summary\" (attempt 1): went through (msg-1). Do not send it again",
            "Sends with an unknown outcome:\n- telegram.send_message, intent \"announce\" (attempt 1): whether it was delivered is not known (timeout). apb refuses to send this intent again until a person resolves it.",
            "Sends that did not go out:\n- telegram.send_message, intent \"args:3f2a\" (attempt 1): it did not go out",
            "Attempts with no connector writes recorded: attempt 2.",
            "- file changes and commits in the working tree (check git status and git log).",
            "A missing line above is not evidence that an action did not happen.",
        ];
        for part in expected {
            assert!(text.contains(part), "missing {part:?} in:\n{text}");
        }
    }

    #[test]
    fn the_scope_spans_executions_until_the_last_success() {
        // A failed execution, then a supervisor retry: attempt numbers
        // restart, the ordinals do not.
        let events = [
            started(1, 1, "claude", "sonnet"),
            finished(2, 1, "succeeded", None),
            node_finished(3, "succeeded"),
            started(4, 1, "claude", "sonnet"),
            finished(5, 1, "interrupted", Some("quota")),
            node_finished(6, "failed"),
            started(7, 1, "claude", "sonnet"),
            finished(8, 1, "interrupted", None),
        ];
        let text = briefing(&events).unwrap();
        assert!(text.contains("Attempt 1 (claude, sonnet): stopped at a usage limit"));
        assert!(text.contains("Attempt 2 (claude, sonnet): was interrupted"));
        assert!(!text.contains("Attempt 3"));
        assert!(text.contains("attempts 1, 2."));
    }

    #[test]
    fn a_replayed_intent_is_told_once_and_the_list_is_bounded() {
        let mut events = vec![
            started(1, 1, "claude", "sonnet"),
            intent(2, "si-a", "ping", "key", 1),
            settled(3, "si-a", "unknown", Some("timeout")),
        ];
        let mut replay = intent(4, "si-b", "ping", "key", 1);
        if let EventPayload::SendIntentRecorded { replay_of, .. } = &mut replay.payload {
            *replay_of = Some("si-a".into());
        }
        events.push(replay);
        events.push(settled(5, "si-b", "confirmed", None));
        let text = briefing(&events).unwrap();
        assert_eq!(text.matches("intent \"ping\"").count(), 1);
        assert!(!text.contains("Sends with an unknown outcome"));

        let mut many = vec![started(1, 1, "claude", "sonnet")];
        for i in 0..35u64 {
            let id = format!("si-{i}");
            many.push(intent(10 + 2 * i, &id, &format!("l{i}"), "none", 1));
            many.push(settled(11 + 2 * i, &id, "confirmed", None));
        }
        let text = briefing(&many).unwrap();
        assert_eq!(text.matches("\n- telegram.").count(), MAX_EFFECT_LINES);
        assert!(text.contains("(5 more sends not listed.)"));
    }

    /// A replay that did not go out leaves the send it repeated unknown,
    /// and a send made without `--intent` is repeated word for word.
    #[test]
    fn an_unknown_send_a_replay_did_not_settle_stays_in_view() {
        let mut replay = intent(4, "si-b", "args:3f2a", "key", 1);
        if let EventPayload::SendIntentRecorded { replay_of, .. } = &mut replay.payload {
            *replay_of = Some("si-a".into());
        }
        let events = [
            started(1, 1, "claude", "sonnet"),
            intent(2, "si-a", "args:3f2a", "key", 1),
            settled(3, "si-a", "unknown", Some("timeout")),
            replay,
            settled(5, "si-b", "not_sent", None),
        ];
        let text = briefing(&events).unwrap();
        assert!(text.contains("Sends with an unknown outcome:"));
        assert!(!text.contains("Sends that did not go out"));
        assert_eq!(text.matches("intent \"args:3f2a\"").count(), 1);
        assert!(text.contains(
            "Repeat it only as the same call with byte-identical arguments and no --intent:"
        ));
    }

    #[test]
    fn an_attempt_with_no_finish_reads_as_having_no_result() {
        let text = briefing(&[started(1, 1, "claude", "sonnet")]).unwrap();
        assert!(text.contains("Attempt 1 (claude, sonnet): ended with no result recorded."));
    }
}
