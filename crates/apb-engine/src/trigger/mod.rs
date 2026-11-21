//! The trigger sweep (`docs/TRIGGERS.md`): one pass over the rules of one
//! project, each rule under its sweep lock, each new event started at once
//! through the fire the caller hands in, or recorded with its reason. The
//! engine never names the start surface: the fire is a parameter, so the
//! start sequence stays where every other start surface builds it.
//!
//! The invariants the pass keeps (design "Invariants"): the event is the
//! unit and its journal line comes first (I2), the start key is the only
//! dedupe (I3), one holder per rule (I4), and the position lives only in
//! the signed state (I5). The hourly cap counts the runs the incarnation
//! started, read from their launch records, never from the journal.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

pub mod view;

use apb_core::clock::Millis;
use apb_core::connector::inbox::{Inbox, InboxError, InboxEvent};
use apb_core::launch::TriggeredBy;
use apb_core::scope::Scope;
use apb_core::trigger::{
    ConditionMark, InboxSource, LogEntry, MAX_EVENTS_PER_PASS, MAX_STARTS_PER_PASS, RuleEntry,
    RuleSource, RuleState, RulesFile, SourceId, TriggerError, TriggerStateFile, Verdict, append,
    check_rule, journal_ids, needs_position, position_at_head, revoke_approval_locked, rule_digest,
};

/// What a start asks the start path to do (the fire of the tools layer).
pub struct FireRequest<'a> {
    pub root: &'a Path,
    pub entry: &'a RuleEntry,
    pub source: &'a InboxSource,
    /// The stored event the run is started for; the params come from it.
    pub event: &'a InboxEvent,
    pub start_key: String,
    pub triggered_by: TriggeredBy,
    /// `apb trigger test`: the checks before preparation, no start,
    /// nothing written.
    pub dry_run: bool,
    /// The limits on new starts (the hourly cap, the pass's start bound),
    /// asked by the fire once the start key said the event starts a new
    /// run and before anything else: a key that names a started run
    /// answers `already_started` before any limit (deduplication before
    /// limits). `Err(code)` is the limit that holds the event.
    pub admit: &'a dyn Fn() -> Result<(), String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FireOutcome {
    Started {
        run_id: String,
    },
    /// The key names a run already: the same request, or (code
    /// `start_key_conflict`) the same key with other params.
    AlreadyStarted {
        run_id: String,
        conflict: bool,
    },
    /// A dry run whose checks before preparation all passed. Never read as
    /// "would start": preparation's checks did not run.
    PreStartPassed,
    /// A refusal, by its `policy` code, and for `param_pattern_mismatch`
    /// the first param the refusal names (a name from the rule, never a
    /// value). The event's verdict.
    Refused {
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// `start_key_busy`: another start with the key is still preparing
    /// (its entry pending, not bound to a started run). Tried again next
    /// pass; the position stays before the event.
    Busy,
    /// A limit on new starts held the event ([`FireRequest::admit`]'s
    /// code: `rate_cap`, `rate_cap_unknown` or `starts_per_pass`). The
    /// position stays before the event and the pass ends.
    Limited {
        code: String,
    },
    /// The resolved approval binding is not the approval's: the rule is
    /// unapproved until a person approves it again. Carries the binding
    /// actually compared.
    BindingChanged {
        binding: String,
    },
    /// The event's verdict; a code, never a sentence.
    Failed {
        code: String,
    },
}

/// The start a pass asks for. Errors are printed by the sweep host and the
/// pass goes on.
pub type Fire<'a> = dyn Fn(&FireRequest<'_>) -> FireOutcome + 'a;

/// One rule's pass as the surfaces report it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleSweep {
    pub evaluated: u32,
    pub started: Vec<String>,
    /// Why the pass ended before the newest event: `rate_cap`,
    /// `start_key_busy`, `change_waiting`, `starts_per_pass`,
    /// `journal_write`.
    pub stopped: Option<String>,
}

