//! The sweep the drive loop runs: every due transition goes out once
//! through the subscription's connector function, as apb's own call under
//! the engine site `@notify`. The send-once ledger records it before the
//! request leaves, so a driver that restarts reads the same journal and
//! sends nothing twice.
//!
//! A sweep never fails the run. It is synchronous, each send is capped at
//! [`SEND_TIMEOUT_SECS`] and at the time its sweep has left, and one sweep
//! sends for at most [`SWEEP_BUDGET_SECS`] ([`TICK_BUDGET_SECS`] from an
//! attempt's poll loop); what is left waits for the next sweep. A send the
//! trust gate refuses (the connector or the account is not approved, or
//! changed since the subscription pinned it) is journaled as a `not_sent`
//! intent with source `gate`, which retires the transition until the
//! subscription is set again. A send refused for any other reason before
//! its intent was recorded is journaled with source `refused` and tried
//! again like a send that failed.
//!
//! A sub-playbook's subscription follows its parent's as it reads at each
//! sweep: a parent turned off, or narrowed, silences the child too.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_core::connector::idempotency::IdempotencyMode;
use apb_core::connector::sites::NOTIFY_SITE;
use apb_core::connector::text_mode::TextMode;
use apb_core::notify::NotifySource;
use serde_json::{Map, Value};

use super::notice::Notice;
use super::record::NotifyTargetRecord;
use super::transitions::{
    GATE, KeyWindow, REFUSED, TransitionId, due, due_reminder, reminder_open, retry_gap_ms, tries,
};
use super::{Active, NotifyState};
use crate::connector::call::{CallContext, CallRequest, LiveTarget, TargetPins};
use crate::connector::intent::{
    self, Admission, IntentDraft, IntentKey, Ledger, Settlement, ledger::NOT_SENT,
};
use crate::connector::result::CallErrorCode;
use crate::event::{Event, EventLog};

/// The longest a notification may take.
pub const SEND_TIMEOUT_SECS: u64 = 15;

/// The longest one sweep keeps sending: a slow target never holds the drive
/// loop (and the stop or pause waiting behind it) for longer.
pub const SWEEP_BUDGET_SECS: u64 = SEND_TIMEOUT_SECS;

/// The longest one sweep from an attempt's poll loop keeps sending, so the
/// question timeout, a cancel or an abort behind it is not held long.
pub const TICK_BUDGET_SECS: u64 = 5;

/// The least time between two sweeps an attempt's poll loop asks for.
const THROTTLE: Duration = Duration::from_secs(1);

/// The longest refusal detail a retired transition keeps.
const DETAIL_CAP: usize = 300;

/// Sends the due notifications of one run. The drive sweeps at every loop
/// turn. Reachable outside the crate only through the `testing`
/// re-export `notify::Notifier`.
pub struct Notifier {
    root: PathBuf,
    run_dir: PathBuf,
    run_id: String,
    /// When [`Notifier::sweep_throttled`] last swept.
    last: Cell<Option<Instant>>,
}

impl Notifier {
    pub fn new(root: &Path, run_dir: &Path, run_id: &str) -> Self {
        Notifier {
            root: root.to_path_buf(),
            run_dir: run_dir.to_path_buf(),
            run_id: run_id.to_string(),
            last: Cell::new(None),
        }
    }

    /// The notifier of a run directory laid out as `<root>/.apb/runs/<id>`,
    /// for a caller that holds only the run directory.
    pub(crate) fn of_run_dir(run_dir: &Path) -> Option<Self> {
        let root = crate::run_config::project_root_of(run_dir)?;
        let run_id = run_dir.file_name()?.to_str()?.to_string();
        Some(Self::new(&root, run_dir, &run_id))
    }

    /// Sends every transition due as of the journal `events`, for at most
    /// [`SWEEP_BUDGET_SECS`]. A sweep with nothing due reads nothing else:
    /// the drive loop runs one every turn.
    pub fn sweep(&self, events: &[Event]) {
        self.sweep_within(events, Duration::from_secs(SWEEP_BUDGET_SECS));
    }

