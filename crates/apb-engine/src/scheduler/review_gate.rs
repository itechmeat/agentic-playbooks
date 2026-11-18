//! The review gate: waiting for a person's decision through the review
//! channel ([`await_decision`]), shared by `human_review` and `publish`, and
//! the review recommendation (issue #165 Part 11, Part 14.4): the drive-side
//! facts the recommendation needs (the gate's options and descriptions, the
//! predecessors' outputs, the visit, the automatic-decision refusal), handed
//! to `decision::review_triage`.

use super::*;

use crate::decision::review_triage::{self, Recommendation};
use crate::event::SCOPE_CHANGED_REASON;
use crate::review::ReviewEntry;

/// Which queued decision answers the open request.
pub(crate) enum Consume<'a> {
    /// The Nth decision for the node, where N is how many it already
    /// consumed: the count-based rule of a `human_review` gate.
    Nth,
    /// The first unconsumed decision carrying exactly this nonce: a decision
    /// queued for a replaced plan never answers a different one (the nonce
    /// names the plan, so one queued for a withdrawn request answers only
    /// an identical later plan). `consumed` is how many decisions with this
    /// nonce were already applied. On a person-only node only an entry
    /// through a person's channel answers; any other is never consumed.
    Nonce {
        nonce: &'a str,
        consumed: usize,
        person_only: bool,
    },
    /// The first decision stamped with the open request (and, on a bound
    /// request, with its binding nonce); for a person-only gate only one
    /// through a person's channel. Unstamped and refused entries are never
    /// consumed: `apb doctor --run` lists them.
    Stamped { person_only: bool },
}

/// The queued decision that answers gate `node`'s open request under
/// `consume`, if any.
fn queued_decision(
    run_dir: &Path,
    events: &[Event],
    node: &str,
    consume: &Consume<'_>,
) -> Result<Option<ReviewEntry>, EngineError> {
    let mut for_node = read_reviews_after(run_dir, None)?
        .into_iter()
        .filter(|e| e.cmd.node == node);
    Ok(match consume {
        Consume::Nth => for_node.nth(crate::event::review_consumed_count(events, node)),
        Consume::Nonce {
            nonce,
            consumed,
            person_only,
        } => for_node
            .filter(|e| e.nonce.as_deref() == Some(*nonce))
            .filter(|e| {
                (!*person_only || crate::review::decides_person_only_entry(run_dir, e))
                    && crate::review::passkey_stamp_ok(run_dir, e, *person_only)
            })
            .nth(*consumed),
        Consume::Stamped { person_only } => {
            let Some((request, binding)) = crate::review::open_request(events, node) else {
                return Ok(None);
            };
            for_node.find(|e| {
                crate::review::answers_stamped(run_dir, e, request, binding, *person_only)
            })
        }
    })
}

/// The request a gate declares once per visit: the `ReviewRequested` event,
/// and a decision the engine posts by itself (an enforced recommendation, or
/// an unattended run's declared decision).
pub(crate) struct GateRequest {
    pub(crate) event: EventPayload,
    /// A decision the engine posts by itself, with who decided it:
    /// `model` for an enforced recommendation, `unattended` for an
    /// unattended run's declared decision, stamped on the channel entry and
    /// journaled as `decided_by`.
    pub(crate) auto: Option<AutoDecision>,
    /// What the request shows beside its options (the rendered `show:`
    /// entries and field defaults, the scope limits), journaled right
    /// after the request as `review_detail` (people-in-the-loop D2/D3).
    pub(crate) detail: crate::review::form::GateDetail,
}

/// A decision the engine posts for a gate: the option, its note, and who
/// decided it (never a person).
pub(crate) struct AutoDecision {
    pub(crate) decision: String,
    pub(crate) note: String,
    pub(crate) by: crate::event::DecidedBy,
}

/// What one look at the gate found.
pub(crate) enum GateStep {
    /// A decision answers the open request. Nothing is journaled yet: the
    /// caller records `ReviewDecided` in its own order.
    Decided(Box<ReviewEntry>),
    /// No decision yet; the caller sleeps and spins the loop.
    Waiting,
    /// A supervisor directive moved the run off the gate: the request was
    /// withdrawn and `current` now names the directed node.
    Moved,
}

