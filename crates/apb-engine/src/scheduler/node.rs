//! Node execution: rendering, adapter dispatch, skill materialization, and frontier advance.
//! Split out of `scheduler` for navigability; shares the parent module's imports via `use super::*`.

use super::*;

use crate::failure_class::{FailureKind, INFRA_RETRY_ACTION};

/// Renders a node's prompt template with the full standard context (compaction
/// summary + uncompacted tail if drive recorded ContextCompacted, otherwise the
/// full context), run hooks, params, prior outputs, and reviews. This is the
/// single rendering sequence shared by `execute_node` (the prompt the agent
/// actually receives) and the drive-loop cache-key computation, so the two can
/// never drift: a prompt that changes changes the key. `run_id` comes from the
/// caller rather than being re-derived from the path, matching every other
/// render site.
pub(crate) fn render_node_prompt(
    run_dir: &Path,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    prompt: &str,
) -> Result<String, EngineError> {
    let context =
        build_context_for_render(run_dir, &read_all(run_dir)?, cfg.instruction.as_deref())?;
    let hooks: BTreeMap<String, String> = crate::hooks::read_hooks(run_dir)?
        .into_iter()
        .map(|(k, secret)| (k, crate::hooks::hook_path(run_id, &secret)))
        .collect();
    Ok(render(
        prompt,
        &cfg.params,
        cfg.instruction.as_deref(),
        &state.outputs,
        &state.reviews,
        &state.rejected_outputs,
        &hooks,
        &context,
    ))
}

/// A single execution of a node. Returns an [`AttemptOutcome`]: `Finished`
/// (status, output, events) for a normal execution, or `Suspended` when an
/// interactive `agent_task` asked a question via the stdout marker instead of
/// finishing (spec 2026-07-20) - drive parks on it and re-invokes on the answer.
///
/// The two attempt-lifecycle events are journaled directly through `journal`:
/// `attempt_started` at spawn time (so a crash mid-attempt leaves an open
/// attempt on disk, later folded to interrupted) and `attempt_finished` at
/// return time (carrying `duration_ms`). Every OTHER event (RetryStarted,
/// FallbackTriggered) is still returned in the Vec for drive to write in its
/// return batch - drive remains the sole writer of those. `journal` wraps the
/// same single log in a Mutex, so this stays safe on the parallel batch's
/// worker threads (each append is one atomic line write).
/// The marker contract paragraph appended to an interactive node's prompt for
/// resume/reprompt agents (spec 2026-07-20, Transport: resume/reprompt block),
/// quoted verbatim. Interpolates [`crate::adapter::QUESTION_MARKER`] so the
/// wording and the constant can never drift.
fn marker_contract() -> String {
    format!(
        "If you need input from the user before you can proceed, print a line \
         containing exactly `{marker}` followed by a JSON object \
         `{{\"question\": \"...\", \"options\": [\"...\", ...]}}` on the next line, \
         then stop without doing further work.",
        marker = crate::adapter::QUESTION_MARKER,
    )
}

/// A `resume`-transport re-invocation of an interactive node (spec 2026-07-20,
/// Task 7). Carries the session id captured from the attempt that asked, plus
/// the user's answer to hand the agent as the follow-up prompt. When present,
/// `execute_node` re-enters the primary executor's own session via its resume
/// form instead of re-invoking from scratch with a transcript. Chosen by the
/// drive loop, which downgrades to a plain (`resume: None`) re-invocation when
/// no session was captured or the agent has no resume form.
pub(crate) struct ResumeContext {
    pub session: String,
    pub answer: String,
}

/// A `live`-transport execution of an interactive node (spec 2026-07-20, Task
/// 11). Present only when the drive loop resolved the node's `interaction` to
/// `Live` on claude/claude-code AND could resolve the current exe; a downgrade
/// hands `None`. When present, `execute_node` injects the `apb __ask-server`
/// sidecar into the claude argv, appends the live prompt paragraph instead of
/// the marker contract, and drives the channel observation on this (the drive)
/// thread via the adapter's per-poll `on_tick`.
pub(crate) struct LiveContext {
    /// Current apb executable, resolved by the drive layer (a resolution
    /// failure downgrades before we get here, so this is always present).
    pub exe: std::path::PathBuf,
    /// Per-server tool timeout in ms handed to the sidecar injection.
    pub timeout_ms: u64,
}

/// The seq of the last control entry currently posted, or `None` when the
/// channel is empty. Used as the mid-attempt observation baseline
/// (`observe_control`) so only messages that arrive AFTER an attempt begins are
/// acknowledged as received live - a command already queued (an unconsumed
/// retry the drive's scan stopped short of) is not re-acked. A read failure is
/// a hard error at attempt start rather than degrading to `None`: a `None`
/// baseline re-reads the whole channel, and a stale already-consumed
/// `Control::Interrupt` must never replay into a fresh attempt.
fn latest_control_seq(run_dir: &Path) -> Result<Option<u64>, EngineError> {
    Ok(crate::control::read_control_after(run_dir, None)?
        .last()
        .map(|e| e.seq))
}

/// A short machine-facing description of a control command for a
/// `control_received` acknowledgment detail (finding 7 of issue #42).
fn control_summary(cmd: &crate::control::Control, seq: u64) -> String {
    format!("{} (control seq {seq})", cmd.kind())
}

/// Journals every control message newer than `seen` as a `control_received`
/// supervisor action (finding 7 of issue #42, third item of issue #40: a
/// message posted mid-attempt must be acknowledged live, not only discovered at
/// the next node boundary). On a [`crate::control::Control::Interrupt`] it
/// additionally journals an explanatory `attempt_interrupted` action and
/// reports the request back, so the caller's poll loop can tear the agent down.
/// Runs on the drive thread through the shared `Journal`, so the single-writer
/// invariant holds. Returns the highest seq acknowledged (the new baseline) and
/// whether an interrupt was requested.
///
/// An interrupt naming a DIFFERENT node is invisible here (spec 2026-08-05
/// section 1.6): the entry is not acknowledged, not journaled, and not consumed,
/// because this observer runs once per ATTEMPT and every concurrent branch reads
/// the same channel - acknowledging another branch's interrupt would put an
/// `attempt_interrupted` for this node in the journal and kill it. Leaving it
/// unconsumed is also what makes the entry still available to the attempt it
/// names, and (when that node is not running) to the drive loop's own scan, which
/// consumes a spent interrupt at the next node boundary. An interrupt with no
/// `node` keeps the documented BROADCAST semantics byte for byte: every running
/// attempt observes it and dies.
fn observe_control(
    run_dir: &Path,
    node_id: &str,
    attempt: u32,
    journal: &Journal,
    seen: Option<u64>,
) -> Result<(Option<u64>, bool), EngineError> {
    let mut cursor = seen;
    let mut interrupt = false;
    for entry in crate::control::read_control_after(run_dir, seen)? {
        if let crate::control::Control::Interrupt {
            node: Some(target), ..
        } = &entry.cmd
            && target != node_id
        {
            continue;
        }
        journal.append(EventPayload::SupervisorAction {
            action: "control_received".into(),
            node: Some(node_id.to_string()),
            detail: control_summary(&entry.cmd, entry.seq),
        })?;
        if let crate::control::Control::Interrupt { reason, .. } = &entry.cmd {
            journal.append(EventPayload::SupervisorAction {
                action: "attempt_interrupted".into(),
                node: Some(node_id.to_string()),
                detail: format!(
                    "supervisor requested interrupt of node `{node_id}` attempt {attempt} (control seq {}): {reason}",
                    entry.seq
                ),
            })?;
            interrupt = true;
        }
        cursor = Some(entry.seq);
    }
    Ok((cursor, interrupt))
}

/// Runs a node's `success_check` against an attempt's effective output and
/// returns the human-readable rejection reason, or `None` when the report is
/// accepted (including a node with no check at all).
///
/// Shared by the two paths that can produce a successful attempt: the ordinary
/// report path, and a success verdict recovered from the status file after the
/// agent process died (spec 2026-08-05 section 2.1). A recovered verdict
/// therefore does NOT bypass the gate.
fn success_check_rejection(
    check: Option<&apb_core::schema::SuccessCheck>,
    run_dir: &Path,
    attempt_workdir: &Path,
    output: &str,
) -> Result<Option<String>, EngineError> {
    match check {
        // Deterministic sh-script check (spec 6.2): a non-zero exit rejects the
        // report regardless of the agent's self-assessment. Run in the SAME
        // attempt workdir the agent worked in (for an isolated node its
        // per-attempt directory, otherwise the shared workdir), otherwise the
        // check would validate a directory the agent never wrote to.
        Some(apb_core::schema::SuccessCheck::Script(check)) => {
            let r = run_script(run_dir, attempt_workdir, check, "sh", None, None)?;
            Ok((r.status != NodeStatus::Succeeded)
                .then(|| format!("success_check `{check}` failed")))
        }
        // Completion-marker check (issue 45 finding 1): the literal marker must
        // appear in the node output, else the reported success is rejected. This
        // defends against a long-running orchestrator that exits early at its
        // first wait phase and records interim text as success.
        Some(apb_core::schema::SuccessCheck::Marker { marker })
            if !output.contains(marker.as_str()) =>
        {
            Ok(Some(format!(
                "success report rejected: completion marker `{marker}` not found in output"
            )))
        }
        Some(apb_core::schema::SuccessCheck::Marker { .. }) | None => Ok(None),
    }
}

/// Journals an attempt that ended without recording a REQUIRED verdict as
/// `interrupted`, preserving whatever it produced (spec 2026-08-05 section 2.2,
/// issue #71 item 1).
///
/// An interrupted attempt is a failure for scheduling: it consumes a retry
/// exactly like any other, and the node's status still comes from its eventual
/// `node_finished`. `RunState::fold` reads only the PRESENCE of an
/// `attempt_finished` (to close the open attempt), never its status label, so
/// the new label cannot confuse the run state machine.
///
/// `failure_kind` is the spec-2.3 classification of the failure detail, or
/// `None` when nothing was classified (an exit-0 attempt that simply recorded no
/// verdict has no failure detail to classify).
///
/// Shared with the drive-entry reaper (`entry::reap_dead_attempts`, spec
/// section 2.4): an attempt whose process died with its driver ended without
/// recording a verdict too, so it earns the same label through the same writer
/// rather than a second hand-built event.
pub(super) fn journal_interrupted_attempt(
    journal: &Journal,
    node_id: &str,
    attempt: u32,
    duration_ms: Option<u64>,
    session: Option<String>,
    partial: &str,
    failure_kind: Option<FailureKind>,
) -> Result<(), EngineError> {
    journal.append(EventPayload::AttemptFinished {
        node: node_id.into(),
        attempt,
        status: NodeStatus::Interrupted.as_str().into(),
        duration_ms,
        session,
        summary: None,
        rejected_output: None,
        partial_output: (!partial.trim().is_empty()).then(|| partial.to_string()),
        failure_kind: failure_kind.map(|k| k.as_str().to_string()),
    })
}

/// The failure kind an attempt's adapter error is treated as (spec 2026-08-05
/// section 2.3), i.e. the curated classification of the detail string plus one
/// structural override.
///
/// The override implements the boundary ruling recorded in the section 2.2
/// addendum: with `require_verdict` in force, an attempt killed on its deadline
/// or lost to a transport error recorded no verdict, so it is an INFRASTRUCTURE
/// interruption and earns the same bounded same-executor retry as any other
/// transient failure. Before this, that shape broke straight to the fallback
/// chain without ever retrying the executor the author had chosen (and, with no
/// fallback chain at all, failed the node after a single attempt).
///
/// Without `require_verdict` a deadline kill keeps its pre-existing meaning
/// (advance the chain), so no existing playbook's timeout behavior moves.
fn effective_failure_kind(detail: &str, class: ErrorClass, require_verdict: bool) -> FailureKind {
    let kind = crate::failure_class::classify(detail);
    if kind == FailureKind::Agent
        && require_verdict
        && matches!(class, ErrorClass::Timeout | ErrorClass::Transport)
    {
        return FailureKind::Transient;
    }
    kind
}

/// The template text a node renders for its own execution, or `None` for a kind
/// that renders none. Scripts, conditions, waits and reviews have no template;
/// a finish-with-prompt composes through `execute_finish_answer` and a
/// sub-playbook instruction through `run_playbook_node`, neither of which passes
/// through [`execute_node`].
fn prompt_template(kind: &NodeKind) -> Option<&str> {
    match kind {
        NodeKind::AgentTask { prompt, .. } | NodeKind::Prompt { prompt } => Some(prompt.as_str()),
        _ => None,
    }
}

