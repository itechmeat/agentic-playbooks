//! The judge node and the judge edges in a run (issue #165 Parts 5-7).
//!
//! A judge node renders its state, asks its questions through the run's
//! decision runner (journal first, replay on resume) and, only when the
//! `judge_node` use is at `enforce` and every answer is valid, publishes the
//! thresholded answers as compact JSON. Otherwise it applies its
//! `on_unavailable`: `route` and `default` succeed with an output naming the
//! reason, `fail` fails like any node, and `emulate` asks the configured
//! `llm_emulation` providers and then the node's profile (an APB agent run
//! once on the primary executor, at most one retry, never in a host-mode
//! run, which spawns no agent CLI), failing the node when that fails too.
//!
//! Judge edges are asked once when their source node succeeds, all edges of
//! that node in one request, journaled before routing; edge selection then
//! reads the answers back from the folded journal.
//! Shares the parent module's imports via `use super::*`.

use super::*;

use apb_core::decisions::DecisionMode;
use apb_decide::{DecideError, DecisionRequest, DecisionResponse, Question, UseSite};

use crate::decision::judge::{output_from_answers, render as render_output};
use crate::decision::{
    AnswerSource, DecisionCall, DecisionJournal, DecisionOutcome, DecisionRunner, FieldClass,
    Judgement, Route, StateField, StateParts,
};
pub(super) use crate::judge_eval::inputs::state_field;
use crate::judge_eval::{Answers, JudgeInputs, JudgeOutcome, JudgeRequest};

/// The profile emulation's retries after its first attempt.
const EMULATION_RETRIES: u32 = 1;

fn finished(status: NodeStatus, output: String, events: Vec<EventPayload>) -> AttemptOutcome {
    AttemptOutcome::Finished {
        status,
        output,
        events,
    }
}

