//! The identity of every attempt of a run, folded from its journal: where
//! in its binding it ran, the digests of that binding, the model apb
//! requested and, for a host attempt, the model each of its host tasks
//! reported. A host attempt is credited to a model only when all its tasks
//! reported the same one; an earlier slot fold kept only the last
//! submission's model.
//!
//! Only [`ReportedModel::One`] credits an outcome to a model: a host task
//! that reported none makes the attempt `Partial`, never `One`, and the
//! hint is never taken for the model that ran.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use super::{ExecStep, StepEntry, StepSource};
use crate::event::{Event, EventPayload};

/// One host task of a host attempt, follow-ups of a blocked task included.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostTaskExec {
    pub task_id: String,
    /// The task's closing status as journaled.
    pub status: String,
    /// The model the task's submission reported; `None` when the host
    /// reported none or the engine closed the task.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// The model an attempt ran on as its host tasks reported it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReportedModel {
    /// Every task reported a model and all name the same one.
    One { model: String },
    /// Every task reported a model and they differ.
    Mixed { models: Vec<String> },
    /// Some task reported none.
    Partial {
        known: Vec<String>,
        unknown_tasks: u32,
    },
    /// No task reported a model (every CLI attempt).
    None,
}

/// The identity of one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionIdentity {
    /// The attempt id: the journal seq of its `attempt_started`.
    pub attempt: u64,
    pub node: String,
    pub item: Option<String>,
    /// The attempt number within its node execution, as journaled.
    pub number: u32,
    /// Where in its binding it ran; `None` when not recorded.
    pub step: Option<ExecStep>,
    pub profile_digest: Option<String>,
    pub bundle_digest: Option<String>,
    pub executor_digest: Option<String>,
    pub ephemeral: bool,
    pub agent: String,
    /// Passed to the CLI, or the hint for a host attempt.
    pub model_requested_by_apb: Option<String>,
    /// One per host task of the attempt; empty for a CLI attempt.
    pub host_tasks: Vec<HostTaskExec>,
    /// The cloud catalog model the attempt ran on (`attempt_started.cloud`).
    pub cloud: Option<crate::event::AttemptCloud>,
}

impl ExecutionIdentity {
    /// The model the host tasks reported, by the coverage rule of
    /// [`ReportedModel`].
    pub fn reported_model(&self) -> ReportedModel {
        reported_of(&self.host_tasks)
    }

    /// `cli`, `host` or `scenario`: as journaled on `exec`, else read from
    /// the attempt (a host task or the host agent).
    pub fn executed_by(&self) -> &str {
        match &self.step {
            Some(step) => &step.executed_by,
            None if self.agent == super::HOST_AGENT || !self.host_tasks.is_empty() => {
                super::EXECUTED_BY_HOST
            }
            None => super::EXECUTED_BY_CLI,
        }
    }

    /// Whether a host executed the attempt.
    pub fn is_host(&self) -> bool {
        self.executed_by() == super::EXECUTED_BY_HOST
    }
}

/// The coverage rule of [`ReportedModel`]: `One` only when every task
/// reported a model and every two of them name the same one
/// ([`same_model`]), so the answer does not depend on the order the tasks
/// reported in. A name joins a group only when it matches every member,
/// and the group keeps its most specific member (the longest token list)
/// as the model it reports, so a bare alias reporting first does not
/// shorten the name the run surfaces show.
fn reported_of(tasks: &[HostTaskExec]) -> ReportedModel {
    let mut groups: Vec<Vec<&String>> = Vec::new();
    let mut unknown_tasks = 0u32;
    for t in tasks {
        match &t.model {
            Some(m) => match groups
                .iter_mut()
                .find(|g| g.iter().all(|k| same_model(k, m)))
            {
                Some(g) => g.push(m),
                None => groups.push(vec![m]),
            },
            None => unknown_tasks += 1,
        }
    }
    let mut known: Vec<String> = groups
        .iter()
        .map(|g| {
            g.iter()
                .max_by_key(|m| tokens(m).len())
                .map(|m| (*m).clone())
                .expect("a group is never empty")
        })
        .collect();
    match (known.len(), unknown_tasks) {
        (0, _) => ReportedModel::None,
        (_, n) if n > 0 => ReportedModel::Partial {
            known,
            unknown_tasks,
        },
        (1, _) => ReportedModel::One {
            model: known.remove(0),
        },
        _ => ReportedModel::Mixed { models: known },
    }
}

