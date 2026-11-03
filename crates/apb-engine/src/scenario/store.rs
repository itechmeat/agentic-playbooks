//! Where scenario runs live and what is kept of them.
//!
//! A scenario run is a run directory under `.apb/scenario-runs/<run id>/`
//! ([`super::RunsDir::Scenario`]), which no run surface and no history
//! reader looks at. Next to the runs, `.apb/scenario-runs/index/<playbook>.jsonl`
//! holds one [`IndexLine`] per finished scenario run, appended under the
//! index lock. Retention keeps the newest [`KEEP_RUNS`] run directories per
//! playbook, scenario and level (with the child runs they started), and at
//! most [`KEEP_LINES`] index lines per playbook, never dropping the line of
//! a kept run.
//!
//! [`execute_with`] is the entry of one scenario run inside the
//! `apb __scenario-run` process: it hands the request to the scenario drive
//! ([`Drive`]), builds the [`ScenarioReport`] from the run it drove, writes
//! it as `report.json` in the run directory and records the index line.
//! Everything it writes lies below the scenario write root the process
//! entered ([`enter`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use apb_core::scenario::ScenarioLevel;
use serde::{Deserialize, Serialize};

use super::RunsDir;
use super::report::{self, REPORT_FILE, ReportInput, ScenarioReport, Subject, Verdict};
use crate::event::EventPayload;

/// The results index directory under `.apb/scenario-runs/`.
pub const INDEX_DIR: &str = "index";
/// Run directories kept per playbook, scenario and level.
pub const KEEP_RUNS: usize = 20;
/// Index lines kept per playbook (the lines of kept runs always stay).
pub const KEEP_LINES: usize = 1000;

/// One finished scenario run in the results index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexLine {
    pub scenario: String,
    pub scenario_digest: String,
    pub level: ScenarioLevel,
    pub version: String,
    pub playbook_digest: String,
    pub verdict: Verdict,
    pub run_id: String,
    pub at_ms: u64,
    #[serde(default)]
    pub decision_requests: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl IndexLine {
    /// The line of a report that names a run; `None` for one that did not
    /// run.
    pub fn of(report: &ScenarioReport, at_ms: u64) -> Option<Self> {
        Some(IndexLine {
            scenario: report.scenario.clone(),
            scenario_digest: report.scenario_digest.clone(),
            level: report.level,
            version: report.version.clone(),
            playbook_digest: report.playbook_digest.clone(),
            verdict: report.verdict,
            run_id: report.run_id.clone()?,
            at_ms,
            decision_requests: report.decision_requests,
            cost_usd: report.cost_usd,
        })
    }
}

/// `.apb/scenario-runs` under the project root.
pub fn runs_root(root: &Path) -> PathBuf {
    RunsDir::Scenario.under(root)
}

fn invalid(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what)
}

/// The index file of `playbook`; refused for an id that is not a plain
/// path segment.
pub fn index_path(root: &Path, playbook: &str) -> io::Result<PathBuf> {
    if !apb_core::registry::is_safe_segment(playbook) {
        return Err(invalid(format!(
            "playbook id `{playbook}` is not a plain name"
        )));
    }
    Ok(runs_root(root)
        .join(INDEX_DIR)
        .join(format!("{playbook}.jsonl")))
}

/// Puts this process in scenario mode for `level`, with the project's
/// scenario runs directory as its only write root. At quick, no decision
/// request may leave the process either. Called once, first thing in
/// `apb __scenario-run`. A scenario runs directory that is a link is
/// refused: the write root never lies outside the project.
pub fn enter(level: ScenarioLevel, root: &Path) -> io::Result<PathBuf> {
    apb_core::process_mode::guard()?;
    let dir = runs_root(root);
    // A link anywhere below the root, the directory itself included, before
    // anything is created through it and again once it exists.
    apb_core::fsutil::ensure_no_symlink_below(root, &dir)?;
    std::fs::create_dir_all(&dir)?;
    apb_core::fsutil::ensure_no_symlink_below(root, &dir)?;
    let write_root = dir.canonicalize()?;
    super::guard::enter(level, &write_root)?;
    Ok(write_root)
}

pub use super::bind::{Drive, DriveRequest};

/// The drive this build plays scenarios with: the engine's own drive
/// ([`crate::scheduler::run_scenario`]) behind [`super::bind`].
pub const DRIVE: Drive = super::bind::drive;