/// One pass over the rules of the project at `root`: one entry per rule
/// considered (or one `"*"` entry when the rules or the state do not
/// verify). The caller has checked that this process is not a run step.
pub fn sweep(
    root: &Path,
    now: &dyn Fn() -> Millis,
    fire: &Fire<'_>,
) -> Vec<(String, Result<RuleSweep, EngineError>)> {
    let now = || now().0;
    // The rules the pass works on, only as this installation signed them
    // for this project: unsigned is the one `"*"` error and nothing starts.
    let rules = match read_signed::<RulesFile>(root, "rules") {
        Ok(loaded) => loaded,
        Err(e) => return star("trigger_rules_unreadable", &e.to_string()),
    };
    // A state file that does not verify or does not belong stops the sweep
    // of the project: a forged file can only stop starts (I5). Each rule
    // reads it again under its own sweep lock; this read only decides the
    // one `"*"` answer.
    if let Err(e) = read_signed::<TriggerStateFile>(root, "state") {
        return star("trigger_state_unreadable", &e.to_string());
    }
    let mut out = Vec::new();
    for entry in &rules.rules {
        // A pass sweeps enabled rules only, and only one approved for its
        // current digest (I1).
        if !entry.enabled {
            continue;
        }
        if !entry
            .approval
            .as_ref()
            .is_some_and(|a| a.rule_digest == rule_digest(entry))
        {
            continue;
        }
        match sweep_one(root, entry, &now, fire) {
            Ok(Some(report)) => out.push((entry.rule.name.clone(), Ok(report))),
            Ok(None) => {}
            Err(e) => out.push((entry.rule.name.clone(), Err(e))),
        }
    }
    out
}

/// The signed file `name` of this project, or the reason when it does not
/// verify: both stores are read with no link followed and refused unless
/// this installation signed them for this project (I5).
fn read_signed<T: apb_core::signed_store::SignedDoc>(
    root: &Path,
    name: &str,
) -> Result<T, TriggerError> {
    let loaded = apb_core::signed_store::read::<T>(root, Scope::Project, name)
        .map_err(TriggerError::Store)?;
    if !loaded.signed {
        return Err(TriggerError::Store(
            apb_core::signed_store::StoreError::Unverified(apb_core::signed_store::file::<T>(
                root,
                Scope::Project,
                name,
            )),
        ));
    }
    Ok(loaded.file)
}

/// The one `"*"` entry the project is swept with when its rules or its
/// state do not verify: nothing starts (I5).
fn star(what: &str, why: &str) -> Vec<(String, Result<RuleSweep, EngineError>)> {
    vec![(
        String::from("*"),
        Err(EngineError::Invalid(format!("{what}: {why}"))),
    )]
}