/// Journals a missing-input anomaly for every `nodes.<id>.output|report` a node's
/// template reads that ACTUALLY renders empty (spec 2026-08-05 section 1.5).
///
/// The read still renders as an empty string, byte for byte: an agent-task cache
/// key is derived from the rendered prompt, so changing the rendering would move
/// every key, and failing the node would break the either-or merges that
/// legitimately reference both branches. What was missing was any trace at all -
/// an agent got a prompt with a hole in it and nothing said so.
///
/// The criterion is emptiness of the RENDERED value, not the source's status,
/// which is what makes the event's claim true by construction. `resolve` reads
/// `state.outputs` unconditionally and the fold records an output for every
/// terminal status, so a `defaults.on_failure` handler reading its failed
/// source - the canonical handler pattern - receives real text and is no anomaly,
/// while a source that finished with an empty output is one whatever its status
/// was.
///
/// One anomaly per node execution (not per attempt), listing every hole with the
/// reason it is one, journaled through the same `WakeRaised { Anomaly }`
/// mechanism as the empty-output anomaly.
fn journal_missing_inputs(
    journal: &Journal,
    run_dir: &Path,
    node_id: &str,
    template: &str,
    state: &RunState,
) -> Result<(), EngineError> {
    let missing: Vec<String> = crate::context::node_output_refs(template)
        .into_iter()
        .filter_map(|(reference, id)| {
            let rendered = state.outputs.get(&id);
            if rendered.is_some_and(|text| !text.is_empty()) {
                return None;
            }
            let why = match (state.nodes.get(&id), rendered.is_some()) {
                (None, _) => "never ran".to_string(),
                (Some(st), false) => st.as_str().to_string(),
                (Some(st), true) => format!("{} with empty output", st.as_str()),
            };
            Some(format!("`{reference}` ({why})"))
        })
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    journal.raise_wake(
        run_dir,
        crate::event::WakeTrigger::Anomaly,
        node_id,
        format!(
            "node `{node_id}` reads {}, so the reference renders empty",
            missing.join(", ")
        ),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_node(
    playbook: &Playbook,
    run_dir: &Path,
    workdir: &Path,
    node_id: &str,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    override_prompt: Option<String>,
    cancel: &AtomicBool,
    env_scrub: &[String],
    journal: &Journal,
    resume: Option<ResumeContext>,
    live: Option<LiveContext>,
) -> Result<AttemptOutcome, EngineError> {
    let node = playbook
        .node(node_id)
        .ok_or_else(|| EngineError::NotFound(node_id.into()))?;
    // Missing-input observability (spec 2026-08-05 section 1.5). Here rather
    // than in the drive loop because this is the ONE site both arms and the
    // interactive path share, and it fires once per execution. Skipped when the
    // node's own template is not what runs: a prompt override replaces it
    // wholesale, and a resume re-invocation carries only the user's answer.
    if override_prompt.is_none()
        && resume.is_none()
        && let Some(template) = prompt_template(&node.kind)
    {
        journal_missing_inputs(journal, run_dir, node_id, template, state)?;
    }
    let mut events: Vec<EventPayload> = Vec::new();
    match &node.kind {
        NodeKind::Start => Ok(AttemptOutcome::Finished {
            status: NodeStatus::Succeeded,
            output: String::new(),
            events,
        }),
        NodeKind::Prompt { prompt } => {
            let text = match &override_prompt {
                Some(p) => p.clone(),
                None => render_node_prompt(run_dir, run_id, state, cfg, prompt)?,
            };
            Ok(AttemptOutcome::Finished {
                status: NodeStatus::Succeeded,
                output: text,
                events,
            })
        }
        NodeKind::Condition { .. } => Ok(AttemptOutcome::Finished {
            status: NodeStatus::Succeeded,
            output: String::new(),
            events,
        }),
        NodeKind::AgentTask {
            prompt,
            profile,
            max_retries,
            timeout_seconds,
            isolation,
            interactive,
            question_timeout_seconds,
            default_answer,
            require_verdict,
            ..
        } => {
            // Live question-timeout enforcement inputs (spec 2026-07-20, Task 11
            // fix): a live attempt enforces `question_timeout_seconds` on the
            // drive thread from `on_tick`, posting `default_answer` (as
            // `"timeout"`) or failing the attempt when none is set. Owned so the
            // per-attempt `on_tick` closure can capture them freely; only ever
            // consulted on the live path.
            let live_q_timeout: Option<u64> = *question_timeout_seconds;
            let live_default: Option<String> = default_answer.clone();
            // On a `resume` re-invocation the follow-up prompt IS the user's
            // answer (the prior context lives in the agent's own session); an
            // ordinary attempt renders the node prompt (or takes the reprompt
            // override the drive loop supplied).
            let mut text = match (&resume, &override_prompt) {
                (Some(rc), _) => rc.answer.clone(),
                (None, Some(p)) => p.clone(),
                (None, None) => render_node_prompt(run_dir, run_id, state, cfg, prompt)?,
            };
            // Issue #45 finding 2 + issue #56 finding 4: deliver the run
            // instruction, every applied supervisor note, and the precedence
            // frame into the agent attempt prompt as trailing sections, even
            // when the template references neither `{{run.context}}` nor
            // `{{run.instruction}}`. Resume re-invocations carry only the user's
            // answer (session holds prior context) and skip this. Script nodes
            // never reach this arm. Assembled outside the cache-key render in
            // `render_node_prompt`, so note/instruction/frame text (all fixed
            // per run) does not shift the cache key.
            // One journal read serves both the assembly here and the
            // interruption-note seed below.
            let journaled: Option<Vec<Event>> = if resume.is_none() {
                Some(read_all(run_dir)?)
            } else {
                None
            };
            if let Some(events) = &journaled {
                text = crate::context::assemble_agent_prompt(
                    &text,
                    cfg.instruction.as_deref(),
                    events,
                );
            }
            let retries = max_retries.or(playbook.defaults.max_retries).unwrap_or(0);
            // Required verdict (spec 2026-08-05 section 2.2). The node field is a
            // plain bool, so it can only turn the requirement ON; a playbook-wide
            // `defaults.require_verdict` turns it on for every agent_task. Same
            // node-then-defaults direction as `max_retries` above.
            let require_verdict =
                *require_verdict || playbook.defaults.require_verdict.unwrap_or(false);
            let timeout = timeout_seconds.map(Duration::from_secs);
            // Stall detection (spec 2026-07-21 run-reliability) fires ONLY for a
            // node whose author set an explicit `expected_duration`, never off
            // the per-kind default, so a run with no estimates raises no false
            // anomalies. `None` here leaves the attempt's stall watch disabled.
            let expected_secs: Option<u64> =
                node.expected_duration.as_ref().and_then(|ed| ed.parsed());

            // Autonomy grant (spec 8.5): reaching node execution means the run
            // already cleared the policy/trust gate, where the user consented
            // to the run's effects. An agent-task node's effects always include
            // acting effects (fs_write/network/external), so we hand the agent
            // its non-interactive permission flags; otherwise a headless
            // one-shot agent blocks on approvals it can never receive.
            //
            // The grant is all-or-nothing: any effective effect beyond FsRead
            // yields the full non-interactive permission set (not a per-effect
            // subset). This matches the pessimistic effect model - inference
            // already unions fs_write/network/external onto every acting node,
            // so a narrower declared effect does not narrow the grant. If the
            // effect taxonomy ever gains finer acting effects, revisit this to
            // avoid silently granting full bypass for a narrow declaration.
            let grant_autonomy = apb_core::effects::effective(playbook)
                .iter()
                .any(|e| !matches!(e, apb_core::schema::Effect::FsRead));

            // A single step of the executor chain. For the profile path it carries
            // the invocation fixed in the manifest (call form + binary) rather than
            // re-deriving it from the live config at execution time (spec 3.6).
            struct Step {
                agent: String,
                model: String,
                soul_delivery: Option<String>,
                invocation: Option<crate::invocation::ResolvedInvocation>,
            }

            // A node's executor is always a profile (schema 2). We take the
            // chain/SOUL/skills from the run's immutable manifest (spec 3.6): editing
            // the live profile after the run has started has no effect on the run.
            let _ = profile;
            let manifest = crate::manifest::read(run_dir)?.ok_or_else(|| {
                EngineError::Invalid(format!(
                    "node `{node_id}` has no execution manifest: this run predates agent profiles and cannot be resumed after the schema 2 upgrade - start a fresh run"
                ))
            })?;
            // The EFFECTIVE binding: a mid-run rebind (issue #45 finding 5)
            // overlays the manifest for future attempts of this node.
            let entry = effective_for_node(run_dir, &manifest, node_id)?.ok_or_else(|| {
                EngineError::Invalid(format!(
                    "no manifest entry for node `{node_id}` (no profile bound)"
                ))
            })?;

            let steps: Vec<Step> = entry
                .chain
                .iter()
                .map(|ri| Step {
                    agent: ri.agent_id.clone(),
                    model: ri.model.clone(),
                    soul_delivery: Some(soul_delivery_str(ri.soul_delivery)),
                    invocation: Some(ri.clone()),
                })
                .collect();
            let soul_text = Some(entry.soul.clone());
            let skill_names: Vec<String> = entry.skills.iter().map(|s| s.name.clone()).collect();
            let profile_key = Some(entry.key());

            if steps.is_empty() {
                return Err(EngineError::Invalid(format!(
                    "node `{node_id}` has an empty executor chain"
                )));
            }

            // Skill delivery (spec 6.4, completion-plan Task 3). For an isolated
            // node (isolation full|best_effort) skills are materialized as REAL
            // copies from the run snapshot into an isolated per-node workdir
            // (`.agents/skills/<name>` + a `.claude/skills` bridge), and the agent is
            // pointed at it: editing the live skill after the run has started has no
            // effect on the run. For `isolation: none` - an advisory string of names
            // in the shared workdir. Skill content is never embedded into the prompt
            // (only names).
            let isolated = matches!(
                isolation,
                Some(Isolation::Full) | Some(Isolation::BestEffort)
            );
            let skills_mode = if isolated { "materialized" } else { "advisory" };
            if !skill_names.is_empty() {
                text = format!(
                    "{text}\n\nRelevant skills: {} - use them via your skills mechanism",
                    skill_names.join(", ")
                );
            }

            // Connector instruction block (spec 6 step 3): when this node holds
            // grants, tell the agent which connectors/accounts/functions it may
            // call and how. Built only from the run snapshot (manifest non-secret
            // fields + snapshotted ConnectorDocs), so no secret reaches the prompt.
            let grants = manifest.grants_for(node_id);
            if !grants.is_empty() {
                let docs =
                    crate::connector::prompt::load_snapshot_docs(run_dir, &manifest.connectors);
                let block = crate::connector::prompt::instruction_block(
                    grants,
                    &manifest.connectors,
                    &docs,
                );
                if !block.is_empty() {
                    text = format!("{text}\n\n{block}");
                }
            }

            // Interactive contract paragraph. A LIVE attempt (spec 2026-07-20,
            // Task 11) gets the `ask_user` paragraph: the tool exists, when to
            // use it, and to route free-form questions through it rather than
            // assuming an answer. A resume/reprompt interactive node gets the
            // marker contract (print the marker plus a JSON question and stop).
            // Appended once here so it rides the first invocation and each
            // re-invocation. Non-interactive nodes receive neither. The marker
            // scan stays active on a live node too, so a live agent that ignores
            // the tool and prints the marker still parks (no regression).
            if live.is_some() {
                text = format!("{text}\n\n{}", crate::adapter::LIVE_PROMPT_PARAGRAPH);
            } else if *interactive {
                text = format!("{text}\n\n{}", marker_contract());
            }

            // Status-file contract (subtask S2): a node with a success_check may
            // hand its final verdict as JSON via $APB_STATUS_FILE, which the
            // engine reads before parsing the textual report. Mentioned when a
            // success_check exists, and - in its stronger form - whenever a
            // verdict is REQUIRED (spec 2026-08-05 section 2.2); a plain node
            // keeps the report-only contract.
            let status_note =
                super::status_file::status_file_note(node.success_check.is_some(), require_verdict);
            if !status_note.is_empty() {
                text = format!("{text}\n\n{status_note}");
            }

            // Connector env isolation (spec 4.3) for every attempt's agent spawn:
            // scrub inherited connector tokens and hand the agent the run-context
            // env that `apb connector call` reads.
            let connector_policy = crate::adapter::ConnectorEnvPolicy {
                scrub: env_scrub.to_vec(),
                run_dir: Some(run_dir.to_path_buf()),
                node_id: Some(node_id.to_string()),
            };

            // Resume argv (spec 2026-07-20, Task 7): when this is a `resume`
            // re-invocation, resolve the primary agent's declarative resume form
            // and substitute the captured session id as a whole argv element.
            // `{prompt}`/`{model}` stay for `build_command`. `None` here means
            // the drive loop already decided resume is unavailable (it hands a
            // `resume: None`); leaving it defensively also collapses to the
            // normal argv. The resume path targets ONLY the primary executor -
            // the session belongs to it, so there is no fallback to a different
            // agent.
            let resume_argv: Option<Vec<String>> = resume.as_ref().and_then(|rc| {
                crate::invocation::resume_argv(&steps[0].agent).map(|tmpl| {
                    tmpl.into_iter()
                        .map(|a| {
                            if a == "{session}" {
                                rc.session.clone()
                            } else {
                                a
                            }
                        })
                        .collect()
                })
            });

            let mut attempt: u32 = 0;
            let mut last_msg = String::new();
            // Set once an attempt of this node ended without recording a required
            // verdict (spec 2026-08-05 section 2.2): every later attempt's prompt
            // then carries the interruption note, so the fresh agent looks for the
            // work already done instead of blindly redoing it (#71 items 3 and 5).
            //
            // SEEDED from the journal, because this flag would otherwise only ever
            // see interruptions of THIS execution. A node whose attempt was closed
            // `interrupted` by a different execution - the drive-entry reaper after
            // a driver death (spec 2.4), a supervisor retry, a `--from-node` re-run
            // - would start its fresh attempt with no note at all, and the reaped
            // case is the one where the note matters most: the process died with
            // its mid-work text, so the journal preserved no partial output either
            // and the prompt is the only channel left.
            //
            // Gated on `require_verdict`: the note closes by telling the agent to
            // record its final verdict in the status file, which is only truthful
            // when the status-file contract is in the prompt. A plain node keeps
            // the report-only contract, so extending the note there needs a text
            // split, not this flag.
            let mut was_interrupted = require_verdict
                && match &journaled {
                    Some(events) => super::journal::last_attempt_interrupted(events, node_id),
                    // A resume re-invocation skipped the read above; pay for one
                    // rather than silently drop the note.
                    None => super::journal::last_attempt_interrupted(&read_all(run_dir)?, node_id),
                };
            // The node's final status once all attempts are exhausted: TimedOut if
            // the last attempt was interrupted by a timeout, otherwise Failed.
            let mut last_timed_out = false;
            // Fallback sameness guard: the (agent, model) pair of the step that
            // was just actually attempted (not the positionally-previous step,
            // which may itself have been skipped). Compared against each
            // candidate step in turn, so a chain X -> Y -> X still attempts the
            // third step (it differs from Y, the step that just failed), while
            // X -> X collapses (identical to the step that just failed, most
            // likely doomed by the same external cause - e.g. a token lacking
            // permission - not by the agent or model).
            let mut last_tried: Option<(String, String)> = None;
            // Agents whose attempt failed non-transiently (spec 2026-08-05
            // section 2.3): an expired credential or an exhausted spend limit is
            // a property of the AGENT and its account, not of the model or the
            // prompt, so every later chain step on that agent is doomed the same
            // way and is skipped without a `fallback_triggered`. This sits
            // beside the `(agent, model)` sameness guard above rather than
            // inside it: that guard only collapses an identical binding, and the
            // gap issue #74 finding 2 describes is exactly a SAME-AGENT,
            // different-model step being walked into after a spend limit.
            // A different agent may well have its own working credential and
            // budget, so cross-agent fallback stays allowed.
            let mut blocked_agents: BTreeSet<String> = BTreeSet::new();
            // A resume re-invocation runs the primary step only (see above);
            // an ordinary attempt walks the whole fallback chain.
            let step_count = if resume.is_some() { 1 } else { steps.len() };
            for (idx, step) in steps.iter().enumerate().take(step_count) {
                if idx > 0 {
                    let same_binding = last_tried
                        .as_ref()
                        .is_some_and(|(agent, model)| *agent == step.agent && *model == step.model);
                    if same_binding || blocked_agents.contains(&step.agent) {
                        continue;
                    }
                    events.push(EventPayload::FallbackTriggered {
                        node: node_id.into(),
                        from: last_tried
                            .as_ref()
                            .map(|(agent, _)| agent.clone())
                            .unwrap_or_else(|| steps[idx - 1].agent.clone()),
                        to: step.agent.clone(),
                        profile: profile_key.clone(),
                        // Both models, so a claude -> claude fallback that only
                        // changed the model is finally legible in the journal
                        // (issue #74 finding 2).
                        from_model: Some(
                            last_tried
                                .as_ref()
                                .map(|(_, model)| model.clone())
                                .unwrap_or_else(|| steps[idx - 1].model.clone()),
                        ),
                        to_model: Some(step.model.clone()),
                    });
                }
                last_tried = Some((step.agent.clone(), step.model.clone()));
                // The profile path builds the adapter from the fixed invocation
                // (call form + canonical binary from the manifest), so that editing
                // agents.<id>.invocation in the config between start and resume does
                // not silently change the prompt contract. The executor path is unchanged.
                let adapter: Box<dyn crate::adapter::AgentAdapter> = match &step.invocation {
                    Some(ri) => {
                        // On a resume re-invocation the primary step's invocation
                        // form is replaced by the agent's resume argv (session
                        // already substituted); the canonical binary, autonomy
                        // flags, and transport are kept. The resume form always
                        // delivers the follow-up via argv `{prompt}`.
                        let spec = match &resume_argv {
                            Some(rargv) => apb_core::config::InvocationDef {
                                argv: rargv.clone(),
                                prompt_via: apb_core::config::PromptVia::Argv,
                                ..ri.spec.clone()
                            },
                            None => ri.spec.clone(),
                        };
                        Box::new(crate::adapter::ClaudeAdapter {
                            program: ri.canonical_executable.to_string_lossy().into_owned(),
                            spec,
                        })
                    }
                    None => adapter_for(&step.agent)?,
                };
                // Hermetic isolation (subtask S1): when the bound profile sets
                // `hermetic: true`, claude/claude-code get an apb-owned minimal
                // settings file (user plugins and hooks off) handed over via
                // `--settings`. Any other agent has no such mechanism, so we warn
                // and proceed without isolation rather than failing the run. The
                // file content is fixed, so writing it once per step is enough.
                let hermetic_settings: Option<PathBuf> = if entry.hermetic {
                    if crate::adapter::agent_supports_hermetic(&step.agent) {
                        Some(crate::adapter::write_hermetic_settings(run_dir)?)
                    } else {
                        eprintln!(
                            "apb: warning: node `{node_id}` profile requests hermetic isolation but agent `{}` has no isolation mechanism; running without it",
                            step.agent
                        );
                        None
                    }
                } else {
                    None
                };
                // The node's own retry budget is walked by `try_i`; an
                // INFRASTRUCTURE retry (spec 2026-08-05 section 2.3) does not
                // advance it, which is why this is a while loop and not
                // `for try_i in 0..=retries`. `infra_used` counts the separate
                // infrastructure budget, whose size IS the length of the backoff
                // schedule (default two: 5 s, then 30 s).
                let mut try_i: u32 = 0;
                let mut infra_used: usize = 0;
                let backoff = crate::failure_class::backoff_schedule();
                while try_i <= retries {
                    // Set by the failure handling below when it spent an
                    // infrastructure retry instead of a node retry: the same
                    // executor is attempted again after a backoff, and `try_i`
                    // stays where it is.
                    let mut infra_retry = false;
                    // Cancellation (this branch lost a join:any) - exit with status
                    // Cancelled, not counting this as a failure.
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(AttemptOutcome::Finished {
                            status: NodeStatus::Cancelled,
                            output: "cancelled".to_string(),
                            events,
                        });
                    }
                    attempt += 1;
                    if try_i > 0 {
                        events.push(EventPayload::RetryStarted {
                            node: node_id.into(),
                            attempt,
                        });
                    }
                    // Attempt working directory. For an isolated node - a FRESH
                    // per-attempt directory `work/<node>/<attempt>` with skills
                    // freshly materialized from the snapshot: a hostile/failed
                    // previous attempt cannot slip a modified bundle to the next one
                    // (skills_mode: materialized would then not reflect
                    // reality). For `isolation: none` - the shared workdir.
                    let attempt_workdir: PathBuf = if isolated {
                        let wd = run_dir.join("work").join(node_id).join(attempt.to_string());
                        // Fail-closed: a missing directory is normal, but any other
                        // cleanup error is NOT swallowed - otherwise we would materialize
                        // skills on top of leftovers from the previous (possibly hostile) attempt.
                        match std::fs::remove_dir_all(&wd) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(e.into()),
                        }
                        materialize_isolated_skills(run_dir, &entry, &wd)?;
                        wd
                    } else {
                        workdir.to_path_buf()
                    };
                    // Where to stream the attempt's NDJSON events (acp transport); one
                    // file per attempt. The headless field ignores it.
                    let stream_log = run_dir
                        .join("agent-stream")
                        .join(format!("{node_id}-{attempt}.jsonl"));
                    // Per-attempt status file (subtask S2): the agent MAY write
                    // its final verdict here as JSON; the engine reads it before
                    // parsing the textual report. Mirrors the agent-stream
                    // naming. The directory is created once per attempt
                    // (idempotent); this is set for EVERY attempt, independent of
                    // any success_check.
                    let status_dir = run_dir.join("agent-status");
                    std::fs::create_dir_all(&status_dir)?;
                    let status_file = status_dir.join(format!("{node_id}-{attempt}.json"));
                    // Stale status-file removal (issue #70 item 3): a resume or
                    // continue_from re-run can restart the attempt counter, so a
                    // status file from a PRIOR execution may still sit at this
                    // attempt's path. Drop it before spawning (ok-ignored: a missing
                    // file is the normal case) so `read_status_file` after the run
                    // can only ever adopt a file THIS attempt actually wrote.
                    let _ = std::fs::remove_file(&status_file);
                    // This attempt's prompt: the assembled node prompt, plus the
                    // interruption note when a previous attempt of this node was
                    // cut off mid-work. Appended here rather than inside
                    // `render_node_prompt`, so the recovery note (fixed text) does
                    // not shift the node's cache key.
                    let attempt_prompt: std::borrow::Cow<'_, str> = if was_interrupted {
                        std::borrow::Cow::Owned(format!(
                            "{text}\n\n{}",
                            super::status_file::INTERRUPTION_NOTE
                        ))
                    } else {
                        std::borrow::Cow::Borrowed(text.as_str())
                    };
                    let task = AgentTask {
                        prompt: attempt_prompt.as_ref(),
                        model: &step.model,
                        workdir: &attempt_workdir,
                        timeout,
                        stream_log: Some(&stream_log),
                        // A resume re-invocation delivers no SOUL: the resumed
                        // session already carries its role prompt, and the
                        // follow-up is only the user's answer.
                        soul: if resume.is_some() {
                            None
                        } else {
                            soul_text.as_deref()
                        },
                        grant_autonomy,
                        connector_policy: &connector_policy,
                        interactive: *interactive,
                        // Ordinary agent_task attempts carry the spec 6.2 report
                        // contract so the agent's self-assessed status routes the node.
                        report_contract: true,
                        node: node_id,
                        agent: &step.agent,
                        // Node-output contract (Finding 2 of issue #56): honor
                        // `outputs.extract` so the persisted output is the
                        // agent's marker-wrapped work product, robust to host
                        // Stop-hook / guardrail turns injected after the work.
                        extract: node.outputs.as_ref().and_then(|o| o.extract.as_deref()),
                        // Handed to the agent as APB_STATUS_FILE; read back below
                        // before the success_check gate.
                        status_file: Some(status_file.clone()),
                        // Hermetic isolation (subtask S1): Some only for a
                        // hermetic profile on an isolation-capable agent.
                        hermetic_settings: hermetic_settings.clone(),
                    };
                    // Spawn-time attempt journaling. The adapter invokes `on_spawn`
                    // right after the agent process starts, so `attempt_started`
                    // (carrying the child pid) is on disk BEFORE the agent does any
                    // work: a crash mid-attempt then leaves an open attempt the
                    // fold maps to interrupted. `spawn_at` records the spawn instant
                    // for `duration_ms`; `spawn_err` carries an append failure from
                    // inside the callback back out so it is not swallowed.
                    let cur_attempt = attempt;
                    let agent_name = step.agent.clone();
                    let soul_del = step.soul_delivery.clone();
                    let smode = Some(skills_mode.to_string());
                    let spawn_at: std::cell::Cell<Option<std::time::Instant>> =
                        std::cell::Cell::new(None);
                    let spawn_err: std::cell::RefCell<Option<EngineError>> =
                        std::cell::RefCell::new(None);
                    let on_spawn = |pid: u32| {
                        spawn_at.set(Some(std::time::Instant::now()));
                        if let Err(e) = journal.append(EventPayload::AttemptStarted {
                            node: node_id.to_string(),
                            attempt: cur_attempt,
                            agent: agent_name.clone(),
                            soul_delivery: soul_del.clone(),
                            skills_mode: smode.clone(),
                            pid: Some(pid),
                        }) {
                            *spawn_err.borrow_mut() = Some(e);
                        }
                    };
                    // Live channel observation (spec 2026-07-20, Task 11): for a
                    // live attempt the adapter's drive-owned poll loop calls
                    // `on_tick` on THIS (the drive) thread each wait iteration,
                    // where drive journals the question/answer round as it lands
                    // through `observe_live_channels`. The single-writer
                    // invariant holds: no second thread journals. A journal
                    // failure is carried out of the closure like `spawn_err`.
                    let tick_err: std::cell::RefCell<Option<EngineError>> =
                        std::cell::RefCell::new(None);
                    // Set by `on_tick` when the open question timed out with no
                    // default answer: the message fails the attempt (Task 5
                    // wording) and `abort` tells the adapter to tear the agent
                    // down. Attempt-local, so only this node fails.
                    let timeout_msg: std::cell::RefCell<Option<String>> =
                        std::cell::RefCell::new(None);
                    let abort = AtomicBool::new(false);
                    let on_tick = || {
                        if tick_err.borrow().is_some() || abort.load(Ordering::Relaxed) {
                            return;
                        }
                        // Unqualified via `use super::*`: node.rs already reaches
                        // scheduler that way, so no new module edge is added.
                        match tick_live_observation(
                            run_dir,
                            node_id,
                            journal,
                            live_q_timeout,
                            live_default.as_deref(),
                        ) {
                            Ok(Some(msg)) => {
                                *timeout_msg.borrow_mut() = Some(msg);
                                abort.store(true, Ordering::Relaxed);
                            }
                            Ok(None) => {}
                            Err(e) => *tick_err.borrow_mut() = Some(e),
                        }
                    };
                    let live_hooks = live.as_ref().map(|lc| crate::adapter::LiveHooks {
                        inject: crate::adapter::LiveInject {
                            exe: lc.exe.clone(),
                            run_id: run_id.to_string(),
                            attempt: cur_attempt,
                            timeout_ms: lc.timeout_ms,
                        },
                        on_tick: &on_tick,
                        abort: &abort,
                    });
                    // Stall anomaly (spec 2026-07-21): the adapter's poll loop
                    // calls this once if the attempt runs past its estimate. It
                    // journals a SupervisorAction marker (which run_status reads
                    // back as `past_estimate`) plus an Anomaly wake so a waiting
                    // supervisor returns. A journal failure is carried out like
                    // `spawn_err`/`tick_err`. Built only for a node that set
                    // `expected_duration`; otherwise the hook is `None`.
                    let stall_err: std::cell::RefCell<Option<EngineError>> =
                        std::cell::RefCell::new(None);
                    let on_stall = |elapsed: Duration| {
                        let detail = format!(
                            "agent_task node `{node_id}` attempt {cur_attempt} is running past its estimate: {}s elapsed vs {}s expected; the run may be stalled",
                            elapsed.as_secs(),
                            expected_secs.unwrap_or(0),
                        );
                        if let Err(e) = journal.append(EventPayload::SupervisorAction {
                            action: crate::stall::STALL_ACTION.to_string(),
                            node: Some(node_id.to_string()),
                            detail: detail.clone(),
                        }) {
                            *stall_err.borrow_mut() = Some(e);
                            return;
                        }
                        if let Err(e) = journal.raise_wake(
                            run_dir,
                            crate::event::WakeTrigger::Anomaly,
                            node_id,
                            detail,
                        ) {
                            *stall_err.borrow_mut() = Some(e);
                        }
                    };
                    let stall_hooks = expected_secs.map(|s| crate::adapter::StallHooks {
                        expected: Duration::from_secs(s),
                        on_stall: &on_stall,
                    });
                    // Live control observation (finding 7 of issue #42, third
                    // item of issue #40): the adapter's poll loop calls this on
                    // THIS (the drive) thread each iteration. It journals every
                    // control message that lands mid-attempt as `control_received`
                    // - so a supervisor sees its message was seen live, not only
                    // at the next node boundary - and, on a `Control::Interrupt`,
                    // journals `attempt_interrupted` and sets `interrupt` so the
                    // poll loop SIGKILLs the agent. A journal failure is carried
                    // out like `tick_err`/`stall_err`. The baseline is the last
                    // control seq already posted when this attempt began, so a
                    // command queued before the attempt is not re-acked.
                    let control_err: std::cell::RefCell<Option<EngineError>> =
                        std::cell::RefCell::new(None);
                    let interrupt = AtomicBool::new(false);
                    // Hard-fail if the baseline cannot be read: a None baseline
                    // would replay the channel and a stale interrupt must never
                    // replay into this fresh attempt.
                    let control_seen: std::cell::Cell<Option<u64>> =
                        std::cell::Cell::new(latest_control_seq(run_dir)?);
                    let on_control_poll = || {
                        if control_err.borrow().is_some() {
                            return;
                        }
                        match observe_control(
                            run_dir,
                            node_id,
                            cur_attempt,
                            journal,
                            control_seen.get(),
                        ) {
                            Ok((new_seen, interrupt_requested)) => {
                                control_seen.set(new_seen);
                                if interrupt_requested {
                                    interrupt.store(true, Ordering::Relaxed);
                                }
                            }
                            Err(e) => *control_err.borrow_mut() = Some(e),
                        }
                    };
                    let control_hooks = crate::adapter::ControlHooks {
                        on_poll: &on_control_poll,
                        interrupt: &interrupt,
                    };
                    let outcome = adapter.run_cancellable(
                        &task,
                        cancel,
                        Some(&on_spawn),
                        live_hooks.as_ref(),
                        stall_hooks.as_ref(),
                        Some(&control_hooks),
                    );
                    if let Some(e) = spawn_err.borrow_mut().take() {
                        return Err(e);
                    }
                    if let Some(e) = tick_err.borrow_mut().take() {
                        return Err(e);
                    }
                    if let Some(e) = stall_err.borrow_mut().take() {
                        return Err(e);
                    }
                    if let Some(e) = control_err.borrow_mut().take() {
                        return Err(e);
                    }
                    // Question-timeout-without-default (spec 2026-07-20, Task 11
                    // fix): the adapter tore the agent down on the abort flag.
                    // Fail this attempt with the node-named message, journaling
                    // the paired `attempt_finished`; no retry/fallback, since a
                    // question timeout does not resolve by re-running the agent.
                    if let Some(msg) = timeout_msg.borrow_mut().take() {
                        let duration_ms = spawn_at.get().map(|t| t.elapsed().as_millis() as u64);
                        journal.append(EventPayload::AttemptFinished {
                            node: node_id.into(),
                            attempt,
                            status: "failed".into(),
                            duration_ms,
                            session: None,
                            summary: None,
                            rejected_output: None,
                            partial_output: None,
                            failure_kind: None,
                        })?;
                        return Ok(AttemptOutcome::Finished {
                            status: NodeStatus::Failed,
                            output: msg,
                            events,
                        });
                    }
                    let spawn_instant = spawn_at.get();
                    // The spawn itself failed before the callback ran: still journal
                    // a started (pid unknown) so every attempt_finished is preceded
                    // by an attempt_started.
                    if spawn_instant.is_none() {
                        journal.append(EventPayload::AttemptStarted {
                            node: node_id.into(),
                            attempt,
                            agent: step.agent.clone(),
                            soul_delivery: step.soul_delivery.clone(),
                            skills_mode: Some(skills_mode.to_string()),
                            pid: None,
                        })?;
                    }
                    let duration_ms = spawn_instant.map(|t| t.elapsed().as_millis() as u64);
                    // The attempt's verdict, read ONCE for both branches (spec
                    // 2026-08-05 section 2.1). The status file is the agent's
                    // explicit completion signal, so it decides the attempt even
                    // when the process then exited non-zero, was signalled, or was
                    // killed on the timeout: that exit is transport-level noise
                    // once the verdict exists (issue #74 finding 1).
                    let verdict = super::status_file::read_status_file(&status_file);
                    match outcome {
                        Ok(mut report) => {
                            // Interactive suspension (spec 2026-07-20): the agent
                            // asked a question via the stdout marker instead of
                            // finishing. The attempt genuinely ran, so journal its
                            // paired `attempt_finished`, then hand drive a
                            // suspension to park on rather than composing a
                            // NodeFinished. The marker is honored only on
                            // interactive nodes.
                            if *interactive && let Some(q) = report.question {
                                journal.append(EventPayload::AttemptFinished {
                                    node: node_id.into(),
                                    attempt,
                                    status: report.status.as_str().into(),
                                    duration_ms,
                                    session: report.session.clone(),
                                    summary: Some(report.summary.clone()),
                                    rejected_output: None,
                                    partial_output: None,
                                    failure_kind: None,
                                })?;
                                return Ok(AttemptOutcome::Suspended {
                                    question: q.question,
                                    options: q.options,
                                });
                            }
                            // Status-file precedence (subtask S2): before the
                            // success_check gate, prefer the agent's JSON verdict
                            // in APB_STATUS_FILE when present and valid. It
                            // overrides the parsed status and, when it carries a
                            // non-empty outputs object, the node output; an absent
                            // or invalid file leaves the textual report intact (the
                            // fallback). The success_check gate below then runs on
                            // the effective status/output, so a status-file success
                            // that success_check rejects still consumes a retry and
                            // records rejected_output (S3 behavior preserved).
                            if let Some(sfr) = &verdict {
                                report.status = sfr.status;
                                if let Some(out) = &sfr.outputs {
                                    report.output = out.clone();
                                }
                            }
                            if require_verdict && verdict.is_none() {
                                // Required verdict (spec 2026-08-05 section 2.2,
                                // issue #71 item 1): the process ended normally but
                                // recorded NO verdict, so whatever it printed is a
                                // mid-work message rather than a result - which is
                                // exactly how a cut-off session used to be recorded
                                // as a success. Classify the attempt interrupted,
                                // preserve the partial text, consume a retry, and
                                // tell the next attempt to look for work already
                                // done.
                                journal_interrupted_attempt(
                                    journal,
                                    node_id,
                                    attempt,
                                    duration_ms,
                                    report.session.clone(),
                                    &report.output,
                                    // The process exited NORMALLY, there is no
                                    // failure detail to classify: what the agent
                                    // printed is mid-work text, not an error
                                    // message from the transport.
                                    None,
                                )?;
                                last_msg = report.output;
                                last_timed_out = false;
                                was_interrupted = true;
                            } else if report.status == NodeStatus::Succeeded {
                                // Empty-output anomaly (issue #42, finding 6): an
                                // attempt that reports success but produced no
                                // output at all is almost always a lost/truncated
                                // reply (e.g. a signal that emptied stdout). It
                                // stays succeeded, but a WakeRaised anomaly is
                                // journaled so the emptiness is visible in the
                                // event log, exactly like the stall anomaly.
                                if report.output.trim().is_empty() {
                                    journal.raise_wake(
                                        run_dir,
                                        crate::event::WakeTrigger::Anomaly,
                                        node_id,
                                        format!(
                                            "agent_task node `{node_id}` attempt {cur_attempt} reported success with empty output"
                                        ),
                                    )?;
                                }
                                // A success_check gates the self-report. It runs only
                                // AFTER this branch's agent has succeeded (meaning this
                                // branch was not cancelled) - we do not propagate
                                // cancellation here.
                                let rejection = success_check_rejection(
                                    node.success_check.as_ref(),
                                    run_dir,
                                    &attempt_workdir,
                                    &report.output,
                                )?;
                                match rejection {
                                    // Rejected: this is an attempt FAILURE, not a
                                    // terminal node failure. Journal it as `failed`
                                    // carrying the discarded report text
                                    // (`rejected_output`), then fall through so the
                                    // retry loop iterates and, once exhausted, the
                                    // fallback chain advances - honoring max_retries
                                    // and fallbacks exactly like an ordinary failure.
                                    // The raw agent text is preserved for the
                                    // downstream `nodes.<id>.rejected_output`.
                                    Some(reason) => {
                                        journal.append(EventPayload::AttemptFinished {
                                            node: node_id.into(),
                                            attempt,
                                            status: "failed".into(),
                                            duration_ms,
                                            session: report.session.clone(),
                                            summary: Some(report.summary.clone()),
                                            rejected_output: Some(report.output.clone()),
                                            partial_output: None,
                                            failure_kind: None,
                                        })?;
                                        // Keep the human-readable reason on the
                                        // terminal failure message while the raw
                                        // agent text lives in `rejected_output`.
                                        last_msg = format!("{reason}: {}", report.output);
                                        last_timed_out = false;
                                    }
                                    None => {
                                        journal.append(EventPayload::AttemptFinished {
                                            node: node_id.into(),
                                            attempt,
                                            status: report.status.as_str().into(),
                                            // Session id captured from this attempt
                                            // (spec 2026-07-20, Task 7); the drive
                                            // loop reads it back to resume the agent
                                            // on the answer round.
                                            duration_ms,
                                            session: report.session.clone(),
                                            // Display-only summary (issue #42 finding
                                            // 1): kept for humans, never node output.
                                            summary: Some(report.summary.clone()),
                                            rejected_output: None,
                                            partial_output: None,
                                            failure_kind: None,
                                        })?;
                                        return Ok(AttemptOutcome::Finished {
                                            status: NodeStatus::Succeeded,
                                            // Node output is the agent's reply body
                                            // (report block stripped), NOT the
                                            // one-line summary (issue #42 finding 1):
                                            // templating, output_match, and
                                            // run_report all read this.
                                            output: report.output,
                                            events,
                                        });
                                    }
                                }
                            } else {
                                journal.append(EventPayload::AttemptFinished {
                                    node: node_id.into(),
                                    attempt,
                                    status: report.status.as_str().into(),
                                    duration_ms,
                                    session: report.session.clone(),
                                    summary: Some(report.summary.clone()),
                                    rejected_output: None,
                                    partial_output: None,
                                    failure_kind: None,
                                })?;
                                last_msg = report.output;
                                last_timed_out = false;
                            }
                        }
                        Err((class, msg)) => {
                            // Cancellation mid-adapter-work: kill returned Transport,
                            // but this is not a failure - mark the node Cancelled.
                            if cancel.load(Ordering::Relaxed) {
                                return Ok(AttemptOutcome::Finished {
                                    status: NodeStatus::Cancelled,
                                    output: "cancelled".to_string(),
                                    events,
                                });
                            }
                            last_timed_out = class == ErrorClass::Timeout;
                            let attempt_status = if last_timed_out {
                                "timed_out"
                            } else {
                                "failed"
                            };
                            // A supervisor-issued interrupt is a CONTROL decision,
                            // not transport noise (spec 2026-08-05 section 2.2
                            // addendum): a verdict written before the kill does not
                            // override it, so `supervisor_interrupt_attempt` keeps
                            // its documented contract (the attempt is journaled
                            // failed and ordinary retry/fallback/patch proceeds).
                            // Distinct from the run-level `cancel` above, which
                            // returns `Cancelled` instead of failing the attempt.
                            let interrupted_by_supervisor = interrupt.load(Ordering::Relaxed);
                            // The spec-2.3 classification of THIS failure, set
                            // only where it applies: no verdict was written and no
                            // supervisor interrupt overruled the attempt. A
                            // written verdict already decided the attempt (a
                            // failure the agent reported about its own work is not
                            // infrastructure), and a supervisor kill is a control
                            // decision, so neither is classified and neither earns
                            // an infrastructure retry.
                            let mut failure_kind: Option<FailureKind> = None;
                            // The verdict decides the attempt even here (spec
                            // 2026-08-05 section 2.1): the process exit, the signal,
                            // or the deadline kill is transport-level noise once the
                            // agent has written its explicit completion signal.
                            match &verdict {
                                // Overruled by the supervisor. The verdict is not
                                // thrown away silently: it rides the anomaly wake
                                // (and `partial_output`) so the supervisor can see
                                // the work existed and accept it explicitly, for
                                // instance by re-running from the next node.
                                Some(sfr) if interrupted_by_supervisor => {
                                    let recorded =
                                        sfr.outputs.clone().unwrap_or_else(|| msg.clone());
                                    journal.raise_wake(
                                        run_dir,
                                        crate::event::WakeTrigger::Anomaly,
                                        node_id,
                                        format!(
                                            "agent_task node `{node_id}` attempt {cur_attempt} recorded a `{}` verdict in its status file, but a supervisor interrupt overrules it and the attempt stays {attempt_status}: {recorded}",
                                            sfr.status.as_str()
                                        ),
                                    )?;
                                    journal.append(EventPayload::AttemptFinished {
                                        node: node_id.into(),
                                        attempt,
                                        status: attempt_status.into(),
                                        duration_ms,
                                        session: None,
                                        summary: None,
                                        rejected_output: None,
                                        partial_output: Some(recorded),
                                        // A supervisor interrupt is a control
                                        // decision, not an infrastructure
                                        // failure: nothing is classified.
                                        failure_kind: None,
                                    })?;
                                    last_msg = msg;
                                }
                                // SUCCESS was recorded before the process died
                                // (issue #74 finding 1: a tail crash used to discard
                                // the finished deliverable). The attempt succeeds
                                // with the outputs the agent wrote, and the abnormal
                                // exit is journaled as an anomaly so it stays
                                // visible. Without a written outputs object the
                                // adapter's failure detail is kept as the output: it
                                // carries the agent's own stderr/stdout tail, which
                                // beats handing downstream nodes nothing.
                                Some(sfr) if sfr.status == NodeStatus::Succeeded => {
                                    journal.raise_wake(
                                        run_dir,
                                        crate::event::WakeTrigger::Anomaly,
                                        node_id,
                                        format!(
                                            "agent_task node `{node_id}` attempt {cur_attempt} recorded a success verdict in its status file, then its process ended abnormally: {msg}"
                                        ),
                                    )?;
                                    let output = sfr.outputs.clone().unwrap_or_else(|| msg.clone());
                                    // The recovered verdict does not bypass the
                                    // gate: a success_check still runs on it,
                                    // exactly as on the Ok branch, and a rejection
                                    // consumes a retry with the discarded text
                                    // preserved (S3 behavior).
                                    match success_check_rejection(
                                        node.success_check.as_ref(),
                                        run_dir,
                                        &attempt_workdir,
                                        &output,
                                    )? {
                                        None => {
                                            journal.append(EventPayload::AttemptFinished {
                                                node: node_id.into(),
                                                attempt,
                                                status: NodeStatus::Succeeded.as_str().into(),
                                                duration_ms,
                                                session: None,
                                                summary: None,
                                                rejected_output: None,
                                                partial_output: None,
                                                failure_kind: None,
                                            })?;
                                            return Ok(AttemptOutcome::Finished {
                                                status: NodeStatus::Succeeded,
                                                output,
                                                events,
                                            });
                                        }
                                        Some(reason) => {
                                            journal.append(EventPayload::AttemptFinished {
                                                node: node_id.into(),
                                                attempt,
                                                status: "failed".into(),
                                                duration_ms,
                                                session: None,
                                                summary: None,
                                                rejected_output: Some(output.clone()),
                                                partial_output: None,
                                                failure_kind: None,
                                            })?;
                                            last_msg = format!("{reason}: {output}");
                                            last_timed_out = false;
                                        }
                                    }
                                }
                                // FAILURE was recorded: the attempt stays failed (a
                                // timeout stays timed out), but the agent's own
                                // outputs become the attempt output instead of the
                                // raw CLI error text.
                                Some(sfr) => {
                                    journal.append(EventPayload::AttemptFinished {
                                        node: node_id.into(),
                                        attempt,
                                        status: attempt_status.into(),
                                        duration_ms,
                                        session: None,
                                        summary: None,
                                        rejected_output: None,
                                        partial_output: None,
                                        failure_kind: None,
                                    })?;
                                    last_msg = sfr.outputs.clone().unwrap_or_else(|| msg.clone());
                                }
                                // No verdict: today's failure semantics, except that
                                // a require_verdict node labels the exit an
                                // interruption (spec section 2.2) and preserves what
                                // the process produced. Either label consumes the
                                // same retry; the label plus `partial_output` say
                                // which of the two it was.
                                None => {
                                    // This is the one shape a curated classifier
                                    // can say something useful about: the process
                                    // died and left only the adapter's detail
                                    // string behind (spec section 2.3).
                                    if !interrupted_by_supervisor {
                                        failure_kind = Some(effective_failure_kind(
                                            &msg,
                                            class,
                                            require_verdict,
                                        ));
                                    }
                                    if require_verdict {
                                        journal_interrupted_attempt(
                                            journal,
                                            node_id,
                                            attempt,
                                            duration_ms,
                                            None,
                                            &msg,
                                            failure_kind,
                                        )?;
                                        was_interrupted = true;
                                    } else {
                                        journal.append(EventPayload::AttemptFinished {
                                            node: node_id.into(),
                                            attempt,
                                            status: attempt_status.into(),
                                            duration_ms,
                                            session: None,
                                            summary: None,
                                            rejected_output: None,
                                            partial_output: None,
                                            failure_kind: failure_kind
                                                .map(|k| k.as_str().to_string()),
                                        })?;
                                    }
                                    last_msg = msg;
                                }
                            }
                            // Bounded infrastructure retry (spec 2026-08-05
                            // section 2.3): a transient failure is the
                            // infrastructure's fault, so the SAME executor is
                            // attempted again out of its own budget - the node's
                            // `max_retries` belongs to the agent's own mistakes -
                            // after a backoff. This also resolves the section 2.2
                            // addendum ruling: a `require_verdict` attempt lost to
                            // a deadline kill or a transport error is classified
                            // transient by `effective_failure_kind`, so it now
                            // retries the chosen executor instead of breaking
                            // straight to fallback, while keeping its
                            // `interrupted` label and its partial output.
                            if failure_kind == Some(FailureKind::Transient)
                                && infra_used < backoff.len()
                            {
                                let wait = backoff[infra_used];
                                infra_used += 1;
                                // Cheapest observable form: a supervisor-action
                                // marker in the attempt timeline, right between
                                // the failed attempt and its infrastructure retry.
                                // No new event type, so old readers are unaffected.
                                journal.append(EventPayload::SupervisorAction {
                                    action: INFRA_RETRY_ACTION.to_string(),
                                    node: Some(node_id.to_string()),
                                    detail: format!(
                                        "agent_task node `{node_id}` attempt {cur_attempt} failed with a transient infrastructure error; waiting {} ms before infrastructure retry {infra_used} of {} on the same executor `{}`",
                                        wait.as_millis(),
                                        backoff.len(),
                                        step.agent,
                                    ),
                                })?;
                                // Tick-polled, so an abort posted during a 30 s
                                // backoff lands within a tick. A cancelled wait
                                // returns here and the loop's own cancellation
                                // check (top of the next iteration) owns the
                                // decision, exactly as it does for a cancel that
                                // arrives between attempts.
                                crate::failure_class::wait_backoff(wait, cancel);
                                infra_retry = true;
                            } else if failure_kind.is_some_and(FailureKind::is_non_transient) {
                                // Non-transient (auth, budget): no further attempt
                                // on this step can succeed, so the remaining node
                                // retries are skipped, and the chain loop above
                                // will skip every later step on this same agent.
                                blocked_agents.insert(step.agent.clone());
                                break;
                            } else if class == ErrorClass::Transport || class == ErrorClass::Timeout
                            {
                                // A transport error and a timeout break the retry loop for this
                                // executor and go to fallback. Unchanged by the verdict
                                // handling above: a step that cannot be re-run usefully is
                                // still abandoned in favor of the next executor.
                                break;
                            }
                        }
                    }
                    // An infrastructure retry re-attempts the same executor
                    // without spending a node retry, so the node's budget only
                    // advances here.
                    if !infra_retry {
                        try_i += 1;
                    }
                }
            }
            let final_status = if last_timed_out {
                NodeStatus::TimedOut
            } else {
                NodeStatus::Failed
            };
            Ok(AttemptOutcome::Finished {
                status: final_status,
                output: last_msg,
                events,
            })
        }
        NodeKind::Script {
            script,
            runner,
            timeout_seconds,
        } => {
            let timeout = timeout_seconds.map(Duration::from_secs);
            // Pass through cancel: in a parallel batch (join:any) the winning
            // branch sets the flag, and a running script is torn down together with
            // its process group - without leaking side effects after a sibling wins.
            let r = run_script(run_dir, workdir, script, runner, timeout, Some(cancel))?;
            Ok(AttemptOutcome::Finished {
                status: r.status,
                output: r.stdout,
                events,
            })
        }
        NodeKind::Finish { .. } => Ok(AttemptOutcome::Finished {
            status: NodeStatus::Succeeded,
            output: String::new(),
            events,
        }),
        // human_review is handled inside drive itself (pause until a decision), it
        // never reaches here; this branch is defensive. wait - subphase 7b.
        NodeKind::HumanReview { .. } => Err(EngineError::Invalid(format!(
            "node `{node_id}` (human_review) must be handled by drive"
        ))),
        NodeKind::Wait { .. } => Err(EngineError::Invalid(format!(
            "node `{node_id}` (wait) must be handled by drive"
        ))),
        NodeKind::Playbook { .. } => Err(EngineError::Invalid(format!(
            "node `{node_id}` (playbook) must be handled by drive"
        ))),
    }
}