/// One scenario run as [`execute_with`] leaves it: the report, and whether
/// storing it (the report file, the index line) failed. A storage failure
/// never changes the verdict.
#[derive(Debug, Clone)]
pub struct Executed {
    pub report: ScenarioReport,
    pub stored: Result<(), String>,
}

/// [`execute_with`] with this build's [`DRIVE`].
pub fn execute(req: &DriveRequest) -> Executed {
    execute_with(req, DRIVE)
}

/// Plays one scenario: drives it, builds the report from the run, writes
/// `report.json` in the run directory and appends the index line (with
/// retention).
pub fn execute_with(req: &DriveRequest, drive: Drive) -> Executed {
    let subject = Subject::of(
        req.level,
        &req.scenario,
        &req.playbook_id,
        &req.version,
        &req.playbook_digest,
    );
    let outcome = match drive(req) {
        Ok(o) => o,
        Err(e) => {
            return Executed {
                report: ScenarioReport::not_run(
                    &subject,
                    Verdict::Incomplete,
                    format!("the scenario could not be driven: {e}"),
                ),
                stored: Ok(()),
            };
        }
    };
    let run_dir = outcome.run_dir.clone();
    let report = report::build(ReportInput {
        subject,
        scenario: &req.scenario,
        playbook: &req.playbook,
        drive: outcome,
    });
    let stored = write_report(&run_dir, &report)
        .and_then(
            |()| match IndexLine::of(&report, apb_core::clock::now_ms() as u64) {
                Some(line) => append(&req.root, &req.playbook_id, &line),
                None => Ok(()),
            },
        )
        .map_err(|e| e.to_string());
    Executed { report, stored }
}

/// Writes `report.json` into a scenario run directory (private, atomic).
pub fn write_report(run_dir: &Path, report: &ScenarioReport) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(report).map_err(io::Error::other)?;
    apb_core::fsutil::atomic_write_private(&run_dir.join(REPORT_FILE), &bytes)
}

