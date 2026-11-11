//! Routine health (D2): the rules a watched playbook is judged by across
//! its runs, evaluated after every run end, by `apb health check`, by
//! `apb resume --all` and by the dashboard; a trip and a recovery each
//! send one notice through the machine's notify target, claimed and
//! settled in the playbook's signed health file; `on_trip: hold` refuses
//! starts until a person clears it.

use std::path::Path;

use apb_core::scope::Scope;

use crate::error::EngineError;

pub mod facts;
pub mod state;
pub mod view;

pub use state::{hold_refusal, reference};
pub use view::{HealthRuleView, HealthView, HoldView};

/// How old a claim may be before its sender is taken for dead.
const STALE_CLAIM_MS: u64 = 60_000;
/// The most tries a failed notice gets.
const MAX_TRIES: u32 = 3;

fn key(scope: Scope, id: &str) -> apb_core::policy_file::PlaybookKey<'_> {
    apb_core::policy_file::PlaybookKey {
        id,
        global: scope == Scope::Global,
    }
}

/// The `current` version of playbook `id` in `scope`.
fn current_playbook(root: &Path, scope: Scope, id: &str) -> Option<apb_core::schema::Playbook> {
    let parent = match scope {
        Scope::Project => root.join(".apb"),
        Scope::Global => apb_core::store::global_playbooks_parent()?,
    };
    apb_core::registry::Registry::open_dir(&parent)
        .ok()?
        .load(id, None)
        .ok()
        .map(|l| l.playbook)
}

/// The block of a watched playbook: its `current` version has a
/// `health:` block and the machine's and the project's `health.playbooks`
/// pass it. A `policy.yaml` that does not read watches nothing.
fn watched(root: &Path, scope: Scope, id: &str) -> Option<apb_core::health::HealthBlock> {
    let block = current_playbook(root, scope, id)?.health?;
    apb_core::policy_file::Policies::load(root)
        .ok()?
        .health_watches(&key(scope, id))
        .then_some(block)
}

fn ms_of(duration: Option<&str>, default: &str) -> u64 {
    apb_core::duration::parse_duration_str(duration.unwrap_or(default))
        .or_else(|| apb_core::duration::parse_duration_str(default))
        .unwrap_or(0)
        .saturating_mul(1000)
}

fn store_err(e: apb_core::signed_store::StoreError) -> EngineError {
    match e {
        apb_core::signed_store::StoreError::Io(e) => EngineError::Io(e),
        other => EngineError::Invalid(other.to_string()),
    }
}

fn is_open(n: &state::NoticeRecord) -> bool {
    matches!(n.state.as_str(), "pending" | "claimed")
        || (n.state == "failed" && n.tries < MAX_TRIES)
}

/// Rows 6: every open notice retired.
fn retire_open(f: &mut state::HealthFile) {
    for n in f.notices.iter_mut().filter(|n| is_open(n)) {
        n.state = "retired".into();
    }
}

fn muted(f: &state::HealthFile, now_ms: u64) -> bool {
    f.muted_until_ms.is_some_and(|m| m > now_ms)
}

/// One claimed notice this process sends.
struct Claim {
    id: String,
    generation: u64,
    record: state::NoticeRecord,
}

/// How long an evaluation waits for another evaluation of the same
/// playbook, which reads every run journal of it.
const EVALUATION_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The evaluation lock of one playbook, beside its state file.
fn hold_evaluation(
    root: &Path,
    scope: Scope,
    id: &str,
) -> Result<apb_core::fsutil::HeldLock, EngineError> {
    if !apb_core::registry::is_safe_segment(id) {
        return Err(EngineError::Invalid(format!("playbook id `{id}`")));
    }
    let dir = apb_core::signed_store::dir(
        root,
        <state::HealthFile as apb_core::signed_store::SignedDoc>::DIR,
        scope,
    );
    Ok(apb_core::fsutil::hold_lock_waiting(
        &dir,
        &format!("{id}.evaluation.lock"),
        EVALUATION_WAIT,
    )?)
}

/// A transaction on the playbook's file under `Unsigned::Refuse`.
fn transact<R>(
    root: &Path,
    scope: Scope,
    id: &str,
    change: impl FnOnce(&mut state::HealthFile) -> (R, bool),
) -> Result<R, apb_core::signed_store::StoreError> {
    apb_core::signed_store::transact(
        root,
        scope,
        id,
        apb_core::signed_store::Unsigned::Refuse,
        |f: &mut state::HealthFile| Ok::<_, apb_core::signed_store::StoreError>(change(f)),
    )
}