    /// [`Notifier::sweep`] bounded by `budget`: each send gets the time left
    /// (at most [`SEND_TIMEOUT_SECS`]), and no send starts with less than a
    /// second left, so the sweep returns within `budget` plus the load of
    /// the target.
    fn sweep_within(&self, events: &[Event], budget: Duration) {
        // A scenario run has no subscription and sends nothing.
        if crate::scenario::session::skips(&self.run_dir, crate::scenario::BoundaryKind::Notify) {
            return;
        }
        let now_ms = apb_core::clock::now_ms_u64();
        if !self.may_be_due(events, now_ms) {
            return;
        }
        let needs_look = || crate::run_outcome::NeedsLook::for_run(&self.run_dir, events).is_some();
        let state = NotifyState::in_force(&self.run_dir, events);
        let Some(active) = &state.active else {
            return;
        };
        let target = &active.target;
        let live = LiveTarget::load(
            &self.root,
            &target.connector,
            Some(&target.account),
            &target.function,
        )
        .map_err(|e| e.message);
        let window = live.as_ref().map_or(KeyWindow::default(), |l| {
            let f = l.function();
            KeyWindow::new(f.send_once().map_or(0, |i| i.window_sec()), f.timeout_sec)
        });
        let started = Instant::now();
        let gap = retry_gap_ms();
        for t in due(events, &state, now_ms, window, gap, &needs_look) {
            let left = budget.saturating_sub(started.elapsed()).as_secs();
            if left == 0 {
                break;
            }
            self.send(
                active,
                live.as_ref(),
                events,
                &t,
                left.min(SEND_TIMEOUT_SECS),
                now_ms,
                window,
                gap,
            );
        }
    }

    /// The cheap check: whether anything may be due a send at `at_ms`. An
    /// inherited subscription is read as it is in force (the parent's
    /// current target and reminder cadence, not what the child's journal
    /// copied at its start) and checked as if its parent had just been set
    /// up again, which lifts every trust refusal; the target's
    /// deduplication window counts as open: at most what may be due.
    fn may_be_due(&self, events: &[Event], at_ms: u64) -> bool {
        let mut own = NotifyState::in_force(&self.run_dir, events);
        if let Some(a) = own
            .active
            .as_mut()
            .filter(|a| a.source == NotifySource::Parent.as_str())
        {
            a.set_at_ms = u64::MAX;
        }
        let needs_look = || crate::run_outcome::NeedsLook::for_run(&self.run_dir, events).is_some();
        !due(
            events,
            &own,
            at_ms,
            KeyWindow::OPEN,
            retry_gap_ms(),
            &needs_look,
        )
        .is_empty()
    }

    /// Whether a send of the run as its journal reads now may be due once
    /// the retry gap has passed: a try that failed or was refused for a
    /// passing reason, with tries left. A send the trust gate retired, or
    /// one that went out, never is.
    /// The exact check, not [`Notifier::may_be_due`]: an inherited
    /// subscription keeps its parent's real set time, so a send the gate
    /// refused stays retired and no waker sweeps for it.
    pub(crate) fn retry_ahead(&self) -> bool {
        if crate::scenario::session::skips(&self.run_dir, crate::scenario::BoundaryKind::Notify) {
            return false;
        }
        let gap = retry_gap_ms();
        let at = apb_core::clock::now_ms_u64().saturating_add(gap);
        let Ok(events) = crate::event::read_all_lossy_tail(&self.run_dir) else {
            return false;
        };
        let state = NotifyState::in_force(&self.run_dir, &events);
        let needs_look =
            || crate::run_outcome::NeedsLook::for_run(&self.run_dir, &events).is_some();
        !due(&events, &state, at, KeyWindow::OPEN, gap, &needs_look).is_empty()
    }

    /// [`Notifier::sweep`] over the journal as it reads now: the drive's
    /// last word on a run that paused, ended or failed.
    pub(crate) fn sweep_now(&self) {
        if let Ok(events) = crate::event::read_all_lossy_tail(&self.run_dir) {
            self.sweep(&events);
        }
    }

