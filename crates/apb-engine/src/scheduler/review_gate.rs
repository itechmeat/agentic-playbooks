//! The review gate: waiting for a person's decision through the review
//! channel ([`await_decision`]), shared by `human_review` and `publish`, and
//! the review recommendation (issue #165 Part 11, Part 14.4): the drive-side
//! facts the recommendation needs (the gate's options and descriptions, the
//! predecessors' outputs, the visit, the automatic-decision refusal), handed
//! to `decision::review_triage`.

use super::*;

use crate::decision::review_triage::{self, Recommendation};
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
        Consume::Nth => for_node.nth(review_decided_count(events, node)),
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
) -> Result<GateStep, EngineError> {
    let GateDrive {
        run_dir,
        log,
        playbook,
        current,
        control_cursor,
        prompt_overrides,
        frontier,
    } = d;
    let (run_dir, playbook) = (*run_dir, *playbook);
    let events = read_all(run_dir)?;
    if let Some(entry) = queued_decision(run_dir, &events, current, &consume)? {
        return Ok(GateStep::Decided(Box::new(entry)));
    }
    // No decision yet: declare the request once, then wait (poll). The event
    // carries an owner-facing pending instruction (issue #42 finding 4) so a
    // supervising agent, or any reader of the log alone, can tell the owner
    // an action is expected, what the options are, and how to answer.
    if review_open_count(&events, current) == 0 {
        let req = request(log, &events)?;
        let requested = log.append(req.event)?.seq;
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
    })
}

/// The request of a `human_review` visit in an unattended run that decides
/// it with its declared option: the same instruction, no recommendation,
/// and the decision posted at once as `unattended`.
pub(crate) fn unattended_review_request(
    run_dir: &Path,
    events: &[Event],
    playbook: &Playbook,
    gate: &str,
    options: &[String],
    prompt: Option<&str>,
    decision: (String, String),
) -> GateRequest {
    let title = playbook.node(gate).and_then(|n| n.title.clone());
    let mut instruction = crate::people::instruction::review_instruction_for(
        gate,
        title.as_deref(),
        options,
        prompt,
        playbook.node(gate).is_some_and(|n| n.person_only),
    );
    let (binding, person_only) = gate_binding(run_dir, playbook, gate, events, &mut instruction);
    GateRequest {
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
    }
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
    let NodeKind::HumanReview { options, prompt } = &node.kind else {
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