/// The report of a scenario run directory, `None` when it has none or it
/// does not parse.
pub fn read_report(run_dir: &Path) -> Option<ScenarioReport> {
    let text = std::fs::read_to_string(run_dir.join(REPORT_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The index lines of `playbook`, in file order. A missing file is none; a
/// line that does not parse is skipped.
pub fn read_index(root: &Path, playbook: &str) -> Vec<IndexLine> {
    let Ok(path) = index_path(root, playbook) else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The newest line per scenario and level.
pub fn latest(root: &Path, playbook: &str) -> BTreeMap<(String, ScenarioLevel), IndexLine> {
    let mut out = BTreeMap::new();
    for line in read_index(root, playbook) {
        let key = (line.scenario.clone(), line.level);
        match out.get(&key) {
            Some(prev) if is_newer(prev, &line) => {}
            _ => {
                out.insert(key, line);
            }
        }
    }
    out
}

/// Whether `a` is newer than `b` (a later file position wins a tie).
fn is_newer(a: &IndexLine, b: &IndexLine) -> bool {
    a.at_ms > b.at_ms
}

/// Appends `line` to the index of `playbook` under the index lock, then
/// applies retention: the run directories past the newest [`KEEP_RUNS`] of
/// the line's scenario and level are removed, and the index keeps at most
/// [`KEEP_LINES`] lines plus the lines of every kept run.
pub fn append(root: &Path, playbook: &str, line: &IndexLine) -> io::Result<()> {
    let path = index_path(root, playbook)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let _lock = apb_core::fsutil::lock_dir(dir, &format!("{playbook}.lock"))?;
    let mut lines = read_index(root, playbook);
    lines.push(line.clone());
    let (keep, drop) = retain(&lines, &line.scenario, line.level);
    for run in &drop {
        remove_run(root, run)?;
    }
    let text: String = keep
        .iter()
        .filter_map(|l| serde_json::to_string(l).ok())
        .map(|l| l + "\n")
        .collect();
    apb_core::fsutil::atomic_write_private(&path, text.as_bytes())
}

/// Which lines stay and which run directories go, after a line of
/// `scenario` at `level` was added.
fn retain(
    lines: &[IndexLine],
    scenario: &str,
    level: ScenarioLevel,
) -> (Vec<IndexLine>, Vec<String>) {
    // Newest first per (scenario, level); a later file position wins a tie.
    let mut groups: BTreeMap<(&str, ScenarioLevel), Vec<usize>> = BTreeMap::new();
    for (i, l) in lines.iter().enumerate() {
        groups
            .entry((l.scenario.as_str(), l.level))
            .or_default()
            .push(i);
    }
    let mut kept_runs: BTreeSet<usize> = BTreeSet::new();
    let mut drop = Vec::new();
    for ((s, lv), mut idx) in groups {
        idx.sort_by(|a, b| (lines[*b].at_ms, *b).cmp(&(lines[*a].at_ms, *a)));
        kept_runs.extend(idx.iter().take(KEEP_RUNS).copied());
        if s == scenario && lv == level {
            drop.extend(idx.iter().skip(KEEP_RUNS).map(|i| lines[*i].run_id.clone()));
        }
    }
    // Every kept run's line, then the newest others up to the cap.
    let mut others: Vec<usize> = (0..lines.len())
        .filter(|i| !kept_runs.contains(i))
        .collect();
    others.sort_by(|a, b| (lines[*b].at_ms, *b).cmp(&(lines[*a].at_ms, *a)));
    let room = KEEP_LINES.saturating_sub(kept_runs.len());
    let mut keep_idx: BTreeSet<usize> = kept_runs;
    keep_idx.extend(others.into_iter().take(room));
    (
        keep_idx.into_iter().map(|i| lines[i].clone()).collect(),
        drop,
    )
}

/// Removes the scenario run `run_id` and every child run it started (and
/// theirs), all of which live next to it. An id that is not a plain name,
/// a link and a missing directory are left alone.
fn remove_run(root: &Path, run_id: &str) -> io::Result<()> {
    let base = runs_root(root);
    let mut todo = vec![run_id.to_string()];
    let mut seen = BTreeSet::new();
    while let Some(id) = todo.pop() {
        if !seen.insert(id.clone()) || !apb_core::registry::is_safe_segment(&id) || id == INDEX_DIR
        {
            continue;
        }
        let dir = base.join(&id);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            _ => continue,
        }
        let events = crate::event::read_all_lossy_tail(&dir).unwrap_or_default();
        todo.extend(events.into_iter().filter_map(|e| match e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id),
            _ => None,
        }));
        apb_core::process_mode::guard_write(&dir)?;
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::report::DriveOutcome;
    use crate::scenario::report::tests::{
        GREEN_EXPECT, PLAYBOOK, green_answers, green_journal, scenario, write_run,
    };
    use apb_core::schema::Playbook;
    use serde_json::json;

    fn request(root: &Path, body: &str) -> DriveRequest {
        DriveRequest {
            root: root.to_path_buf(),
            playbook_id: "loop".into(),
            version: "1.0.0".into(),
            playbook_digest: "sha256:pb".into(),
            playbook: Playbook::from_yaml(PLAYBOOK).unwrap(),
            scenario: scenario(body),
            level: ScenarioLevel::Quick,
            max_decision_requests: None,
        }
    }

    /// Stands in for the drive: leaves the green journal in a scenario run.
    fn fake_drive(req: &DriveRequest) -> Result<DriveOutcome, String> {
        let run_dir = runs_root(&req.root).join("r1");
        write_run(&run_dir, &green_journal());
        Ok(DriveOutcome {
            run_id: "r1".into(),
            run_dir,
            answers: green_answers(),
            ..DriveOutcome::default()
        })
    }

    fn failing_drive(_: &DriveRequest) -> Result<DriveOutcome, String> {
        Err("prepare refused".into())
    }

    /// A played scenario leaves its report in the run directory and one
    /// line in the playbook's index; a scenario the drive could not play
    /// is `incomplete` with the reason and leaves neither.
    #[test]
    fn a_played_scenario_leaves_its_report_and_one_index_line() {
        let root = tempfile::tempdir().unwrap();
        let done = execute_with(&request(root.path(), GREEN_EXPECT), fake_drive);
        assert_eq!(done.stored, Ok(()));
        assert_eq!(
            done.report.verdict,
            Verdict::Passed,
            "{:#?}",
            done.report.checks
        );
        let run_dir = runs_root(root.path()).join("r1");
        assert_eq!(read_report(&run_dir), Some(done.report.clone()));
        let lines = read_index(root.path(), "loop");
        assert_eq!(lines.len(), 1);
        assert_eq!(
            (lines[0].run_id.as_str(), lines[0].verdict, lines[0].level),
            ("r1", Verdict::Passed, ScenarioLevel::Quick)
        );
        assert_eq!(lines[0].scenario_digest, done.report.scenario_digest);

        let other = tempfile::tempdir().unwrap();
        let done = execute_with(&request(other.path(), GREEN_EXPECT), failing_drive);
        assert_eq!(done.report.verdict, Verdict::Incomplete);
        assert!(done.report.reason.unwrap().contains("prepare refused"));
        assert!(read_index(other.path(), "loop").is_empty());
    }

    fn line(scenario: &str, level: ScenarioLevel, run_id: &str, at_ms: u64) -> IndexLine {
        IndexLine {
            scenario: scenario.into(),
            scenario_digest: "sha256:s".into(),
            level,
            version: "1.0.0".into(),
            playbook_digest: "sha256:pb".into(),
            verdict: Verdict::Passed,
            run_id: run_id.into(),
            at_ms,
            decision_requests: 0,
            cost_usd: None,
        }
    }

    /// Retention keeps the newest twenty runs of a scenario at a level and
    /// removes the older ones with the child runs they started; another
    /// level's runs stay, and an index line naming a path outside the
    /// scenario runs directory removes nothing.
    #[test]
    fn retention_keeps_twenty_runs_per_scenario_and_level_with_their_children() {
        let root = tempfile::tempdir().unwrap();
        let base = runs_root(root.path());
        let outside = root.path().join(".apb/outside");
        std::fs::create_dir_all(&outside).unwrap();
        let mut text = serde_json::to_string(&line("s", ScenarioLevel::Quick, "../outside", 0))
            .unwrap()
            + "\n";
        for i in 1..=21u64 {
            let id = format!("q{i:02}");
            let mut journal = vec![
                json!({"seq": 1, "ts": 1, "type": "run_started", "playbook": "loop", "version": "1.0.0"}),
            ];
            if i == 1 {
                journal.push(json!({"seq": 2, "ts": 2, "type": "child_run_started", "node_id": "c", "run_id": "q01-child"}));
                write_run(&base.join("q01-child"), &journal[..1]);
            }
            write_run(&base.join(&id), &journal);
            text.push_str(
                &(serde_json::to_string(&line("s", ScenarioLevel::Quick, &id, i)).unwrap() + "\n"),
            );
        }
        write_run(&base.join("smoke1"), &[]);
        text.push_str(
            &(serde_json::to_string(&line("s", ScenarioLevel::Smoke, "smoke1", 0)).unwrap() + "\n"),
        );
        let path = index_path(root.path(), "loop").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        write_run(&base.join("q22"), &[]);

        append(
            root.path(),
            "loop",
            &line("s", ScenarioLevel::Quick, "q22", 22),
        )
        .unwrap();

        for gone in ["q01", "q01-child", "q02"] {
            assert!(!base.join(gone).exists(), "{gone} kept");
        }
        for kept in ["q03", "q22", "smoke1"] {
            assert!(base.join(kept).exists(), "{kept} removed");
        }
        assert!(outside.exists(), "a line named a path outside the runs");
        let latest = latest(root.path(), "loop");
        assert_eq!(
            latest[&("s".to_string(), ScenarioLevel::Quick)].run_id,
            "q22"
        );
        assert_eq!(
            latest[&("s".to_string(), ScenarioLevel::Smoke)].run_id,
            "smoke1"
        );
        assert!(index_path(root.path(), "../loop").is_err());
    }

    /// The index keeps at most a thousand lines per playbook, the newest,
    /// and never the line of a kept run.
    #[test]
    fn the_index_keeps_the_newest_thousand_lines() {
        let root = tempfile::tempdir().unwrap();
        let path = index_path(root.path(), "loop").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = (0..KEEP_LINES as u64 + 30)
            .map(|i| {
                serde_json::to_string(&line("s", ScenarioLevel::Quick, &format!("r{i}"), i))
                    .unwrap()
                    + "\n"
            })
            .collect();
        std::fs::write(&path, text).unwrap();
        let older = line("t", ScenarioLevel::Quick, "t-old", 0);
        append(root.path(), "loop", &older).unwrap();
        let lines = read_index(root.path(), "loop");
        assert_eq!(lines.len(), KEEP_LINES);
        assert!(
            lines.iter().any(|l| l.run_id == "t-old"),
            "a kept run's line was dropped"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.run_id == format!("r{}", KEEP_LINES + 29))
        );
        assert!(!lines.iter().any(|l| l.run_id == "r0"));
    }

    /// Re-runs this test binary for the test `path`, the child doing the
    /// body: entering scenario mode cannot be undone in a process.
    fn in_child(path: &str, body: impl FnOnce()) {
        const ENV: &str = "APB_TEST_SCENARIO_ENTER_CHILD";
        if std::env::var(ENV).as_deref() == Ok(path) {
            body();
            return;
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([path, "--exact", "--nocapture", "--test-threads=1"])
            .env(ENV, path)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "child failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn ask(base_url: &str) -> Result<apb_decide::DecisionResponse, apb_decide::DecideError> {
        use apb_decide::DecisionProvider;
        let provider =
            apb_decide::SystemOne::new("p", base_url, "m", None, std::time::Duration::from_secs(2));
        let mut questions = std::collections::BTreeMap::new();
        questions.insert(
            "ok".to_string(),
            apb_decide::Question::Noul {
                instructions: json!("is it ok"),
                criteria: None,
            },
        );
        provider.decide(&apb_decide::DecisionRequest {
            use_site: apb_decide::UseSite::JudgeNode,
            state: json!({}),
            state_order: Vec::new(),
            questions,
        })
    }

    /// A quick process writes only below the project's scenario runs and
    /// sends no decision request at all.
    #[test]
    fn entering_quick_confines_writes_and_forbids_decision_requests() {
        in_child(
            "scenario::store::tests::entering_quick_confines_writes_and_forbids_decision_requests",
            || {
                let root = tempfile::tempdir().unwrap();
                let root = root.path().canonicalize().unwrap();
                let stub = apb_decide::testing::StubServer::start(Vec::new());
                let write_root = enter(ScenarioLevel::Quick, &root).unwrap();
                assert_eq!(write_root, root.join(".apb/scenario-runs"));
                assert!(
                    apb_core::process_mode::guard_write(&write_root.join("r1/events.jsonl"))
                        .is_ok()
                );
                assert!(
                    apb_core::process_mode::guard_write(&root.join(".apb/runs/r1/events.jsonl"))
                        .is_err()
                );
                assert!(ask(&stub.base_url).is_err());
                assert_eq!(stub.count(), 0, "a quick process sent a decision request");
            },
        );
    }

    /// A smoke process may ask this machine's providers.
    #[test]
    fn entering_smoke_still_lets_decision_requests_out() {
        in_child(
            "scenario::store::tests::entering_smoke_still_lets_decision_requests_out",
            || {
                let root = tempfile::tempdir().unwrap();
                let stub = apb_decide::testing::StubServer::start(Vec::new());
                enter(ScenarioLevel::Smoke, &root.path().canonicalize().unwrap()).unwrap();
                let _ = ask(&stub.base_url);
                // The stub answers 500, which the client retries.
                assert!(stub.count() >= 1, "a smoke process sent no request");
            },
        );
    }

    /// A scenario runs directory that is a link is refused before the
    /// process enters the mode: the write root never lies elsewhere.
    #[cfg(unix)]
    #[test]
    fn a_linked_scenario_runs_directory_is_refused() {
        in_child(
            "scenario::store::tests::a_linked_scenario_runs_directory_is_refused",
            || {
                let root = tempfile::tempdir().unwrap();
                let elsewhere = tempfile::tempdir().unwrap();
                std::fs::create_dir_all(root.path().join(".apb")).unwrap();
                std::os::unix::fs::symlink(
                    elsewhere.path(),
                    root.path().join(".apb/scenario-runs"),
                )
                .unwrap();
                assert!(enter(ScenarioLevel::Quick, &root.path().canonicalize().unwrap()).is_err());
                assert!(apb_core::process_mode::scenario().is_none());
                // A linked `.apb`: nothing is created where it points.
                let other = tempfile::tempdir().unwrap();
                std::fs::create_dir_all(other.path().join("p")).unwrap();
                std::os::unix::fs::symlink(elsewhere.path(), other.path().join("p/.apb")).unwrap();
                let project = other.path().join("p").canonicalize().unwrap();
                assert!(enter(ScenarioLevel::Quick, &project).is_err());
                assert!(!elsewhere.path().join("scenario-runs").exists());
                assert!(apb_core::process_mode::scenario().is_none());
            },
        );
    }
}