/// The drive state a gate may move when a directive takes the run elsewhere.
pub(crate) struct GateDrive<'a> {
    pub(crate) run_dir: &'a Path,
    /// The run's working tree: where the scope reads the commits from.
    pub(crate) workdir: &'a Path,
    pub(crate) log: &'a mut EventLog,
    pub(crate) playbook: &'a Playbook,
    pub(crate) current: &'a mut String,
    pub(crate) control_cursor: &'a mut Option<u64>,
    pub(crate) prompt_overrides: &'a mut BTreeMap<String, String>,
    pub(crate) frontier: &'a mut Vec<String>,
}

/// One look at a gate: consume a queued decision, or declare the request
/// once (built lazily by `request`, which sees the journal), or apply a
/// directive that moves the run elsewhere. Only the drive writes events;
/// decision makers write the review channel (`crate::review`).
pub(crate) fn await_decision(
    d: &mut GateDrive<'_>,
    consume: Consume<'_>,
    request: impl FnOnce(&mut EventLog, &[Event]) -> Result<GateRequest, EngineError>,
    detail: impl FnOnce() -> Result<crate::review::form::GateDetail, EngineError>,
) -> Result<GateStep, EngineError> {
    let GateDrive {
        run_dir,
        workdir,
        log,
        playbook,
        current,
        control_cursor,
        prompt_overrides,
        frontier,
    } = d;
    let (run_dir, workdir, playbook) = (*run_dir, *workdir, *playbook);
    // Only a gate that uses a form feature journals a detail
    // (`NodeKind::needs_review_detail`, the one decision every
    // reader shares): any other gate writes its request alone, the journal
    // an older apb writes, so an older apb still resumes a run waiting at
    // it.
    let needs_detail = playbook
        .node(current)
        .is_some_and(|n| n.kind.needs_review_detail());
    journal_missing_detail(run_dir, workdir, log, playbook, current, detail)?;
    let mut events = read_all(run_dir)?;
    if let Some(entry) = queued_decision(run_dir, &events, current, &consume)? {
        // A form gate's scope is measured again before its decision is
        // consumed: work that landed while the request waited (a due retry
        // `deferred::give_way` let run) withdraws the request, and the gate
        // asks again below on what is there now.
        if !(needs_detail && withdraw_if_rescoped(workdir, run_dir, log, &events, current)?) {
            return Ok(GateStep::Decided(Box::new(entry)));
        }
        events = read_all(run_dir)?;
    }
    // No decision yet: declare the request once, then wait (poll). The event
    // carries an owner-facing pending instruction (issue #42 finding 4) so a
    // supervising agent, or any reader of the log alone, can tell the owner
    // an action is expected, what the options are, and how to answer.
    if review_open_count(&events, current) == 0 {
        let req = request(log, &events)?;
        // What the decision covers, journaled with its request in one
        // write (people-in-the-loop D2), so no reader ever sees the request
        // without it: the scope since the gate's previous decision, the
        // rendered form, and the options withheld above `max_scope`. A gate
        // without a form feature writes its request alone.
        let requested = if needs_detail {
            let detail = detail_event(workdir, run_dir, &events, current, req.detail);
            log.append_pair(req.event, detail)?.seq
        } else {
            log.append(req.event)?.seq
        };
        // The engine's own decision (an enforced recommendation, or an
        // unattended run's declared outcome): posted through the review
        // channel for this request and stamped with who decided, so it is
        // consumed and journaled like a person's decision but never reads
        // as one. A decision a person already queued wins (the channel
        // refuses a second one).
        if let Some(AutoDecision { decision, note, by }) = req.auto {
            let cmd = crate::review::ReviewCommand {
                node: current.clone(),
                decision,
                note,
            };
            let _ = crate::review::post_engine_decision(run_dir, cmd, requested, by);
            // The declared decision is consumed right away, in this look,
            // like a decision queued before the request. A failure to post
            // it is the next look's to settle ([`post_unattended`]). An
            // enforced recommendation that fails to post waits for the
            // person (fail-closed).
            if by == crate::event::DecidedBy::Unattended
                && let Some(entry) =
                    queued_decision(run_dir, &read_all(run_dir)?, current, &consume)?
            {
                return Ok(GateStep::Decided(Box::new(entry)));
            }
        }
    }
    // A directive that moves the run elsewhere (`node_retry`,
    // `run_continue_from`) is what a supervisor sends when it sees, parked on
    // this gate, that an earlier node went wrong. The top-of-loop scan leaves
    // exactly such a directive at the head of the pending commands (it
    // answers a wake, and a gate raises none), so it is applied here: the
    // open request is withdrawn - no one can decide a gate the run has left -
    // and the run moves on. Reaching the gate again asks anew.
    if let Some(entry) = read_control_after(run_dir, **control_cursor)?
        .into_iter()
        .next()
        && matches!(
            entry.cmd,
            Control::Retry { .. } | Control::ContinueFrom { .. }
        )
    {
        // A directive past a person-only or approving gate no decision
        // passed is refused and journaled; the request stays open.
        if supervisor::refuse_gate_skip(
            run_dir,
            log,
            control_cursor,
            playbook,
            &entry.cmd,
            entry.seq,
        )? {
            return Ok(GateStep::Waiting);
        }
        let reason = match &entry.cmd {
            Control::Retry { node, .. } => format!("node_retry `{node}`"),
            Control::ContinueFrom { node } => format!("run_continue_from `{node}`"),
            _ => String::new(),
        };
        log.append(EventPayload::ReviewWithdrawn {
            node: current.clone(),
            reason,
        })?;
        supervisor::apply_move_directive(
            run_dir,
            log,
            control_cursor,
            prompt_overrides,
            current,
            entry.cmd,
            entry.seq,
        )?;
        // The directed node runs again: the same revival as a batch retry
        // (issue #203), but no head rebuild, the frontier is still the
        // drive's own.
        revive::revive(
            log,
            playbook,
            &read_all(run_dir)?,
            std::slice::from_ref(*current),
            frontier,
            &[],
        )?;
        return Ok(GateStep::Moved);
    }
    Ok(GateStep::Waiting)
}