/// Every attempt's identity in the order it started. The binding of an
/// attempt without `exec` resolves to the manifest entry only when the run
/// has no rebind and no applied tier route for its node; otherwise it is
/// `None`: the current overlay is never attributed to past attempts.
pub fn identities(run_dir: &Path, events: &[Event]) -> Vec<ExecutionIdentity> {
    let manifest = crate::manifest::read(run_dir).ok().flatten();
    let rebound = rebound_nodes(events);
    let mut out = fold(events);
    for id in &mut out {
        let binding = match &id.step {
            Some(step) => super::read_binding(run_dir, &step.binding),
            None if rebound.contains(&id.node) => None,
            None => manifest
                .as_ref()
                .and_then(|m| m.for_node(&id.node))
                .cloned(),
        };
        if let Some(b) = binding {
            id.executor_digest = Some(super::digest::executor_digest(&b));
            id.profile_digest = Some(b.profile_digest);
            id.bundle_digest = Some(b.bundle_digest);
            id.ephemeral = b.ephemeral;
        }
    }
    out
}

/// Nodes whose binding changed during the run: a supervisor rebind or an
/// applied tier route.
fn rebound_nodes(events: &[Event]) -> BTreeSet<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ProfileRebound { node, .. } => Some(node.clone()),
            EventPayload::DecisionMade {
                use_site,
                node: Some(node),
                applied: true,
                ..
            } if use_site == "routing" => Some(node.clone()),
            _ => None,
        })
        .collect()
}

/// One attempt while the journal is walked.
struct Slot {
    id: ExecutionIdentity,
    started: bool,
    finished: bool,
    /// The hint of the attempt's first host task.
    hint: Option<Option<String>>,
}

/// The journal-only half of [`identities`]: attempts, their chain
/// positions, the models apb requested and every host task, no digests.
pub(crate) fn fold(events: &[Event]) -> Vec<ExecutionIdentity> {
    let mut walk = IdentityWalk::default();
    for e in events {
        walk.step(e);
    }
    walk.finish()
        .into_iter()
        .filter_map(|(started, id)| started.then_some(id))
        .collect()
}

/// The journal walk behind [`fold`], one event at a time, shared with the
/// run retro so both read an attempt one way. One slot per attempt
/// execution: a loop back into a node numbers its attempts from 1 again,
/// so a `(node, item, attempt)` whose latest slot already started or
/// finished opens a new one. A host attempt's first task is requested
/// before its `attempt_started`, a blocked task's follow-up after it; both
/// land on the same slot.
#[derive(Default)]
pub(crate) struct IdentityWalk {
    slots: Vec<Slot>,
    /// Host task id -> (slot, task index in the slot).
    tasks: BTreeMap<String, (usize, usize)>,
}

impl IdentityWalk {
    fn open(&mut self, node: &str, item: Option<&str>, n: u32, starting: bool) -> usize {
        let found = self
            .slots
            .iter()
            .rposition(|s| s.id.node == node && s.id.item.as_deref() == item && s.id.number == n)
            .filter(|&i| !self.slots[i].finished && !(starting && self.slots[i].started));
        if let Some(i) = found {
            return i;
        }
        self.slots.push(Slot {
            id: ExecutionIdentity {
                attempt: 0,
                node: node.to_string(),
                item: item.map(str::to_string),
                number: n,
                step: None,
                profile_digest: None,
                bundle_digest: None,
                executor_digest: None,
                ephemeral: false,
                agent: String::new(),
                model_requested_by_apb: None,
                host_tasks: Vec::new(),
                cloud: None,
            },
            started: false,
            finished: false,
            hint: None,
        });
        self.slots.len() - 1
    }