/// Composes the run answer for a finish-with-prompt (spec B). A reduced
/// `agent_task`: the profile chain + SOUL come from the run manifest (identical
/// resolution/trust to an agent_task), the prompt renders with the full
/// standard context, but no skills are delivered and there is no success_check
/// and no isolation. Timeout/retries fall back to `defaults`. Returns
/// (status, answer, events). Like `execute_node`, the two attempt-lifecycle
/// events are journaled directly (`attempt_started` with pid at spawn,
/// `attempt_finished` with `duration_ms` at return) so a crash during the
/// terminal answer composition leaves an open attempt on disk; every other
/// event is returned for drive to write in its return batch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_finish_answer(
    playbook: &Playbook,
    run_dir: &Path,
    workdir: &Path,
    node_id: &str,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    prompt: &str,
    cancel: &AtomicBool,
    env_scrub: &[String],
    journal: &Journal,
) -> Result<(NodeStatus, String, Vec<EventPayload>), EngineError> {
    // The terminal answer is composed from the FULL log fold, never the lossy
    // compacted render view (issue #42 finding 5): the closing answer must see
    // every completed node's raw output, which always survives verbatim in the
    // append-only log even after the compaction that repeated resume +
    // patch-migration cycles trigger. See `build_terminal_context`.
    let events = read_all(run_dir)?;
    // Issue #70 item 1: the terminal composer must see the run instruction ONLY
    // as quoted reference context (attached below by `assemble_finish_answer_prompt`),
    // never as a `## run instruction` directive header. So the auto context here
    // carries the completed nodes' recorded output but NOT the instruction header.
    let context = build_terminal_context(&events, None);
    let hooks: BTreeMap<String, String> = crate::hooks::read_hooks(run_dir)?
        .into_iter()
        .map(|(k, secret)| (k, crate::hooks::hook_path(run_id, &secret)))
        .collect();
    let text = render(
        prompt,
        &cfg.params,
        cfg.instruction.as_deref(),
        &state.outputs,
        &state.reviews,
        &state.rejected_outputs,
        &hooks,
        &context,
    );
    // Finish-with-prompt scopes its composer prompt (issue #70 item 1): the run
    // instruction rides along ONLY as quoted reference context and the composer is
    // told its sole deliverable is a human-readable closing message. Unlike an
    // ordinary agent_task it gets no precedence frame (which would make it read
    // the instruction as an overriding order) and, paired with `report_contract:
    // false` on its task below, no status-verdict protocol. Supervisor notes are
    // still delivered as steering.
    // Spec 2026-08-05 section 2.7 (issue #74 finding 6): the context above
    // reaches the composer ONLY through a `{{run.context}}` substitution, so a
    // finish prompt that places no context reference of its own used to be handed
    // zero upstream output while the deliverable statement still told it to
    // summarize "the recorded run context above". Such a template gets the
    // terminal context appended; one that reads the context itself (through
    // `{{run.context}}` or an explicit `{{nodes.*}}` field) keeps the byte-identical
    // assembly it has today, since its author already chose what the composer sees.
    let auto_context =
        (!crate::context::reads_recorded_context(prompt)).then_some(context.as_str());
    // A finish prompt reading `{{nodes.X.output}}` is subject to the same
    // missing-input hole as any node template, and it does not pass through
    // `execute_node` where that check lives (Task 4 handover note 5).
    journal_missing_inputs(journal, run_dir, node_id, prompt, state)?;
    let text = crate::context::assemble_finish_answer_prompt(
        &text,
        cfg.instruction.as_deref(),
        &events,
        auto_context,
    );
    let retries = playbook.defaults.max_retries.unwrap_or(0);
    let timeout = playbook.defaults.timeout_seconds.map(Duration::from_secs);
    let grant_autonomy = apb_core::effects::effective(playbook)
        .iter()
        .any(|e| !matches!(e, apb_core::schema::Effect::FsRead));

    let manifest = crate::manifest::read(run_dir)?.ok_or_else(|| {
        EngineError::Invalid(format!("finish node `{node_id}` has no execution manifest"))
    })?;
    // The EFFECTIVE binding: honor a mid-run rebind (issue #45 finding 5).
    let entry = effective_for_node(run_dir, &manifest, node_id)?.ok_or_else(|| {
        EngineError::Invalid(format!("no manifest entry for finish node `{node_id}`"))
    })?;
    if entry.chain.is_empty() {
        return Err(EngineError::Invalid(format!(
            "finish node `{node_id}` has an empty executor chain"
        )));
    }

    // The drive's run-level cancel flag (Task 8), the same one the inline
    // agent_task path gets: a stop posted while this finish-answer agent is
    // composing the run answer kills its process tree instead of waiting it
    // out. Before Task 8 this was a fresh, permanently-false local token.
    // Connector env isolation (spec 4.3): the finish-answer agent is a spawned
    // agent too, so its inherited connector tokens are scrubbed and it gets the
    // run-context env.
    let connector_policy = crate::adapter::ConnectorEnvPolicy {
        scrub: env_scrub.to_vec(),
        run_dir: Some(run_dir.to_path_buf()),
        node_id: Some(node_id.to_string()),
    };
    let mut events: Vec<EventPayload> = Vec::new();
    let mut attempt: u32 = 0;
    let mut last_msg = String::new();
    let mut last_timed_out = false;
    // Fallback sameness guard (same semantics as `execute_node` above): compare
    // each candidate step against the (agent, model) pair actually attempted
    // last, not against the positionally-previous step.
    let mut last_tried: Option<(String, String)> = None;
    for (idx, ri) in entry.chain.iter().enumerate() {
        if idx > 0 {
            let same_binding = last_tried
                .as_ref()
                .is_some_and(|(agent, model)| *agent == ri.agent_id && *model == ri.model);
            if same_binding {
                continue;
            }
            events.push(EventPayload::FallbackTriggered {
                node: node_id.into(),
                from: last_tried
                    .as_ref()
                    .map(|(agent, _)| agent.clone())
                    .unwrap_or_else(|| entry.chain[idx - 1].agent_id.clone()),
                to: ri.agent_id.clone(),
                profile: Some(entry.key()),
                // Models, as in the agent_task chain above. The finish-answer
                // composer does not classify failures (spec 2.3 targets the
                // agent_task attempt lifecycle), so it gets the observability
                // half only.
                from_model: Some(
                    last_tried
                        .as_ref()
                        .map(|(_, model)| model.clone())
                        .unwrap_or_else(|| entry.chain[idx - 1].model.clone()),
                ),
                to_model: Some(ri.model.clone()),
            });
        }
        last_tried = Some((ri.agent_id.clone(), ri.model.clone()));
        let adapter = crate::adapter::ClaudeAdapter {
            program: ri.canonical_executable.to_string_lossy().into_owned(),
            spec: ri.spec.clone(),
        };
        for try_i in 0..=retries {
            attempt += 1;
            if try_i > 0 {
                events.push(EventPayload::RetryStarted {
                    node: node_id.into(),
                    attempt,
                });
            }
            let stream_log = run_dir
                .join("agent-stream")
                .join(format!("{node_id}-{attempt}.jsonl"));
            let task = AgentTask {
                prompt: &text,
                model: &ri.model,
                workdir,
                timeout,
                stream_log: Some(&stream_log),
                soul: Some(entry.soul.as_str()),
                grant_autonomy,
                connector_policy: &connector_policy,
                // A finish-answer node is never interactive: it composes the
                // run's terminal answer, it does not ask the user questions.
                interactive: false,
                // Issue #70 item 1: the composer's whole reply IS the closing
                // message, so it gets no status-verdict protocol. Its scoped prompt
                // (assemble_finish_answer_prompt) already states the deliverable.
                report_contract: false,
                node: node_id,
                agent: &ri.agent_id,
                // Finish-answer composition uses today's last-message output.
                extract: None,
                // Internal finish-answer composition: no status-file protocol.
                status_file: None,
                // Internal finish-answer composition: no hermetic isolation.
                hermetic_settings: None,
            };
            // Spawn-time attempt journaling (identical shape to execute_node):
            // `on_spawn` journals attempt_started with the child pid before the
            // agent runs, and records the spawn instant for duration_ms, so a
            // crash during the terminal answer composition leaves an open attempt
            // the fold maps to interrupted.
            let cur_attempt = attempt;
            let agent_name = ri.agent_id.clone();
            let soul_del = Some(soul_delivery_str(ri.soul_delivery));
            let spawn_at: std::cell::Cell<Option<std::time::Instant>> = std::cell::Cell::new(None);
            let spawn_err: std::cell::RefCell<Option<EngineError>> = std::cell::RefCell::new(None);
            let on_spawn = |pid: u32| {
                spawn_at.set(Some(std::time::Instant::now()));
                if let Err(e) = journal.append(EventPayload::AttemptStarted {
                    node: node_id.to_string(),
                    attempt: cur_attempt,
                    agent: agent_name.clone(),
                    soul_delivery: soul_del.clone(),
                    skills_mode: None,
                    pid: Some(pid),
                }) {
                    *spawn_err.borrow_mut() = Some(e);
                }
            };
            // A finish-answer node is never interactive, so it never runs the
            // live sidecar (`None`), it carries no `expected_duration`, so no
            // stall watch (`None`), and the terminal answer composition is not a
            // supervisor-interruptible node, so no control observation (`None`).
            let outcome = adapter.run_cancellable(&task, cancel, Some(&on_spawn), None, None, None);
            if let Some(e) = spawn_err.borrow_mut().take() {
                return Err(e);
            }
            let spawn_instant = spawn_at.get();
            // Spawn failed before the callback ran: still journal a started
            // (pid unknown) so every attempt_finished is preceded by a started.
            if spawn_instant.is_none() {
                journal.append(EventPayload::AttemptStarted {
                    node: node_id.into(),
                    attempt,
                    agent: ri.agent_id.clone(),
                    soul_delivery: Some(soul_delivery_str(ri.soul_delivery)),
                    skills_mode: None,
                    pid: None,
                })?;
            }
            let duration_ms = spawn_instant.map(|t| t.elapsed().as_millis() as u64);
            match outcome {
                Ok(report) => {
                    journal.append(EventPayload::AttemptFinished {
                        node: node_id.into(),
                        attempt,
                        status: report.status.as_str().into(),
                        duration_ms,
                        session: report.session.clone(),
                        summary: Some(report.summary.clone()),
                        rejected_output: None,
                        partial_output: None,
                        failure_kind: None,
                    })?;
                    if report.status == NodeStatus::Succeeded {
                        // The composed finish answer is the agent's reply body,
                        // not the one-line summary (issue #42 finding 1).
                        return Ok((NodeStatus::Succeeded, report.output, events));
                    }
                    last_msg = report.output;
                    last_timed_out = false;
                }
                Err((class, msg)) => {
                    last_timed_out = class == ErrorClass::Timeout;
                    journal.append(EventPayload::AttemptFinished {
                        node: node_id.into(),
                        attempt,
                        status: if last_timed_out {
                            "timed_out"
                        } else {
                            "failed"
                        }
                        .into(),
                        duration_ms,
                        session: None,
                        summary: None,
                        rejected_output: None,
                        partial_output: None,
                        failure_kind: None,
                    })?;
                    last_msg = msg;
                    if class == ErrorClass::Transport || class == ErrorClass::Timeout {
                        break;
                    }
                }
            }
        }
    }
    let final_status = if last_timed_out {
        NodeStatus::TimedOut
    } else {
        NodeStatus::Failed
    };
    Ok((final_status, last_msg, events))
}

