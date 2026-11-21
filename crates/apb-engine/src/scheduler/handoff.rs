//! Warm session handoff between nodes (issue #67 item 1): the decision
//! whether a node with `continue_session` can continue the agent session its
//! source node finished in, read from the run's own journal.

use super::*;
use apb_core::schema::SessionMode;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// How a node with `continue_session` starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Handoff {
    /// Continue session `id`, which ran in `workdir`; with `fork`, start
    /// from a copy of it through the agent's fork form.
    Warm {
        id: String,
        workdir: PathBuf,
        fork: bool,
    },
    /// Start a fresh agent, for this reason.
    Cold(String),
}

/// What the journal says about the attempt a node's latest successful
/// execution finished with.
struct SourceAttempt {
    session: Option<String>,
    agent: String,
    model: Option<String>,
    workdir: Option<String>,
    /// The digest of the binding snapshot the attempt ran on (its
    /// `attempt_started.exec`), `None` in a journal that recorded none.
    binding: Option<String>,
}

/// The attempt `source`'s latest successful execution succeeded with, or why
/// there is none to continue.
fn source_attempt(events: &[Event], source: &str) -> Result<SourceAttempt, String> {
    let finished = events
        .iter()
        .rposition(|e| {
            matches!(&e.payload, EventPayload::NodeFinished { node, status, .. }
                if node == source && status == NodeStatus::Succeeded.as_str())
        })
        .ok_or_else(|| format!("`{source}` has not succeeded in this run"))?;
    let (at, attempt, session) = events[..finished]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, e)| match &e.payload {
            EventPayload::AttemptFinished {
                node,
                attempt,
                status,
                session,
                item: None,
                ..
            } if node == source && status == NodeStatus::Succeeded.as_str() => {
                Some((i, *attempt, session.clone()))
            }
            // An earlier execution's attempts are not this result's.
            EventPayload::NodeStarted { node, .. } if node == source => Some((usize::MAX, 0, None)),
            _ => None,
        })
        .filter(|(i, _, _)| *i != usize::MAX)
        .ok_or_else(|| {
            format!("`{source}` produced its result without an agent attempt (a cache hit)")
        })?;
    events[..at]
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::AttemptStarted {
                node,
                attempt: a,
                agent,
                model,
                workdir,
                item: None,
                exec,
                ..
            } if node == source && *a == attempt => Some(SourceAttempt {
                session: session.clone(),
                agent: agent.clone(),
                model: model.clone(),
                workdir: workdir.clone(),
                binding: exec.as_ref().map(|x| x.binding.clone()),
            }),
            _ => None,
        })
        .ok_or_else(|| format!("`{source}` left no record of its attempt"))
}

/// What a fork's handoff checks beyond a continuation's: that the node's
/// resolved profile is the source's (`profile`: the cold reason when it is
/// not), and the installed CLI's help check of the fork form, run last and
/// only when every other requirement holds ([`fork_help`]).
pub(crate) struct ForkChecks<'a> {
    pub profile: Result<(), String>,
    pub help: &'a mut dyn FnMut() -> Result<(), String>,
}