/// Executes one judge node: the shared evaluation
/// ([`crate::judge_eval::evaluate`]) over the run's inputs, then, when it
/// hands over to the emulation, the configured `llm_emulation` providers and
/// the node's profile.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute(
    playbook: &Playbook,
    run_dir: &Path,
    workdir: &Path,
    node_id: &str,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    cancel: &AtomicBool,
    env_scrub: &[String],
    journal: &Journal,
    decisions: Option<&DecisionRunner>,
) -> Result<AttemptOutcome, EngineError> {
    let node = playbook
        .node(node_id)
        .ok_or_else(|| EngineError::NotFound(node_id.into()))?;
    let NodeKind::Judge {
        state: fields,
        questions,
        thresholds,
        ..
    } = &node.kind
    else {
        return Err(EngineError::Invalid(format!(
            "node `{node_id}` is not a judge"
        )));
    };
    let inputs = JudgeInputs::from_state(
        playbook,
        run_dir,
        run_id,
        state,
        cfg,
        &playbook.context_budget(node_id),
    )?;
    for (name, template) in fields.iter() {
        crate::scenario::session::note_empty_references(
            run_dir,
            node_id,
            &format!("state.{name}"),
            || inputs.unfilled(template, run_dir),
        );
    }
    // 1. The node's own question, to the decision models proper, and the
    // declared fallback. In a scenario run the answer book stands in for
    // the providers at quick.
    let staged = crate::scenario::decide::stage(
        run_dir,
        crate::scenario::BoundaryKind::JudgeNative,
        node_id,
        None,
    )?;
    let evaluated = crate::judge_eval::evaluate(
        &JudgeRequest {
            playbook,
            node_id,
            inputs: &inputs,
            clip_dir: run_dir,
        },
        Answers::Runner {
            runner: decisions,
            journal,
        },
    )?;
    drop(staged);
    let parts = evaluated.parts;
    let wire = evaluated.wire;
    let reason = match evaluated.evaluation.as_configured {
        JudgeOutcome::Output { output } | JudgeOutcome::Fallback { output, .. } => {
            return Ok(finished(
                NodeStatus::Succeeded,
                render_output(&output),
                Vec::new(),
            ));
        }
        JudgeOutcome::Fail { reason } => {
            return Ok(finished(NodeStatus::Failed, reason, Vec::new()));
        }
        JudgeOutcome::Emulate { reason } => reason,
    };
    // 2. The declared emulation.
    let emulated = |answers: &BTreeMap<String, crate::event::DecisionAnswer>| Judgement {
        applied: output_from_answers(questions, thresholds, answers, "").is_ok(),
        would_change: None,
    };
    let use_answers = |outcome: DecisionOutcome, source: Option<AnswerSource>| match outcome {
        DecisionOutcome::Answered { answers, .. } => {
            let by = format!(
                "emulated:{}",
                source.map_or_else(|| "unknown".to_string(), |s| s.provider)
            );
            output_from_answers(questions, thresholds, &answers, &by).ok()
        }
        _ => None,
    };
    // 2a. The configured emulation endpoints, when the layer is on. A quick
    // scenario run models none: its judge goes on to the profile, a
    // scripted attempt.
    let emulation_endpoints = !matches!(
        crate::scenario::session::at_run(
            run_dir,
            crate::scenario::BoundaryKind::JudgeEmulation,
            &crate::scenario::BoundaryKey {
                node: Some(node_id.to_string()),
                ..Default::default()
            },
        ),
        crate::scenario::Resolution::Skip
    );
    if let Some(runner) = decisions
        && emulation_endpoints
        && runner.mode_for(UseSite::JudgeNode) > DecisionMode::Off
        && runner.has_emulation()
    {
        let (outcome, source) = runner.decide_routed(
            journal,
            crate::judge_eval::call(node_id, &parts, &wire, &emulated),
            Route::Emulation,
        );
        if let Some(out) = use_answers(outcome, source) {
            return Ok(finished(
                NodeStatus::Succeeded,
                render_output(&out),
                Vec::new(),
            ));
        }
    }
    // 2b. The node's profile. A host-mode run spawns no agent CLI, so the
    // profile emulation is unavailable there: the node fails like an
    // emulation that gave no answer (docs/DECISIONS.md).
    if crate::manifest::run_execution_mode(run_dir)? == apb_core::execution::ExecutionMode::Host {
        return Ok(finished(
            NodeStatus::Failed,
            format!(
                "judge node `{node_id}` has no usable answer ({reason}) and cannot emulate through its profile: host execution mode spawns no agent CLI"
            ),
            Vec::new(),
        ));
    }
    let fresh;
    let runner = match decisions {
        Some(r) => r,
        None => {
            fresh = DecisionRunner::for_emulation(
                &cfg_root(run_dir),
                run_dir,
                &read_all(run_dir)?,
                env_scrub,
            );
            &fresh
        }
    };
    let agent = ProfileEmulation {
        playbook,
        run_dir,
        workdir,
        node_id,
        cancel,
        env_scrub,
        overlay: cfg.spawn_env(),
        output_limits: cfg.output_limits(),
        journal,
        attempt: std::cell::Cell::new(attempt_started_count(
            &read_all(run_dir)?,
            crate::exec_key::ExecKey::node(node_id),
        ) as u32),
    };
    let Some((id, model)) = agent.binding()? else {
        return Ok(finished(
            NodeStatus::Failed,
            format!("judge node `{node_id}` cannot emulate: no profile bound in the run manifest"),
            Vec::new(),
        ));
    };
    let ask = |req: &DecisionRequest| agent.ask(req, &id, &model);
    let (outcome, source) = runner.decide_routed(
        journal,
        crate::judge_eval::call(node_id, &parts, &wire, &emulated),
        Route::Custom {
            id: &id,
            model: &model,
            ask: &ask,
        },
    );
    if matches!(&outcome, DecisionOutcome::Failed { error_kind } if error_kind == "cancelled") {
        return Ok(finished(
            NodeStatus::Cancelled,
            "cancelled".to_string(),
            Vec::new(),
        ));
    }
    match use_answers(outcome, source) {
        Some(out) => Ok(finished(
            NodeStatus::Succeeded,
            render_output(&out),
            Vec::new(),
        )),
        None => Ok(finished(
            NodeStatus::Failed,
            format!(
                "judge node `{node_id}` has no usable answer ({reason}) and its emulation gave none either"
            ),
            Vec::new(),
        )),
    }
}