/// Materializes profile skills as REAL copies from the run snapshot into the
/// isolated per-node workdir (completion-plan Task 3). The source is the snapshot
/// (`run_dir/profiles/<scope>/<name>/skills/<sscope>/<sname>`), NOT the live
/// `.agents/skills`: editing a skill after the run has started has no effect on
/// the run. The `.claude/skills` bridge is aimed at the real copies via symlinks.
/// The workdir is created even without skills (an isolated node execution directory).
pub(crate) fn materialize_isolated_skills(
    run_dir: &Path,
    entry: &ManifestProfile,
    workdir: &Path,
) -> Result<(), EngineError> {
    let skills_parent = workdir.join(".agents/skills");
    std::fs::create_dir_all(&skills_parent)?;
    for sk in &entry.skills {
        let src = run_dir
            .join("profiles")
            .join(&entry.scope)
            .join(&entry.name)
            .join("skills")
            .join(&sk.scope)
            .join(&sk.name);
        copy_tree(&src, &skills_parent.join(&sk.name))?;
    }
    if !entry.skills.is_empty() {
        let claude_parent = workdir.join(".claude/skills");
        // Fail-closed: the isolated node's workdir is fresh, so the
        // `.claude/skills` bridge must be laid down cleanly. Any note here is a
        // real failure (a symlink could not be created, etc.), not a benign case of
        // "already exists/foreign bridge"; silently continuing would mean running the
        // agent without skills visible via `.claude` and passing off an incorrect run as a success.
        let notes = apb_core::skills::ensure_claude_bridge(&skills_parent, &claude_parent);
        if !notes.is_empty() {
            return Err(EngineError::Invalid(format!(
                "isolated skill bridge failed: {}",
                notes.join("; ")
            )));
        }
    }
    Ok(())
}