/// Whether the node about to start with executor `agent`/`model` in `dir`
/// can continue (or, with `fork`, fork) `source`'s session. Every
/// requirement is checked against the journal and the arguments, so the
/// answer is the same for a fresh run, a resume and a `continue_from`. A
/// fork's help probe runs after every cheaper check passed, never for a
/// node that will not fork.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide(
    events: &[Event],
    source: &str,
    agent: &str,
    model: &str,
    dir: &Path,
    isolated: bool,
    mode: SessionMode,
    fork: Option<ForkChecks<'_>>,
) -> Handoff {
    let src = match source_attempt(events, source) {
        Ok(s) => s,
        Err(why) => return Handoff::Cold(why),
    };
    let canonical = apb_core::detect::canonical_agent_id;
    let forks = mode == SessionMode::Fork;
    if forks {
        if crate::invocation::fork_argv(agent).is_none() {
            return Handoff::Cold(format!("agent `{agent}` cannot fork a session"));
        }
    } else if crate::invocation::resume_argv(agent).is_none() {
        return Handoff::Cold(format!("agent `{agent}` cannot resume a session"));
    }
    if canonical(&src.agent) != canonical(agent) || src.model.as_deref() != Some(model) {
        return Handoff::Cold(format!(
            "`{source}` ran on {} model {}, this node runs on {agent} model {model}",
            src.agent,
            src.model.as_deref().unwrap_or("unknown"),
        ));
    }
    let mut fork = fork;
    if forks && let Some(Err(why)) = fork.as_ref().map(|f| f.profile.clone()) {
        return Handoff::Cold(why);
    }
    let Some(id) = src.session else {
        return Handoff::Cold(format!("`{source}` recorded no session id"));
    };
    if isolated {
        return Handoff::Cold("an isolated node runs in its own directory".to_string());
    }
    let same_dir = src.workdir.as_deref().is_some_and(|w| {
        let w = Path::new(w);
        w == dir || w.canonicalize().ok() == dir.canonicalize().ok()
    });
    if !same_dir {
        return Handoff::Cold(format!(
            "`{source}` ran in {}, this node runs in {}",
            src.workdir.as_deref().unwrap_or("an unknown directory"),
            dir.display()
        ));
    }
    if forks
        && let Some(f) = fork.as_mut()
        && let Err(why) = (f.help)()
    {
        return Handoff::Cold(why);
    }
    Handoff::Warm {
        id,
        workdir: dir.to_path_buf(),
        fork: forks,
    }
}

/// Whether `node` runs under the resolved profile `source` ran under,
/// compared whole (the work part of each, `scheduler::cache::profile_work`):
/// the source's side is the binding snapshot its succeeded attempt recorded
/// when it ran (`attempt_started.exec.binding`, under `runs/<id>/bindings/`),
/// never its binding now, so a source rebound after it ran does not become
/// a fork's source under its new profile; the node's side is `target`, the
/// binding the execution entry will actually run on (a deferred retry's
/// pinned binding, else the manifest with its rebind overlay). The cold reason
/// when they differ or either cannot be read: a fork hands the source's
/// whole conversation to the node.
pub(crate) fn same_profile(
    run_dir: &Path,
    events: &[Event],
    source: &str,
    target: &crate::manifest::ManifestProfile,
) -> Result<(), String> {
    let unknown = |why: String| format!("the profile of `{source}` cannot be compared: {why}");
    let ran = source_attempt(events, source).map_err(unknown)?;
    let digest = ran
        .binding
        .ok_or_else(|| unknown(format!("`{source}`'s attempt recorded no binding")))?;
    let a = crate::execution_identity::read_binding(run_dir, &digest)
        .ok_or_else(|| unknown(format!("the binding `{source}` ran on does not read")))?;
    let b = target;
    if super::cache::profile_work(&a) == super::cache::profile_work(b) {
        Ok(())
    } else {
        Err(format!(
            "`{source}` ran under profile {}, this node runs under {}",
            a.key(),
            b.key()
        ))
    }
}

/// The reason a fork that reported its source's own session id is never
/// continued.
pub(crate) const FORK_REPORTED_SOURCE: &str = "the fork reported the source's own session id, so it did not fork: the next attempt starts a fresh agent";

/// How long a help probe that could not read the help is kept before the
/// next fork of that binary probes again. A definite answer (the help read,
/// with or without the flag) is kept for the process: it is keyed by the
/// executable's fingerprint, so a replaced binary is probed again.
pub(crate) const FORK_HELP_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// One cached answer: the result and, for a help that did not read, when
/// it expires.
type ForkHelpAnswer = Option<(Result<(), String>, Option<std::time::Instant>)>;
/// The help checks of fork forms this process made, by agent id and the
/// executable fingerprint (`path:size:mtime_ms` of the canonical program),
/// each behind a lock of its own.
type ForkHelpCache = Mutex<HashMap<(String, String), std::sync::Arc<Mutex<ForkHelpAnswer>>>>;
static FORK_HELP: OnceLock<ForkHelpCache> = OnceLock::new();