    /// A sweep at most once a [`THROTTLE`] and for at most
    /// [`TICK_BUDGET_SECS`]: for the poll loop of an attempt that is still
    /// running (a live question, a host task's question), which ticks far
    /// more often and must get back to its own checks.
    pub(crate) fn sweep_throttled(&self) {
        let now = Instant::now();
        if self.last.get().is_some_and(|t| now < t + THROTTLE) {
            return;
        }
        self.last.set(Some(now));
        if let Ok(events) = crate::event::read_all_lossy_tail(&self.run_dir) {
            self.sweep_within(&events, Duration::from_secs(TICK_BUDGET_SECS));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn send(
        &self,
        active: &Active,
        live: Result<&LiveTarget, &String>,
        events: &[Event],
        t: &TransitionId,
        timeout_secs: u64,
        now_ms: u64,
        window: KeyWindow,
        gap_ms: u64,
    ) {
        let target = &active.target;
        let label = t.label();
        // A reminder is admitted only while it is still due, decided over
        // the journal as it is under the lock that appends its intent (the
        // ordering boundary): the sweep's own slice may be stale. A notice
        // is never rechecked: it reports a transition that happened, open
        // or not.
        // The due reminder is computed again from the locked journal and
        // must be the one selected, under the same target and cadence: a
        // person who turned reminders off, changed the cadence or the
        // target in between gets no intent; the next sweep selects anew.
        // The clock is read again there too: a threshold passed since the
        // selection makes a later reminder the due one.
        let reminder = t.reminder;
        let predicate = move |locked: &[Event]| -> bool {
            let now_ms = apb_core::clock::now_ms_u64().max(now_ms);
            let in_force = NotifyState::in_force(&self.run_dir, locked);
            let Some(active_now) = in_force.active else {
                return false;
            };
            active_now.target == active.target
                && active_now.remind_ms == active.remind_ms
                && reminder_open(locked, &active_now, t)
                && due_reminder(locked, &active_now, t, now_ms, window, gap_ms) == Some(reminder)
        };
        let still_due: Option<crate::connector::call::StillDue> =
            (reminder > 0).then_some(&predicate as &dyn Fn(&[Event]) -> bool);
        let live = match live {
            Ok(live) => live,
            Err(why) => {
                return self.record_unsent(
                    target,
                    &label,
                    IdempotencyMode::None,
                    REFUSED,
                    why,
                    still_due,
                );
            }
        };
        let mode = live
            .function()
            .send_once()
            .and_then(|i| i.mode())
            .unwrap_or(IdempotencyMode::None);
        let Some(args) = args_for(
            live,
            target,
            &Notice::build_with(
                &self.root,
                &self.run_dir,
                &self.run_id,
                events,
                t,
                // A reminder names the threshold of the cadence in force,
                // the one its number was counted over.
                (reminder > 0)
                    .then(|| active.remind_ms.get(usize::from(reminder) - 1).copied())
                    .flatten(),
            ),
        ) else {
            return self.record_unsent(
                target,
                &label,
                mode,
                GATE,
                "the function no longer names the args a notification fills",
                still_due,
            );
        };
        let before = tries(&Ledger::fold(events), &label).len();
        let (value, ok) = crate::connector::call::execute_with_within(
            CallRequest {
                run_dir: &self.run_dir,
                root: &self.root,
                node_id: NOTIFY_SITE,
                connector: &target.connector,
                function: &target.function,
                account: Some(&target.account),
                args,
                dry_run: false,
                full: false,
            },
            CallContext::notify(&label, pins_of(target), target.digest(), still_due),
            timeout_secs,
        );
        if ok || value.get("intent").is_some() || self.recorded_since(&label, before) {
            // It went out, or its own intent says what became of it.
            return;
        }
        let code = value["error"]["code"].as_str().unwrap_or_default();
        if reminder > 0 && code == CallErrorCode::AttemptClosed.as_str() {
            // The admission recheck found the reminder no longer due: it
            // records nothing (design D9 item 5).
            return;
        }
        if code == CallErrorCode::InFlight.as_str() || code == CallErrorCode::UnknownEffect.as_str()
        {
            // An earlier try is still going out, or nobody knows what
            // became of it: the ledger keeps its own record.
            return;
        }
        let source = if code == CallErrorCode::Permission.as_str() {
            GATE
        } else {
            REFUSED
        };
        let detail = value["error"]["message"].as_str().unwrap_or("refused");
        self.record_unsent(target, &label, mode, source, detail, still_due);
    }

    /// Whether the call path recorded a try of `label` itself (and settled
    /// it) since `before` tries were known.
    fn recorded_since(&self, label: &str, before: usize) -> bool {
        crate::event::read_all_lossy_tail(&self.run_dir)
            .map(|events| tries(&Ledger::fold(&events), label).len() > before)
            .unwrap_or(false)
    }

    /// Records a try that never left, settled `not_sent` from `source`:
    /// [`GATE`] retires the transition, [`REFUSED`] leaves it to a later
    /// sweep. Best effort: a journal that cannot take it leaves the
    /// transition due, and the next sweep meets the same refusal.
    fn record_unsent(
        &self,
        target: &NotifyTargetRecord,
        label: &str,
        mode: IdempotencyMode,
        source: &'static str,
        why: &str,
        still_due: Option<crate::connector::call::StillDue>,
    ) {
        let key = IntentKey {
            node: NOTIFY_SITE.to_string(),
            connector: target.connector.clone(),
            function: target.function.clone(),
            account: target.account.clone(),
            label: label.to_string(),
            item: None,
        };
        let digest = target.digest();
        let draft = IntentDraft {
            key: &key,
            mode,
            attempt: None,
            attempt_seq: None,
            timeout_sec: 0,
            now_ms: apb_core::clock::now_ms_u64(),
            target_digest: Some(&digest),
        };
        let Some(new) = intent::admitted(&draft, &Admission::New { resend_of: None }) else {
            return;
        };
        // A reminder's not-sent intent is appended under the journal lock
        // with the same still-due recheck the send path decides by: a wait
        // that closed before the append records nothing at all. A notice
        // (no recheck) appends as before.
        let recorded = match still_due {
            Some(still_due) => {
                let mut skipped = false;
                let recorded = EventLog::open_foreign(&self.run_dir).and_then(|mut log| {
                    log.append_decided_synced(|locked| {
                        if !still_due(locked) {
                            skipped = true;
                            return None;
                        }
                        Some(new.payload.clone())
                    })
                });
                if skipped {
                    return;
                }
                recorded
            }
            None => EventLog::open_foreign(&self.run_dir)
                .and_then(|mut log| log.append(new.payload))
                .map(Some),
        };
        if recorded.is_ok() {
            let _ = intent::settle(
                &self.run_dir,
                &new.id,
                Settlement {
                    outcome: NOT_SENT,
                    source,
                    detail: Some(why.chars().take(DETAIL_CAP).collect()),
                    effect_ref: None,
                    by: None,
                },
            );
        }
    }
}

/// The digests the subscription pinned.
fn pins_of(target: &NotifyTargetRecord) -> TargetPins {
    TargetPins {
        connector: target.connector_digest.clone(),
        account: target.account_digest.clone(),
    }
}

/// The call's args from the function's `people:` block: the recipient, the
/// text, and the subject where the function has one. A text the function
/// sends as one literal line gets the one-line notice.
pub(crate) fn args_for(
    live: &LiveTarget,
    target: &NotifyTargetRecord,
    notice: &Notice,
) -> Option<Value> {
    let f = live.function();
    let people = f.people.as_ref()?;
    let to = people.to.as_ref()?;
    let one_line = f.text_args.get(&people.text) == Some(&TextMode::Literal);
    let mut args = Map::new();
    args.insert(to.clone(), Value::String(target.to.clone()));
    args.insert(
        people.text.clone(),
        Value::String(if one_line {
            notice.one_line()
        } else {
            notice.text.clone()
        }),
    );
    if let Some(subject) = &people.subject {
        args.insert(subject.clone(), Value::String(notice.subject.clone()));
    }
    Some(Value::Object(args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventLog, EventPayload};
    use apb_core::connector::sites::NOTIFY_SITE;

    /// A run directory whose journal is `events` (seqs in order), with a
    /// playbook snapshot, and no connector installed: every live target
    /// load fails, which is the not-sent path the tests exercise.
    fn seeded_run(events: &[Event]) -> (tempfile::TempDir, PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let run_dir = root.join(".apb/runs/r-n");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: gate, type: human_review }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: gate }\n  - { from: gate, to: f }\n",
        )
        .unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();
        for e in events {
            log.append(e.payload.clone()).unwrap();
        }
        drop(log);
        // The journal's times are the test's: rewrite them in place.
        let path = run_dir.join("events.jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        let mut out = String::new();
        for (i, line) in content.lines().enumerate() {
            let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
            v["ts"] = serde_json::json!(events[i].ts as u64);
            out.push_str(&serde_json::to_string(&v).unwrap());
            out.push('\n');
        }
        std::fs::write(&path, out).unwrap();
        (dir, run_dir, "r-n".to_string())
    }

    /// The project root of a run directory laid out as `<root>/.apb/runs/<id>`.
    fn root_of(run_dir: &Path) -> PathBuf {
        run_dir
            .ancestors()
            .nth(3)
            .expect("the run directory's layout")
            .to_path_buf()
    }

    fn target() -> NotifyTargetRecord {
        NotifyTargetRecord {
            connector: "chat".into(),
            account: "me".into(),
            function: "send".into(),
            to: "42".into(),
            connector_digest: "sha256:c".into(),
            account_digest: "sha256:a".into(),
        }
    }

    fn ev(seq: u64, ts: u128, payload: EventPayload) -> Event {
        Event { seq, ts, payload }
    }

    fn started() -> EventPayload {
        EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
            definition: None,
            external_data: false,
        }
    }

