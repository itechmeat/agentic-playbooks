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
    /// nonce were already applied.
    Nonce { nonce: &'a str, consumed: usize },
}

/// The request a gate declares once per visit: the `ReviewRequested` event,
/// and a decision the engine posts by itself (an enforced recommendation).
pub(crate) struct GateRequest {
    pub(crate) event: EventPayload,
    pub(crate) auto: Option<(String, String)>,
}

/// What one look at the gate found.
pub(crate) enum GateStep {
    /// A decision answers the open request. Nothing is journaled yet: the
    /// caller records `ReviewDecided` in its own order.
    Decided(ReviewEntry),
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
    let mut for_current = read_reviews_after(run_dir, None)?
        .into_iter()
        .filter(|e| e.cmd.node == **current);
    let found = match consume {
        Consume::Nth => for_current.nth(review_decided_count(&events, current)),
        Consume::Nonce { nonce, consumed } => for_current
            .filter(|e| e.nonce.as_deref() == Some(nonce))
            .nth(consumed),
    };
    if let Some(entry) = found {
        return Ok(GateStep::Decided(entry));
    }
    // No decision yet: declare the request once, then wait (poll). The event
    // carries an owner-facing pending instruction (issue #42 finding 4) so a
    // supervising agent, or any reader of the log alone, can tell the owner
    // an action is expected, what the options are, and how to answer.
    if review_open_count(&events, current) == 0 {
        let req = request(log, &events)?;
        log.append(req.event)?;
        // Enforced: post through the review channel, so it is consumed and
        // journaled like a person's decision; a decision a person already
        // queued wins (the channel refuses a second one), and any failure
        // waits for the person (fail-closed).
        if let Some((decision, note)) = req.auto {
            let _ = crate::review::post_review(
                run_dir,
                crate::review::ReviewCommand {
                    node: current.clone(),
                    decision,
                    note,
                },
            );
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
    let mut instruction =
        crate::people::instruction::review_instruction(gate, title.as_deref(), options, prompt);
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
        },
        auto: rec.auto,
    })
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
        None => crate::gate::tree_effects(root, playbook, &node::parent_run_origin(run_dir))?,
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