/// Records a notice (row 1) when a target exists; its id, or `None`.
fn record_notice(
    f: &mut state::HealthFile,
    ctx: &NoticeCtx<'_>,
    rule: &str,
    kind: &str,
    detail: &str,
    held: bool,
) -> Option<String> {
    let target = ctx.target.clone()?;
    let id = format!("h-{}", apb_core::connector::marker::new_nonce());
    let notice =
        crate::notify::notice::health_notice(ctx.id, rule, kind, detail, held, &ctx.reference);
    f.notices.push(state::NoticeRecord {
        id: id.clone(),
        rule: rule.to_string(),
        kind: kind.to_string(),
        recorded_ms: ctx.now_ms,
        subject: ctx.scrubber.scrub(&notice.subject),
        text: ctx.scrubber.scrub(&notice.text),
        target,
        state: "pending".into(),
        claimant: None,
        claimed_ms: None,
        claim_generation: None,
        tries: 0,
        detail: None,
    });
    Some(id)
}

struct NoticeCtx<'a> {
    id: &'a str,
    reference: String,
    target: Option<crate::notify::record::NotifyTargetRecord>,
    scrubber: apb_core::scrub::Scrubber,
    now_ms: u64,
}

/// Phase 1 under the file's lock: the rule states, the notices to record,
/// the hold, the stale claims, and this process's claims.
fn phase_one(
    f: &mut state::HealthFile,
    block: &apb_core::health::HealthBlock,
    facts: &[apb_core::health::RunFact],
    ctx: &NoticeCtx<'_>,
) -> Vec<Claim> {
    let now = ctx.now_ms;
    if f.first_seen_ms == 0 {
        f.first_seen_ms = now;
    }
    let quiet = muted(f, now);
    let held_on_trip = block.on_trip == apb_core::health::OnTrip::Hold;
    let cooldown = ms_of(
        block.cooldown.as_deref(),
        apb_core::health::DEFAULT_COOLDOWN,
    );
    let verdicts = apb_core::health::evaluate(
        block,
        facts,
        f.counted_since_ms,
        f.counted_since_ms.unwrap_or(f.first_seen_ms),
        now,
    );
    for v in &verdicts {
        let name = v.rule.as_str().to_string();
        let mut st = f.rules.remove(&name).unwrap_or_default();
        let notice_state = |f: &state::HealthFile, id: &Option<String>| {
            id.as_ref()
                .and_then(|id| f.notices.iter().find(|n| &n.id == id))
                .map(|n| n.state.clone())
        };
        match (st.tripped, v.tripped) {
            (false, true) => {
                st.tripped = true;
                st.since_ms = now;
                st.detail = v.detail.clone();
                st.trip_notice = None;
            }
            (true, true) => st.detail = v.detail.clone(),
            (true, false) => {
                let trip_state = notice_state(f, &st.trip_notice);
                if !quiet && trip_state.as_deref() == Some("sent") {
                    let detail = format!("{}: no longer tripped", v.rule.as_str());
                    record_notice(f, ctx, &name, "recovery", &detail, false);
                }
                if let Some(tid) = &st.trip_notice
                    && let Some(n) = f.notices.iter_mut().find(|n| &n.id == tid)
                    && is_open(n)
                {
                    n.state = "retired".into();
                }
                st.tripped = false;
                st.trip_notice = None;
                st.detail = v.detail.clone();
            }
            (false, false) => st.detail = v.detail.clone(),
        }
        if st.tripped {
            let retired = notice_state(f, &st.trip_notice).as_deref() == Some("retired");
            let due = st
                .delivered_ms
                .is_none_or(|d| now.saturating_sub(d) >= cooldown);
            if (st.trip_notice.is_none() || retired) && !quiet && due {
                let held = held_on_trip || f.hold.is_some();
                if let Some(nid) = record_notice(f, ctx, &name, "trip", &st.detail, held) {
                    st.trip_notice = Some(nid);
                }
            }
        }
        f.rules.insert(name, st);
    }
    if held_on_trip && f.hold.is_none() {
        let first = verdicts.iter().find(|v| v.tripped);
        if let Some(v) = first {
            f.hold = Some(state::Hold {
                since_ms: now,
                rule: v.rule.as_str().to_string(),
                detail: v.detail.clone(),
            });
        }
    }
    let nonce = state::process_nonce();
    let generation = f.generation;
    let mut claims = Vec::new();
    for n in f.notices.iter_mut() {
        if n.state == "claimed"
            && n.claimed_ms
                .is_some_and(|c| now.saturating_sub(c) > STALE_CLAIM_MS)
        {
            n.state = "unknown".into();
            n.detail = Some("its sender stopped during the send".into());
            continue;
        }
        let claimable = n.state == "pending" || (n.state == "failed" && n.tries < MAX_TRIES);
        if claimable && !quiet {
            n.state = "claimed".into();
            n.claimant = Some(nonce.to_string());
            n.claimed_ms = Some(now);
            n.claim_generation = Some(generation);
            claims.push(Claim {
                id: n.id.clone(),
                generation,
                record: n.clone(),
            });
        }
    }
    f.evaluated_at_ms = now;
    state::trim_ledger(&mut f.notices);
    claims
}