/// One rule's pass, under its sweep lock. `Ok(None)`: the rule was skipped
/// (its lock was held, or it was gone, disabled or unapproved when re-read)
/// and the result carries no entry for it.
fn sweep_one(
    root: &Path,
    entry: &RuleEntry,
    now: &dyn Fn() -> u64,
    fire: &Fire<'_>,
) -> Result<Option<RuleSweep>, EngineError> {
    // The name reaches the lock files: one rule name, checked first.
    let name = apb_core::trigger::checked_rule_name(&entry.rule.name)
        .map_err(store_error)?
        .to_string();
    let lock_dir = root.join(".apb/trigger-state");
    // One holder per rule (I4): a held lock skips the rule, the next holder
    // of the lock is the next pass.
    let _sweep = match apb_core::fsutil::try_hold_lock(&lock_dir, &format!("{name}.sweep.lock")) {
        Ok(lock) => lock,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    // Re-read under the lock: gone, disabled or unapproved means untouched.
    let rules = read_signed::<RulesFile>(root, "rules").map_err(store_error)?;
    let Some(entry) = rules.rules.iter().find(|e| e.rule.name == name) else {
        return Ok(None);
    };
    let Some(approval) = entry
        .approval
        .as_ref()
        .filter(|a| a.rule_digest == rule_digest(entry))
    else {
        return Ok(None);
    };
    if !entry.enabled {
        return Ok(None);
    }
    let incarnation = entry.incarnation.clone();
    let digest = rule_digest(entry);

    // The journal ids under the sweep lock (the file only grows under it);
    // this is also where the rewrite to the newest LOG_KEEP lines happens.
    // A journal that is refused (over the read cap, a link, not a regular
    // file) stops the rule for this pass: no ids are substituted, nothing
    // is evaluated and nothing starts; the refusal is this pass's answer
    // for the rule, and other rules go on.
    let mut ids = journal_ids(root, &incarnation).map_err(|e| {
        EngineError::Invalid(apb_core::trigger::journal_refused_text(
            &name,
            &incarnation,
            &e,
        ))
    })?;
    // The state as the previous holder of this lock committed it (I4, I5):
    // read here, under the sweep lock, never before it.
    let state = read_signed::<TriggerStateFile>(root, "state").map_err(store_error)?;
    // The position (I5), line first (I2): a state missing, removed after it
    // did not verify, or of another incarnation positions the rule at the
    // head the pass reads, and starts nothing for it in this pass. The
    // transaction positions only while that is still so.
    if needs_position(&state, entry) {
        let source = inbox_source(entry);
        let head = Inbox::open(&source.connector, &source.account)
            .map_err(inbox_error)?
            .head_seq()
            .map_err(inbox_error)?;
        position_at_head(root, entry, head, now()).map_err(store_error)?;
        return Ok(Some(RuleSweep::default()));
    }
    let seen_through = state
        .rules
        .iter()
        .find(|s| s.rule == name && s.incarnation == incarnation)
        .map(|s| s.seen_through)
        .unwrap_or(0);

    // The rule's source, read outside every transaction (lock order 5);
    // `read_events` registers nothing (I5). A read error is printed and the
    // pass ends with nothing logged.
    let source = inbox_source(entry);
    let inbox = match Inbox::open(&source.connector, &source.account) {
        Ok(inbox) => inbox,
        Err(e) => {
            eprintln!("trigger sweep: {name}: the source cannot be opened: {e}");
            return Err(inbox_error(e));
        }
    };
    let events = match inbox.read_events() {
        Ok(events) => events,
        Err(e) => {
            eprintln!("trigger sweep: {name}: the source cannot be read: {e}");
            return Err(inbox_error(e));
        }
    };
    let new_events: Vec<&InboxEvent> = events
        .iter()
        .filter(|e| e.seq > seen_through)
        .take(MAX_EVENTS_PER_PASS)
        .collect();
    let state_source = SourceId {
        connector: source.connector.clone(),
        account: source.account.clone(),
    };

    // A gap (I5): retention removed events the rule had not seen. One gap
    // line with the range, then one state transaction to the last kept
    // event's predecessor; the pass continues from the oldest kept event.
    if let Some(oldest) = new_events.first()
        && oldest.seq > seen_through + 1
    {
        let gap = LogEntry::for_gap(&incarnation, now(), seen_through + 1, oldest.seq - 1);
        append(root, &incarnation, &mut ids, &[gap]).map_err(EngineError::Io)?;
        advance(root, &name, &incarnation, &state_source, oldest.seq - 1)?;
    }

    let own = |run: &str| own_run(root, run);
    let mut report = RuleSweep::default();
    let mut batch: Vec<LogEntry> = Vec::new();
    let starts_this_pass = std::cell::Cell::new(0u32);
    for event in new_events {
        report.evaluated += 1;
        let seq = event.seq;
        let (verdict, conditions) =
            apb_core::trigger::evaluate_event(&entry.rule, &source, event, &incarnation, &own);
        let start_key = match verdict {
            apb_core::trigger::EventVerdict::Skipped { code, name: about } => {
                let mut line = LogEntry::for_event(
                    &incarnation,
                    seq,
                    now(),
                    Verdict::Skipped,
                    Some(code),
                    about.as_deref(),
                );
                line.conditions = condition_marks(&conditions);
                batch.push(line);
                continue;
            }
            apb_core::trigger::EventVerdict::Matched { start_key } => start_key,
        };

        // 1. The change lock (I4): a pass yields to a mutation after its
        // current start. A probe that fails is treated as held.
        match apb_core::fsutil::flock_held(&lock_dir, &format!("{name}.change.lock")) {
            Ok(false) => {}
            Ok(true) | Err(_) => {
                if let Err(early) = flush(
                    root,
                    &incarnation,
                    &state_source,
                    &mut ids,
                    &mut batch,
                    &name,
                ) {
                    return Ok(Some(early));
                }
                report.stopped = Some("change_waiting".to_string());
                return Ok(Some(report));
            }
        }
        // 2. The entry again, right before the start (I4). Under the sweep
        // lock it never differs; this read is what shows it does not.
        let again = read_signed::<RulesFile>(root, "rules").map_err(store_error)?;
        let current = again.rules.iter().find(|e| e.rule.name == name);
        let still = current.is_some_and(|e| {
            e.enabled
                && e.incarnation == incarnation
                && rule_digest(e) == digest
                && e.approval.as_ref().is_some_and(|a| a.rule_digest == digest)
        });
        if !still {
            if let Err(early) = flush(
                root,
                &incarnation,
                &state_source,
                &mut ids,
                &mut batch,
                &name,
            ) {
                return Ok(Some(early));
            }
            return Ok(Some(report));
        }
        let entry = current.unwrap();
        // 3. The rule checks again (the playbook or the connector may have
        // changed): a refusal is the event's verdict and the next event
        // follows, its position moving past.
        let issues = check_rule(
            &entry.rule,
            &apb_core::trigger::RuleContext::load(root, &entry.rule),
        );
        let errors: Vec<&apb_core::validate::Issue> = issues
            .iter()
            .filter(|i| i.severity == apb_core::validate::Severity::Error)
            .collect();
        if !errors.is_empty() {
            let mut line = LogEntry::for_event(
                &incarnation,
                seq,
                now(),
                Verdict::Refused,
                Some("check_rule"),
                None,
            );
            line.codes = errors.iter().map(|i| i.code.to_string()).collect();
            line.conditions = condition_marks(&conditions);
            batch.push(line);
            continue;
        }
        // 4. The position never passes a match that did not start (I2).
        if let Err(early) = flush(
            root,
            &incarnation,
            &state_source,
            &mut ids,
            &mut batch,
            &name,
        ) {
            return Ok(Some(early));
        }
        // 5. The limits on new starts, asked by the fire once the start
        // key said the event starts a new run (deduplication before
        // limits): the hourly cap (I3), read before every new start so a
        // run any earlier outcome of this pass left behind counts at once
        // (unreadable is reached, never free), then the pass's own start
        // bound, a fairness bound between rules.
        let max_per_hour = entry.rule.limits.max_per_hour;
        let admit = || -> Result<(), String> {
            let counted = match starts_in_last_hour(root, &incarnation, Millis(now())) {
                Ok(counted) => counted,
                Err(e) => {
                    eprintln!("trigger sweep: {name}: the runs cannot be counted: {e}");
                    COUNT_UNKNOWN
                }
            };
            if counted >= max_per_hour {
                return Err(if counted == COUNT_UNKNOWN {
                    "rate_cap_unknown".to_string()
                } else {
                    "rate_cap".to_string()
                });
            }
            if starts_this_pass.get() >= MAX_STARTS_PER_PASS as u32 {
                return Err("starts_per_pass".to_string());
            }
            Ok(())
        };
        // 6. The start, with only the sweep lock held (lock order 7).
        let outcome = fire(&FireRequest {
            root,
            entry,
            source: &source,
            event,
            start_key: start_key.expose().to_string(),
            triggered_by: TriggeredBy {
                rule: name.clone(),
                incarnation: incarnation.clone(),
                source: "inbox".to_string(),
                connector: Some(source.connector.clone()),
                account: Some(source.account.clone()),
                event_seq: Some(seq),
            },
            dry_run: false,
            admit: &admit,
        });
        // 7. The outcome (I2): the line is appended and synced, then one
        // state transaction moves the position past the event. `Busy`, a
        // limit and a changed binding leave the position before the event
        // and end the pass.
        match outcome {
            FireOutcome::Limited { code } if code == "starts_per_pass" => {
                report.stopped = Some(code);
                return Ok(Some(report));
            }
            FireOutcome::Limited { code } => {
                let line =
                    LogEntry::for_event(&incarnation, seq, now(), Verdict::Held, Some(&code), None);
                report.stopped = Some(if hold_line(root, &incarnation, &mut ids, &line, &name) {
                    "rate_cap".to_string()
                } else {
                    "journal_write".to_string()
                });
                return Ok(Some(report));
            }
            FireOutcome::Busy => {
                let line = LogEntry::for_event(
                    &incarnation,
                    seq,
                    now(),
                    Verdict::Held,
                    Some("start_key_busy"),
                    None,
                );
                report.stopped = Some(if hold_line(root, &incarnation, &mut ids, &line, &name) {
                    "start_key_busy".to_string()
                } else {
                    "journal_write".to_string()
                });
                return Ok(Some(report));
            }
            FireOutcome::BindingChanged { binding } => {
                // D1 "Approval" item 5, in this order: the line, then the
                // revocation, then the end of the pass, the position before
                // the event.
                let line = LogEntry::for_binding_changed(
                    &incarnation,
                    seq,
                    now(),
                    &binding,
                    &approval.binding,
                );
                if let Err(e) = append(root, &incarnation, &mut ids, &[line]) {
                    eprintln!("trigger sweep: {name}: the journal cannot be written: {e}");
                    report.stopped = Some("journal_write".to_string());
                    return Ok(Some(report));
                }
                revoke_approval_locked(root, &name, &incarnation, &digest, &approval.binding)
                    .map_err(store_error)?;
                return Ok(Some(report));
            }
            FireOutcome::Started { run_id } => {
                let mut line =
                    LogEntry::for_event(&incarnation, seq, now(), Verdict::Started, None, None);
                line.run_id = Some(run_id.clone());
                if settle(
                    root,
                    &incarnation,
                    &state_source,
                    &mut ids,
                    &[line],
                    &name,
                    seq,
                )
                .is_err()
                {
                    report.stopped = Some("journal_write".to_string());
                    return Ok(Some(report));
                }
                starts_this_pass.set(starts_this_pass.get() + 1);
                report.started.push(run_id);
            }
            FireOutcome::AlreadyStarted { run_id, conflict } => {
                let mut line = LogEntry::for_event(
                    &incarnation,
                    seq,
                    now(),
                    Verdict::AlreadyStarted,
                    conflict.then_some("start_key_conflict"),
                    None,
                );
                line.run_id = Some(run_id.clone());
                if settle(
                    root,
                    &incarnation,
                    &state_source,
                    &mut ids,
                    &[line],
                    &name,
                    seq,
                )
                .is_err()
                {
                    report.stopped = Some("journal_write".to_string());
                    return Ok(Some(report));
                }
            }
            FireOutcome::Refused { code, name: about } => {
                let line = LogEntry::for_event(
                    &incarnation,
                    seq,
                    now(),
                    Verdict::Refused,
                    Some(&code),
                    about.as_deref(),
                );
                if settle(
                    root,
                    &incarnation,
                    &state_source,
                    &mut ids,
                    &[line],
                    &name,
                    seq,
                )
                .is_err()
                {
                    report.stopped = Some("journal_write".to_string());
                    return Ok(Some(report));
                }
            }
            FireOutcome::Failed { code } => {
                let line = LogEntry::for_event(
                    &incarnation,
                    seq,
                    now(),
                    Verdict::Failed,
                    Some(&code),
                    None,
                );
                if settle(
                    root,
                    &incarnation,
                    &state_source,
                    &mut ids,
                    &[line],
                    &name,
                    seq,
                )
                .is_err()
                {
                    report.stopped = Some("journal_write".to_string());
                    return Ok(Some(report));
                }
            }
            // PreStartPassed is impossible with dry_run false; recorded
            // defensively as a failed start.
            FireOutcome::PreStartPassed => {
                let line = LogEntry::for_event(
                    &incarnation,
                    seq,
                    now(),
                    Verdict::Failed,
                    Some("start_failed"),
                    None,
                );
                if settle(
                    root,
                    &incarnation,
                    &state_source,
                    &mut ids,
                    &[line],
                    &name,
                    seq,
                )
                .is_err()
                {
                    report.stopped = Some("journal_write".to_string());
                    return Ok(Some(report));
                }
            }
        }
    }
    // 8. The open batch, then the lock goes with `_sweep`.
    if let Err(early) = flush(
        root,
        &incarnation,
        &state_source,
        &mut ids,
        &mut batch,
        &name,
    ) {
        return Ok(Some(early));
    }
    Ok(Some(report))
}

/// The count no cap stays under: the scan could not read what it needed
/// (strictly, never loosely).
const COUNT_UNKNOWN: u32 = u32::MAX;

/// The condition marks a journal line keeps: per condition, its index and
/// whether it passed, never the literal nor the event's value.
fn condition_marks(
    conditions: &[apb_core::connector::event_filter::ConditionResult],
) -> Vec<ConditionMark> {
    conditions
        .iter()
        .enumerate()
        .map(|(index, c)| ConditionMark {
            index: index as u32,
            passed: c.passed,
        })
        .collect()
}

/// Appends one `held` line without moving the position; `true` when the
/// append and its sync went through.
fn hold_line(
    root: &Path,
    incarnation: &str,
    ids: &mut HashSet<String>,
    line: &LogEntry,
    name: &str,
) -> bool {
    match append(root, incarnation, ids, std::slice::from_ref(line)) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("trigger sweep: {name}: the journal cannot be written: {e}");
            false
        }
    }
}

