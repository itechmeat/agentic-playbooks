//! Host execution mode on the drive side (0.23.0): the [`HostAdapter`] that
//! turns an agent attempt into a host task and waits for the host's reply,
//! and the execution a sub-playbook inherits. The channel files and the
//! views live in `crate::host_task`.
//! Shares the parent module's imports via `use super::*`.

use super::*;

use crate::adapter::{AgentFailure, AgentReport, ControlHooks, LiveHooks, StallHooks};
use crate::host_task::{self, SubmitStatus, TaskRecord};

/// How often a parked host attempt looks for the host's reply.
const HOST_POLL: Duration = Duration::from_millis(100);

/// The agent id journaled for a host-task attempt (`attempt_started.agent`,
/// fallback events): the host executes it, not the profile's CLI.
pub(crate) const HOST_AGENT: &str = "host";

/// `session_handoff.reason` of a `continue_session` node in host mode.
pub(crate) const HOST_MODE_REASON: &str = "host_mode";

/// The paragraph an interactive node's host task carries instead of the
/// `ask_user` tool or the stdout marker contract.
pub(crate) const HOST_QUESTION_PARAGRAPH: &str = "If you need input from the user before you can proceed, stop without doing further work and reply with only the question for the user. Do not guess the answer.";

/// The execution request a sub-playbook child of the run in `run_dir` starts
/// with: its parent's mode, fallback and host, marked inherited.
pub(crate) fn child_execution_request(
    run_dir: &Path,
) -> Result<apb_core::execution::ExecutionRequest, EngineError> {
    let Some(manifest) = crate::manifest::read(run_dir)? else {
        return Ok(apb_core::execution::ExecutionRequest::default());
    };
    let mode = manifest.execution_mode()?;
    Ok(apb_core::execution::ExecutionRequest {
        mode: Some(mode),
        host_session: mode == apb_core::execution::ExecutionMode::Host
            || manifest.falls_back_to_host(),
        client: manifest.host_client().map(str::to_string),
        inherited: true,
    })
}

/// The role of every step of `entry`'s executor chain, in chain order: the
/// routed tiers in front of the profile's executor (see
/// `decision::routing::routed_entry`), the profile's own executor, then its
/// fallback entries.
pub(crate) fn chain_sources(
    entry: &crate::manifest::ManifestProfile,
) -> Vec<host_task::HintSource> {
    use host_task::HintSource;
    let profile = entry.name.clone();
    let mut tiers = crate::decision::routing::routed_tier_names(entry);
    if tiers.len() >= entry.chain.len() {
        tiers.clear();
    }
    let lead = tiers.len();
    let of = entry.chain.len().saturating_sub(lead + 1) as u32;
    (0..entry.chain.len())
        .map(|i| match i.cmp(&lead) {
            std::cmp::Ordering::Less => HintSource::Tier {
                tier: tiers[i].clone(),
                profile: profile.clone(),
            },
            std::cmp::Ordering::Equal => HintSource::Primary {
                profile: profile.clone(),
            },
            std::cmp::Ordering::Greater => HintSource::Fallback {
                index: (i - lead) as u32,
                of,
                profile: profile.clone(),
            },
        })
        .collect()
}

/// An agent attempt executed by the host session instead of a CLI.
///
/// `run_cancellable` writes the task (`crate::host_task`), journals
/// `host_task_requested`, and then waits on the drive thread (or the parallel
/// batch's worker thread) for the host's submission, observing cancellation,
/// supervisor interrupts, the stall watch and the task deadline the same way
/// a CLI attempt's poll loop does. The reply comes back as an ordinary
/// [`AgentReport`], so everything after it (report block, status file,
/// `success_check`, the completion check, retries and fallbacks) is the CLI
/// path unchanged.
pub(crate) struct HostAdapter<'a, 'j> {
    pub run_dir: &'a Path,
    pub journal: &'a Journal<'j>,
    /// This attempt's number within the node execution.
    pub attempt: u32,
    /// Skill paths handed to the host.
    pub skills: Vec<String>,
    /// The node's `outputs` contract.
    pub outputs: Option<serde_json::Value>,
    /// The model a fallback entry or tier routing declares for this step.
    pub model_hint: Option<String>,
    /// The chain step this attempt runs: the label of `model_hint`.
    pub hint_source: Option<host_task::HintSource>,
    /// For the first attempt of a later chain step: what closed the step
    /// before it.
    pub fallback_of: Option<host_task::FallbackOf>,
    /// Where the attempt records how its task closed (`succeeded`,
    /// `failed`, `expired`, `interrupted`, `cancelled`,
    /// `question_timeout`), for the next chain step's `fallback_of`.
    pub closed: Option<&'a std::cell::Cell<Option<&'static str>>>,
    /// The node's `question_timeout_seconds` and `default_answer`, enforced
    /// while a `blocked` task waits for the person.
    pub question_timeout: Option<u64>,
    pub default_answer: Option<String>,
}