/// Whether the record is still this process's claim of `generation`.
fn still_ours(f: &state::HealthFile, claim: &Claim) -> bool {
    f.generation == claim.generation
        && f.notices.iter().any(|n| {
            n.id == claim.id
                && n.state == "claimed"
                && n.claimant.as_deref() == Some(state::process_nonce())
                && n.claim_generation == Some(claim.generation)
        })
}

/// Phases 2 and 3 of one claimed notice.
fn dispatch(root: &Path, scope: Scope, id: &str, claim: Claim, now_ms: u64) {
    #[cfg(any(test, feature = "testing"))]
    crate::deadline_hooks::at(
        crate::deadline_hooks::Hook::HealthBeforeDispatch,
        &root.join(".apb").join("health"),
    );
    let go = transact(root, scope, id, |f| {
        (still_ours(f, &claim) && !muted(f, now_ms), false)
    })
    .unwrap_or(false);
    if !go {
        return;
    }
    let notice = crate::notify::notice::Notice {
        subject: claim.record.subject.clone(),
        text: claim.record.text.clone(),
    };
    let outcome = crate::notify::health::send(root, &claim.record.target, &notice);
    #[cfg(any(test, feature = "testing"))]
    crate::deadline_hooks::at(
        crate::deadline_hooks::Hook::HealthBeforeSettle,
        &root.join(".apb").join("health"),
    );
    use crate::notify::health::SendOutcome;
    let late = match &outcome {
        SendOutcome::Sent => "sent".to_string(),
        SendOutcome::NotSent(why) => format!("not sent: {why}"),
        SendOutcome::Unknown(why) => format!("unknown: {why}"),
    };
    let settled = transact(root, scope, id, |f| {
        if still_ours(f, &claim) {
            let Some(n) = f.notices.iter_mut().find(|n| n.id == claim.id) else {
                return ((), false);
            };
            match &outcome {
                SendOutcome::Sent => n.state = "sent".into(),
                SendOutcome::NotSent(why) => {
                    n.state = "failed".into();
                    n.tries += 1;
                    n.detail = Some(why.clone());
                }
                SendOutcome::Unknown(why) => {
                    n.state = "unknown".into();
                    n.detail = Some(why.clone());
                }
            }
            n.claimant = None;
            if outcome == SendOutcome::Sent && n.kind == "trip" {
                let rule = n.rule.clone();
                if let Some(r) = f.rules.get_mut(&rule)
                    && r.trip_notice.as_deref() == Some(claim.id.as_str())
                {
                    r.delivered_ms = Some(now_ms);
                }
            }
            ((), true)
        } else if let Some(n) = f
            .notices
            .iter_mut()
            .find(|n| n.id == claim.id && n.state == "retired")
        {
            n.detail = Some(format!("outcome after retirement: {late}"));
            ((), true)
        } else {
            ((), false)
        }
    });
    if let Err(e) = settled {
        eprintln!("apb: health of `{id}`: the notice outcome was not recorded: {e}");
    }
}

