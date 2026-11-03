//! Scenario runs on every surface: the request and answer shapes MCP and
//! the HTTP API share, over the engine's results index
//! (`apb_engine::scenario::store::latest`) and a run's stored report
//! (`apb_engine::scenario::store::read_report`), and the one way a scenario
//! is played, which `apb eval --level quick|smoke` uses too.
//!
//! - MCP `playbook_scenarios { id }` and `GET /api/playbooks/{id}/scenarios`
//!   answer [`PlaybookScenarios`] ([`scenarios`]);
//! - MCP `playbook_scenario_run { id, ... }` and `POST
//!   /api/playbooks/{id}/scenario-runs` take a [`ScenarioRunRequest`] and
//!   answer a [`ScenarioRunView`] ([`run_scenario`]);
//! - `GET /api/playbooks/{id}/scenario-runs/{run}` answers a
//!   [`ScenarioRunView`] ([`scenario_run_view`]).
//!
//! A scenario is never played in the process that asks: a child `apb
//! __scenario-run` plays it in scenario mode, and [`play`] waits for it with
//! a timeout (quick 60 s, smoke 300 s) and kills its process group when the
//! timeout comes first (verdict `incomplete`). Nothing here reads
//! `.apb/runs`.
//!
//! Who may play what: a quick run sends nothing and writes only below
//! `.apb/scenario-runs/`, so anyone may start one. A smoke run spends
//! decision requests: it needs `max_decision_requests` named (1 to 200),
//! an approved version (and an approved import), and it is refused from a
//! run step and from a dashboard without keys.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use apb_core::registry::{Registry, is_safe_segment};
use apb_core::scenario::{LoadedScenario, ScenarioLevel};
use apb_core::step_env::StepCaller;
use apb_core::validate::{IssueView, Severity};
use apb_engine::candidate::summary::ScenarioResultRef;
use apb_engine::run_outcome::RunGraph;
use apb_engine::scenario::report::{ScenarioReport, Subject, Verdict};
use apb_engine::scenario::store::{self, IndexLine};
use serde::{Deserialize, Serialize};

use super::{ToolError, outside_run};

/// Playing one scenario of a playbook.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioRunRequest {
    /// The scenario's name (its file stem).
    pub scenario: String,
    pub level: ScenarioLevel,
    /// The playbook version; `current` when absent.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub version: Option<String>,
    /// The decision request cap of a smoke run (required there, refused at
    /// quick).
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub max_decision_requests: Option<u32>,
}

/// One scenario of the version, with its latest result per level.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScenarioEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    /// The scenario file's digest now.
    pub digest: String,
    /// Why the scenario does not fit the version now (its `versions` or its
    /// digest pin); absent when it fits.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub stale: Option<String>,
    /// The newest result at each level that has one, quick first.
    pub latest: Vec<ScenarioResultRef>,
}

impl ScenarioEntry {
    /// `scenario` of the version whose trust digest is `playbook_digest`,
    /// with its newest results from `latest` (the results index by
    /// scenario and level), each marked fresh or not.
    pub fn of(
        scenario: &LoadedScenario,
        version: &str,
        playbook_digest: &str,
        latest: &BTreeMap<(String, ScenarioLevel), IndexLine>,
    ) -> Self {
        let results = [ScenarioLevel::Quick, ScenarioLevel::Smoke]
            .into_iter()
            .filter_map(|level| latest.get(&(scenario.name.clone(), level)))
            .map(|line| result_ref(line, playbook_digest, &scenario.digest))
            .collect();
        ScenarioEntry {
            name: scenario.name.clone(),
            title: scenario.scenario.title.clone(),
            digest: scenario.digest.clone(),
            stale: apb_core::scenario::staleness(&scenario.scenario, version, playbook_digest),
            latest: results,
        }
    }
}

pub use apb_engine::candidate::summary::result_ref;

/// The scenarios of one playbook version.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaybookScenarios {
    pub id: String,
    pub version: String,
    /// The version's trust digest: a result is fresh only for this one.
    pub playbook_digest: String,
    pub scenarios: Vec<ScenarioEntry>,
    /// One issue per scenario file that did not load (V146).
    pub issues: Vec<IssueView>,
}

/// One stored scenario run: its report and the run graph of the scenario
/// run directory, drawn like any run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScenarioRunView {
    pub report: ScenarioReport,
    /// Absent when the run directory holds no journal to draw.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub graph: Option<RunGraph>,
}