/// Whether the open request of form gate `node` was withdrawn because its
/// scope changed while it waited: the scope is measured again with the
/// function and the window its `review_detail` used, and when it differs
/// one `review_withdrawn` names the reason ([`SCOPE_CHANGED_REASON`]). The
/// decision found is then not consumed (the count-based rule passes it,
/// `review_consumed_count`; a stamped one names the withdrawn request), and
/// the caller asks again at once with a fresh request and detail, against
/// which `max_scope` and the channel's checks run. An equal scope writes
/// nothing, so a re-ask happens only on a change: no loop without
/// progress. A request whose detail carries no scope is consumed as is.
fn withdraw_if_rescoped(
    workdir: &Path,
    run_dir: &Path,
    log: &mut EventLog,
    events: &[Event],
    node: &str,
) -> Result<bool, EngineError> {
    let Some(measured) = crate::review::form::detail_of(events, node).and_then(|d| d.scope) else {
        return Ok(false);
    };
    let now = crate::review::scope::scope_since(
        workdir,
        run_dir,
        events,
        measured.since_seq,
        apb_core::clock::now_ms_u64(),
    );
    if now == measured {
        return Ok(false);
    }
    log.append(EventPayload::ReviewWithdrawn {
        node: node.to_string(),
        reason: format!(
            "{SCOPE_CHANGED_REASON}: work landed while the request waited, so the gate asks again on what is there now"
        ),
    })?;
    Ok(true)
}

/// An open request of a gate that needs a detail
/// (`NodeKind::needs_review_detail`) but has none (a write torn
/// between the two lines): the detail is journaled now, before any
/// decision is posted, consumed or awaited, so the checks of `max_scope`
/// and `fields` always have one to read. The drive calls it before an
/// unattended run posts its declared decision, and again in every look at
/// the gate; nothing is written for any other gate or a request that has
/// its detail.
pub(crate) fn journal_missing_detail(
    run_dir: &Path,
    workdir: &Path,
    log: &mut EventLog,
    playbook: &Playbook,
    node: &str,
    detail: impl FnOnce() -> Result<crate::review::form::GateDetail, EngineError>,
) -> Result<(), EngineError> {
    if !playbook
        .node(node)
        .is_some_and(|n| n.kind.needs_review_detail())
    {
        return Ok(());
    }
    let events = read_all(run_dir)?;
    if let Some((request, _)) = crate::review::open_request(&events, node)
        && crate::review::form::detail_of(&events, node).is_none()
    {
        let payload = detail_event(workdir, run_dir, &events, node, detail()?);
        log.append(payload(request))?;
    }
    Ok(())
}