/// Recursively copies a skill-snapshot tree. Symlinks are RECREATED as symlinks
/// (not dereferenced), in parity with `content::snapshot_tree`, which
/// preserves in-tree relative symlinks: otherwise a symlinked directory would fail
/// in `fs::copy` with EISDIR and abort the run. `file_type()` from `read_dir` does not
/// follow symlinks, so a symlink is never `is_dir()` - we check it first.
pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<(), EngineError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&from)?;
                std::os::unix::fs::symlink(&target, &to)?;
            }
            #[cfg(not(unix))]
            {
                // Off unix, skill symlinks are not supported - copy the target instead.
                std::fs::copy(&from, &to)?;
            }
        } else if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// The run id of the latest ChildRunStarted for `node_id`, if any.
pub(crate) fn latest_child_run(events: &[Event], node_id: &str) -> Option<String> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ChildRunStarted { node_id: n, run_id } if n == node_id => {
            Some(run_id.clone())
        }
        _ => None,
    })
}

/// Whether a run directory has reached a terminal run status.
///
/// Honest errors (review I7/R1-I9): the child's event log is the sole source of
/// truth for terminality, so an unreadable/corrupt child dir must NOT be guessed
/// at. The old `read_all(..).unwrap_or_default()` folded a read failure into an
/// empty log, which reads as "not terminal" and would make the reattach path in
/// `run_playbook_node` resume the same broken child forever. Returning the read
/// error instead propagates as a hard node/run failure: this cannot loop (no
/// silent reattach) and cannot fake success (no empty-log Running/Succeeded).
/// `read_all` already returns Ok(empty) for a genuinely absent log, so only a
/// real IO/parse fault surfaces here.
pub(crate) fn run_is_terminal(root: &Path, run_id: &str) -> Result<bool, EngineError> {
    let dir = root.join(".apb/runs").join(run_id);
    let events = read_all(&dir)?;
    Ok(matches!(
        RunState::fold(&events).run_status,
        RunStatus::Succeeded | RunStatus::Failed | RunStatus::Aborted
    ))
}