fn failure(e: EngineError) -> AgentFailure {
    AgentFailure::new(ErrorClass::Transport, format!("host task: {e}"))
}

/// The environment a CLI attempt of this task would have had, which the host
/// sets for its subagent.
fn task_env(task: &AgentTask) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let Some(run_dir) = &task.connector_policy.run_dir {
        env.insert(
            "APB_RUN_DIR".to_string(),
            run_dir.to_string_lossy().into_owned(),
        );
        // The run id (0.23.0 provenance), as a CLI attempt gets it: the run
        // directory's name, for an `Apb-Run:` commit trailer.
        if let Some(id) = run_dir.file_name() {
            env.insert("APB_RUN_ID".to_string(), id.to_string_lossy().into_owned());
        }
    }
    if let Some(node) = &task.connector_policy.node_id {
        env.insert("APB_NODE_ID".to_string(), node.clone());
    }
    if let Some(sf) = &task.status_file {
        env.insert(
            "APB_STATUS_FILE".to_string(),
            sf.to_string_lossy().into_owned(),
        );
    }
    env
}

/// The prompt a CLI attempt would receive, plus the values of the variables
/// it names: a host subagent may not see the environment the host was asked
/// to set, and a path in the prompt costs nothing.
fn task_prompt(task: &AgentTask, env: &BTreeMap<String, String>) -> String {
    let mut prompt = crate::adapter::transport_prompt(task);
    let named: Vec<String> = env
        .iter()
        .filter(|(k, _)| prompt.contains(k.as_str()))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if !named.is_empty() {
        prompt.push_str(&format!(
            "\n\nEnvironment of this step: {}",
            named.join(", ")
        ));
    }
    prompt
}

/// A task [`HostAdapter::request`] opened or adopted.
struct Requested {
    task_id: String,
    deadline: Option<std::time::Instant>,
    /// The status file an adopted task named, when it differs from this
    /// attempt's (the attempt counter restarts on resume).
    prior_status_file: Option<String>,
}

impl HostAdapter<'_, '_> {
    /// Records how this attempt's task closed.
    fn mark(&self, status: &'static str) {
        if let Some(c) = self.closed {
            c.set(Some(status));
        }
    }

    /// Opens a task for `prompt`, writes its files and journals the request.
    /// With `adopt` (an attempt's first request), a task of the node a dead
    /// driver left open is re-exposed under its own id instead, and any other
    /// open task of the node is closed as `superseded`. Returns the task id,
    /// its deadline and, for an adopted task, the status file its subagent
    /// was told to write.
    fn request(
        &self,
        task: &AgentTask,
        prompt: &str,
        env: &BTreeMap<String, String>,
        adopt: bool,
    ) -> Result<Requested, EngineError> {
        let adoption = if adopt {
            host_task::adoptable(self.run_dir, &read_all(self.run_dir)?, task.node)
        } else {
            host_task::Adoption::default()
        };
        for old in &adoption.superseded {
            self.close(
                old,
                "superseded",
                "a resumed drive re-exposed another task of this node".into(),
            )?;
        }
        let (task_id, prior_status_file) = match adoption.adopted {
            Some(r) => (r.task_id, r.env.get("APB_STATUS_FILE").cloned()),
            None => (host_task::allocate(self.run_dir, task.node)?, None),
        };
        // A submission already there keeps the task as the host saw it: its
        // files are not rewritten under the reply.
        if host_task::read_submission(self.run_dir, &task_id).is_some() {
            return Ok(Requested {
                task_id,
                deadline: task.timeout.map(|t| std::time::Instant::now() + t),
                prior_status_file,
            });
        }
        let now_ms = apb_core::clock::now_ms() as u64;
        let deadline_ms = task
            .timeout
            .map(|t| now_ms.saturating_add(t.as_millis() as u64));
        let record = TaskRecord {
            task_id: task_id.clone(),
            node: task.node.to_string(),
            attempt: self.attempt,
            workdir: task.workdir.to_string_lossy().into_owned(),
            skills: self.skills.clone(),
            outputs: self.outputs.clone(),
            deadline_ms,
            model_hint: self.model_hint.clone(),
            hint_source: self.hint_source.clone(),
            fallback_of: self.fallback_of.clone(),
            env: env.clone(),
            requested_ts: now_ms,
        };
        host_task::write_task(self.run_dir, &record, prompt, task.soul)?;
        self.journal.append(EventPayload::HostTaskRequested {
            task_id: task_id.clone(),
            node: task.node.to_string(),
            attempt: self.attempt,
            prompt_ref: host_task::prompt_ref(&task_id),
            role_prompt_ref: task
                .soul
                .filter(|s| !s.trim().is_empty())
                .map(|_| host_task::role_ref(&task_id)),
            skills: self.skills.clone(),
            workdir: record.workdir.clone(),
            outputs: self.outputs.clone(),
            deadline_ms,
            model_hint: self.model_hint.clone(),
            hint_source: self.hint_source.clone(),
            fallback_of: self.fallback_of.clone(),
            hint_note: host_task::describe_hint(
                self.model_hint.as_deref(),
                self.hint_source.as_ref(),
                self.fallback_of.as_ref(),
            ),
        })?;
        Ok(Requested {
            task_id,
            deadline: task.timeout.map(|t| std::time::Instant::now() + t),
            prior_status_file,
        })
    }