/// The smoke cap when the CLI names none.
pub const DEFAULT_MAX_DECISION_REQUESTS: u32 = 20;
/// The highest smoke cap.
pub const MAX_DECISION_REQUESTS: u32 = 200;
/// How long a quick scenario run may take.
pub const QUICK_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a smoke scenario run may take.
pub const SMOKE_TIMEOUT: Duration = Duration::from_secs(300);
/// How often the wait looks at the child.
const POLL: Duration = Duration::from_millis(50);
/// How long the pipes are read after the child's group is gone.
const PIPE_GRACE: Duration = Duration::from_secs(2);
/// The tail of a child's error output kept in an `incomplete` reason.
const STDERR_TAIL: usize = 2000;

/// The timeout of a scenario run at `level`.
pub fn timeout(level: ScenarioLevel) -> Duration {
    match level {
        ScenarioLevel::Quick => QUICK_TIMEOUT,
        ScenarioLevel::Smoke => SMOKE_TIMEOUT,
    }
}

/// One version of a playbook, as both sides of a scenario run read it.
pub struct ScenarioVersion {
    pub playbook: apb_core::schema::Playbook,
    pub version: String,
    /// The version's trust digest.
    pub digest: String,
    /// `.apb/playbooks/<id>`.
    pub dir: PathBuf,
    /// The validator's errors for the version: a run of it is refused, so
    /// a scenario of it is refused before any run too.
    pub invalid: Vec<String>,
}

/// Project playbook `id` at `version` (`current` when absent). A refusal is
/// `(code, message)`.
pub fn load_version(
    root: &Path,
    id: &str,
    version: Option<&str>,
) -> Result<ScenarioVersion, (String, String)> {
    let reg =
        Registry::open(root).map_err(|e| ("no_project".into(), format!("no project here: {e}")))?;
    let loaded = reg
        .load(id, version)
        .map_err(|e| ("not_found".into(), e.to_string()))?;
    let digest = loaded
        .trust_digest()
        .map_err(|e| ("no_digest".into(), e.to_string()))?;
    let ctx = apb_core::validate::ValidationContext::for_registry(
        &reg,
        apb_core::profile_store::PlaybookOrigin::Project,
    );
    let invalid = apb_core::validate::validate(&loaded.playbook, &ctx)
        .issues
        .into_iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| format!("{} {}", i.code, i.message))
        .collect();
    Ok(ScenarioVersion {
        playbook: loaded.playbook,
        version: loaded.version,
        digest,
        dir: root.join(".apb/playbooks").join(id),
        invalid,
    })
}

/// The errors of scenario `s` against `playbook`, one line each.
pub fn scenario_errors(s: &LoadedScenario, playbook: &apb_core::schema::Playbook) -> Vec<String> {
    apb_core::scenario::validate_scenario(&s.name, &s.scenario, playbook)
        .into_iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| format!("{} {}: {}", s.name, i.code, i.message))
        .collect()
}

/// The trust a smoke run needs: the version's digest approved, and an
/// imported version's scripts approved. A refusal is `(code, message)`.
pub fn smoke_trust(id: &str, v: &ScenarioVersion) -> Result<(), (String, String)> {
    if !apb_core::trust::TrustStore::load().is_approved(&v.digest) {
        return Err((
            "untrusted".into(),
            format!(
                "`{id}` {} is not approved (digest {}): a smoke run sends its text to decision providers, so approve the version first (`apb trust approve`)",
                v.version, v.digest
            ),
        ));
    }
    if let Err(refusal) = apb_engine::gate::check_import_approval(&v.dir, id, &v.version, &v.digest)
    {
        return Err((
            "imported_scripts_require_approval".into(),
            refusal["detail"].as_str().unwrap_or_default().to_string(),
        ));
    }
    Ok(())
}

/// What one `apb __scenario-run` child is asked to play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioChild {
    pub root: PathBuf,
    pub playbook: String,
    pub version: String,
    pub scenario: String,
    pub level: ScenarioLevel,
    pub max_decision_requests: Option<u32>,
    /// The digests the parent checked: anything else is not played.
    pub scenario_digest: String,
    pub playbook_digest: String,
}

impl ScenarioChild {
    /// The child for scenario `s` of version `v` of playbook `id`.
    pub fn of(
        root: &Path,
        id: &str,
        v: &ScenarioVersion,
        s: &LoadedScenario,
        level: ScenarioLevel,
        max_decision_requests: Option<u32>,
    ) -> Self {
        ScenarioChild {
            root: root.to_path_buf(),
            playbook: id.to_string(),
            version: v.version.clone(),
            scenario: s.name.clone(),
            level,
            max_decision_requests,
            scenario_digest: s.digest.clone(),
            playbook_digest: v.digest.clone(),
        }
    }