/// The `review_detail` of a request of `node` over the journal before it
/// (or with it, for a request already open): the scope since the gate's
/// previous decision, the rendered form, and the options withheld above
/// `max_scope`, waiting for the request's seq.
fn detail_event(
    workdir: &Path,
    run_dir: &Path,
    events: &[Event],
    node: &str,
    detail: crate::review::form::GateDetail,
) -> impl FnOnce(u64) -> EventPayload + use<> {
    let scope = crate::review::scope::scope_since(
        workdir,
        run_dir,
        events,
        crate::review::scope::window_start(events, node, detail.floor_seq),
        apb_core::clock::now_ms_u64(),
    );
    let (withheld, withheld_reason) = match &detail.max_scope {
        Some(max) => crate::review::scope::withheld(max, &scope, &detail.options),
        None => (Vec::new(), None),
    };
    let node = node.to_string();
    move |request| EventPayload::ReviewDetail {
        node,
        request,
        scope: Some(scope),
        show: detail.show,
        fields: detail.fields,
        withheld,
        withheld_reason,
    }
}

/// An unattended run's declared decision for gate `gate` when its request
/// is already open: a drive that stopped after journaling the request and
/// before the decision reached the channel (a crash, a failed write) would
/// otherwise find a request and no decision, and wait for nobody. Posts the
/// decision for that request now; one already queued for it (this one
/// before the crash, or a person's) is kept. `Err` with the reason when it
/// cannot be posted: the run fails then rather than wait.
pub(crate) fn post_unattended(
    run_dir: &Path,
    gate: &str,
    decision: &(String, String),
) -> Result<Result<(), String>, EngineError> {
    let events = read_all(run_dir)?;
    if review_open_count(&events, gate) == 0 {
        return Ok(Ok(()));
    }
    let Some(requested) = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ReviewRequested { node, .. } if node == gate => Some(e.seq),
        _ => None,
    }) else {
        return Ok(Ok(()));
    };
    let cmd = crate::review::ReviewCommand {
        node: gate.to_string(),
        decision: decision.0.clone(),
        note: decision.1.clone(),
    };
    match crate::review::post_engine_decision(
        run_dir,
        cmd,
        requested,
        crate::event::DecidedBy::Unattended,
    ) {
        Ok(_) | Err(EngineError::AlreadyAnswered(_)) => Ok(Ok(())),
        Err(e) => Ok(Err(format!(
            "{}: human_review `{gate}` could not record its unattended decision `{}`: {e}",
            crate::run_config::HITL_REQUIRED,
            decision.0
        ))),
    }
}

/// The request of a `human_review` visit: the instruction, and the review
/// recommendation (issue #165 Part 11, 14.4) asked once per visit before the
/// request is declared; an enforced one becomes the automatic decision.
#[allow(clippy::too_many_arguments)]
pub(crate) fn human_review_request(
    root: &Path,
    run_dir: &Path,
    log: &mut EventLog,
    decisions: Option<&crate::decision::DecisionRunner>,
    playbook: &Playbook,
    gate: &str,
    options: &[String],
    prompt: Option<&str>,
    events: &[Event],
    form: &FormCtx<'_>,
) -> Result<GateRequest, EngineError> {
    let title = playbook.node(gate).and_then(|n| n.title.clone());
    let mut instruction = crate::people::instruction::review_instruction_for(
        gate,
        title.as_deref(),
        options,
        prompt,
        playbook.node(gate).is_some_and(|n| n.person_only),
    );
    let (binding, person_only) = gate_binding(run_dir, playbook, gate, events, &mut instruction);
    let rec = match decisions {
        Some(runner) => {
            let journal = Journal::new(&mut *log);
            recommend(root, run_dir, runner, &journal, playbook, gate, events)
        }
        None => Default::default(),
    };
    if let Some(r) = &rec.recommendation {
        instruction.push_str(&review_triage::instruction_line(r));
    }
    Ok(GateRequest {
        event: EventPayload::ReviewRequested {
            node: gate.to_string(),
            options: apb_core::schema::effective_review_options(options),
            title,
            instruction,
            prompt: prompt.map(str::to_string),
            recommendation: rec.recommendation,
            plan_digest: None,
            binding,
            person_only,
        },
        // Only a person decides a person-only gate: a recommendation stays
        // advice there, never a decision.
        auto: rec
            .auto
            .filter(|_| !person_only)
            .map(|(decision, note)| AutoDecision {
                decision,
                note,
                by: crate::event::DecidedBy::Model,
            }),
        detail: render_form(form, gate)?,
    })
}

