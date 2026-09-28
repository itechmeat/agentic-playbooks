//! The judge node and the judge edges in a run (issue #165 Parts 5-7).
//!
//! A judge node renders its state, asks its questions through the run's
//! decision runner (journal first, replay on resume) and, only when the
//! `judge_node` use is at `enforce` and every answer is valid, publishes the
//! thresholded answers as compact JSON. Otherwise it applies its
//! `on_unavailable`: `route` and `default` succeed with an output naming the
//! reason, `fail` fails like any node, and `emulate` asks the configured
//! `llm_emulation` providers and then the node's profile (an APB agent run
//! once on the primary executor, at most one retry), failing the node when
//! that fails too.
//!
//! Judge edges are asked once when their source node succeeds, all edges of
//! that node in one request, journaled before routing; edge selection then
//! reads the answers back from the folded journal.
//! Shares the parent module's imports via `use super::*`.

use super::*;

use apb_core::decisions::DecisionMode;
use apb_core::judge::JudgeFallback;
use apb_decide::{DecideError, DecisionRequest, DecisionResponse, Question, UseSite};

use crate::decision::judge::{
    differs_from_fallback, fallback_output, node_questions, output_from_answers,
    render as render_output,
};
use crate::decision::{
    AnswerSource, DecisionCall, DecisionJournal, DecisionOutcome, DecisionRunner, FieldClass,
    Judgement, Route, StateField, StateParts, intern,
};

/// Each state field's own clip, head and tail kept, before the share of
/// `privacy.max_state_bytes` it may take.
const FIELD_HEAD: usize = 12 * 1024;
const FIELD_TAIL: usize = 12 * 1024;
/// The profile emulation's retries after its first attempt.
const EMULATION_RETRIES: u32 = 1;

fn finished(status: NodeStatus, output: String, events: Vec<EventPayload>) -> AttemptOutcome {
    AttemptOutcome::Finished {
        status,
        output,
        events,
    }
}

/// Executes one judge node.
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
        on_unavailable,
        ..
    } = &node.kind
    else {
        return Err(EngineError::Invalid(format!(
            "node `{node_id}` is not a judge"
        )));
    };
    let fallback = on_unavailable.as_ref();
    let parts = render_state(playbook, run_dir, run_id, state, cfg, node_id, fields)?;
    let wire = node_questions(questions);
    let enforce =
        decisions.is_some_and(|r| r.mode_for(UseSite::JudgeNode) == DecisionMode::Enforce);

    // 1. The node's own question, to the decision models proper.
    let reason: String = match decisions {
        None => "not_configured".into(),
        Some(runner) if runner.mode_for(UseSite::JudgeNode) == DecisionMode::Off => "off".into(),
        Some(runner) => {
            let judge = |answers: &BTreeMap<String, crate::event::DecisionAnswer>| {
                let derived = output_from_answers(questions, thresholds, answers, "").ok();
                Judgement {
                    applied: enforce && derived.is_some(),
                    would_change: match (&derived, enforce) {
                        (_, true) => None,
                        (Some(d), false) => {
                            differs_from_fallback(d, fallback, questions, thresholds)
                        }
                        (None, false) => None,
                    },
                }
            };
            let (outcome, source) =
                runner.decide_routed(journal, call(node_id, &parts, &wire, &judge), Route::Native);
            match outcome {
                DecisionOutcome::Answered { answers, .. } if enforce => {
                    let by = source
                        .as_ref()
                        .map_or_else(|| "unknown".to_string(), decided_by);
                    match output_from_answers(questions, thresholds, &answers, &by) {
                        Ok(out) => {
                            return Ok(finished(
                                NodeStatus::Succeeded,
                                render_output(&out),
                                Vec::new(),
                            ));
                        }
                        Err(_) => "invalid".into(),
                    }
                }
                DecisionOutcome::Answered { .. } => "mode".into(),
                DecisionOutcome::Skipped { reason } => reason.into(),
                DecisionOutcome::Failed { error_kind } => error_kind,
            }
        }
    };

    // 2. The declared fallback.
    if let Some(out) = fallback_output(fallback, questions, thresholds, &reason) {
        return Ok(finished(
            NodeStatus::Succeeded,
            render_output(&out),
            Vec::new(),
        ));
    }
    if !matches!(fallback, Some(JudgeFallback::Emulate)) {
        return Ok(finished(
            NodeStatus::Failed,
            format!(
                "judge node `{node_id}` has no usable answer ({reason}) and on_unavailable is fail"
            ),
            Vec::new(),
        ));
    }
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
    // 2a. The configured emulation endpoints, when the layer is on.
    if let Some(runner) = decisions
        && runner.mode_for(UseSite::JudgeNode) > DecisionMode::Off
        && runner.has_emulation()
    {
        let (outcome, source) = runner.decide_routed(
            journal,
            call(node_id, &parts, &wire, &emulated),
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
    // 2b. The node's profile.
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
        journal,
        attempt: std::cell::Cell::new(attempt_started_count(&read_all(run_dir)?, node_id) as u32),
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
        call(node_id, &parts, &wire, &emulated),
        Route::Custom {
            id: &id,
            model: &model,
            ask: &ask,
        },
    );
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

/// The execution root a run belongs to: `<root>/.apb/runs/<id>`.
pub(super) fn cfg_root(run_dir: &Path) -> PathBuf {
    run_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| run_dir.to_path_buf())
}