    /// Folds one event; returns the slot it belongs to when it is an
    /// attempt event (started, finished, host task requested or submitted).
    pub(crate) fn step(&mut self, e: &Event) -> Option<usize> {
        match &e.payload {
            EventPayload::HostTaskRequested {
                task_id,
                node,
                attempt,
                model_hint,
                ..
            } => {
                let i = self.open(node, None, *attempt, false);
                let s = &mut self.slots[i];
                if s.hint.is_none() {
                    s.hint = Some(model_hint.clone().filter(|m| !m.trim().is_empty()));
                }
                // An adopted task is requested again under its own id.
                let t = match s.id.host_tasks.iter().position(|t| &t.task_id == task_id) {
                    Some(t) => t,
                    None => {
                        s.id.host_tasks.push(HostTaskExec {
                            task_id: task_id.clone(),
                            status: "open".to_string(),
                            model: None,
                        });
                        s.id.host_tasks.len() - 1
                    }
                };
                self.tasks.insert(task_id.clone(), (i, t));
                Some(i)
            }
            EventPayload::AttemptStarted {
                node,
                attempt,
                agent,
                model,
                item,
                exec,
                cloud,
                ..
            } => {
                let i = self.open(node, item.as_deref(), *attempt, true);
                let s = &mut self.slots[i];
                s.started = true;
                s.id.attempt = e.seq;
                s.id.agent.clone_from(agent);
                s.id.step.clone_from(exec);
                s.id.cloud.clone_from(cloud);
                s.id.model_requested_by_apb = model.clone().filter(|m| !m.trim().is_empty());
                Some(i)
            }
            EventPayload::AttemptFinished {
                node,
                attempt,
                item,
                ..
            } => {
                let i = self.open(node, item.as_deref(), *attempt, false);
                self.slots[i].finished = true;
                Some(i)
            }
            EventPayload::HostTaskSubmitted {
                task_id,
                status,
                model,
                submitted_by,
                ..
            } => {
                let &(i, t) = self.tasks.get(task_id)?;
                let task = self.slots[i].id.host_tasks.get_mut(t)?;
                task.status.clone_from(status);
                // An engine closure (expired, cancelled, ...) is no host reply.
                task.model = model
                    .clone()
                    .filter(|m| submitted_by != "engine" && !m.trim().is_empty());
                Some(i)
            }
            _ => None,
        }
    }

    /// The node and attempt number of slot `i`.
    pub(crate) fn key(&self, i: usize) -> (&str, u32) {
        (&self.slots[i].id.node, self.slots[i].id.number)
    }

    /// Every slot in the order it opened, with whether its attempt started.
    pub(crate) fn finish(self) -> Vec<(bool, ExecutionIdentity)> {
        self.slots
            .into_iter()
            .map(|mut s| {
                // A host attempt: what apb requested is its first task's
                // hint, never the model `attempt_started` names for the step.
                if s.id.is_host() {
                    s.id.model_requested_by_apb = s.hint.flatten();
                }
                (s.started, s.id)
            })
            .collect()
    }
}

/// Every attempt of the run at `run_dir` as the run surfaces show it.
pub fn views(run_dir: &Path, events: &[Event]) -> Vec<ExecutionIdentityView> {
    let manifest = crate::manifest::read(run_dir).ok().flatten();
    let expected = manifest.as_ref().map(primary_models_of).unwrap_or_default();
    let classes = manifest
        .as_ref()
        .map(primary_classes_of)
        .unwrap_or_default();
    fold(events)
        .into_iter()
        .map(|id| view_of(id, &expected))
        .map(|mut v| {
            if let Some(primary) = classes.get(&v.node) {
                let ran = v.cloud.as_ref().map(|c| c.privacy_class);
                v.privacy_class_left = !v.is_host_view() && ran != *primary;
            }
            v
        })
        .collect()
}

impl ExecutionIdentityView {
    fn is_host_view(&self) -> bool {
        self.executed_by == super::EXECUTED_BY_HOST
    }
}

/// The privacy class of each node's profile executor (the first entry of
/// its chain, as [`primary_models_of`] reads it): `None` for a model
/// outside any catalog.
pub fn primary_classes_of(
    m: &crate::manifest::RunExecutionManifest,
) -> BTreeMap<String, Option<apb_core::cloud_models::PrivacyClass>> {
    m.node_bindings
        .iter()
        .filter_map(|(node, key)| {
            let p = m.profiles.iter().find(|p| &p.key() == key)?;
            let first = p.chain.first()?;
            Some((
                node.clone(),
                first.cloud.as_ref().map(|c| c.model.privacy_class),
            ))
        })
        .collect()
}