/// Evaluates playbook `id` now: rule states, the hold, the notices, and
/// their sends. An unwatched playbook is only read. `now` is the clock,
/// read once the evaluation lock is held.
pub fn evaluate(
    root: &Path,
    scope: Scope,
    id: &str,
    now: &dyn Fn() -> u64,
) -> Result<HealthView, EngineError> {
    let Some(block) = watched(root, scope, id) else {
        return Ok(view(root, scope, id, now())?.unwrap_or_else(|| unwatched(scope, id)));
    };
    // One evaluation of a playbook at a time, from its facts to its state:
    // an evaluation that read the facts before another run ended never
    // applies them over the state a later evaluation made from newer ones.
    #[cfg(any(test, feature = "testing"))]
    crate::deadline_hooks::at(
        crate::deadline_hooks::Hook::HealthBeforeLock,
        &root.join(".apb").join("health"),
    );
    let evaluation = hold_evaluation(root, scope, id)?;
    // The time is read once the lock is held: an evaluation that waited
    // never measures at a time older than the one that went before it.
    let now_ms = now();
    let facts = facts::facts(root, scope, id)?;
    #[cfg(any(test, feature = "testing"))]
    crate::deadline_hooks::at(
        crate::deadline_hooks::Hook::HealthAfterFacts,
        &root.join(".apb").join("health"),
    );
    let ctx = NoticeCtx {
        id,
        reference: reference(scope, id),
        target: crate::notify::health::health_target(root, &key(scope, id)),
        scrubber: apb_core::scrub::Scrubber::for_workspace(root, Vec::new()),
        now_ms,
    };
    let claims = match transact(root, scope, id, |f| {
        (phase_one(f, &block, &facts, &ctx), true)
    }) {
        Ok(claims) => claims,
        Err(apb_core::signed_store::StoreError::Unverified(path)) => {
            return Ok(unreadable_view(scope, id, &path));
        }
        Err(e) => return Err(store_err(e)),
    };
    drop(evaluation);
    for claim in claims {
        dispatch(root, scope, id, claim, now_ms);
    }
    Ok(view(root, scope, id, now_ms)?.unwrap_or_else(|| unwatched(scope, id)))
}

fn unwatched(scope: Scope, id: &str) -> HealthView {
    HealthView {
        playbook: id.to_string(),
        scope: scope.as_str().to_string(),
        status: "unwatched".into(),
        rules: Vec::new(),
        hold: None,
        muted_until: None,
        last_notice: None,
    }
}

fn unreadable_view(scope: Scope, id: &str, path: &Path) -> HealthView {
    HealthView {
        playbook: id.to_string(),
        scope: scope.as_str().to_string(),
        status: "held".into(),
        rules: Vec::new(),
        hold: Some(HoldView {
            since: String::new(),
            rule: "unreadable".into(),
            detail: format!("health state unreadable: {}", path.display()),
            clear: format!("apb health clear {}", reference(scope, id)),
        }),
        muted_until: None,
        last_notice: None,
    }
}

/// The ids of every project playbook and every global playbook, each
/// with its scope.
fn playbooks(root: &Path) -> Vec<(Scope, String)> {
    let mut out = Vec::new();
    let mut ids = |scope: Scope, parent: Option<std::path::PathBuf>| {
        let Some(parent) = parent else { return };
        if let Ok(reg) = apb_core::registry::Registry::open_dir(&parent) {
            for id in reg.playbook_ids() {
                out.push((scope, id));
            }
        }
    };
    ids(Scope::Project, Some(root.join(".apb")));
    ids(Scope::Global, apb_core::store::global_playbooks_parent());
    out
}

/// Whether a global playbook's runs live in `root` (or its state does).
fn lives_here(root: &Path, scope: Scope, id: &str) -> bool {
    scope == Scope::Project
        || apb_core::signed_store::file::<state::HealthFile>(root, scope, id).exists()
        // Facts that do not read: evaluated, so the error is reported.
        || !matches!(facts::facts(root, scope, id), Ok(f) if f.is_empty())
}

/// Evaluates every watched playbook whose runs live in `root`.
pub fn sweep(root: &Path, now: &dyn Fn() -> u64) -> Vec<(String, Result<HealthView, EngineError>)> {
    playbooks(root)
        .into_iter()
        .filter(|(scope, id)| watched(root, *scope, id).is_some() && lives_here(root, *scope, id))
        .map(|(scope, id)| (reference(scope, &id), evaluate(root, scope, &id, now)))
        .collect()
}

fn iso(ms: u64) -> String {
    apb_core::clock::iso_utc(ms)
}