/// Appends the open batch and moves the position to its last seq (I2): the
/// lines are durable before the transaction. An append error ends the pass
/// with the position where it was.
fn flush(
    root: &Path,
    incarnation: &str,
    source: &SourceId,
    ids: &mut HashSet<String>,
    batch: &mut Vec<LogEntry>,
    name: &str,
) -> Result<(), RuleSweep> {
    if batch.is_empty() {
        return Ok(());
    }
    let lines = std::mem::take(batch);
    let to = lines.last().and_then(|l| l.seq).unwrap_or(0);
    if let Err(e) = append(root, incarnation, ids, &lines) {
        eprintln!("trigger sweep: {name}: the journal cannot be written: {e}");
        return Err(RuleSweep {
            stopped: Some("journal_write".to_string()),
            ..RuleSweep::default()
        });
    }
    advance(root, name, incarnation, source, to).map_err(|e| {
        eprintln!("trigger sweep: {name}: the position cannot move: {e}");
        RuleSweep {
            stopped: Some("journal_write".to_string()),
            ..RuleSweep::default()
        }
    })?;
    Ok(())
}

/// One verdict's line appended and synced, then one state transaction to
/// `seq` (I2). An error leaves the position where it was.
fn settle(
    root: &Path,
    incarnation: &str,
    source: &SourceId,
    ids: &mut HashSet<String>,
    lines: &[LogEntry],
    name: &str,
    seq: u64,
) -> Result<(), ()> {
    if let Err(e) = append(root, incarnation, ids, lines) {
        eprintln!("trigger sweep: {name}: the journal cannot be written: {e}");
        return Err(());
    }
    advance(root, name, incarnation, source, seq).map_err(|e| {
        eprintln!("trigger sweep: {name}: the position cannot move: {e}");
    })
}