/// What [`render_node_prompt`] takes, gathered once per visit: the context
/// the gate's `show` texts and field defaults render over
/// (people-in-the-loop D3).
pub(crate) struct FormCtx<'a> {
    pub(crate) playbook: &'a Playbook,
    pub(crate) run_dir: &'a Path,
    pub(crate) run_id: &'a str,
    pub(crate) state: &'a crate::state::RunState,
    pub(crate) cfg: &'a crate::run_config::RunConfig,
    pub(crate) root: &'a Path,
}

/// The detail a request shows beside its options: the gate's `max_scope`,
/// its effective options, the rendered `show` entries and the rendered
/// field defaults (people-in-the-loop D3). A text is rendered whole (no
/// context budget), scrubbed of the run's secret values, cut at [`apb_core::review_form::RENDERED_MAX_BYTES`]
/// and marked when it reads untrusted text. Every text of the detail goes
/// through that one path (`scrub_then_clip`): a template and a literal
/// default alike, and the authored `show` labels and enum options too.
/// Field names need none: the validator holds them to `[a-z][a-z0-9_]*`.
pub(crate) fn render_form(
    ctx: &FormCtx<'_>,
    gate: &str,
) -> Result<crate::review::form::GateDetail, EngineError> {
    use crate::review::form::FieldView;
    use apb_core::review_form::Required;
    let mut detail = crate::review::form::GateDetail::default();
    let Some(node) = ctx.playbook.node(gate) else {
        return Ok(detail);
    };
    let apb_core::schema::NodeKind::HumanReview {
        fields: spec,
        show,
        max_scope,
        options,
        ..
    } = &node.kind
    else {
        return Ok(detail);
    };
    detail.max_scope = max_scope.clone();
    detail.options = apb_core::schema::effective_review_options(options);
    let analysis = apb_core::untrusted::analyze(ctx.playbook);
    let redactor = crate::run_redact::RunRedactor::for_run(ctx.root, ctx.run_dir);
    // Rendered whole, with no context budget: a budget cuts each output it
    // inserts, and a secret cut in part no longer matches the scrubber.
    // The whole text is scrubbed first and only then cut, at
    // `RENDERED_MAX_BYTES` (`scrub_then_clip`).
    let budget = apb_core::schema::ContextBudget::UNLIMITED;
    // The untrusted mark is about the source: whether the TEMPLATE reads
    // untrusted text, judged before it renders.
    let render = |text: &str| -> Result<(String, bool, bool), EngineError> {
        let untrusted =
            !apb_core::untrusted::text_reads_untrusted(ctx.playbook, &analysis, text).is_empty();
        let rendered = crate::scheduler::node::render_node_prompt(
            ctx.playbook,
            ctx.run_dir,
            ctx.run_id,
            ctx.state,
            ctx.cfg,
            text,
            &budget,
        )?;
        let (text, clipped) = scrub_then_clip(&redactor, &rendered);
        Ok((text, untrusted, clipped))
    };
    detail.show = show
        .iter()
        .map(|entry| {
            render(&entry.text).map(
                |(text, untrusted, clipped)| crate::review::form::ShownEntry {
                    label: scrub_then_clip(&redactor, &entry.label).0,
                    text,
                    untrusted,
                    clipped,
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    detail.fields = spec
        .iter()
        .map(|f| -> Result<FieldView, EngineError> {
            let required = match &f.required {
                Required::Flag(true) => detail.options.clone(),
                Required::Flag(false) => Vec::new(),
                Required::Options(list) => list.clone(),
            };
            // A literal default renders to itself: one path for both, so
            // a literal is scrubbed and cut like a template.
            let (default, default_problem, untrusted) = match f.default.as_ref() {
                Some(d) => {
                    let (text, reads_untrusted, _clipped) = render(d)?;
                    match apb_core::review_form::coerce(f, &serde_json::Value::String(text)) {
                        Ok(v) => (Some(v), None, reads_untrusted),
                        // The problem repeats no value or option (the
                        // wording of `coerce`); scrubbed and cut like every
                        // other text of the detail all the same.
                        Err(e) => (
                            None,
                            Some(scrub_then_clip(&redactor, &e).0),
                            reads_untrusted,
                        ),
                    }
                }
                None => (None, None, false),
            };
            Ok(FieldView {
                name: f.name.clone(),
                kind: f.kind,
                required,
                default,
                default_problem,
                options: f
                    .options
                    .iter()
                    .map(|o| scrub_then_clip(&redactor, o).0)
                    .collect(),
                max_len: f.max_len,
                untrusted,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(detail)
}

/// A rendered text as a request journals it: scrubbed of the run's secret
/// values and token-shaped strings first (withheld whole when the run's
/// secrets do not resolve), then cut at
/// [`apb_core::review_form::RENDERED_MAX_BYTES`] on a char boundary, so a
/// secret that crosses the cut is never kept in part. Whether it was cut.
fn scrub_then_clip(redactor: &crate::run_redact::RunRedactor, rendered: &str) -> (String, bool) {
    use apb_core::review_form::RENDERED_MAX_BYTES;
    let mut text = redactor
        .secret_text(rendered)
        .unwrap_or_else(|_| crate::run_redact::WITHHELD.to_string());
    if text.len() <= RENDERED_MAX_BYTES {
        return (text, false);
    }
    let mut cut = RENDERED_MAX_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    (text, true)
}

/// The request of a `human_review` visit in an unattended run that decides
/// it with its declared option: the same instruction, no recommendation,
/// and the decision posted at once as `unattended`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn unattended_review_request(
    run_dir: &Path,
    events: &[Event],
    playbook: &Playbook,
    gate: &str,
    options: &[String],
    prompt: Option<&str>,
    decision: (String, String),
    form: &FormCtx<'_>,
) -> Result<GateRequest, EngineError> {
    let title = playbook.node(gate).and_then(|n| n.title.clone());
    let mut instruction = crate::people::instruction::review_instruction_for(
        gate,
        title.as_deref(),
        options,
        prompt,
        playbook.node(gate).is_some_and(|n| n.person_only),
    );
    let (binding, person_only) = gate_binding(run_dir, playbook, gate, events, &mut instruction);
    Ok(GateRequest {
        event: EventPayload::ReviewRequested {
            node: gate.to_string(),
            options: apb_core::schema::effective_review_options(options),
            title,
            instruction,
            prompt: prompt.map(str::to_string),
            recommendation: None,
            plan_digest: None,
            binding,
            person_only,
        },
        auto: Some(AutoDecision {
            decision: decision.0,
            note: decision.1,
            by: crate::event::DecidedBy::Unattended,
        }),
        detail: render_form(form, gate)?,
    })
}

/// The binding and the `person_only` flag of gate `gate`'s next request,
/// with the binding named in the instruction.
fn gate_binding(
    run_dir: &Path,
    playbook: &Playbook,
    gate: &str,
    events: &[Event],
    instruction: &mut String,
) -> (Option<crate::approval::ReviewBinding>, bool) {
    let Some(node) = playbook.node(gate) else {
        return (None, false);
    };
    let binding = node
        .approves
        .as_ref()
        .map(|a| super::approval::bind_request(run_dir, gate, a, events));
    if let Some(b) = &binding {
        instruction.push_str(&super::approval::binding_line(b));
    }
    (binding, node.person_only)
}

/// The effects of every sub-playbook the playbook runs, recursively and in
/// any scope. A gated run weighs the child versions its gate pinned
/// (`expected_children`), which are the ones it will execute; an ungated run
/// has no pins and resolves the tree as the run gate would. `None` when the
/// tree does not resolve or the run config does not read: the automatic
/// decision is then refused (fail-closed).
fn inherited_effects(
    root: &Path,
    run_dir: &Path,
    playbook: &Playbook,
) -> Option<Vec<apb_core::schema::Effect>> {
    let cfg = crate::run_config::read_run_config(run_dir).ok()?;
    let set = match &cfg.expected_children {
        Some(pins) => crate::gate::pinned_tree_effects(root, playbook, pins)?,
        None => crate::gate::tree_effects(
            root,
            playbook,
            &crate::run_origin::parent_run_origin(run_dir),
        )?,
    };
    Some(set.into_iter().collect())
}

/// Asks for the recommendation of one gate visit.
pub(crate) fn recommend(
    root: &Path,
    run_dir: &Path,
    runner: &crate::decision::DecisionRunner,
    journal: &Journal,
    playbook: &Playbook,
    gate: &str,
    events: &[Event],
) -> Recommendation {
    let Some(node) = playbook.node(gate) else {
        return Recommendation::default();
    };
    let NodeKind::HumanReview {
        options, prompt, ..
    } = &node.kind
    else {
        return Recommendation::default();
    };
    let options = apb_core::schema::effective_review_options(options);
    // Direct predecessors that ran: their latest output.
    let mut inputs: Vec<(String, String)> = Vec::new();
    for e in playbook.edges.iter().filter(|e| e.to == gate) {
        if inputs.iter().any(|(id, _)| *id == e.from) {
            continue;
        }
        let out = events.iter().rev().find_map(|ev| match &ev.payload {
            EventPayload::NodeFinished { node, output, .. } if *node == e.from => {
                Some(output.clone())
            }
            _ => None,
        });
        if let Some(out) = out {
            inputs.push((e.from.clone(), out));
        }
    }
    let visit = crate::event::review_requested_count(events, gate) as u32 + 1;
    // A sub-playbook run cannot see what its parent does after it returns
    // (a push, a deploy, declared effects), so it never decides by itself.
    // A run config that does not read counts as a child (fail-closed).
    let child_run =
        || crate::run_config::read_run_config(run_dir).map_or(true, |c| c.parent_run.is_some());
    let refusal = node.auto_decide.as_ref().and_then(|_| {
        if child_run() {
            return Some("effects");
        }
        match inherited_effects(root, run_dir, playbook) {
            // The installed connectors' flags (0.23.0): a granted
            // `irreversible` function after the gate refuses too, and so
            // does a bound connector whose manifest no longer loads or that
            // is not installed (its effects are unknown: fail-closed).
            Some(inherited) => apb_core::validate::auto_decide_run_refusal(
                playbook,
                gate,
                &inherited,
                &apb_core::connector::resolve::validation_facts(),
            )
            .map(|_| "effects"),
            None => Some("effects"),
        }
    });
    review_triage::recommend(
        runner,
        journal,
        review_triage::Gate {
            node: gate,
            title: node.title.as_deref(),
            prompt: prompt.as_deref(),
            options: &options,
            descriptions: &node.option_descriptions,
            inputs,
            visit,
            auto_decide: node.auto_decide.as_ref(),
            refusal,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::scrub_then_clip;
    use apb_core::review_form::RENDERED_MAX_BYTES;
    use apb_core::secret::Secret;

    /// A secret that crosses the cut is scrubbed whole before the text is
    /// cut: no prefix of it is kept.
    #[test]
    fn a_secret_across_the_cut_is_scrubbed_before_the_text_is_cut() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(".apb/runs/r-1");
        std::fs::create_dir_all(&dir).unwrap();
        let secret = "Zq7vK2mXpL9wR4tY8nB3cF6hJ1sD5gA0";
        let redactor = crate::run_redact::RunRedactor::with_workspace(
            vec![Secret::new(secret)],
            root.path(),
            &dir,
        );
        assert!(!redactor.withholds());
        // The secret starts 10 bytes before the cut.
        let rendered = format!("{}{secret} tail", "x".repeat(RENDERED_MAX_BYTES - 10));
        let (text, clipped) = scrub_then_clip(&redactor, &rendered);
        assert!(clipped);
        assert!(text.len() <= RENDERED_MAX_BYTES);
        assert!(
            !text.contains(&secret[..10]),
            "a prefix of the secret survived"
        );
        // A short text is scrubbed and kept whole.
        let (short, clipped) = scrub_then_clip(&redactor, &format!("key {secret}"));
        assert!(!clipped && !short.contains(secret) && short.starts_with("key "));
    }
}