    /// Closes a task the host never submitted (`expired`, `cancelled`,
    /// `interrupted`), so no view keeps offering it.
    fn close(&self, task_id: &str, status: &str, note: String) -> Result<(), EngineError> {
        self.journal.append(EventPayload::HostTaskSubmitted {
            task_id: task_id.to_string(),
            status: status.to_string(),
            output_ref: None,
            usage: None,
            submitted_by: "engine".to_string(),
            client: None,
            note: Some(note),
            model: None,
        })
    }

    /// Waits for the person's answer to the question a `blocked` task asked.
    /// `Ok(Some(answer))` once it lands; `Err` when the attempt must end.
    fn await_answer(
        &self,
        task: &AgentTask,
        answers_before: usize,
        cancel: &AtomicBool,
        control: Option<&ControlHooks>,
    ) -> Result<String, AgentFailure> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                self.mark("cancelled");
                return Err(AgentFailure::new(ErrorClass::Transport, "cancelled"));
            }
            if let Some(c) = control {
                (c.on_poll)();
                if c.interrupt.load(Ordering::Relaxed) {
                    self.mark("interrupted");
                    return Err(AgentFailure::new(
                        ErrorClass::ProcessExit,
                        "host task interrupted by the supervisor",
                    ));
                }
            }
            if let Some(msg) = super::live::tick_live_observation(
                self.run_dir,
                task.node,
                self.journal,
                self.question_timeout,
                self.default_answer.as_deref(),
            )
            .map_err(failure)?
            {
                self.mark("question_timeout");
                return Err(AgentFailure::new(ErrorClass::Timeout, msg));
            }
            let answers: Vec<_> = read_answers_after(self.run_dir, None)
                .map_err(failure)?
                .into_iter()
                .filter(|a| a.node == task.node)
                .collect();
            if let Some(a) = answers.get(answers_before) {
                // Journal the QuestionAnswered before the follow-up task.
                super::live::observe_live_channels(self.run_dir, task.node, self.journal)
                    .map_err(failure)?;
                return Ok(a.answer.clone());
            }
            std::thread::sleep(HOST_POLL);
        }
    }

    /// Turns a consumed `succeeded`/`failed` submission into the report the
    /// CLI path would have produced.
    fn report(&self, task: &AgentTask, sub: &host_task::Submission) -> AgentReport {
        let text = sub.output.clone();
        let (mut status, mut output, summary) = if task.report_contract {
            let r = crate::adapter::interpret_report(&text);
            (r.status, r.output, r.summary)
        } else {
            let t = text.trim().to_string();
            (NodeStatus::Succeeded, t.clone(), t)
        };
        if sub.status == SubmitStatus::Failed {
            status = NodeStatus::Failed;
        }
        if let Some(tag) = task.extract
            && let Some(inner) = crate::adapter::extract_marker(&text, tag)
        {
            output = inner;
        }
        // The submission is the host's explicit completion signal, so it is
        // the attempt's verdict unless the subagent wrote its own status file
        // (which may carry named outputs and then wins).
        if let Some(sf) = &task.status_file
            && !sf.exists()
        {
            let verdict = match status {
                NodeStatus::Succeeded => "success",
                _ => "failure",
            };
            let _ = apb_core::fsutil::atomic_write_private(
                sf,
                format!("{{\"status\":\"{verdict}\"}}").as_bytes(),
            );
        }
        AgentReport {
            status,
            output,
            summary,
            raw: text,
            question: None,
            session: None,
            usage: sub.usage.as_ref().map(|u| u.to_agent_usage()),
        }
    }
}