/// One state transaction moving the rule's position to `to` (I5): only the
/// state file, never a start, never an inbox read, never a journal append
/// (lock order 4).
fn advance(
    root: &Path,
    name: &str,
    incarnation: &str,
    source: &SourceId,
    to: u64,
) -> Result<(), EngineError> {
    let project = apb_core::projects::registered_id(root);
    apb_core::signed_store::transact(
        root,
        Scope::Project,
        "state",
        apb_core::signed_store::Unsigned::Refuse,
        |file: &mut TriggerStateFile| -> Result<((), bool), TriggerError> {
            if let Some(id) = project.clone() {
                file.project = id;
            }
            let changed = match file
                .rules
                .iter_mut()
                .find(|s| s.rule == name && s.incarnation == incarnation)
            {
                Some(state) => {
                    if state.seen_through < to {
                        state.seen_through = to;
                        true
                    } else {
                        false
                    }
                }
                None => {
                    file.rules.push(RuleState {
                        rule: name.to_string(),
                        incarnation: incarnation.to_string(),
                        source: source.clone(),
                        seen_through: to,
                    });
                    true
                }
            };
            Ok(((), changed))
        },
    )
    .map_err(store_error)?;
    Ok(())
}

/// The rule's inbox source. A later source kind gets its own arm here.
fn inbox_source(entry: &RuleEntry) -> InboxSource {
    match &entry.rule.source {
        RuleSource::Inbox(source) => source.clone(),
    }
}