/// What asking a judge's questions came to (a list item's judge check).
pub(super) enum Asked {
    /// Answered with the `judge_node` use at enforce; `by` names who
    /// answered (`<provider>/<model>`).
    Answered {
        answers: BTreeMap<String, crate::event::DecisionAnswer>,
        by: String,
    },
    /// No usable answer, and why: `not_configured`, `off`, `mode` (below
    /// enforce), a skip reason or an error kind.
    Unavailable(String),
}

/// Asks a judge's questions over `parts` through the run's decision runner
/// ([`crate::judge_eval::ask`], the one path a judge node and a list
/// item's judge check share).
pub(super) fn ask(
    decisions: Option<&DecisionRunner>,
    journal: &Journal,
    node_id: &str,
    parts: &StateParts,
    wire: &BTreeMap<String, Question>,
    judge: &dyn Fn(&BTreeMap<String, crate::event::DecisionAnswer>, bool) -> Judgement,
) -> Asked {
    let asked = crate::judge_eval::ask(decisions, journal, node_id, parts, wire, judge);
    match asked.verdict {
        crate::judge_eval::Verdict::Answered => Asked::Answered {
            answers: asked.raw,
            by: asked.by.unwrap_or_else(|| "unknown".to_string()),
        },
        crate::judge_eval::Verdict::Unavailable(reason) => Asked::Unavailable(reason),
    }
}

/// The execution root a run belongs to: `<root>/.apb/runs/<id>` (or
/// `.apb/scenario-runs/<id>`, the same depth).
pub(super) fn cfg_root(run_dir: &Path) -> PathBuf {
    run_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| run_dir.to_path_buf())
}

/// The profile backend of the LLM emulation: one agent attempt on the
/// node's primary executor (at most one retry), fresh session, no handoff,
/// its reply parsed as the emulated answer. Journaled as a normal attempt.
struct ProfileEmulation<'a, 'j> {
    playbook: &'a Playbook,
    run_dir: &'a Path,
    workdir: &'a Path,
    node_id: &'a str,
    cancel: &'a AtomicBool,
    env_scrub: &'a [String],
    /// The eval env overlay (`RunConfig::spawn_env`).
    overlay: Vec<(String, String)>,
    /// The run's output limits (`RunConfig::output_limits`).
    output_limits: apb_core::output_limits::OutputLimits,
    journal: &'a Journal<'j>,
    attempt: std::cell::Cell<u32>,
}

impl ProfileEmulation<'_, '_> {
    /// `(provider id, model)` the answers are journaled under:
    /// `profile:<name>` and the primary executor's model.
    fn binding(&self) -> Result<Option<(String, String)>, EngineError> {
        let Some(manifest) = crate::manifest::read(self.run_dir)? else {
            return Ok(None);
        };
        let Some(entry) = effective_for_node(self.run_dir, &manifest, self.node_id)? else {
            return Ok(None);
        };
        Ok(entry
            .chain
            .first()
            .map(|ri| (format!("profile:{}", entry.name), ri.model.clone())))
    }