    fn subscribed() -> EventPayload {
        EventPayload::NotifySet {
            target: Some(target()),
            on: vec!["review".into()],
            source: "run".into(),
            by: None,
            problem: None,
            remind_ms: vec![600_000],
        }
    }

    fn review() -> EventPayload {
        EventPayload::ReviewRequested {
            node: "gate".into(),
            options: vec!["approve".into(), "reject".into()],
            title: None,
            instruction: String::new(),
            prompt: None,
            recommendation: None,
            plan_digest: None,
            binding: None,
            person_only: false,
        }
    }

    fn notice_confirmed(label: &str) -> Vec<EventPayload> {
        vec![
            EventPayload::SendIntentRecorded {
                intent: format!("i-{label}"),
                nonce: format!("n-{label}"),
                node: NOTIFY_SITE.into(),
                attempt: None,
                attempt_seq: None,
                connector: "chat".into(),
                function: "send".into(),
                account: "me".into(),
                mode: "none".into(),
                label: label.into(),
                pid: 1,
                deadline_ms: 0,
                replay_of: None,
                resend_of: None,
                item: None,
                target_digest: None,
            },
            EventPayload::SendIntentSettled {
                intent: format!("i-{label}"),
                outcome: "confirmed".into(),
                source: "response".into(),
                detail: None,
                effect_ref: None,
                by: None,
            },
        ]
    }