/// The state as it reads now, with a read-only evaluation: no write, no
/// send. `None` for a playbook with neither a block nor a state file.
pub fn view(
    root: &Path,
    scope: Scope,
    id: &str,
    now_ms: u64,
) -> Result<Option<HealthView>, EngineError> {
    let path = apb_core::signed_store::file::<state::HealthFile>(root, scope, id);
    let present = path.exists();
    let block = current_playbook(root, scope, id).and_then(|p| p.health);
    if !present && block.is_none() {
        return Ok(None);
    }
    let loaded = match apb_core::signed_store::read::<state::HealthFile>(root, scope, id) {
        Ok(l) => l,
        Err(apb_core::signed_store::StoreError::InvalidId(_)) => return Ok(None),
        Err(_) => return Ok(Some(unreadable_view(scope, id, &path))),
    };
    if present && !loaded.signed {
        return Ok(Some(unreadable_view(scope, id, &path)));
    }
    let f = loaded.file;
    let watched_block = watched(root, scope, id);
    let mut rules = Vec::new();
    if let Some(block) = &watched_block {
        let first_seen = if f.first_seen_ms == 0 {
            now_ms
        } else {
            f.first_seen_ms
        };
        let verdicts = apb_core::health::evaluate(
            block,
            &facts::facts(root, scope, id)?,
            f.counted_since_ms,
            f.counted_since_ms.unwrap_or(first_seen),
            now_ms,
        );
        for v in verdicts {
            let stored = f.rules.get(v.rule.as_str());
            let since = v
                .tripped
                .then(|| stored.filter(|s| s.tripped).map_or(now_ms, |s| s.since_ms));
            rules.push(HealthRuleView {
                rule: v.rule.as_str().to_string(),
                tripped: v.tripped,
                since: since.map(iso),
                detail: v.detail,
            });
        }
    }
    let hold = f.hold.as_ref().map(|h| HoldView {
        since: iso(h.since_ms),
        rule: h.rule.clone(),
        detail: h.detail.clone(),
        clear: format!("apb health clear {}", reference(scope, id)),
    });
    let status = if hold.is_some() {
        "held"
    } else if watched_block.is_none() {
        "unwatched"
    } else if rules.iter().any(|r| r.tripped) {
        "tripped"
    } else {
        "ok"
    };
    let last_notice = f
        .notices
        .iter()
        .max_by_key(|n| n.recorded_ms)
        .map(|n| format!("{} {} {} {}", n.rule, n.kind, n.state, iso(n.recorded_ms)));
    Ok(Some(HealthView {
        playbook: id.to_string(),
        scope: scope.as_str().to_string(),
        status: status.to_string(),
        rules,
        hold,
        muted_until: f.muted_until_ms.filter(|m| *m > now_ms).map(iso),
        last_notice,
    }))
}

/// A person's clear: drops the hold and every trip, starts the count and
/// `overdue` over. A file that does not verify is moved aside first.
pub fn clear(root: &Path, scope: Scope, id: &str, now_ms: u64) -> Result<(), EngineError> {
    let change = |f: &mut state::HealthFile| {
        f.generation += 1;
        f.hold = None;
        f.rules.clear();
        f.counted_since_ms = Some(now_ms);
        if f.first_seen_ms == 0 {
            f.first_seen_ms = now_ms;
        }
        retire_open(f);
        ((), true)
    };
    match transact(root, scope, id, change) {
        Ok(()) => Ok(()),
        Err(apb_core::signed_store::StoreError::Unverified(path)) => {
            let aside = path.with_file_name(format!("{id}.json.unreadable-{now_ms}"));
            std::fs::rename(&path, &aside)?;
            transact(root, scope, id, change).map_err(store_err)
        }
        Err(e) => Err(store_err(e)),
    }
}

/// A person's mute until `until_ms`; `None` unmutes. Either retires every
/// open notice (a claim made before it is never dispatched).
pub fn mute(
    root: &Path,
    scope: Scope,
    id: &str,
    until_ms: Option<u64>,
    now_ms: u64,
) -> Result<(), EngineError> {
    transact(root, scope, id, |f| {
        f.generation += 1;
        f.muted_until_ms = until_ms;
        if f.first_seen_ms == 0 {
            f.first_seen_ms = now_ms;
        }
        retire_open(f);
        ((), true)
    })
    .map_err(store_err)
}