    fn ask(
        &self,
        req: &DecisionRequest,
        id: &str,
        model: &str,
    ) -> Result<DecisionResponse, DecideError> {
        let started = std::time::Instant::now();
        let prompt = apb_decide::llm_emulation::EmulationPrompt::new(req, true).single_message();
        let mut last = DecideError::Unavailable("emulation agent gave no answer".into());
        for _ in 0..=EMULATION_RETRIES {
            if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(DecideError::Cancelled);
            }
            match self.run_once(&prompt) {
                Ok(Some(text)) => match apb_decide::llm_emulation::parse_reply(req, &text) {
                    Ok((answers, ignored_items)) => {
                        return Ok(DecisionResponse {
                            provider: id.to_string(),
                            model: model.to_string(),
                            calibrated: false,
                            answers,
                            usage: Default::default(),
                            latency_ms: started.elapsed().as_millis() as u64,
                            cached: false,
                            ignored_items,
                        });
                    }
                    Err(e) => last = e,
                },
                Ok(None) => {}
                Err(_) => {
                    return Err(DecideError::Unavailable(
                        "emulation agent could not run".into(),
                    ));
                }
            }
        }
        Err(last)
    }

    /// One agent attempt; `Ok(Some(reply))` when it succeeded.
    fn run_once(&self, prompt: &str) -> Result<Option<String>, EngineError> {
        let manifest = crate::manifest::read(self.run_dir)?
            .ok_or_else(|| EngineError::Invalid("no execution manifest".into()))?;
        let entry = effective_for_node(self.run_dir, &manifest, self.node_id)?
            .ok_or_else(|| EngineError::Invalid("no profile bound".into()))?;
        let ri = entry
            .chain
            .first()
            .ok_or_else(|| EngineError::Invalid("empty executor chain".into()))?;
        let attempt = self.attempt.get() + 1;
        self.attempt.set(attempt);
        let adapter: Box<dyn crate::adapter::AgentAdapter> =
            match crate::scenario::session::is_scenario(self.run_dir) {
                // A scenario run's emulation is a scripted attempt.
                true => Box::new(crate::scenario::adapter::ScenarioAdapter {
                    run_dir: self.run_dir,
                    node: self.node_id,
                    item: None,
                }),
                false => Box::new(crate::adapter::ClaudeAdapter {
                    program: ri.canonical_executable.to_string_lossy().into_owned(),
                    spec: ri.spec.clone(),
                }),
            };
        let spawn_env = crate::adapter::SpawnEnv::for_node(
            self.env_scrub.to_vec(),
            self.run_dir,
            self.node_id,
            self.overlay.clone(),
        )
        .with_attempt(attempt)
        .with_process(crate::agent_env::plan_for_entry(
            entry.process_env.as_ref(),
            &ri.agent_id,
            &ri.model,
            self.env_scrub,
        ));
        let stream_log = self
            .run_dir
            .join("agent-stream")
            .join(format!("{}-{attempt}.jsonl", self.node_id));
        let task = AgentTask {
            prompt,
            untrusted_section: None,
            model: &ri.model,
            workdir: self.workdir,
            timeout: self
                .playbook
                .defaults
                .timeout_seconds
                .map(Duration::from_secs),
            stream_log: Some(&stream_log),
            soul: Some(entry.soul.as_str()),
            // An emulated decision reads and answers; it is granted nothing.
            grant_autonomy: false,
            spawn_env: &spawn_env,
            interactive: false,
            // The reply IS the JSON answer: no status-verdict protocol.
            report_contract: false,
            node: self.node_id,
            agent: &ri.agent_id,
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: self.output_limits,
        };
        // Billed use (`pause::billing`): work the run holds no confirmation
        // for is not started; the attempt fails without spending.
        if crate::pause::billing::spawn_hold(
            self.run_dir,
            &ri.agent_id,
            crate::adapter::read_cost_basis(&task).basis,
        )
        .is_some()
        {
            return Ok(None);
        }
        let spawn_at: std::cell::Cell<Option<std::time::Instant>> = std::cell::Cell::new(None);
        let spawn_err: std::cell::RefCell<Option<EngineError>> = std::cell::RefCell::new(None);
        let started = |pid: Option<u32>, spawn_ms: Option<u64>| EventPayload::AttemptStarted {
            item: None,
            node: self.node_id.to_string(),
            attempt,
            agent: ri.agent_id.clone(),
            soul_delivery: Some(soul_delivery_str(ri.soul_delivery)),
            skills_mode: None,
            pid,
            spawn_ms,
            model: None,
            workdir: None,
            transcript: None,
            token: spawn_env.attempt_token.clone(),
            isolation: None,
            // A judge's ask is no step of an executor chain.
            exec: None,
        };
        let on_spawn = |pid: u32, spawn_ms: u64| {
            spawn_at.set(Some(std::time::Instant::now()));
            if let Err(e) = self.journal.append(started(Some(pid), Some(spawn_ms))) {
                *spawn_err.borrow_mut() = Some(e);
            }
        };
        if let Some(plan) = &spawn_env.process {
            self.journal.append(crate::agent_env::applied_event(
                plan,
                self.node_id,
                attempt,
                &ri.agent_id,
            ))?;
        }
        let outcome =
            adapter.run_cancellable(&task, self.cancel, Some(&on_spawn), None, None, None);
        if let Some(e) = spawn_err.borrow_mut().take() {
            return Err(e);
        }
        if spawn_at.get().is_none() {
            self.journal.append(started(None, None))?;
        }
        let duration_ms = spawn_at.get().map(|t| t.elapsed().as_millis() as u64);
        let (status, output, session, usage, capture) = match outcome {
            Ok(report) => (
                report.status,
                Some(report.output),
                report.session,
                report.usage,
                report.capture,
            ),
            Err(crate::adapter::AgentFailure {
                class,
                usage,
                capture,
                ..
            }) => (
                if class == ErrorClass::Timeout {
                    NodeStatus::TimedOut
                } else {
                    NodeStatus::Failed
                },
                None,
                None,
                usage.map(|u| *u),
                capture,
            ),
        };
        self.journal.append(EventPayload::AttemptFinished {
            item: None,
            output_truncated: capture.truncated,
            transcript_incomplete: capture.transcript_incomplete,
            node: self.node_id.into(),
            attempt,
            status: status.as_str().into(),
            duration_ms,
            session,
            summary: None,
            rejected_output: None,
            partial_output: None,
            failure_kind: None,
            usage,
            rejected_for: None,
            rejection_fingerprint: None,
            stalled: false,
        })?;
        Ok(output.filter(|_| status == NodeStatus::Succeeded))
    }
}

