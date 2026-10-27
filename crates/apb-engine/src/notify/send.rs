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
use super::transitions::{GATE, KeyWindow, REFUSED, TransitionId, due, tries};
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

/// Sends the due notifications of one run.
pub(crate) struct Notifier {
    root: PathBuf,
    run_dir: PathBuf,
    run_id: String,
    /// When [`Notifier::sweep_throttled`] last swept.
    last: Cell<Option<Instant>>,
}

impl Notifier {
    pub(crate) fn new(root: &Path, run_dir: &Path, run_id: &str) -> Self {
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
        let root = crate::run_retro::project_root_of(run_dir)?;
        let run_id = run_dir.file_name()?.to_str()?.to_string();
        Some(Self::new(&root, run_dir, &run_id))
    }

    /// Sends every transition due as of the journal `events`, for at most
    /// [`SWEEP_BUDGET_SECS`]. A sweep with nothing due reads nothing else:
    /// the drive loop runs one every turn.
    pub(crate) fn sweep(&self, events: &[Event]) {
        self.sweep_within(events, Duration::from_secs(SWEEP_BUDGET_SECS));
    }

    /// [`Notifier::sweep`] bounded by `budget`: each send gets the time left
    /// (at most [`SEND_TIMEOUT_SECS`]), and no send starts with less than a
    /// second left, so the sweep returns within `budget` plus the load of
    /// the target.
    fn sweep_within(&self, events: &[Event], budget: Duration) {
        let now_ms = apb_core::clock::now_ms_u64();
        // The cheap check first, over the run's own journal. An inherited
        // subscription is checked as if its parent had just been set up
        // again, which lifts every trust refusal: at most what may be due.
        let mut own = NotifyState::fold(events);
        if let Some(a) = own
            .active
            .as_mut()
            .filter(|a| a.source == NotifySource::Parent.as_str())
        {
            a.set_at_ms = u64::MAX;
        }
        if due(events, &own, now_ms, KeyWindow::OPEN).is_empty() {
            return;
        }
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
            KeyWindow::new(
                f.idempotency.as_ref().map_or(0, |i| i.window_sec()),
                f.timeout_sec,
            )
        });
        let started = Instant::now();
        for t in due(events, &state, now_ms, window) {
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
            );
        }
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

    fn send(
        &self,
        active: &Active,
        live: Result<&LiveTarget, &String>,
        events: &[Event],
        t: &TransitionId,
        timeout_secs: u64,
    ) {
        let target = &active.target;
        let label = t.label();
        let live = match live {
            Ok(live) => live,
            Err(why) => {
                return self.record_unsent(target, &label, IdempotencyMode::None, REFUSED, why);
            }
        };
        let mode = live
            .function()
            .idempotency
            .as_ref()
            .map_or(IdempotencyMode::None, |i| i.mode());
        let Some(args) = args_for(
            live,
            target,
            &Notice::build(&self.root, &self.run_dir, &self.run_id, events, t),
        ) else {
            return self.record_unsent(
                target,
                &label,
                mode,
                GATE,
                "the function no longer names the args a notification fills",
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
            CallContext::notify(&label, pins_of(target)),
            timeout_secs,
        );
        if ok || value.get("intent").is_some() || self.recorded_since(&label, before) {
            // It went out, or its own intent says what became of it.
            return;
        }
        let code = value["error"]["code"].as_str().unwrap_or_default();
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
        self.record_unsent(target, &label, mode, source, detail);
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
    ) {
        let key = IntentKey {
            node: NOTIFY_SITE.to_string(),
            connector: target.connector.clone(),
            function: target.function.clone(),
            account: target.account.clone(),
            label: label.to_string(),
            item: None,
        };
        let draft = IntentDraft {
            key: &key,
            mode,
            attempt: None,
            attempt_seq: None,
            timeout_sec: 0,
            now_ms: apb_core::clock::now_ms_u64(),
        };
        let Some(new) = intent::admitted(&draft, &Admission::New { resend_of: None }) else {
            return;
        };
        let recorded =
            EventLog::open_foreign(&self.run_dir).and_then(|mut log| log.append(new.payload));
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