/// The views of every playbook with a `health:` block or a state file,
/// `<ref>` order: what `apb health` lists.
pub fn all_views(root: &Path, now_ms: u64) -> Vec<HealthView> {
    playbooks(root)
        .into_iter()
        .filter(|(scope, id)| lives_here(root, *scope, id) || *scope == Scope::Project)
        .filter_map(|(scope, id)| view(root, scope, &id, now_ms).ok().flatten())
        .collect()
}

/// The views of the watched playbooks `only` names (`<id>` or
/// `global:<id>`), every watched one without it, whether or not they have
/// runs: the `health` field of `apb stats`.
pub fn watched_views(root: &Path, only: Option<&str>, now_ms: u64) -> Vec<HealthView> {
    playbooks(root)
        .into_iter()
        .filter(|(scope, id)| only.is_none_or(|o| o == reference(*scope, id) || o == id))
        .filter(|(scope, id)| watched(root, *scope, id).is_some() && lives_here(root, *scope, id))
        .filter_map(|(scope, id)| view(root, scope, &id, now_ms).ok().flatten())
        .collect()
}

/// A `<id>` or `global:<id>` reference as a scope and an id.
pub fn parse_reference(r: &str) -> (Scope, &str) {
    match r.strip_prefix(apb_core::policy_file::GLOBAL_PREFIX) {
        Some(id) => (Scope::Global, id),
        None => (Scope::Project, r),
    }
}

/// One line of a view: `<rule> tripped (<detail>)` for each tripped rule.
pub fn tripped_rules(v: &HealthView) -> Vec<String> {
    v.rules
        .iter()
        .filter(|r| r.tripped)
        .map(|r| format!("{} tripped ({})", r.rule, r.detail))
        .collect()
}

/// The doctor's `health` line: `(warn, detail)`; `None` when nothing is
/// watched and no state file is unreadable.
pub fn doctor_line(root: &Path, now_ms: u64) -> Option<(bool, String)> {
    let mut watched_n = 0;
    let (mut tripped, mut held, mut unreadable) = (Vec::new(), Vec::new(), Vec::new());
    for (scope, id) in playbooks(root) {
        let is_watched = watched(root, scope, &id).is_some();
        let Ok(Some(v)) = view(root, scope, &id, now_ms) else {
            continue;
        };
        if is_watched {
            watched_n += 1;
        }
        let r = reference(scope, &id);
        if v.hold.as_ref().is_some_and(|h| h.rule == "unreadable") {
            unreadable.push(
                v.hold
                    .as_ref()
                    .map(|h| h.detail.clone())
                    .unwrap_or_default(),
            );
        } else if v.status == "held" {
            held.push(r.clone());
        }
        if v.rules.iter().any(|x| x.tripped) {
            tripped.push(r);
        }
    }
    if watched_n == 0 && unreadable.is_empty() && held.is_empty() {
        return None;
    }
    if tripped.is_empty() && held.is_empty() && unreadable.is_empty() {
        return Some((
            false,
            format!("{watched_n} playbooks watched, none tripped"),
        ));
    }
    let mut parts = Vec::new();
    if !tripped.is_empty() {
        parts.push(format!(
            "{} tripped ({})",
            tripped.len(),
            tripped.join(", ")
        ));
    }
    if !held.is_empty() {
        parts.push(format!("{} on hold ({})", held.len(), held.join(", ")));
    }
    parts.extend(unreadable);
    Some((true, parts.join(", ")))
}

/// Registers [`observe_ended`] as the journal's terminal-event observer
/// ([`crate::terminal_hook`]). The `apb` binary calls it before anything
/// else; a second call changes nothing.
pub fn install() {
    crate::terminal_hook::install(observe_ended);
}