// --- judge edges (issue #165 Part 7) ----------------------------------------

/// Asks the judge edges of `node_id` after it succeeded: one `noul` per
/// judge edge (`edge_<index>`, the edge's position among the node's outgoing
/// edges), over the node's output and its title as `step`, in one request.
/// Journaled before routing, under the node's execution count as `attempt`,
/// so loop executions stay separate. Idempotent: an execution already
/// decided is not asked again. Returns whether a decision was journaled.
pub(crate) fn decide_edges(
    playbook: &Playbook,
    run_dir: &Path,
    node_id: &str,
    status: NodeStatus,
    decisions: Option<&DecisionRunner>,
    journal: &dyn DecisionJournal,
) -> Result<bool, EngineError> {
    use apb_core::schema::EdgeCondition;
    let edges: Vec<(usize, &str, f64, bool)> = playbook
        .edges
        .iter()
        .filter(|e| e.from == node_id)
        .enumerate()
        .filter_map(|(i, e)| match &e.condition {
            Some(EdgeCondition::Judge {
                question,
                min_p,
                on_unavailable,
            }) => Some((
                i,
                question.as_str(),
                min_p.0,
                on_unavailable.unwrap_or(false),
            )),
            _ => None,
        })
        .collect();
    let Some(runner) = decisions else {
        return Ok(false);
    };
    let mode = runner.mode_for(UseSite::JudgeEdge);
    if edges.is_empty() || status != NodeStatus::Succeeded || mode == DecisionMode::Off {
        return Ok(false);
    }
    let events = read_all(run_dir)?;
    let execution = node_finished_count(&events, node_id) as u32;
    let decided = events.iter().any(|e| {
        matches!(&e.payload, EventPayload::DecisionMade { use_site, node: Some(n), attempt: Some(a), .. }
            if use_site == "judge_edge" && n == node_id && *a == execution)
    });
    if execution == 0 || decided {
        return Ok(false);
    }
    let output = RunState::fold(&events)
        .outputs
        .get(node_id)
        .cloned()
        .unwrap_or_default();
    let step = playbook
        .node(node_id)
        .and_then(|n| n.title.clone())
        .unwrap_or_else(|| node_id.to_string());
    let questions: BTreeMap<String, Question> = edges
        .iter()
        .map(|(i, q, _, _)| {
            (
                format!("edge_{i}"),
                Question::Noul {
                    instructions: serde_json::json!(q),
                    criteria: None,
                },
            )
        })
        .collect();
    let enforce = mode == DecisionMode::Enforce;
    let judge = |answers: &BTreeMap<String, crate::event::DecisionAnswer>| Judgement {
        applied: enforce,
        would_change: (!enforce).then(|| {
            edges.iter().any(|(i, _, min_p, fallback)| {
                answers
                    .get(&format!("edge_{i}"))
                    .and_then(|a| a.p)
                    .is_some_and(|p| (p >= *min_p) != *fallback)
            })
        }),
    };
    // A quick scenario run answers from the scenario's edge answers.
    let _staged = crate::scenario::decide::stage(
        run_dir,
        crate::scenario::BoundaryKind::JudgeEdges,
        node_id,
        None,
    )?;
    let addresses = apb_core::edge_address::edge_addresses(playbook);
    let edge_addresses: BTreeMap<String, String> = addresses
        .iter()
        .filter(|a| playbook.edges[a.index].from == node_id)
        .filter(|a| edges.iter().any(|(rank, ..)| *rank == a.rank))
        .map(|a| (format!("edge_{}", a.rank), a.rendered.clone()))
        .collect();
    let outcome = runner.decide_routed(
        journal,
        DecisionCall {
            enforce: None,
            join: BTreeMap::new(),
            join_from: None,
            edge_addresses,
            site: UseSite::JudgeEdge,
            node: Some(node_id),
            attempt: Some(execution),
            state: StateParts {
                fields: vec![
                    StateField {
                        name: "step",
                        class: FieldClass::Prompt,
                        text: step,
                        head: 2 * 1024,
                        tail: 0,
                    },
                    StateField {
                        name: "output",
                        class: FieldClass::Output,
                        text: output,
                        head: 2 * 1024,
                        tail: 16 * 1024,
                    },
                ],
                meta: Default::default(),
            },
            questions,
            baseline: None,
            judge: &judge,
        },
        Route::Native,
    );
    Ok(!matches!(
        outcome.0,
        DecisionOutcome::Skipped { reason: "off" }
    ))
}