/// The shortened digest a surface shows.
fn short(digest: &str) -> String {
    let hex = digest.rsplit(':').next().unwrap_or(digest);
    hex.chars().take(12).collect()
}

/// One attempt for the surfaces, checked against the node's expected model:
/// a CLI attempt by the model apb requested, a host attempt only when its
/// tasks reported [`ReportedModel::One`].
pub(crate) fn view_of(
    id: ExecutionIdentity,
    expected: &BTreeMap<String, String>,
) -> ExecutionIdentityView {
    let reported = id.reported_model();
    let expected = expected.get(&id.node).cloned();
    let compared = if id.is_host() {
        match &reported {
            ReportedModel::One { model } => Some(model.clone()),
            _ => None,
        }
    } else {
        id.model_requested_by_apb.clone()
    };
    let mismatch = matches!((&compared, &expected), (Some(m), Some(x)) if !same_model(m, x));
    ExecutionIdentityView {
        executed_by: id.executed_by().to_string(),
        node: id.node,
        item: id.item,
        attempt: id.number,
        agent: id.agent,
        requested_model: id.model_requested_by_apb,
        reported,
        host_tasks: id.host_tasks,
        expected,
        mismatch,
        step: id.step.map(|s| ExecStepView {
            binding: short(&s.binding),
            step: s.step,
            source: s.source,
            entered: s.entered,
        }),
        cloud: id.cloud,
        privacy_class_left: false,
    }
}

/// The chain position of an attempt as the run surfaces show it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecStepView {
    /// The binding snapshot's digest, shortened.
    pub binding: String,
    pub step: u32,
    pub source: StepSource,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entered: Option<StepEntry>,
}

/// One attempt on the run surfaces (`run_status`, the HTTP run detail,
/// `apb runs <id>`, the dashboard attempt list).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionIdentityView {
    pub node: String,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The attempt number within its node execution.
    pub attempt: u32,
    /// `cli`, `host` or `scenario`.
    pub executed_by: String,
    pub agent: String,
    /// The model apb requested: passed to the CLI, or the hint of a host
    /// attempt. Never shown as the model that ran.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    pub reported: ReportedModel,
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<HostTaskExec>>", optional))]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub host_tasks: Vec<HostTaskExec>,
    /// The primary model of the node's profile, from the run manifest.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// Computed only for a host attempt whose reported model is `One` and
    /// for the requested model of a CLI attempt.
    pub mismatch: bool,
    /// `None`: position not recorded.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<ExecStepView>,
    /// The cloud catalog model the attempt ran on, with its privacy class;
    /// absent for any other model.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud: Option<crate::event::AttemptCloud>,
    /// The attempt ran on another privacy class than the node's profile
    /// executor (a fallback or a tier on another class, or on a model
    /// outside any catalog).
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub privacy_class_left: bool,
}

/// Words a model id may carry beside the model itself: the vendor or the
/// release channel. `claude-opus-4-1` and `opus` name one model; a word
/// outside this list (`flash`, `mini`, `haiku`) names another one.
const NEUTRAL_WORDS: &[&str] = &[
    "claude",
    "anthropic",
    "openai",
    "google",
    "gemini",
    "latest",
    "preview",
    "exp",
    "experimental",
    "stable",
];

fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether two model names name the same model. Both are split into
/// lowercase tokens on every non-alphanumeric character. They match when
/// the token lists are equal, or when the shorter list is a subsequence of
/// the longer one and every extra token of the longer one is a date (six or
/// more digits), a vendor or channel word ([`NEUTRAL_WORDS`]), or a version
/// number while the shorter name carries no number at all (a bare alias).
/// So `opus` matches `claude-opus-4-1` and `sonnet` matches
/// `claude-sonnet-4-5-20250929`, while `glm-5.3` does not match
/// `glm-5.3-flash`, `gpt-5` does not match `gpt-5-mini` and
/// `claude-sonnet-4` does not match `claude-sonnet-4-5`: a variant word or
/// another version is a different model, which is what the mismatch flag
/// is for. An alias matches every version, so the relation is not
/// transitive; [`reported_of`] compares every pair.
pub fn same_model(a: &str, b: &str) -> bool {
    let (a, b) = (tokens(a), tokens(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let digits = |t: &String| t.chars().all(|c| c.is_ascii_digit());
    let bare_alias = !short.iter().any(digits);
    let mut want = short.iter().peekable();
    for t in long {
        if want.peek() == Some(&t) {
            want.next();
        } else if !((digits(t) && (bare_alias || t.len() >= 6))
            || NEUTRAL_WORDS.contains(&t.as_str()))
        {
            return false;
        }
    }
    want.peek().is_none()
}

/// The primary model of each node's profile, from the run manifest
/// (`node_bindings` and the profile's first chain step). Empty when the run
/// has no manifest.
pub fn primary_models(run_dir: &Path) -> BTreeMap<String, String> {
    match crate::manifest::read(run_dir) {
        Ok(Some(m)) => primary_models_of(&m),
        _ => BTreeMap::new(),
    }
}

/// [`primary_models`] over a manifest already read.
pub fn primary_models_of(m: &crate::manifest::RunExecutionManifest) -> BTreeMap<String, String> {
    m.node_bindings
        .iter()
        .filter_map(|(node, key)| {
            let p = m.profiles.iter().find(|p| &p.key() == key)?;
            Some((node.clone(), p.chain.first()?.model.clone()))
        })
        .filter(|(_, model)| !model.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, v: serde_json::Value) -> Event {
        let mut v = v;
        v["seq"] = seq.into();
        v["ts"] = seq.into();
        serde_json::from_value(v).expect("event")
    }

    /// One host attempt: its first task, then one follow-up per entry of
    /// `follow_ups`; each entry is the model that task's submission
    /// reported. The first task is a `blocked` question.
    fn host_attempt(first: Option<&str>, follow_ups: &[Option<&str>]) -> Vec<Event> {
        let mut seq = 0;
        let mut next = || {
            seq += 1;
            seq
        };
        let mut events = vec![
            ev(
                next(),
                serde_json::json!({"type": "host_task_requested", "task_id": "w-1", "node": "w", "attempt": 1, "model_hint": "sonnet"}),
            ),
            ev(
                next(),
                serde_json::json!({"type": "attempt_started", "node": "w", "attempt": 1, "agent": "host", "model": "haiku"}),
            ),
            ev(
                next(),
                serde_json::json!({"type": "host_task_submitted", "task_id": "w-1", "status": "blocked", "submitted_by": "host", "model": first}),
            ),
        ];
        for (i, m) in follow_ups.iter().enumerate() {
            let id = format!("w-1-f{i}");
            events.push(ev(
                next(),
                serde_json::json!({"type": "host_task_requested", "task_id": id, "node": "w", "attempt": 1}),
            ));
            events.push(ev(
                next(),
                serde_json::json!({"type": "host_task_submitted", "task_id": id, "status": "succeeded", "submitted_by": "host", "model": m}),
            ));
        }
        events.push(ev(
            next(),
            serde_json::json!({"type": "attempt_finished", "node": "w", "attempt": 1, "status": "succeeded"}),
        ));
        events
    }

    fn reported(events: &[Event]) -> ReportedModel {
        let ids = fold(events);
        assert_eq!(ids.len(), 1);
        ids[0].reported_model()
    }

    /// Card A5: a blocked task without a model, then a follow-up reporting
    /// A, is `partial`, never A; the slot fold credited A.
    #[test]
    fn a_task_without_a_model_makes_the_attempt_partial() {
        assert_eq!(
            reported(&host_attempt(None, &[Some("opus")])),
            ReportedModel::Partial {
                known: vec!["opus".into()],
                unknown_tasks: 1
            }
        );
    }

    /// Card A5: two tasks reporting different models are `mixed`; the
    /// same model under two spellings is `one`.
    #[test]
    fn follow_ups_on_other_models_are_mixed_and_one_model_is_one() {
        assert_eq!(
            reported(&host_attempt(Some("opus"), &[Some("glm-5.3-flash")])),
            ReportedModel::Mixed {
                models: vec!["opus".into(), "glm-5.3-flash".into()]
            }
        );
        assert_eq!(
            reported(&host_attempt(Some("opus"), &[Some("claude-opus-4-1")])),
            ReportedModel::One {
                model: "claude-opus-4-1".into()
            }
        );
        assert_eq!(reported(&host_attempt(None, &[])), ReportedModel::None);
    }

    /// A host attempt's requested model is its task's hint, not the
    /// declared model `attempt_started` names; one reporting a model only
    /// partly is never compared with the expected model.
    #[test]
    fn a_host_attempt_requests_its_hint_and_partial_never_mismatches() {
        let events = host_attempt(None, &[Some("glm-5.3-flash")]);
        let ids = fold(&events);
        assert_eq!(ids[0].model_requested_by_apb.as_deref(), Some("sonnet"));
        assert_eq!(ids[0].attempt, 2, "the attempt id is its seq");
        assert_eq!(ids[0].host_tasks.len(), 2);
        let expected = BTreeMap::from([("w".to_string(), "opus".to_string())]);
        let view = view_of(ids[0].clone(), &expected);
        assert_eq!(view.executed_by, "host");
        assert!(!view.mismatch, "a partial report is not credited");
        let events = host_attempt(Some("glm-5.3-flash"), &[]);
        let view = view_of(fold(&events).remove(0), &expected);
        assert!(view.mismatch, "one reported model is compared");
    }

    /// A CLI attempt is compared by the model apb requested, and an engine
    /// closure of a host task reports no model.
    #[test]
    fn a_cli_attempt_compares_what_apb_requested() {
        let events = [
            ev(
                1,
                serde_json::json!({"type": "attempt_started", "node": "a", "attempt": 1, "agent": "claude", "model": "haiku"}),
            ),
            ev(
                2,
                serde_json::json!({"type": "attempt_finished", "node": "a", "attempt": 1, "status": "failed"}),
            ),
            ev(
                3,
                serde_json::json!({"type": "host_task_requested", "task_id": "a-2", "node": "a", "attempt": 2}),
            ),
            ev(
                4,
                serde_json::json!({"type": "attempt_started", "node": "a", "attempt": 2, "agent": "host"}),
            ),
            ev(
                5,
                serde_json::json!({"type": "host_task_submitted", "task_id": "a-2", "status": "expired", "submitted_by": "engine", "model": "opus"}),
            ),
        ];
        let expected = BTreeMap::from([("a".to_string(), "sonnet".to_string())]);
        let ids = fold(&events);
        let views: Vec<_> = ids.iter().cloned().map(|i| view_of(i, &expected)).collect();
        assert_eq!(views[0].executed_by, "cli");
        assert_eq!(views[0].requested_model.as_deref(), Some("haiku"));
        assert!(views[0].mismatch);
        assert_eq!(ids[1].host_tasks[0].model, None, "an engine closure");
        assert_eq!(ids[1].host_tasks[0].status, "expired");
        assert_eq!(views[1].reported, ReportedModel::None);
    }

    #[test]
    fn a_reentered_node_and_a_host_attempt_each_get_their_own_slot() {
        let events = [
            ev(
                1,
                serde_json::json!({"type": "attempt_started", "node": "a", "attempt": 1, "agent": "claude", "model": "sonnet"}),
            ),
            ev(
                2,
                serde_json::json!({"type": "attempt_finished", "node": "a", "attempt": 1, "status": "succeeded"}),
            ),
            // The loop back into `a` numbers its attempts from 1 again; its
            // host task is requested before the attempt starts.
            ev(
                3,
                serde_json::json!({"type": "host_task_requested", "task_id": "t1", "node": "a", "attempt": 1}),
            ),
            ev(
                4,
                serde_json::json!({"type": "attempt_started", "node": "a", "attempt": 1, "agent": "host", "model": "sonnet"}),
            ),
            ev(
                5,
                serde_json::json!({"type": "host_task_submitted", "task_id": "t1", "status": "succeeded", "submitted_by": "host", "model": "glm-5.3-flash"}),
            ),
            ev(
                6,
                serde_json::json!({"type": "attempt_finished", "node": "a", "attempt": 1, "status": "succeeded"}),
            ),
        ];
        let expected = BTreeMap::from([("a".to_string(), "claude-sonnet-4-5".to_string())]);
        let views: Vec<_> = fold(&events)
            .into_iter()
            .map(|i| view_of(i, &expected))
            .collect();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].executed_by, "cli");
        assert_eq!(views[0].requested_model.as_deref(), Some("sonnet"));
        assert!(!views[0].mismatch);
        assert_eq!(views[1].executed_by, "host");
        assert_eq!(
            views[1].reported,
            ReportedModel::One {
                model: "glm-5.3-flash".into()
            }
        );
        assert!(views[1].mismatch);
    }

    #[test]
    fn aliases_and_dated_ids_are_the_same_model() {
        assert!(same_model("opus", "claude-opus-4-1"));
        assert!(same_model("sonnet", "claude-sonnet-4-5-20250929"));
        assert!(same_model(
            "claude-sonnet-4-5",
            "anthropic/claude-sonnet-4-5-latest"
        ));
        assert!(same_model("GLM-5.3-Flash", "glm_5.3_flash"));
        assert!(same_model("gpt-5", "openai/gpt-5"));
    }

    #[test]
    fn a_variant_word_or_another_family_is_another_model() {
        assert!(!same_model("glm-5.3", "glm-5.3-flash"));
        assert!(!same_model("gpt-5", "gpt-5-mini"));
        assert!(!same_model("sonnet", "haiku"));
        assert!(!same_model("sonnet", "claude-haiku-4-5"));
        assert!(!same_model("GLM-5.3-Flash", "opus"));
        assert!(!same_model("", "opus"));
        assert!(!same_model("-", "opus"));
    }

    /// A version number is part of the model: only a date (six or more
    /// digits) may be extra, unless the shorter name carries no number.
    #[test]
    fn another_version_number_is_another_model() {
        assert!(!same_model("claude-sonnet-4", "claude-sonnet-4-5"));
        assert!(!same_model("gpt-5", "gpt-5-1"));
        assert!(!same_model("glm-5", "glm-5.3"));
        assert!(!same_model("claude-sonnet-4-5", "claude-sonnet-4-6"));
        assert!(same_model(
            "claude-sonnet-4-5",
            "claude-sonnet-4-5-20250929"
        ));
    }

    /// The model shown for a `One` is the most specific name in the group
    /// (the longest token list), whatever the order the tasks reported in:
    /// `opus` and `claude-opus-4-1` are one model, and the group keeps the
    /// full name both ways round.
    #[test]
    fn one_model_is_the_most_specific_name_in_every_order() {
        let names = ["opus", "claude-opus-4-1"];
        for order in [[0usize, 1usize], [1, 0]] {
            let [a, b] = order.map(|i| names[i]);
            let r = reported(&host_attempt(Some(a), &[Some(b)]));
            assert_eq!(
                r,
                ReportedModel::One {
                    model: "claude-opus-4-1".into()
                },
                "{order:?}: {r:?}"
            );
        }
    }

    /// Two reported versions are never folded into one model, whatever the
    /// order the tasks reported them in.
    #[test]
    fn reported_versions_are_mixed_in_every_order() {
        let names = ["claude-sonnet-4", "claude-sonnet-4-5", "claude-sonnet-4-6"];
        for order in [[0, 1, 2], [1, 2, 0], [2, 0, 1]] {
            let [a, b, c] = order.map(|i| names[i]);
            let r = reported(&host_attempt(Some(a), &[Some(b), Some(c)]));
            assert!(matches!(r, ReportedModel::Mixed { .. }), "{order:?}: {r:?}");
        }
        let r = reported(&host_attempt(
            Some("opus"),
            &[Some("claude-opus-4-1"), Some("claude-opus-4-5")],
        ));
        assert!(matches!(r, ReportedModel::Mixed { .. }), "{r:?}");
    }
}