/// The parent run's definition origin (from its RunProvenance event), used to
/// resolve a child's `scope: auto` the same way the policy gate does (parent
/// origin first, then global). Defaults to Project when the label is absent.
fn parent_run_origin(run_dir: &Path) -> apb_core::scope::Origin {
    use apb_core::scope::Origin;
    let events = read_all(run_dir).unwrap_or_default();
    for e in &events {
        if let EventPayload::RunProvenance {
            origin: Some(label),
            ..
        } = &e.payload
        {
            return if label == "global" {
                Origin::Global
            } else {
                Origin::Project { workspace_id: None }
            };
        }
    }
    Origin::Project { workspace_id: None }
}

/// Builds the child run's [`RunOptions`] from the (optional) verified pin. A
/// gated run carries a pin (`Some`) and threads every anti-TOCTOU `expected_*`
/// map through verbatim, including the child's own verified connector permit
/// maps (finding 2 of issue #42) - without them a sub-playbook that binds
/// connectors would be refused at prepare ("connector bindings present but no
/// connector permit"). An ungated (CLI, `pin: None`) child resolves its
/// connectors live at prepare time, so the maps default to empty there.
fn child_run_options(
    pin: Option<&crate::run_config::ChildExpectation>,
    child_instruction: Option<String>,
    parent_run_id: &str,
    depth: usize,
    continued_from: Option<String>,
) -> RunOptions {
    RunOptions {
        instruction: child_instruction,
        allow_shared_workdir: true,
        parent_run: Some(parent_run_id.to_string()),
        continued_from,
        depth,
        expected_digest: pin.map(|p| p.playbook_digest.clone()),
        expected_profile_bundles: pin.map(|p| p.profile_bundles.clone()),
        expected_children: pin.map(|p| p.children.clone()),
        expected_connectors: pin.map(|p| p.connectors.clone()).unwrap_or_default(),
        expected_connector_accounts: pin
            .map(|p| p.connector_accounts.clone())
            .unwrap_or_default(),
        ..Default::default()
    }
}