// --- the judge patch refusal -----------------------------------------------

/// The reason a patch is refused because it would move an answer a judge
/// edge already has, or `None`.
///
/// Routing reads a judge edge's answer by the edge's rank among its source's
/// outgoing edges. A patch that adds, removes, reorders or changes any
/// outgoing edge of a node whose latest execution has judge-edge answers
/// (applied or shadow) that routing has not used yet could make a stored
/// rank point at another edge. Routing has used them once the drive moved
/// past that execution's routing: a declared-edge hop it routed, or a later
/// `node_started` or `run_finished` (a dead end writes no hop). It is refused unless
/// `continue_from` runs that node again, which asks again. An older apb
/// applying a patch keeps its own behaviour.
pub(crate) fn patch_refusal(
    running: &Playbook,
    patched: &Playbook,
    events: &[Event],
    continue_from: &str,
) -> Option<String> {
    let outgoing = |pb: &Playbook, node: &str| -> Vec<serde_json::Value> {
        pb.edges
            .iter()
            .filter(|e| e.from == node)
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect()
    };
    let index = crate::identity::index(events);
    let mut pending: Vec<&str> = running
        .nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|node| *node != continue_from)
        .filter(|node| outgoing(running, node) != outgoing(patched, node))
        .filter(|node| {
            let Some(latest) = index.executions(node).iter().rev().find(|x| !x.is_open()) else {
                return false;
            };
            let Some(decided) = events
                .iter()
                .filter(|e| {
                    matches!(&e.payload, EventPayload::DecisionMade { use_site, node: Some(n), attempt: Some(a), .. }
                        if use_site == "judge_edge" && n == node && *a == latest.ordinal)
                })
                .map(|e| e.seq)
                .max()
            else {
                return false;
            };
            // Routing ran for that execution once the drive moved past it:
            // a hop over a declared edge it routed (a policy hop reads no
            // answer), or any later start of a node or end of the run, which
            // the drive writes only after routing what finished (a dead end
            // included). A pause or a crash in between leaves them pending.
            let consumed = events.iter().filter(|e| e.seq > decided).any(|e| match &e.payload {
                EventPayload::EdgeTraversed {
                    from, via_policy, ..
                } if from == node && !via_policy => index
                    .routed_by(node, e.seq)
                    .is_some_and(|x| x.ordinal == latest.ordinal),
                EventPayload::NodeStarted { .. } | EventPayload::RunFinished { .. } => true,
                _ => false,
            });
            !consumed
        })
        .collect();
    pending.sort_unstable();
    pending.dedup();
    if pending.is_empty() {
        return None;
    }
    Some(format!(
        "judge_edge_answers_pending: the patch changes the outgoing edges of [{}], whose judge-edge answers routing has not used yet; those answers are kept by edge position, so apply the patch with continue_from on that node to ask again",
        pending.join(", ")
    ))
}

#[cfg(test)]
mod patch_refusal_tests {
    use super::*;