/// Whether a marker's `run` names a run of this project this installation
/// created: one safe path segment, a real directory (no link) directly
/// under this project's runs directory, carrying a verified origin stamp.
/// The one own-run predicate the sweep and the dry test share; the value
/// comes from an event, so it is checked before it becomes a path.
pub fn own_run(root: &Path, run: &str) -> bool {
    if !apb_core::registry::is_safe_segment(run)
        || std::path::Path::new(run).components().count() != 1
    {
        return false;
    }
    let dir = runs_dir(root).join(run);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        _ => return false,
    }
    apb_core::run_origin::verify(&dir, run)
}

fn inbox_error(e: InboxError) -> EngineError {
    EngineError::Invalid(format!("the rule's source cannot be read: {e}"))
}

/// A store or rule error as an engine error; the text names the store's
/// own reason (never a value).
fn store_error(e: TriggerError) -> EngineError {
    EngineError::Invalid(format!("trigger store: {e}"))
}

fn runs_dir(root: &Path) -> PathBuf {
    root.join(".apb").join("runs")
}

/// The runs the incarnation started in the last hour, read from the runs'
/// own launch records. `Err` when the runs directory cannot be read: the
/// cap counts as reached, never as free. A scan truncated by
/// [`crate::run_view::STARTED_RUNS_SCAN`] answers [`COUNT_UNKNOWN`], which
/// no cap stays under: stricter, never looser (I3).
pub fn starts_in_last_hour(root: &Path, incarnation: &str, now: Millis) -> io::Result<u32> {
    let now = now.0;
    let dirs = run_dirs_strict(root)?;
    // Newest journal write first; a run started in the hour wrote its
    // journal in the hour.
    let mut stamped: Vec<(u64, PathBuf)> = Vec::new();
    for dir in dirs {
        match std::fs::metadata(dir.join("events.jsonl")) {
            Ok(m) => {
                let age_ms = m
                    .modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .map(|e| e.as_millis() as u64);
                match age_ms {
                    Some(age) if age <= HOUR_MS => stamped.push((now.saturating_sub(age), dir)),
                    // A journal whose age cannot be read stays in the hour:
                    // counting it is the strict direction.
                    Some(_) => {}
                    None => stamped.push((now, dir)),
                }
            }
            // No journal, or one whose metadata does not read: nothing
            // tells when it was written, so it stays in the window and
            // reaches the classification. Only its remaining records
            // (no manifest, a record of another incarnation or of no
            // rule) leave it out; a record of this incarnation counts
            // (strict, I3).
            Err(_) => stamped.push((now, dir)),
        }
    }
    stamped.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    if stamped.len() > crate::run_view::STARTED_RUNS_SCAN {
        return Ok(COUNT_UNKNOWN);
    }
    let hour_ago = now.saturating_sub(HOUR_MS);
    let mut count: u32 = 0;
    for (_, dir) in stamped {
        match classify(dir, incarnation, hour_ago) {
            Count::Counts => count += 1,
            Count::Excluded => {}
        }
    }
    Ok(count)
}