/// Executes a `playbook` node (spec C): starts (or, on resume, reattaches to) a
/// full child run and maps its terminal state to this node's status/output. The
/// child runs in-process, synchronously, with `allow_shared_workdir: true` (the
/// parent already holds the workdir lock; see the module notes). ChildRunStarted
/// is appended here (drive thread, single writer) BEFORE the child is driven.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_playbook_node(
    root: &Path,
    run_dir: &Path,
    log: &mut EventLog,
    _playbook: &Playbook,
    cfg: &RunConfig,
    run_id: &str,
    node_id: &str,
    child_ref: &apb_core::schema::QualifiedPlaybookRef,
    node_instruction: Option<&str>,
) -> Result<(NodeStatus, String), EngineError> {
    // Depth backstop.
    if cfg.depth + 1 > MAX_SUBPLAYBOOK_DEPTH {
        return Ok((
            NodeStatus::Failed,
            format!(
                "sub-playbook depth limit ({}) exceeded",
                MAX_SUBPLAYBOOK_DEPTH
            ),
        ));
    }

    // Resume reattach: a still-running child from a prior ChildRunStarted is
    // resumed, not restarted (the event log is the source of truth). The child
    // runs on this drive thread while the parent still holds the workdir lock,
    // so its resume must allow the shared workdir (no second acquire).
    //
    // Single read (review M1): this `events` snapshot is reused below for the
    // instruction render context. No log write happens between the reattach
    // check and that render (the reattach branch returns before any append, and
    // ChildRunStarted is written only much later), so reading once is sound.
    let events = read_all(run_dir)?;
    if let Some(existing) = latest_child_run(&events, node_id)
        && !run_is_terminal(root, &existing)?
    {
        let res = resume_inner(root, &existing, None, false, true)?;
        // Nested resume can also mirror wakes onto the parent log.
        log.resync_seq()?;
        return Ok(map_child_outcome(root, &existing, res.outcome));
    }

    // Render the node instruction with the parent context; the result is the
    // child's explicit instruction (Part A precedence). Absent -> None (child
    // falls back to its own draft). Reuses the `events` read above (review M1).
    let child_instruction = match node_instruction {
        Some(t) => {
            let context = build_context_for_render(run_dir, &events, cfg.instruction.as_deref())?;
            let hooks: BTreeMap<String, String> = crate::hooks::read_hooks(run_dir)?
                .into_iter()
                .map(|(k, secret)| (k, crate::hooks::hook_path(run_id, &secret)))
                .collect();
            let state = RunState::fold(&events);
            Some(render(
                t,
                &cfg.params,
                cfg.instruction.as_deref(),
                &state.outputs,
                &state.reviews,
                &state.rejected_outputs,
                &hooks,
                &context,
            ))
        }
        None => None,
    };

    // Resolve the child reference. A gate pin (cfg.expected_children) fixes the
    // scope + version verbatim (anti-TOCTOU); without a pin (CLI path) we live
    // resolve with the same candidate order the policy gate uses: an explicit
    // scope pins the origin, `auto` prefers the parent origin then global.
    use apb_core::profile::ProfileScope;
    use apb_core::scope::{Origin, PlaybookRef, scope_candidates};
    // Fail-closed pins (review I4): `expected_children == None` is the ungated
    // (CLI) path and lives-resolves. But a gated run (`Some(map)`) MUST carry a
    // pin for every playbook node its permit walked; a missing entry means this
    // node was outside the verified tree, so we FAIL the node rather than
    // silently live-resolving unverified content.
    let pin = match &cfg.expected_children {
        None => None,
        Some(map) => match map.get(node_id) {
            Some(p) => Some(p),
            None => {
                return Ok((
                    NodeStatus::Failed,
                    format!(
                        "sub-playbook node `{node_id}`: run permit carried no pin for it; refusing to live-resolve under a gated run"
                    ),
                ));
            }
        },
    };
    let resolved = if let Some(p) = pin {
        // The pin's scope is a resolved origin (never `Auto`), so map it back to
        // a concrete `Origin` (review I2 - no string comparison).
        let origin = match p.scope {
            ProfileScope::Global => Origin::Global,
            _ => Origin::Project { workspace_id: None },
        };
        let cref = PlaybookRef {
            origin,
            id: child_ref.id.clone(),
            version: Some(p.version.clone()),
        };
        // A resolve failure here (issue #42 finding 3b) is a refusal to run
        // THIS node, not an engine-fatal fault: return it as an ordinary node
        // failure (like the depth-limit and missing-pin cases above) so it
        // gets its `node_finished` and normal failure handling (fallback edge
        // or supervisor wake) instead of aborting the whole drive loop with no
        // record of why.
        match apb_core::store::resolve(root, &cref) {
            Ok(r) => r,
            Err(e) => {
                return Ok((
                    NodeStatus::Failed,
                    format!("sub-playbook `{}`: {e}", child_ref.id),
                ));
            }
        }
    } else {
        let candidates = scope_candidates(child_ref.scope, &parent_run_origin(run_dir));
        let mut resolved_opt = None;
        for cand in &candidates {
            let cref = PlaybookRef {
                origin: cand.clone(),
                id: child_ref.id.clone(),
                version: None,
            };
            if let Ok(r) = apb_core::store::resolve(root, &cref) {
                resolved_opt = Some(r);
                break;
            }
        }
        match resolved_opt {
            Some(r) => r,
            None => {
                return Ok((
                    NodeStatus::Failed,
                    format!(
                        "sub-playbook `{}` (node `{}`) did not resolve in any candidate scope",
                        child_ref.id, node_id
                    ),
                ));
            }
        }
    };

    let predecessor_child =
        latest_child_run(&events, node_id).filter(|id| run_is_terminal(root, id).unwrap_or(false));

    let opts = child_run_options(
        pin,
        child_instruction,
        run_id,
        cfg.depth + 1,
        predecessor_child,
    );

    // Prepare (get the run id) -> record ChildRunStarted -> drive to terminal.
    let t = PrepareTarget {
        definition_parent: resolved.definition_parent.clone(),
        execution_root: resolved.execution_root.clone(),
        origin_label: resolved.origin_label,
    };
    // A prepare refusal here - most notably "connector bindings present but no
    // connector permit" (issue #42 finding 3b) - is likewise a node failure,
    // not a fatal engine error: the child's own run directory already exists
    // at this point (`prepare_run_target` creates it before any of its own
    // fallible steps) and `prepare_run_target` has already closed it out with
    // its own `RunError` + `run_finished(failed)`, so returning `Ok` here just
    // lets the PARENT record the same reason against this node instead of
    // aborting the parent's own drive loop with nothing to explain why.
    let mut cp = match prepare_run_target(&t, &resolved.id, Some(&resolved.version), opts) {
        Ok(p) => p,
        Err(e) => {
            return Ok((
                NodeStatus::Failed,
                format!("sub-playbook `{}` (node `{node_id}`): {e}", child_ref.id),
            ));
        }
    };
    let child_run_id = cp.run_id.clone();
    log.append(EventPayload::ChildRunStarted {
        node_id: node_id.to_string(),
        run_id: child_run_id.clone(),
    })?;
    let res = drive(
        cp.playbook.clone(),
        &cp.run_dir,
        &resolved.execution_root,
        &mut cp.log,
        &cp.cfg,
        cp.start_node.clone(),
        StartMode::Rerun,
        cp.run_id.clone(),
        RunMode::Autonomous,
        cp.supervisor_expected,
    )?;
    // Child may have mirrored wakes onto this parent log while we held it open
    // (issue #45 finding 8). Re-sync next_seq before any further parent appends.
    log.resync_seq()?;
    Ok(map_child_outcome(root, &child_run_id, res.outcome))
}

/// Maps a child run's terminal status to the parent node's (status, output).
///
/// Honest errors (review I7/R1-I9): on a Succeeded child we must read its event
/// log to compose the answer. The old `read_all(..).unwrap_or_default()` turned
/// an unreadable/corrupt child dir into an empty log, which then yielded node
/// SUCCESS with an empty answer - a corrupted run masquerading as a legit
/// promptless finish. We now distinguish the two: a genuine read failure FAILS
/// the parent node with a diagnostic naming the child run id and the error,
/// while a successful read whose `run_answer` is None (a promptless finish, a
/// legitimately empty answer) stays Succeeded with "".
fn map_child_outcome(root: &Path, child_run_id: &str, outcome: RunStatus) -> (NodeStatus, String) {
    match outcome {
        RunStatus::Succeeded => {
            let dir = root.join(".apb/runs").join(child_run_id);
            match read_all(&dir) {
                Ok(events) => {
                    let answer = crate::progress::run_answer(&dir, &events).unwrap_or_default();
                    (NodeStatus::Succeeded, answer)
                }
                Err(e) => (
                    NodeStatus::Failed,
                    format!(
                        "sub-playbook child run `{child_run_id}` succeeded but its events could not be read: {e}"
                    ),
                ),
            }
        }
        other => (
            NodeStatus::Failed,
            format!(
                "sub-playbook child run `{child_run_id}` ended {}",
                other.as_str()
            ),
        ),
    }
}

/// Whether a node is slow (external work - agent or script), such that it
/// makes sense to execute it in parallel with other branches.
pub(crate) fn is_agent_or_script(playbook: &Playbook, node: &str) -> bool {
    matches!(
        playbook.node(node).map(|n| &n.kind),
        Some(NodeKind::AgentTask { .. }) | Some(NodeKind::Script { .. })
    )
}

/// Whether a node is an interactive `agent_task` (spec 2026-07-20). Such a node
/// may park mid-run on a question, so drive keeps it out of the concurrent
/// batch (which cannot park) and runs it through the sequential park-and-poll
/// path instead.
pub(crate) fn is_interactive(playbook: &Playbook, node: &str) -> bool {
    matches!(
        playbook.node(node).map(|n| &n.kind),
        Some(NodeKind::AgentTask {
            interactive: true,
            ..
        })
    )
}

/// Whether a node may run as a MEMBER of the concurrent batch: slow external
/// work, not interactive, and not a join.
///
/// Joins are excluded because a join's readiness verdict is the sequential
/// path's business. A join whose input already failed is `ReadyFailure` and is
/// deliberately pushed into the frontier so the drive loop can journal it
/// `failed` with the barrier's own reason and, in supervised mode, raise a wake
/// on it. The batch has no such arm: it would simply execute the node and carry
/// the run past a failure the supervisor never saw (review finding I1 of
/// 2026-08-05 Task 3). A `ReadySuccess` join runs sequentially right after
/// instead, which is semantically identical - and most joins are `prompt` nodes,
/// which never batched anyway.
pub(crate) fn is_batchable(playbook: &Playbook, node: &str) -> bool {
    is_agent_or_script(playbook, node)
        && !is_interactive(playbook, node)
        && !parallel::is_join(playbook, node)
}

/// Context compaction (spec 8.5): if enabled (cfg.context_max_bytes) and the
/// full context exceeds the threshold, old sections are compacted by a cheap model
/// into context_compact.md, and a ContextCompacted event is returned, which drive
/// writes (the sole writer of the log). The context_compact.md file is a
/// materialized artifact outside the primary log, so writing it directly here does
/// not violate the single-writer-of-events invariant. The summary does NOT go into
/// the log (a non-deterministic LLM output), which preserves replay determinism.
/// Returns None when compaction is disabled, the threshold is not exceeded, or
/// everything old is already compacted (idempotent on resume). A model failure is
/// not critical: it also returns None then, and the run works on the full context.
pub(crate) fn maybe_compact_context(
    run_dir: &Path,
    workdir: &Path,
    cfg: &RunConfig,
    events: &[Event],
    env_scrub: &[String],
) -> Result<Option<EventPayload>, EngineError> {
    let Some(max_bytes) = cfg.context_max_bytes else {
        return Ok(None);
    };
    if max_bytes == 0 || build_context(events).len() <= max_bytes {
        return Ok(None);
    }
    // We keep the tail at roughly half the limit and compact the rest.
    let Some(boundary) = crate::context::compaction_boundary(events, max_bytes / 2) else {
        return Ok(None);
    };
    let prev = crate::context::latest_compaction(events);
    let prev_up_to = prev.as_ref().map(|(_, s)| *s).unwrap_or(0);
    if boundary <= prev_up_to {
        // Everything old is already compacted - nothing left to compact.
        return Ok(None);
    }
    let prev_summary = prev
        .as_ref()
        .map(|(f, _)| std::fs::read_to_string(run_dir.join(f)).unwrap_or_default())
        .unwrap_or_default();
    let newly_old = crate::context::sections_between(events, prev_up_to, boundary);
    let model = cfg
        .context_compact_model
        .clone()
        .unwrap_or_else(|| "haiku".to_string());
    let adapter = adapter_for("claude-code")?;
    let prompt = format!(
        "Summarize the following playbook run context concisely, preserving key facts, \
         decisions, and outputs that later steps may need. Keep it to a few short \
         paragraphs. Do not add commentary or preamble.\n\n{prev_summary}\n\n{newly_old}"
    );
    // Compaction is synchronous inside drive: without a timeout, a hung model would
    // stall the entire run. We bound it with a finite deadline; on overrun (as with
    // any model error) compaction is not critical - we work on the full context.
    const COMPACTION_TIMEOUT: Duration = Duration::from_secs(120);
    // Connector env isolation (spec 4.3): scrub inherited connector tokens even
    // from this internal summarizer. It performs no connector calls, so it gets
    // no run-context env.
    let connector_policy = crate::adapter::ConnectorEnvPolicy {
        scrub: env_scrub.to_vec(),
        run_dir: None,
        node_id: None,
    };
    let task = AgentTask {
        prompt: &prompt,
        model: &model,
        workdir,
        timeout: Some(COMPACTION_TIMEOUT),
        stream_log: None,
        soul: None,
        // Context compaction only summarizes text; it needs no file or network
        // access, so it stays in the default permission posture.
        grant_autonomy: false,
        connector_policy: &connector_policy,
        // Internal summarizer: not a playbook node, never interactive.
        interactive: false,
        // Preserve the historical report-contract posture for the summarizer; the
        // report block is harmless (interpret_report strips it) and this keeps its
        // behavior byte-identical.
        report_contract: true,
        node: "__context_compact",
        agent: "claude-code",
        // Internal summarizer keeps today's last-message output.
        extract: None,
        // Internal summarizer: no status-file protocol.
        status_file: None,
        // Internal summarizer: no hermetic isolation.
        hermetic_settings: None,
    };
    // The compacted context is the summarizer's full reply body (issue #42
    // finding 1), not its one-line report summary.
    let summary = match adapter.run(&task) {
        Ok(report) => report.output,
        Err(_) => return Ok(None),
    };
    let compact_file = "context_compact.md";
    apb_core::fsutil::atomic_write(&run_dir.join(compact_file), summary.as_bytes())?;
    Ok(Some(EventPayload::ContextCompacted {
        compact_file: compact_file.to_string(),
        model,
        up_to_seq: boundary,
    }))
}

/// Adds the ready successors of a finished node `node` to the frontier. A
/// join target is added only if it is ready (otherwise the branch waits at the
/// join). On a ready join:any it cancels the other unfinished frontier branches
/// (marking them cancelled). The sole writer of events (cancelled) is the
/// calling drive, so the single-writer invariant is preserved.
/// The ready successors a node hands the frontier: its outgoing edges evaluated
/// against the folded status and outputs, dropping the node itself and any join
/// that is not yet ready. Pure - it reads state and writes nothing, so a resume
/// can ask "would advancing past this node have anything to run" WITHOUT any
/// journal side effect. `advance_frontier` layers the join:any cancellation and
/// the frontier writes on top of this.
pub(crate) fn seed_successors(
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    active: &[String],
) -> Vec<String> {
    let mut runnable: Vec<String> = Vec::new();
    for s in parallel::successors(playbook, node, state) {
        let ready = if parallel::is_join(playbook, &s) {
            !matches!(
                parallel::join_readiness(playbook, &s, state, active),
                JoinReadiness::NotReady
            )
        } else {
            true
        };
        if ready && s != node && !runnable.contains(&s) {
            runnable.push(s);
        }
    }
    runnable
}