    /// The arguments of `apb __scenario-run`.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![
            "__scenario-run".to_string(),
            "--root".into(),
            self.root.display().to_string(),
            "--playbook".into(),
            self.playbook.clone(),
            "--version".into(),
            self.version.clone(),
            "--scenario".into(),
            self.scenario.clone(),
            "--level".into(),
            self.level.as_str().into(),
            "--scenario-digest".into(),
            self.scenario_digest.clone(),
            "--playbook-digest".into(),
            self.playbook_digest.clone(),
        ];
        if let Some(n) = self.max_decision_requests {
            argv.extend(["--max-decision-requests".into(), n.to_string()]);
        }
        argv
    }

    /// The subject of the report this child plays.
    pub fn subject(&self) -> Subject {
        Subject {
            level: self.level,
            scenario: self.scenario.clone(),
            scenario_digest: self.scenario_digest.clone(),
            playbook: self.playbook.clone(),
            version: self.version.clone(),
            playbook_digest: self.playbook_digest.clone(),
        }
    }
}

/// How a child ended.
enum Ended {
    Exited {
        ok: bool,
        stdout: String,
        stderr: String,
    },
    TimedOut,
    Interrupted,
    NotStarted(String),
}

/// Runs `cmd` in its own process group and waits up to `timeout`; on the
/// timeout or when `stop` says so the whole group is killed, and so is
/// whatever the child left behind once it ended. The pipes are read for at
/// most [`PIPE_GRACE`] after that, so a program outside the group holding
/// them can never keep the parent waiting.
fn run_bounded(cmd: &mut Command, timeout: Duration, stop: &dyn Fn() -> bool) -> Ended {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match apb_core::fsutil::spawn_when_not_busy(cmd) {
        Ok(c) => c,
        Err(e) => return Ended::NotStarted(e.to_string()),
    };
    let out = Drain::of(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = Drain::of(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let ended = loop {
        // The leader is not reaped before its group is killed: an exited
        // leader stays a zombie until then, which keeps its pid, and so the
        // group id, from going to another process.
        if apb_engine::proc::exited_unreaped(&mut child) {
            apb_engine::proc::kill_process_group(child.id());
            let ok = child.wait().is_ok_and(|s| s.success());
            let deadline = Instant::now() + PIPE_GRACE;
            return Ended::Exited {
                ok,
                stdout: out.read(deadline),
                stderr: err.read(deadline),
            };
        }
        if stop() {
            break Ended::Interrupted;
        }
        if started.elapsed() >= timeout {
            break Ended::TimedOut;
        }
        std::thread::sleep(POLL);
    };
    apb_engine::proc::kill_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
    let deadline = Instant::now() + PIPE_GRACE;
    let _ = (out.read(deadline), err.read(deadline));
    ended
}

/// A child's pipe read on a thread of its own. A process outside the
/// child's group may keep the pipe open: what was read by a deadline is
/// taken and the thread is left to end with the pipe.
struct Drain {
    read: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    done: std::sync::mpsc::Receiver<()>,
}

impl Drain {
    fn of(pipe: Option<Box<dyn Read + Send>>) -> Self {
        let read = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, done) = std::sync::mpsc::channel();
        let into = read.clone();
        std::thread::spawn(move || {
            if let Some(mut p) = pipe {
                let mut chunk = [0u8; 8192];
                while let Ok(n @ 1..) = p.read(&mut chunk) {
                    into.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend_from_slice(&chunk[..n]);
                }
            }
            let _ = tx.send(());
        });
        Drain { read, done }
    }

    /// What was read once the pipe closed, or by `deadline`.
    fn read(self, deadline: Instant) -> String {
        let _ = self
            .done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let bytes = self.read.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

fn tail(text: &str) -> String {
    let text = text.trim();
    let n = text.chars().count();
    text.chars().skip(n.saturating_sub(STDERR_TAIL)).collect()
}

/// Plays one scenario through the child `cmd` and reads the report it
/// prints; a child that does not end in time, is stopped, or ends without a
/// report is an `incomplete` report of `subject`.
pub fn play(
    cmd: &mut Command,
    timeout: Duration,
    subject: &Subject,
    stop: &dyn Fn() -> bool,
) -> ScenarioReport {
    let incomplete = |why: String| ScenarioReport::not_run(subject, Verdict::Incomplete, why);
    match run_bounded(cmd, timeout, stop) {
        Ended::Exited { stdout, stderr, ok } => match serde_json::from_str(stdout.trim()) {
            Ok(report) if about(&report, subject) => report,
            Ok(_) => {
                incomplete("the scenario run printed a report of another subject or run id".into())
            }
            Err(_) => incomplete(format!(
                "the scenario run ended {} without a report: {}",
                if ok { "cleanly" } else { "with an error" },
                tail(&stderr)
            )),
        },
        Ended::TimedOut => incomplete(format!(
            "the scenario run did not end within {} s and was stopped",
            timeout.as_secs()
        )),
        Ended::Interrupted => incomplete("interrupted".into()),
        Ended::NotStarted(e) => incomplete(format!("the scenario run could not start: {e}")),
    }
}

/// Whether `report` is about `subject` and names its run, if any, by a
/// plain name: a report of anything else is not this request's result.
fn about(report: &ScenarioReport, subject: &Subject) -> bool {
    report.level == subject.level
        && report.scenario == subject.scenario
        && report.scenario_digest == subject.scenario_digest
        && report.playbook == subject.playbook
        && report.version == subject.version
        && report.playbook_digest == subject.playbook_digest
        && report.run_id.as_deref().is_none_or(is_safe_segment)
}

/// `report` as a surface answers it: every string scrubbed with its
/// scenario run's redactor (the workspace's secrets, every value of the
/// run's tree, credential shapes), or the workspace's scrubber when it
/// names no run. When the run's secrets could not all be read, only its
/// identity and verdict are answered.
pub fn shown(root: &Path, report: ScenarioReport) -> ScenarioReport {
    let scrubbed = |scrubber: &apb_core::scrub::Scrubber, report: &ScenarioReport| {
        serde_json::to_value(report)
            .ok()
            .map(|v| scrubber.redact_value(v))
            .and_then(|v| serde_json::from_value::<ScenarioReport>(v).ok())
    };
    let subject = Subject {
        level: report.level,
        scenario: report.scenario.clone(),
        scenario_digest: report.scenario_digest.clone(),
        playbook: report.playbook.clone(),
        version: report.version.clone(),
        playbook_digest: report.playbook_digest.clone(),
    };
    let withheld = |run_id: Option<String>| ScenarioReport {
        run_id,
        ..ScenarioReport::not_run(
            &subject,
            report.verdict,
            apb_engine::run_redact::WITHHELD.to_string(),
        )
    };
    match report.run_id.clone().filter(|r| is_safe_segment(r)) {
        Some(run) => {
            let redactor = apb_engine::run_redact::RunRedactor::for_run(
                root,
                &store::runs_root(root).join(&run),
            );
            match redactor.scrubber() {
                Ok(scrubber) => scrubbed(scrubber, &report),
                Err(_) => None,
            }
            .unwrap_or_else(|| withheld(Some(run)))
        }
        None => scrubbed(
            &apb_core::scrub::Scrubber::for_workspace(root, Vec::new()),
            &report,
        )
        .unwrap_or_else(|| withheld(None)),
    }
}

/// A refusal before any run, as the surfaces answer it: a missing playbook
/// or scenario is not found, anything else is the request's fault.
fn refusal((code, message): (String, String)) -> ToolError {
    if code == "not_found" {
        ToolError::NotFound(message)
    } else {
        ToolError::Engine(format!("{code}: {message}"))
    }
}

fn safe(what: &str, name: &str) -> Result<(), ToolError> {
    if is_safe_segment(name) {
        Ok(())
    } else {
        Err(ToolError::NotFound(format!("{what} `{name}`")))
    }
}

/// The scenarios of project playbook `id` at `version` (`current` when
/// absent), each with its newest result per level, and one issue per file
/// that does not load.
pub fn scenarios(
    root: &Path,
    id: &str,
    version: Option<&str>,
) -> Result<PlaybookScenarios, ToolError> {
    safe("playbook", id)?;
    let v = load_version(root, id, version).map_err(refusal)?;
    let all = apb_core::scenario::load_all(&v.dir);
    let latest = store::latest(root, id);
    Ok(PlaybookScenarios {
        id: id.to_string(),
        version: v.version.clone(),
        playbook_digest: v.digest.clone(),
        scenarios: all
            .scenarios
            .iter()
            .map(|s| ScenarioEntry::of(s, &v.version, &v.digest, &latest))
            .collect(),
        issues: all.issues.iter().map(IssueView::from).collect(),
    })
}

/// Who asks for a scenario run.
#[derive(Debug, Clone, Copy)]
pub struct ScenarioCaller<'a> {
    pub step: &'a StepCaller,
    /// A dashboard serving without keys: anything on this machine reaches
    /// it, so it never starts a smoke run.
    pub keyless_dashboard: bool,
}

/// Plays scenario `req.scenario` of project playbook `id` once, through
/// the `apb` executable `exe`, and answers its report and graph. What is
/// refused before any run: a request cap at quick, a smoke run without a
/// cap, outside 1 to 200, from a run step, from a dashboard without keys or
/// of a version not approved; an invalid version; a scenario that does not
/// load or does not fit the version. A stale scenario is not played: its
/// report says `stale` and why.
pub fn run_scenario(
    root: &Path,
    id: &str,
    req: &ScenarioRunRequest,
    caller: ScenarioCaller,
    exe: &Path,
) -> Result<ScenarioRunView, ToolError> {
    safe("playbook", id)?;
    safe("scenario", &req.scenario)?;
    let cap = match (req.level, req.max_decision_requests) {
        (ScenarioLevel::Quick, None) => None,
        (ScenarioLevel::Quick, Some(_)) => {
            return Err(ToolError::Engine(
                "bad_cap: max_decision_requests is for a smoke run: a quick run sends no request"
                    .into(),
            ));
        }
        (ScenarioLevel::Smoke, None) => {
            return Err(ToolError::Engine(
                "needs_cap: a smoke run spends: name max_decision_requests (1 to 200)".into(),
            ));
        }
        (ScenarioLevel::Smoke, Some(n)) if n == 0 || n > MAX_DECISION_REQUESTS => {
            return Err(ToolError::Engine(format!(
                "bad_cap: max_decision_requests must be 1 to {MAX_DECISION_REQUESTS}"
            )));
        }
        (ScenarioLevel::Smoke, Some(n)) => {
            outside_run(caller.step, "a smoke scenario run")?;
            if caller.keyless_dashboard {
                return Err(ToolError::Conflict(
                    "a smoke scenario run spends decision requests and is refused by a dashboard without keys, since anything on this machine can reach it; run it at quick, use `apb eval --level smoke`, or issue a key with `apb server key issue` and sign in".into(),
                ));
            }
            Some(n)
        }
    };
    let v = load_version(root, id, req.version.as_deref()).map_err(refusal)?;
    if !v.invalid.is_empty() {
        return Err(ToolError::Engine(format!(
            "invalid_playbook: `{id}` {} is invalid: {}",
            v.version,
            v.invalid.join("; ")
        )));
    }
    let s = apb_core::scenario::load(&v.dir, &req.scenario).map_err(|e| {
        if apb_core::scenario::scenarios_dir(&v.dir)
            .join(format!("{}.yaml", req.scenario))
            .exists()
        {
            ToolError::Engine(format!("invalid_scenario: {e}"))
        } else {
            ToolError::NotFound(format!("scenario `{}`: {e}", req.scenario))
        }
    })?;
    let child = ScenarioChild::of(root, id, &v, &s, req.level, cap);
    if let Some(why) = apb_core::scenario::staleness(&s.scenario, &v.version, &v.digest) {
        let report = ScenarioReport::not_run(&child.subject(), Verdict::Stale, why);
        return Ok(ScenarioRunView {
            report: shown(root, report),
            graph: None,
        });
    }
    let problems = scenario_errors(&s, &v.playbook);
    if !problems.is_empty() {
        return Err(ToolError::Engine(format!(
            "invalid_scenario: {}",
            problems.join("; ")
        )));
    }
    if cap.is_some() {
        smoke_trust(id, &v).map_err(|(code, msg)| ToolError::Conflict(format!("{code}: {msg}")))?;
    }
    let mut cmd = Command::new(exe);
    cmd.args(child.argv());
    let report = play(&mut cmd, timeout(req.level), &child.subject(), &|| false);
    let graph = report
        .run_id
        .as_deref()
        .and_then(|run| graph_of(&store::runs_root(root).join(run), run));
    Ok(ScenarioRunView {
        report: shown(root, report),
        graph,
    })
}

/// The run graph of the scenario run directory `dir`, drawn like any run.
fn graph_of(dir: &Path, run: &str) -> Option<RunGraph> {
    let view = apb_engine::run_view::RunView::load(dir, run).ok()?;
    Some(view.graph(dir))
}

/// The stored scenario run `run` of playbook `id`: its report and graph.
/// A run of another playbook, or none, is not found.
pub fn scenario_run_view(root: &Path, id: &str, run: &str) -> Result<ScenarioRunView, ToolError> {
    safe("playbook", id)?;
    safe("scenario run", run)?;
    let dir = store::runs_root(root).join(run);
    let report = store::read_report(&dir)
        .filter(|r| r.playbook == id)
        .ok_or_else(|| ToolError::NotFound(format!("scenario run `{run}` of `{id}`")))?;
    let graph = graph_of(&dir, run);
    Ok(ScenarioRunView {
        report: shown(root, report),
        graph,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

    fn subject() -> Subject {
        Subject {
            level: ScenarioLevel::Quick,
            scenario: "green".into(),
            scenario_digest: "sha256:aa".into(),
            playbook: "p".into(),
            version: "1.0.0".into(),
            playbook_digest: "sha256:bb".into(),
        }
    }

    /// A stored report whose check observed a credential-shaped output, or
    /// a value the run declared secret, answers without either.
    #[test]
    fn a_stored_report_is_answered_redacted() {
        let root = tempfile::tempdir().unwrap();
        let dir = store::runs_root(root.path()).join("s-1");
        std::fs::create_dir_all(&dir).unwrap();
        apb_engine::run_config::write_run_config(
            &dir,
            &apb_engine::run_config::RunConfig {
                params: BTreeMap::from([("api_token".to_string(), "violet hedgehog".to_string())]),
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::write(
            dir.join("playbook.yaml"),
            "params:\n  - { name: api_token, secret: true }\n",
        )
        .unwrap();
        let mut report = ScenarioReport::not_run(&subject(), Verdict::Passed, "ran".into());
        report.run_id = Some("s-1".into());
        report
            .checks
            .push(apb_engine::scenario::report::ExpectationResult {
                check: "outputs[p1]".into(),
                passed: true,
                expected: "non_empty".into(),
                observed: format!("token {TOKEN} and violet hedgehog"),
            });
        store::write_report(&dir, &report).unwrap();
        let view = scenario_run_view(root.path(), "p", "s-1").unwrap();
        let all = serde_json::to_string(&view.report).unwrap();
        assert!(!all.contains(TOKEN), "{all}");
        assert!(!all.contains("violet hedgehog"), "{all}");
        assert_eq!(view.report.playbook_digest, "sha256:bb");
        assert_eq!(view.report.verdict, Verdict::Passed);
    }

    /// A process the child left outside its group that keeps the pipes
    /// open never keeps the caller waiting: the report the child printed
    /// is read and the wait ends within the drain's grace.
    #[cfg(unix)]
    #[test]
    fn a_leftover_holding_the_pipes_does_not_hold_the_wait() {
        let mut fine = ScenarioReport::not_run(&subject(), Verdict::Passed, "ran".into());
        fine.run_id = Some("s-1".into());
        let json = serde_json::to_string(&fine).unwrap();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "setsid sleep 15 & printf '%s' \"$1\"", "sh", &json]);
        let started = Instant::now();
        let got = play(&mut cmd, Duration::from_secs(30), &subject(), &|| false);
        assert_eq!(got.verdict, Verdict::Passed, "{got:?}");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{:?}",
            started.elapsed()
        );
    }

    /// A child's report about another playbook, scenario or digest, or
    /// with a run id that is not a plain name, is not this request's
    /// result: it reads `incomplete`.
    #[cfg(unix)]
    #[test]
    fn a_report_of_another_subject_is_incomplete() {
        let mut other = ScenarioReport::not_run(&subject(), Verdict::Passed, "ran".into());
        other.playbook_digest = "sha256:cc".into();
        let mut unsafe_id = ScenarioReport::not_run(&subject(), Verdict::Passed, "ran".into());
        unsafe_id.run_id = Some("../../x".into());
        let mut fine = ScenarioReport::not_run(&subject(), Verdict::Passed, "ran".into());
        fine.run_id = Some("s-1".into());
        for (report, verdict) in [
            (other, Verdict::Incomplete),
            (unsafe_id, Verdict::Incomplete),
            (fine, Verdict::Passed),
        ] {
            let json = serde_json::to_string(&report).unwrap();
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "printf '%s' \"$1\"", "sh", &json]);
            let got = play(&mut cmd, Duration::from_secs(10), &subject(), &|| false);
            assert_eq!(got.verdict, verdict, "{got:?}");
        }
    }
}