fn decided_by(s: &AnswerSource) -> String {
    format!("{}/{}", s.provider, s.model)
}

fn call<'a>(
    node_id: &'a str,
    parts: &StateParts,
    wire: &BTreeMap<String, Question>,
    judge: &'a dyn Fn(&BTreeMap<String, crate::event::DecisionAnswer>) -> Judgement,
) -> DecisionCall<'a> {
    DecisionCall {
        enforce: None,
        join: BTreeMap::new(),
        join_from: None,
        site: UseSite::JudgeNode,
        node: Some(node_id),
        attempt: None,
        state: parts.clone(),
        questions: wire.clone(),
        baseline: None,
        judge,
    }
}

/// The rendered state: each field a template rendered like a prompt; a
/// field reading a node's output is `output` material, any other `prompt`.
fn render_state(
    playbook: &Playbook,
    run_dir: &Path,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    node_id: &str,
    fields: &apb_core::judge::OrderedMap<String>,
) -> Result<StateParts, EngineError> {
    let mut parts = StateParts::default();
    for (name, template) in fields.iter() {
        let text = render_node_prompt(
            run_dir,
            run_id,
            state,
            cfg,
            template,
            &playbook.context_budget(node_id),
        )?;
        let reads_output = apb_core::template::refs(template)
            .iter()
            .any(|r| r.starts_with("nodes.") || r == "run.context");
        parts.fields.push(StateField {
            name: intern(name),
            class: if reads_output {
                FieldClass::Output
            } else {
                FieldClass::Prompt
            },
            text,
            head: FIELD_HEAD,
            tail: FIELD_TAIL,
        });
    }
    Ok(parts)
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
        let adapter = crate::adapter::ClaudeAdapter {
            program: ri.canonical_executable.to_string_lossy().into_owned(),
            spec: ri.spec.clone(),
        };
        let connector_policy = crate::adapter::ConnectorEnvPolicy {
            scrub: self.env_scrub.to_vec(),
            run_dir: Some(self.run_dir.to_path_buf()),
            node_id: Some(self.node_id.to_string()),
        };
        let stream_log = self
            .run_dir
            .join("agent-stream")
            .join(format!("{}-{attempt}.jsonl", self.node_id));
        let task = AgentTask {
            prompt,
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
            connector_policy: &connector_policy,
            interactive: false,
            // The reply IS the JSON answer: no status-verdict protocol.
            report_contract: false,
            node: self.node_id,
            agent: &ri.agent_id,
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
        };
        let spawn_at: std::cell::Cell<Option<std::time::Instant>> = std::cell::Cell::new(None);
        let spawn_err: std::cell::RefCell<Option<EngineError>> = std::cell::RefCell::new(None);
        let started = |pid: Option<u32>, spawn_ms: Option<u64>| EventPayload::AttemptStarted {
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
        };
        let on_spawn = |pid: u32, spawn_ms: u64| {
            spawn_at.set(Some(std::time::Instant::now()));
            if let Err(e) = self.journal.append(started(Some(pid), Some(spawn_ms))) {
                *spawn_err.borrow_mut() = Some(e);
            }
        };
        let outcome =
            adapter.run_cancellable(&task, self.cancel, Some(&on_spawn), None, None, None);
        if let Some(e) = spawn_err.borrow_mut().take() {
            return Err(e);
        }
        if spawn_at.get().is_none() {
            self.journal.append(started(None, None))?;
        }
        let duration_ms = spawn_at.get().map(|t| t.elapsed().as_millis() as u64);
        let (status, output, session, usage) = match outcome {
            Ok(report) => (
                report.status,
                Some(report.output),
                report.session,
                report.usage,
            ),
            Err(crate::adapter::AgentFailure { class, usage, .. }) => (
                if class == ErrorClass::Timeout {
                    NodeStatus::TimedOut
                } else {
                    NodeStatus::Failed
                },
                None,
                None,
                usage,
            ),
        };
        self.journal.append(EventPayload::AttemptFinished {
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
    let outcome = runner.decide_routed(
        journal,
        DecisionCall {
            enforce: None,
            join: BTreeMap::new(),
            join_from: None,
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