/// The nodes the run can still execute at the moment a frontier is advanced or
/// a join is weighed: the node in hand, the other branch heads, and whatever
/// `also_active` adds - the members of a concurrent batch that may still be
/// running, or on a resume the [`parallel::pending_heads`] rebuilt from the
/// journal because the in-memory frontier was lost.
///
/// Join readiness treats an input source outside the region reachable from this
/// set as dead, so the set must never under-count: an over-wide set only falls
/// back to the plain wait-until-terminal behavior, while a too-narrow one lets a
/// join fire without a branch that was still going to arrive.
pub(crate) fn active_set(node: &str, frontier: &[String], also_active: &[String]) -> Vec<String> {
    let mut active = vec![node.to_string()];
    for n in frontier.iter().chain(also_active) {
        if !active.contains(n) {
            active.push(n.clone());
        }
    }
    active
}

/// Re-installs the branch heads a previous driver lost. The frontier lives only
/// in the driver's memory, so a drive over an existing run dir has to rebuild it
/// from the journal ([`parallel::pending_heads`]); a fresh run has no finished
/// node yet and so gets nothing. Without this the heads inform a single liveness
/// query and are then forgotten, and the next advance - which computes liveness
/// from the frontier alone - writes the unstarted branches off as dead.
///
/// A head that is a JOIN is restored only when its barrier has already DECIDED.
/// Nothing re-gates readiness once a join becomes `current`, so a `NotReady` join
/// sitting in the frontier would execute early; a join that is already
/// `ReadySuccess` or `ReadyFailure`, on the other hand, has to be restored here,
/// because nothing else will ever offer it again. A `NotReady` one is the case
/// [`advance_frontier`] covers: it re-enters as soon as a further input lands,
/// and that path checks readiness. A join every input of which landed BEFORE the
/// crash gets no further input and no further advance, so leaving it out
/// unconditionally silently dropped it - the run either completed with the
/// barrier never executed or died on the unrelated "has no outgoing edge" error.
///
/// Readiness is weighed with every rebuilt head counted as active, so a join fed
/// by a branch that has not run yet stays `NotReady` and is left to
/// `advance_frontier` exactly as before. A restored `ReadyFailure` join is what
/// the drive loop's own readiness arm expects: it journals the node failed with
/// the barrier's reason instead of executing it.
pub(crate) fn restore_frontier(
    playbook: &Playbook,
    state: &RunState,
    current: &str,
    frontier: &mut Vec<String>,
) {
    let heads = parallel::pending_heads(playbook, state);
    let active = active_set(current, frontier, &heads);
    for head in &heads {
        if head == current || frontier.contains(head) {
            continue;
        }
        if parallel::is_join(playbook, head)
            && matches!(
                parallel::join_readiness(playbook, head, state, &active),
                JoinReadiness::NotReady
            )
        {
            continue;
        }
        frontier.push(head.clone());
    }
}

/// Journals the branch inputs a join is about to proceed WITHOUT, at the moment
/// the engine acts on the readiness verdict (Task 4; Task 1 handover note 1).
///
/// A dead input is a legitimate and common outcome (an either-or merge has one by
/// construction, and the implicit barrier widened how many nodes can have one),
/// but writing it off inside a pure function left no trace at all: a run report
/// showed the skipped branch pending forever while the join reported success.
/// Contrast the `join: any` sibling cancel, which journals `cancelled`.
///
/// One event per decision, listing every source written off for it, on the
/// dedicated [`EventPayload::JoinInputDead`] variant: the same decision is
/// legitimately journaled twice (a resume re-advancing through this function, a
/// loop re-entering an either-or fork), which is honest on its own variant and
/// would be a false "looping supervisor" on `SupervisorAction`. A wake is
/// deliberately NOT raised either: this is routine graph bookkeeping, not an
/// anomaly that needs a supervisor's attention.
fn journal_dead_inputs(
    log: &mut EventLog,
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    active: &[String],
) -> Result<(), EngineError> {
    let sources = parallel::dead_inputs(playbook, node, state, active);
    if sources.is_empty() {
        return Ok(());
    }
    log.append(EventPayload::JoinInputDead {
        node: node.to_string(),
        sources,
    })?;
    Ok(())
}

pub(crate) fn advance_frontier(
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    frontier: &mut Vec<String>,
    also_active: &[String],
    log: &mut EventLog,
) -> Result<(), EngineError> {
    let active = active_set(node, frontier, also_active);
    let mut runnable: Vec<String> = seed_successors(playbook, node, state, &active)
        .into_iter()
        .filter(|s| !frontier.contains(s))
        .collect();
    if let Some(join) = runnable
        .iter()
        .find(|s| {
            matches!(
                parallel::join_kind(playbook, s),
                Some(parallel::JoinKind::Explicit(parallel::JoinMode::Any))
            )
        })
        .cloned()
    {
        for other in std::mem::take(frontier) {
            match parallel::is_join(playbook, &other) {
                // A pending join elsewhere in the graph is not part of this
                // race: it keeps waiting for its own inputs instead of being
                // cancelled. It has to be put back explicitly - the frontier was
                // taken whole, so anything not re-pushed here is simply lost.
                true => frontier.push(other),
                false => {
                    log.append(EventPayload::NodeFinished {
                        node: other,
                        status: "cancelled".into(),
                        attempt: 1,
                        output: String::new(),
                        artifacts: Vec::new(),
                    })?;
                }
            }
        }
        runnable.retain(|s| s == &join);
    }
    // The edges actually selected out of `node` for this advance. Used only to
    // decide which pushes cross a bounded edge and must be journaled. Computed
    // from the same `state`, so it agrees with the `runnable` set above.
    let selected = parallel::selected_edges(playbook, node, state);
    for s in runnable {
        if !frontier.contains(&s) {
            journal_dead_inputs(log, playbook, &s, state, &active)?;
            // Journal a traversal ONLY when the edge taken carries
            // max_traversals (keeps the journal lean). The cap check itself
            // already happened in the pure `selected_edges`/`seed_successors`
            // evaluation; this is where the edge is actually taken, so this is
            // the single counting site (never in the pure seed evaluation), and
            // the resume StartMode::After path counts through here exactly once
            // because it advances via this same function.
            if selected
                .iter()
                .any(|e| e.to == s && e.max_traversals.is_some())
            {
                log.append(EventPayload::EdgeTraversed {
                    from: node.to_string(),
                    to: s.clone(),
                    via_policy: false,
                })?;
            }
            frontier.push(s);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{Control, post_control};
    use apb_core::profile::ProfileScope;
    use std::collections::BTreeMap;

    /// A corrupt control channel at attempt start must surface as an error, not
    /// degrade to a `None` baseline that would replay a stale interrupt.
    #[test]
    fn latest_control_seq_surfaces_read_error_instead_of_none_baseline() {
        let dir = tempfile::tempdir().unwrap();
        // Valid interrupt that would be fatal if re-observed with baseline None.
        let seq = post_control(
            dir.path(),
            Control::Interrupt {
                reason: "already spent".into(),
                node: None,
            },
        )
        .unwrap();
        // Append a corrupt line so the next full-channel read fails.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("control.jsonl"))
            .unwrap();
        writeln!(f, "{{not-valid-json").unwrap();
        f.flush().unwrap();

        let err = latest_control_seq(dir.path()).expect_err(
            "corrupt control.jsonl must fail attempt-start baseline, not degrade to None",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("yaml") || msg.contains("invalid") || msg.contains("expected"),
            "expected a parse/IO style error, got: {msg}"
        );
        // Sanity: the stale interrupt is still on disk (the hazard we refuse to
        // replay by refusing the None baseline).
        let raw = std::fs::read_to_string(dir.path().join("control.jsonl")).unwrap();
        assert!(raw.contains("\"cmd\":\"interrupt\""));
        assert!(raw.contains(&format!("\"seq\":{seq}")) || raw.contains(r#""seq":0"#));
    }

    /// With a correct baseline past a already-posted interrupt, observe_control
    /// must not treat that interrupt as a live request for a fresh attempt.
    #[test]
    fn observe_control_skips_interrupt_at_or_before_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::create(dir.path()).unwrap();
        let journal = Journal::new(&mut log);
        let seq = post_control(
            dir.path(),
            Control::Interrupt {
                reason: "stale".into(),
                node: None,
            },
        )
        .unwrap();

        let (seen, interrupt) = observe_control(dir.path(), "n1", 1, &journal, Some(seq)).unwrap();
        assert_eq!(
            seen,
            Some(seq),
            "baseline must not move without new entries"
        );
        assert!(
            !interrupt,
            "a stale interrupt at-or-before the baseline must not kill a fresh attempt"
        );
    }

    /// Documents the hazard option (a) prevents: baseline None re-reads the
    /// whole channel and would set interrupt on a stale entry.
    #[test]
    fn observe_control_none_baseline_would_see_stale_interrupt() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::create(dir.path()).unwrap();
        let journal = Journal::new(&mut log);
        post_control(
            dir.path(),
            Control::Interrupt {
                reason: "stale".into(),
                node: None,
            },
        )
        .unwrap();

        let (_seen, interrupt) = observe_control(dir.path(), "n1", 1, &journal, None).unwrap();
        assert!(
            interrupt,
            "None baseline replays the channel; latest_control_seq must not degrade to None on error"
        );
    }

    /// A targeted interrupt (spec 2026-08-05 section 1.6) is invisible to every
    /// other node: not acknowledged, not journaled, and not consumed - the entry
    /// stays in the channel for the attempt it names.
    #[test]
    fn observe_control_ignores_an_interrupt_targeting_another_node() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::create(dir.path()).unwrap();
        let journal = Journal::new(&mut log);
        post_control(
            dir.path(),
            Control::Interrupt {
                reason: "the other branch is wedged".into(),
                node: Some("n2".into()),
            },
        )
        .unwrap();

        let (seen, interrupt) = observe_control(dir.path(), "n1", 1, &journal, None).unwrap();
        assert!(
            !interrupt,
            "an interrupt naming another node must not kill this attempt"
        );
        assert_eq!(
            seen, None,
            "the entry must not be consumed on another node's behalf"
        );
        let actions: Vec<String> = crate::event::read_all(dir.path())
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::SupervisorAction { action, .. } => Some(action),
                _ => None,
            })
            .collect();
        assert!(
            actions.is_empty(),
            "another node's interrupt must not be journaled here, got {actions:?}"
        );
    }

    /// The same entry addressed to THIS node is honored exactly like a broadcast.
    #[test]
    fn observe_control_honors_an_interrupt_targeting_this_node() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::create(dir.path()).unwrap();
        let journal = Journal::new(&mut log);
        let seq = post_control(
            dir.path(),
            Control::Interrupt {
                reason: "this branch is wedged".into(),
                node: Some("n1".into()),
            },
        )
        .unwrap();

        let (seen, interrupt) = observe_control(dir.path(), "n1", 1, &journal, None).unwrap();
        assert!(interrupt, "the named node's attempt must be interrupted");
        assert_eq!(seen, Some(seq));
    }

    /// A gated child spawn threads the pin's verified connector permit maps into
    /// the child's `expected_connectors`/`expected_connector_accounts` verbatim
    /// (finding 2 of issue #42), so the child prepare no longer refuses a
    /// connector-binding sub-playbook for want of a permit.
    #[test]
    fn child_run_options_threads_pin_connectors() {
        let mut connectors = BTreeMap::new();
        connectors.insert("mock-tracker".to_string(), "sha256:conn".to_string());
        let mut accounts = BTreeMap::new();
        accounts.insert("mock-tracker/acct1".to_string(), "sha256:acct".to_string());
        let pin = crate::run_config::ChildExpectation {
            id: "child".into(),
            scope: ProfileScope::Project,
            version: "1.0.0".into(),
            playbook_digest: "sha256:pb".into(),
            profile_bundles: BTreeMap::new(),
            connectors: connectors.clone(),
            connector_accounts: accounts.clone(),
            children: BTreeMap::new(),
        };

        let opts = child_run_options(Some(&pin), None, "parent-run", 2, None);
        assert_eq!(opts.expected_connectors, connectors);
        assert_eq!(opts.expected_connector_accounts, accounts);
        assert_eq!(opts.expected_digest.as_deref(), Some("sha256:pb"));
        assert_eq!(opts.depth, 2);
        assert!(opts.allow_shared_workdir);
    }

    /// An ungated (CLI, no pin) child resolves connectors live at prepare, so
    /// the spawn passes empty expected maps rather than an unverified pin.
    #[test]
    fn child_run_options_ungated_has_empty_connector_maps() {
        let opts = child_run_options(None, None, "parent-run", 1, None);
        assert!(opts.expected_connectors.is_empty());
        assert!(opts.expected_connector_accounts.is_empty());
        assert!(opts.expected_digest.is_none());
    }

    /// Lineage threading (issue #40) and connector-permit threading (issue
    /// #42) must both survive the same helper call: a re-created child after a
    /// prior terminal attempt carries `continued_from` alongside the pin's
    /// connector maps.
    #[test]
    fn child_run_options_threads_both_lineage_and_connectors() {
        let mut connectors = BTreeMap::new();
        connectors.insert("mock-tracker".to_string(), "sha256:conn".to_string());
        let pin = crate::run_config::ChildExpectation {
            id: "child".into(),
            scope: ProfileScope::Project,
            version: "1.0.0".into(),
            playbook_digest: "sha256:pb".into(),
            profile_bundles: BTreeMap::new(),
            connectors: connectors.clone(),
            connector_accounts: BTreeMap::new(),
            children: BTreeMap::new(),
        };

        let opts = child_run_options(
            Some(&pin),
            None,
            "parent-run",
            2,
            Some("predecessor-run".to_string()),
        );
        assert_eq!(opts.continued_from.as_deref(), Some("predecessor-run"));
        assert_eq!(opts.expected_connectors, connectors);
    }
}