impl crate::adapter::AgentAdapter for HostAdapter<'_, '_> {
    fn run(&self, task: &AgentTask) -> Result<AgentReport, AgentFailure> {
        self.run_cancellable(task, &AtomicBool::new(false), None, None, None, None)
    }

    fn run_cancellable(
        &self,
        task: &AgentTask,
        cancel: &AtomicBool,
        on_spawn: Option<&dyn Fn(u32, u64)>,
        _live: Option<&LiveHooks>,
        stall: Option<&StallHooks>,
        control: Option<&ControlHooks>,
    ) -> Result<AgentReport, AgentFailure> {
        let env = task_env(task);
        let mut prompt = task_prompt(task, &env);
        let Requested {
            mut task_id,
            mut deadline,
            prior_status_file,
        } = self.request(task, &prompt, &env, true).map_err(failure)?;
        // A verdict the adopted task's subagent wrote at the path it was
        // given is this attempt's verdict.
        let adopt_verdict = || {
            if let (Some(old), Some(new)) = (&prior_status_file, &task.status_file) {
                let old = std::path::Path::new(old);
                if old != new.as_path()
                    && old.is_file()
                    && !new.exists()
                    && let Ok(bytes) = std::fs::read(old)
                {
                    let _ = apb_core::fsutil::atomic_write_private(new, &bytes);
                }
            }
        };
        // The attempt is "running" in the drive process itself: its pid is
        // the driver's, so the attempt reads as lost exactly when the driver
        // is gone, and the entry reaper closes it then.
        if let Some(cb) = on_spawn {
            cb(std::process::id(), 0);
        }
        let started = std::time::Instant::now();
        let mut stall_watch =
            crate::stall::StallWatch::new(stall.map(|s| s.expected), stall.map(|s| s.on_stall));
        loop {
            if let Some(sub) = host_task::read_submission(self.run_dir, &task_id) {
                host_task::write_output(self.run_dir, &task_id, &sub.output).map_err(failure)?;
                self.journal
                    .append(EventPayload::HostTaskSubmitted {
                        task_id: task_id.clone(),
                        status: sub.status.as_str().to_string(),
                        output_ref: Some(host_task::output_ref(&task_id)),
                        usage: sub.usage.as_ref().map(|u| u.to_agent_usage()),
                        submitted_by: sub.submitted_by.clone(),
                        client: sub.client.clone(),
                        note: sub.note.clone(),
                        model: sub.model.clone(),
                    })
                    .map_err(failure)?;
                if sub.status != SubmitStatus::Blocked {
                    self.mark(sub.status.as_str());
                    adopt_verdict();
                    return Ok(self.report(task, &sub));
                }
                // Blocked: the subagent needs the person. Post the question on
                // the interactive channel (the run parks on it like on any
                // question), wait for the answer, then hand the host a
                // follow-up task that carries the question and the answer.
                let answers_before = read_answers_after(self.run_dir, None)
                    .map_err(failure)?
                    .into_iter()
                    .filter(|a| a.node == task.node)
                    .count();
                post_question(
                    self.run_dir,
                    task.node,
                    self.attempt,
                    sub.output.trim(),
                    Vec::new(),
                )
                .map_err(failure)?;
                super::live::observe_live_channels(self.run_dir, task.node, self.journal)
                    .map_err(failure)?;
                let answer = self.await_answer(task, answers_before, cancel, control)?;
                prompt = format!(
                    "{prompt}\n\nYou stopped to ask the person: {}\nTheir answer: {answer}\nContinue the task from where you stopped.",
                    sub.output.trim()
                );
                let next = self.request(task, &prompt, &env, false).map_err(failure)?;
                (task_id, deadline) = (next.task_id, next.deadline);
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                self.mark("cancelled");
                let _ = self.close(&task_id, "cancelled", "the run was stopped".into());
                return Err(AgentFailure::new(ErrorClass::Transport, "cancelled"));
            }
            if let Some(c) = control {
                (c.on_poll)();
                if c.interrupt.load(Ordering::Relaxed) {
                    self.mark("interrupted");
                    let _ = self.close(
                        &task_id,
                        "interrupted",
                        "a supervisor interrupted the attempt".into(),
                    );
                    return Err(AgentFailure::new(
                        ErrorClass::ProcessExit,
                        "host task interrupted by the supervisor",
                    ));
                }
            }
            stall_watch.tick(started.elapsed());
            if let Some(d) = deadline
                && std::time::Instant::now() >= d
            {
                let secs = task.timeout.map(|t| t.as_secs()).unwrap_or(0);
                let msg = format!(
                    "host_task_timeout: host task `{task_id}` of node `{}` was not submitted within {secs}s",
                    task.node
                );
                self.mark("expired");
                let _ = self.close(&task_id, "expired", msg.clone());
                return Err(AgentFailure::new(ErrorClass::Timeout, msg));
            }
            std::thread::sleep(HOST_POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ManifestProfile, ManifestTier};
    use host_task::HintSource;

    fn ri(model: &str) -> crate::invocation::ResolvedInvocation {
        crate::invocation::ResolvedInvocation {
            agent_id: "claude".into(),
            model: model.into(),
            spec: crate::invocation::builtin("claude").unwrap(),
            soul_delivery: apb_core::config::SoulDelivery::default(),
            canonical_executable: PathBuf::from("/bin/true"),
            executable_fingerprint: "0:0".into(),
        }
    }

    fn tier(name: &str, model: Option<&str>) -> ManifestTier {
        ManifestTier {
            name: name.into(),
            for_work: "work".into(),
            invocation: model.map(ri),
        }
    }

    /// Tiers `light`, `mid`, the executor (the tier without an invocation,
    /// `use: executor`, sitting in the middle of the list), then `heavy`;
    /// chain: the primary `exec` and two fallbacks `fb1`, `fb2`.
    fn entry() -> ManifestProfile {
        ManifestProfile {
            scope: "project".into(),
            name: "p".into(),
            profile_digest: String::new(),
            bundle_digest: String::new(),
            soul: String::new(),
            soul_requirement: apb_core::profile::SoulRequirement::Any,
            skills: Vec::new(),
            chain: vec![ri("exec"), ri("fb1"), ri("fb2")],
            ephemeral: false,
            hermetic: false,
            zcode_mode: None,
            tiers: vec![
                tier("light", Some("l")),
                tier("mid", Some("m")),
                tier("executor", None),
                tier("heavy", Some("h")),
            ],
            routed_tier: None,
            cascade: 0,
        }
    }

    fn labels(e: &ManifestProfile) -> Vec<(String, String)> {
        e.chain
            .iter()
            .zip(chain_sources(e))
            .map(|(ri, s)| {
                let role = match s {
                    HintSource::Primary { .. } => "primary".to_string(),
                    HintSource::Fallback { index, of, .. } => format!("fallback {index}/{of}"),
                    HintSource::Tier { tier, .. } => format!("tier {tier}"),
                    HintSource::HostFallback { .. } => "host fallback".to_string(),
                };
                (ri.model.clone(), role)
            })
            .collect()
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn every_chain_step_is_labelled_by_its_role_in_the_profile() {
        let e = entry();
        assert_eq!(
            labels(&e),
            pairs(&[
                ("exec", "primary"),
                ("fb1", "fallback 1/2"),
                ("fb2", "fallback 2/2")
            ])
        );
        // Routed below the executor: every tier up to the executor, then the
        // profile's own chain with both fallbacks.
        let below = crate::decision::routing::routed_entry(&e, "light").unwrap();
        assert_eq!(below.cascade, 2);
        assert_eq!(
            labels(&below),
            pairs(&[
                ("l", "tier light"),
                ("m", "tier mid"),
                ("exec", "primary"),
                ("fb1", "fallback 1/2"),
                ("fb2", "fallback 2/2"),
            ])
        );
        let mid = crate::decision::routing::routed_entry(&e, "mid").unwrap();
        assert_eq!(
            labels(&mid)[..2],
            pairs(&[("m", "tier mid"), ("exec", "primary")])[..]
        );
        // A tier above the executor runs first with no cascade: it is still
        // a tier, and the profile's executor behind it is the primary.
        let above = crate::decision::routing::routed_entry(&e, "heavy").unwrap();
        assert_eq!(above.cascade, 0);
        assert_eq!(
            labels(&above),
            pairs(&[
                ("h", "tier heavy"),
                ("exec", "primary"),
                ("fb1", "fallback 1/2"),
                ("fb2", "fallback 2/2"),
            ])
        );
    }
}