    fn decided() -> EventPayload {
        EventPayload::ReviewDecided {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
            plan_digest: None,
            decided_by: None,
            binding: None,
            via: None,
        }
    }

    /// The journal as a sweep read it while the gate was open: subscribed
    /// under a ten-minute cadence, the review open eleven minutes, its
    /// notice confirmed.
    fn stale_slice(now: u128) -> Vec<Event> {
        let opened = now - 660_000;
        let mut out = vec![
            ev(0, opened, started()),
            ev(1, opened, subscribed()),
            ev(2, opened, review()),
        ];
        for (i, p) in notice_confirmed("notify:review.2").into_iter().enumerate() {
            out.push(ev(3 + i as u64, opened + 1_000, p));
        }
        out
    }

    /// The reminder the sweep over `events` would send, as its label.
    fn reminder_of(events: &[Event], now_ms: u64) -> Option<String> {
        let state = NotifyState::fold(events);
        let d = due(
            events,
            &state,
            now_ms,
            KeyWindow::default(),
            30_000,
            &|| false,
        );
        d.iter().find(|t| t.reminder > 0).map(|t| t.label())
    }

    /// The not-sent path of a closed wait whose live target does not
    /// resolve (no connector installed here): the wait decided between the
    /// sweep's read and the append, so no intent and no settlement are
    /// recorded for the reminder (design D9 item 8; the args variant and
    /// the send path are in `notify_remind_test`).
    #[test]
    fn a_closed_wait_gets_no_unsent_reminder() {
        let now = apb_core::clock::now_ms_u64() as u128;
        let stale = stale_slice(now);
        let mut on_disk = stale.clone();
        on_disk.push(ev(5, now - 500, decided()));
        assert_eq!(
            reminder_of(&stale, now as u64).as_deref(),
            Some("notify:review.2.r1"),
            "over the stale slice the reminder is due"
        );
        let (_dir, run_dir, run_id) = seeded_run(&on_disk);
        Notifier::new(&root_of(&run_dir), &run_dir, &run_id).sweep(&stale);
        let events = crate::event::read_all(&run_dir).unwrap();
        assert_eq!(
            events.len(),
            on_disk.len(),
            "no intent and no settlement of the reminder"
        );
    }
}