/// One hour in milliseconds, the window of `max_per_hour`.
const HOUR_MS: u64 = 60 * 60 * 1000;

/// How one run directory of the last hour sits toward this incarnation's
/// cap (design D1 "One pass over one rule" item 6.2.1).
enum Count {
    /// This incarnation started it in the hour, or nobody can tell whose
    /// it was: counted.
    Counts,
    /// Not a start of any rule, or a start of another rule or of another
    /// creation of this one, or a start this incarnation made before the
    /// hour began: not counted.
    Excluded,
}

/// Classifies one run directory of the last hour.
fn classify(dir: PathBuf, incarnation: &str, hour_ago: u64) -> Count {
    // The origin stamp decides whether this installation created the run;
    // a stamp that does not verify is unattributable (counted).
    let id = match dir.file_name().and_then(|n| n.to_str()) {
        Some(id) => id.to_string(),
        None => return Count::Counts,
    };
    if !apb_core::run_origin::verify(&dir, &id) {
        return Count::Counts;
    }
    // A stamped directory with no manifest file at all is excluded: its
    // remaining records prove no run started there, since a start writes
    // its manifest, launch record included, before any work (a failed
    // preparation's `prep_try_unstarted` leaves exactly this), and
    // counting one would let a single failed start hold every rule.
    let manifest = match crate::manifest::read(&dir) {
        Ok(Some(m)) => m,
        Ok(None) => return Count::Excluded,
        Err(_) => return Count::Counts,
    };
    let Some(launch) = manifest.launch else {
        // A manifest that reads with no launch record cannot say whose run
        // it is: counted (the strict direction).
        return Count::Counts;
    };
    // A child of a run, by its record's kind or by the parent its run
    // config names, is never a rule's start.
    if launch.kind == "child" {
        return Count::Excluded;
    }
    match crate::run_config::read_run_config(&dir) {
        Ok(cfg) if cfg.parent_run.is_some() => return Count::Excluded,
        Ok(_) => {}
        Err(_) => return Count::Counts,
    }
    let Some(by) = launch.triggered_by else {
        // A rule's record that lost `triggered_by` (an older apb's
        // `manifest::amend` keeps the known string field `initiator`) is
        // unattributable: counted toward the cap of every rule.
        if launch.initiator.starts_with("trigger:") {
            return Count::Counts;
        }
        return Count::Excluded;
    };
    if by.incarnation != incarnation {
        // Another creation of a rule (or another rule): its own cap counts
        // it, this one does not.
        return Count::Excluded;
    }
    // Attributed: counted when its run started within the hour (a run
    // started before it that is still writing sits in the window by its
    // journal's mtime alone).
    match crate::run_history::started(&dir) {
        Some(head) if head.ts as u64 >= hour_ago => Count::Counts,
        Some(_) => Count::Excluded,
        // No readable `run_started` under a record that names this
        // incarnation: counted (strict).
        None => Count::Counts,
    }
}

/// Every run directory of the project, enumerated fallibly: a directory
/// that cannot be listed, or an entry whose metadata cannot be read, is an
/// error, never a silent empty answer.
pub fn run_dirs_strict(root: &Path) -> io::Result<Vec<PathBuf>> {
    let runs = runs_dir(root);
    let entries = match std::fs::read_dir(&runs) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry?;
        // The entry itself, links unfollowed: only real directories.
        if entry.metadata()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

use crate::EngineError;