/// The evaluation after a run's terminal event is durable; returns at
/// once for a child, a run that is not production or external data, a
/// run without provenance, or an unwatched playbook. Never fails.
fn observe_ended(run_dir: &Path) {
    let Some(root) = crate::run_config::project_root_of(run_dir) else {
        return;
    };
    if crate::run_config::read_run_config(run_dir)
        .ok()
        .is_some_and(|c| c.parent_run.is_some())
    {
        return;
    }
    if !matches!(
        crate::manifest::RunOrigin::of_read(&crate::manifest::read(run_dir)),
        crate::manifest::RunOrigin::Production | crate::manifest::RunOrigin::ExternalData
    ) {
        return;
    }
    let Ok(head) = crate::run_history::started_checked(run_dir) else {
        return;
    };
    let Some(head) = head else {
        return;
    };
    let Some(scope) = head.origin.as_deref().and_then(Scope::parse) else {
        return;
    };
    if watched(&root, scope, &head.playbook).is_none() {
        return;
    }
    if let Err(e) = evaluate(&root, scope, &head.playbook, &apb_core::clock::now_ms_u64) {
        eprintln!("apb: health of `{}`: {e}", head.playbook);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::signed_store::{self, StoreError, Unsigned};
    use state::{HealthFile, NoticeRecord};

    const H: u64 = 3_600_000;

    /// A project with playbook `p` carrying `health`, and `failed` failed
    /// runs of it an hour apart before `at`.
    fn project(health: &str, failed: u64, at: u64) -> tempfile::TempDir {
        apb_core::run_origin::ensure_key().unwrap();
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        let vdir = root.join(".apb/playbooks/p/1.0.0");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(
            vdir.join("playbook.yaml"),
            format!(
                "schema: 2\nid: p\nname: p\nversion: 1.0.0\n{health}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
            ),
        )
        .unwrap();
        std::fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
        for i in 0..failed {
            run(root, &format!("p-{i}"), at - (failed - i) * H, "failed");
        }
        t
    }

    fn run(root: &Path, id: &str, ts: u64, outcome: &str) {
        let dir = root.join(".apb/runs").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        apb_core::run_origin::stamp(&dir, id).unwrap();
        let lines = [
            serde_json::json!({"seq": 1, "ts": ts, "type": "run_started", "playbook": "p", "version": "1.0.0"}),
            serde_json::json!({"seq": 2, "ts": ts, "type": "run_provenance", "origin": "project"}),
            serde_json::json!({"seq": 3, "ts": ts + 10, "type": "run_finished", "outcome": outcome}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(dir.join("events.jsonl"), text).unwrap();
    }

    fn file(root: &Path) -> HealthFile {
        signed_store::read::<HealthFile>(root, Scope::Project, "p")
            .unwrap()
            .file
    }

    fn open_notice(id: &str, state: &str) -> NoticeRecord {
        NoticeRecord {
            id: id.into(),
            rule: "consecutive_failures".into(),
            kind: "trip".into(),
            recorded_ms: 1,
            subject: "s".into(),
            text: "t".into(),
            target: crate::notify::record::NotifyTargetRecord {
                connector: "c".into(),
                account: "a".into(),
                function: "f".into(),
                to: "42".into(),
                connector_digest: "cd".into(),
                account_digest: "ad".into(),
            },
            state: state.into(),
            claimant: None,
            claimed_ms: None,
            claim_generation: None,
            tries: 1,
            detail: None,
        }
    }

    const T: u64 = 1_000 * H;

    #[test]
    fn a_trip_is_recorded_without_a_target() {
        let t = project("health:\n  consecutive_failures: 2\n", 2, T);
        let v = evaluate(t.path(), Scope::Project, "p", &|| T).unwrap();
        assert_eq!(v.status, "tripped");
        let f = file(t.path());
        assert!(f.rules["consecutive_failures"].tripped);
        assert!(f.notices.is_empty(), "no target: no notice");
        assert_eq!(f.evaluated_at_ms, T);
    }

    #[test]
    fn hold_is_set_on_trip_with_on_trip_hold() {
        let t = project(
            "health:\n  consecutive_failures: 2\n  on_trip: hold\n",
            2,
            T,
        );
        let v = evaluate(t.path(), Scope::Project, "p", &|| T).unwrap();
        assert_eq!(v.status, "held");
        let hold = v.hold.unwrap();
        assert_eq!(hold.rule, "consecutive_failures");
        assert_eq!(hold.clear, "apb health clear p");
        let refusal = hold_refusal(t.path(), Scope::Project, "p").unwrap_err();
        assert_eq!(refusal["policy"], "playbook_on_hold");
        assert!(
            refusal["detail"]
                .as_str()
                .unwrap()
                .contains("a person clears it with `apb health clear p`"),
            "{refusal}"
        );
    }

    #[test]
    fn clear_drops_the_hold_and_starts_over() {
        let t = project(
            "health:\n  consecutive_failures: 2\n  on_trip: hold\n",
            2,
            T,
        );
        evaluate(t.path(), Scope::Project, "p", &|| T).unwrap();
        clear(t.path(), Scope::Project, "p", T + 1).unwrap();
        let f = file(t.path());
        assert!(f.hold.is_none());
        assert!(f.rules.is_empty());
        assert_eq!(f.counted_since_ms, Some(T + 1));
        assert!(hold_refusal(t.path(), Scope::Project, "p").is_ok());
        let v = evaluate(t.path(), Scope::Project, "p", &|| T + 2).unwrap();
        assert_eq!(v.status, "ok", "the failures before the clear do not count");
    }

    #[test]
    fn clear_and_mute_raise_the_generation_and_retire_open_notices() {
        let t = project("health:\n  consecutive_failures: 2\n", 0, T);
        signed_store::transact(
            t.path(),
            Scope::Project,
            "p",
            Unsigned::Refuse,
            |f: &mut HealthFile| {
                f.first_seen_ms = T;
                f.notices.push(open_notice("h-1", "pending"));
                f.notices.push(open_notice("h-2", "sent"));
                Ok::<_, StoreError>(((), true))
            },
        )
        .unwrap();
        mute(t.path(), Scope::Project, "p", Some(T + H), T).unwrap();
        let f = file(t.path());
        assert_eq!(f.generation, 1);
        assert_eq!(f.muted_until_ms, Some(T + H));
        assert_eq!(f.notices[0].state, "retired");
        assert_eq!(f.notices[1].state, "sent");
        signed_store::transact(
            t.path(),
            Scope::Project,
            "p",
            Unsigned::Refuse,
            |f: &mut HealthFile| {
                f.notices.push(open_notice("h-3", "failed"));
                Ok::<_, StoreError>(((), true))
            },
        )
        .unwrap();
        mute(t.path(), Scope::Project, "p", None, T).unwrap();
        assert_eq!(file(t.path()).generation, 2);
        assert_eq!(file(t.path()).notices[2].state, "retired");
        clear(t.path(), Scope::Project, "p", T).unwrap();
        assert_eq!(file(t.path()).generation, 3);
    }

    #[test]
    fn mute_suppresses_the_trip_notice() {
        let t = project("health:\n  consecutive_failures: 2\n", 2, T);
        mute(t.path(), Scope::Project, "p", Some(T + H), T).unwrap();
        let v = evaluate(t.path(), Scope::Project, "p", &|| T).unwrap();
        assert_eq!(v.status, "tripped");
        assert!(v.muted_until.is_some());
        assert!(
            file(t.path()).rules["consecutive_failures"]
                .trip_notice
                .is_none()
        );
    }

    #[test]
    fn an_unverified_file_reads_held() {
        let t = project("health:\n  consecutive_failures: 2\n", 0, T);
        let path = signed_store::file::<HealthFile>(t.path(), Scope::Project, "p");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"schema\": 1}").unwrap();
        let v = view(t.path(), Scope::Project, "p", T).unwrap().unwrap();
        assert_eq!(v.status, "held");
        assert_eq!(v.hold.as_ref().unwrap().rule, "unreadable");
        assert!(v.rules.is_empty());
        assert!(hold_refusal(t.path(), Scope::Project, "p").is_err());
        // A clear moves the file aside and starts fresh.
        clear(t.path(), Scope::Project, "p", T).unwrap();
        assert!(hold_refusal(t.path(), Scope::Project, "p").is_ok());
        let aside = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("p.json.unreadable-")
            });
        assert!(aside, "the unreadable file is kept aside");
    }

    #[test]
    fn view_writes_nothing() {
        let t = project("health:\n  consecutive_failures: 2\n", 2, T);
        let v = view(t.path(), Scope::Project, "p", T).unwrap().unwrap();
        assert_eq!(v.status, "tripped");
        assert_eq!(v.rules.len(), 1);
        assert!(!signed_store::file::<HealthFile>(t.path(), Scope::Project, "p").exists());
        assert!(view(t.path(), Scope::Project, "nope", T).unwrap().is_none());
    }

    #[test]
    fn the_first_evaluation_counts_every_fact() {
        let t = project("health:\n  consecutive_failures: 3\n", 3, T);
        let v = evaluate(t.path(), Scope::Project, "p", &|| T).unwrap();
        assert_eq!(v.status, "tripped");
        assert_eq!(file(t.path()).first_seen_ms, T);
    }
}