    const RUNNING: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: review, type: script, script: s, runner: sh }\n  - { id: fix, type: finish, outcome: success }\n  - { id: ship, type: finish, outcome: success }\nedges:\n  - { from: start, to: review }\n  - { from: review, to: fix, condition: { type: judge, question: q, min_p: 0.7, on_unavailable: false } }\n  - { from: review, to: ship, fallback: true }\n";

    fn pb(yaml: &str) -> Playbook {
        Playbook::from_yaml(yaml).unwrap()
    }

    /// The running playbook with an edge inserted before the judge edge.
    fn inserted() -> Playbook {
        pb(&RUNNING.replace(
            "  - { from: review, to: fix, condition",
            "  - { from: review, to: ship, condition: { type: node_status, node: review, equals: failure } }\n  - { from: review, to: fix, condition",
        ))
    }

    fn ev(seq: u64, body: serde_json::Value) -> Event {
        let mut v = body;
        v["seq"] = seq.into();
        v["ts"] = 0.into();
        serde_json::from_value(v).unwrap()
    }

    fn answered(applied: bool) -> Vec<Event> {
        vec![
            ev(
                1,
                serde_json::json!({"type": "node_started", "node": "review", "attempt": 1}),
            ),
            ev(
                2,
                serde_json::json!({"type": "node_finished", "node": "review", "status": "succeeded", "attempt": 1, "output": "", "artifacts": []}),
            ),
            ev(
                3,
                serde_json::json!({"type": "decision_made", "use_site": "judge_edge", "node": "review", "attempt": 1, "applied": applied, "answers": {"edge_0": {"value": "true", "p": 0.9}}}),
            ),
        ]
    }

    #[test]
    fn an_unconsumed_answer_refuses_an_edge_change_applied_or_shadow() {
        for applied in [true, false] {
            let events = answered(applied);
            let reason = patch_refusal(&pb(RUNNING), &inserted(), &events, "start").unwrap();
            assert!(reason.contains("judge_edge_answers_pending"), "{reason}");
            assert!(reason.contains("[review]"), "{reason}");
        }
    }

    #[test]
    fn continue_from_the_source_asks_again_and_lifts_the_refusal() {
        let events = answered(true);
        assert!(patch_refusal(&pb(RUNNING), &inserted(), &events, "review").is_none());
    }

    #[test]
    fn a_consumed_answer_or_an_unchanged_edge_list_is_no_refusal() {
        let mut events = answered(true);
        assert!(patch_refusal(&pb(RUNNING), &pb(RUNNING), &events, "start").is_none());
        events.push(ev(
            4,
            serde_json::json!({"type": "edge_traversed", "from": "review", "to": "fix", "uncounted": true}),
        ));
        assert!(patch_refusal(&pb(RUNNING), &inserted(), &events, "start").is_none());
    }

    #[test]
    fn a_dead_end_or_a_later_start_consumes_and_a_policy_hop_or_a_pause_does_not() {
        let with = |body: serde_json::Value| {
            let mut events = answered(true);
            events.push(ev(4, body));
            patch_refusal(&pb(RUNNING), &inserted(), &events, "start")
        };
        // A dead end ended the run: routing ran, nothing to move.
        assert!(with(serde_json::json!({"type": "run_finished", "outcome": "failed"})).is_none());
        // The drive went on to another node after routing.
        assert!(
            with(serde_json::json!({"type": "node_started", "node": "ship", "attempt": 1}))
                .is_none()
        );
        // A policy hop reads no judge answer.
        assert!(
            with(serde_json::json!({"type": "edge_traversed", "from": "review", "to": "ship", "via_policy": true, "uncounted": true}))
                .is_some()
        );
        // A pause between the answer and routing leaves it pending.
        assert!(with(serde_json::json!({"type": "run_paused", "reason": "operator"})).is_some());
    }

    #[test]
    fn an_answer_of_an_earlier_execution_is_not_pending() {
        let mut events = answered(true);
        events.push(ev(
            4,
            serde_json::json!({"type": "node_started", "node": "review", "attempt": 1}),
        ));
        events.push(ev(
            5,
            serde_json::json!({"type": "node_finished", "node": "review", "status": "failed", "attempt": 1, "output": "", "artifacts": []}),
        ));
        assert!(patch_refusal(&pb(RUNNING), &inserted(), &events, "start").is_none());
    }
}