/// Whether the installed CLI `program` of `agent` lists every flag of the
/// agent's fork form in its help, or the cold reason when it does not or its
/// help does not read. The help is probed at most once per process and
/// binary (one probe at a time per binary, none holding the others back);
/// a help that did not read is probed again after [`FORK_HELP_RETRY`].
/// Nothing is written to disk.
pub(crate) fn fork_help(agent: &str, program: &Path) -> Result<(), String> {
    let canonical = program
        .canonicalize()
        .unwrap_or_else(|_| program.to_path_buf());
    let key = (agent.to_string(), apb_core::detect::fingerprint(&canonical));
    let entry = {
        let cache = FORK_HELP.get_or_init(|| Mutex::new(HashMap::new()));
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.entry(key).or_default().clone()
    };
    let mut answer = entry.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((result, expires)) = answer.as_ref()
        && expires.is_none_or(|at| std::time::Instant::now() < at)
    {
        return result.clone();
    }
    let (result, transient) = probe_fork_help(agent, &canonical);
    let expires = transient.then(|| std::time::Instant::now() + FORK_HELP_RETRY);
    *answer = Some((result.clone(), expires));
    result
}

/// One probe of the help behind [`fork_help`], and whether its failure is
/// one a later probe may not repeat (the help did not read).
fn probe_fork_help(agent: &str, program: &Path) -> (Result<(), String>, bool) {
    let Some(module) = apb_core::agents::lookup(agent) else {
        return (Err(format!("agent `{agent}` cannot fork a session")), false);
    };
    let Some(argv) = module.forms().and_then(|f| f.fork_argv()) else {
        return (Err(format!("agent `{agent}` cannot fork a session")), false);
    };
    let id = module.descriptor().id;
    let args: Vec<String> = module
        .descriptor()
        .help_args
        .iter()
        .map(|a| a.to_string())
        .collect();
    let help = match apb_core::detect::probe_output(program, &args) {
        Ok(h) => h,
        Err(why) => {
            return (
                Err(format!("the help of {id} could not be read: {why}")),
                true,
            );
        }
    };
    let flags = apb_core::agents::check::argv_flags(argv);
    let missing = apb_core::agents::check::missing_flags(&flags, &help);
    if missing.is_empty() {
        (Ok(()), false)
    } else {
        (
            Err(format!(
                "the installed {id} CLI does not list {} in its help",
                missing.join(", ")
            )),
            false,
        )
    }
}

/// Whether any node of `playbook` continues or forks `node_id`'s session, so its
/// successful attempts must leave a session id behind (an opencode session is
/// otherwise looked up only when a retry needs it).
pub(crate) fn is_source(playbook: &Playbook, node_id: &str) -> bool {
    playbook.nodes.iter().any(|n| {
        matches!(&n.kind, NodeKind::AgentTask { continue_session: Some(s), .. } if s.from() == node_id)
    })
}

/// The line that opens a warm node's prompt: the session holds the earlier
/// step's work, and this is a new task in it.
pub(crate) const HANDOFF_PREAMBLE: &str = "Next step of the same run. Your earlier work in this session is context for it; the task for this step follows.";

#[cfg(test)]
mod fork_help_tests {
    use super::*;

    /// A probe that could not read the help is kept for
    /// [`FORK_HELP_RETRY`]; once that lifetime passed the next fork of the
    /// binary probes again, while a definite answer is kept for good.
    #[cfg(unix)]
    #[test]
    fn a_failed_probe_is_probed_again_after_its_lifetime() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("claude");
        std::fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' -p --output-format --model --resume --fork-session\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let key = (
            "claude".to_string(),
            apb_core::detect::fingerprint(&program.canonicalize().unwrap()),
        );
        let cache = FORK_HELP.get_or_init(|| Mutex::new(HashMap::new()));
        let entry = cache.lock().unwrap().entry(key).or_default().clone();
        // A read failure still in its lifetime is answered from the cache.
        *entry.lock().unwrap() = Some((
            Err("the help of claude could not be read: busy".into()),
            Some(std::time::Instant::now() + FORK_HELP_RETRY),
        ));
        assert!(fork_help("claude", &program).is_err());
        // Past its lifetime it is probed again, and the definite answer kept.
        *entry.lock().unwrap() = Some((
            Err("the help of claude could not be read: busy".into()),
            Some(std::time::Instant::now() - std::time::Duration::from_millis(1)),
        ));
        assert_eq!(fork_help("claude", &program), Ok(()));
        assert!(matches!(&*entry.lock().unwrap(), Some((Ok(()), None))));
    }
}
