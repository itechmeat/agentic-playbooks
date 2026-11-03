//! `apb eval <id> --level quick|smoke`: scenario runs, and `--scenarios`,
//! the list of a playbook's scenarios with their latest results.
//!
//! A scenario run never runs in this process. Each scenario is played by a
//! child, `apb __scenario-run`, which enters scenario mode before anything
//! else (it may write only below `.apb/scenario-runs/`, starts no program,
//! calls no connector, and at quick sends no decision request), drives the
//! run, stores its report and index line, and prints the report. This
//! process waits for it with a timeout (quick 60 s, smoke 300 s) and kills
//! its process group when the timeout or an interruption comes first, which
//! makes the verdict `incomplete`.
//!
//! What is checked before a child starts: the scenario names, every file's
//! own rules (V146) and its rules against the version (V147 to V150, both
//! exit 2), staleness (`stale`, nothing runs, unless `--allow-stale`), and
//! for smoke a person's go-ahead (a terminal question or `--yes`, never from
//! a run step), the decision request cap (default 20, at most 200) and the
//! version's trust (an approved digest, and an approved import). The child
//! is handed the scenario and version digests it was checked against and
//! refuses to play anything else.
//!
//! Exit codes: 0 every scenario passed, 1 one failed, 2 refused before any
//! run, 3 one was invalid, stale or incomplete (3 wins over 1).

use std::path::Path;
use std::process::{Command, ExitCode};

use apb_core::scenario::{LoadedScenario, ScenarioLevel};
use apb_engine::scenario::report::{ScenarioReport, Subject, Verdict};
use apb_engine::scenario::store::{self, DriveRequest};
pub(crate) use apb_mcp::tools::scenario::ScenarioChild as ChildArgs;
use apb_mcp::tools::scenario::{
    DEFAULT_MAX_DECISION_REQUESTS, MAX_DECISION_REQUESTS, ScenarioVersion as Version, load_version,
    play, scenario_errors as errors, shown, smoke_trust, timeout,
};
use serde_json::json;

use crate::util::print_json;

/// What `apb eval --level quick|smoke` was asked.
pub(crate) struct ScenarioArgs {
    pub id: String,
    pub version: Option<String>,
    pub level: ScenarioLevel,
    /// Only these scenarios (all when empty).
    pub scenarios: Vec<String>,
    pub max_decision_requests: Option<u32>,
    pub allow_stale: bool,
    pub yes: bool,
    pub json: bool,
}

fn refuse(json_out: bool, code: &str, message: String) -> ExitCode {
    if json_out {
        print_json(&json!({ "error": code, "message": message }));
    } else {
        eprintln!("eval: {message}");
    }
    ExitCode::from(2)
}

/// The scenarios asked for (every one when none is named). A name outside
/// the grammar, a file that does not load and a playbook with none are
/// refusals.
fn select(dir: &Path, names: &[String]) -> Result<Vec<LoadedScenario>, (String, String)> {
    if names.is_empty() {
        let all = apb_core::scenario::load_all(dir);
        if let Some(issue) = all.issues.first() {
            return Err((
                "invalid_scenario".into(),
                format!("{}: {}", issue.code, issue.message),
            ));
        }
        if all.scenarios.is_empty() {
            return Err((
                "no_scenarios".into(),
                format!(
                    "no scenario under {}",
                    apb_core::scenario::scenarios_dir(dir).display()
                ),
            ));
        }
        return Ok(all.scenarios);
    }
    names
        .iter()
        .map(|n| apb_core::scenario::load(dir, n).map_err(|e| ("invalid_scenario".into(), e)))
        .collect()
}

/// The checks only smoke needs: a person, a cap, a trusted version.
fn smoke_checks(args: &ScenarioArgs, v: &Version, count: usize) -> Result<u32, (String, String)> {
    if let Some(why) = apb_core::step_env::StepCaller::from_env().refusal("a smoke scenario run") {
        return Err(("refused_in_run_step".into(), why));
    }
    let cap = args
        .max_decision_requests
        .unwrap_or(DEFAULT_MAX_DECISION_REQUESTS);
    if cap == 0 || cap > MAX_DECISION_REQUESTS {
        return Err((
            "bad_cap".into(),
            format!("--max-decision-requests must be 1 to {MAX_DECISION_REQUESTS}"),
        ));
    }
    smoke_trust(&args.id, v)?;
    let question = format!(
        "A smoke run sends up to {cap} decision requests per scenario ({count} scenario(s)) to this machine's decision providers, at their cost. Continue?"
    );
    if !crate::eval::confirm(args.yes, &question) {
        return Err((
            "needs_confirmation".into(),
            "a smoke run spends: confirm at a terminal or pass --yes".into(),
        ));
    }
    Ok(cap)
}

/// `apb eval`: the scenario list, a scenario level, or the eval cases
/// (deep). A flag of another level is refused, never ignored.
pub(crate) fn eval_entry(root: &Path, args: crate::eval::EvalArgs) -> ExitCode {
    if args.list_scenarios {
        return list_cmd(root, &args.id, args.version.as_deref(), args.json);
    }
    if let Some(flag) = crate::eval::misplaced_flag(&args) {
        let level = args.level.scenario().map_or("deep", ScenarioLevel::as_str);
        return refuse(
            args.json,
            "bad_flag",
            format!("{flag} does not apply to --level {level}"),
        );
    }
    let Some(level) = args.level.scenario() else {
        return crate::eval::eval_cmd(root, args);
    };
    scenario_cmd(
        root,
        ScenarioArgs {
            id: args.id,
            version: args.version,
            level,
            scenarios: args.scenarios,
            max_decision_requests: args.max_decision_requests,
            allow_stale: args.allow_stale,
            yes: args.yes,
            json: args.json,
        },
    )
}

/// `apb eval <id> --level quick|smoke`.
pub(crate) fn scenario_cmd(root: &Path, args: ScenarioArgs) -> ExitCode {
    if args.level == ScenarioLevel::Quick && args.max_decision_requests.is_some() {
        return refuse(
            args.json,
            "bad_flag",
            "--max-decision-requests is for --level smoke: a quick run sends no request".into(),
        );
    }
    let v = match load_version(root, &args.id, args.version.as_deref()) {
        Ok(v) => v,
        Err((code, msg)) => return refuse(args.json, &code, msg),
    };
    if !v.invalid.is_empty() {
        return refuse(
            args.json,
            "invalid_playbook",
            format!(
                "`{}` {} is invalid: {}",
                args.id,
                v.version,
                v.invalid.join("; ")
            ),
        );
    }
    let selected = match select(&v.dir, &args.scenarios) {
        Ok(s) => s,
        Err((code, msg)) => return refuse(args.json, &code, msg),
    };
    let mut reports = Vec::new();
    let mut to_run = Vec::new();
    for s in selected {
        let stale = apb_core::scenario::staleness(&s.scenario, &v.version, &v.digest);
        match stale {
            Some(why) if !args.allow_stale => {
                let subject = Subject::of(args.level, &s, &args.id, &v.version, &v.digest);
                reports.push(ScenarioReport::not_run(&subject, Verdict::Stale, why));
            }
            _ => to_run.push(s),
        }
    }
    let problems: Vec<String> = to_run.iter().flat_map(|s| errors(s, &v.playbook)).collect();
    if !problems.is_empty() {
        if args.json {
            print_json(&json!({ "error": "invalid_scenario", "problems": problems }));
        } else {
            eprintln!(
                "eval: the scenarios do not fit `{}` {}:",
                args.id, v.version
            );
            for p in &problems {
                eprintln!("  {p}");
            }
        }
        return ExitCode::from(2);
    }
    let cap = match args.level {
        ScenarioLevel::Quick => None,
        ScenarioLevel::Smoke if to_run.is_empty() => None,
        ScenarioLevel::Smoke => match smoke_checks(&args, &v, to_run.len()) {
            Ok(cap) => Some(cap),
            Err((code, msg)) => return refuse(args.json, &code, msg),
        },
    };
    crate::interrupt::install();
    let exe = match apb_core::fsutil::reexec_exe() {
        Ok(e) => e,
        Err(e) => {
            return refuse(
                args.json,
                "no_exe",
                format!("cannot find the apb binary: {e}"),
            );
        }
    };
    for s in &to_run {
        let subject = Subject::of(args.level, s, &args.id, &v.version, &v.digest);
        let child = ChildArgs::of(root, &args.id, &v, s, args.level, cap);
        let mut cmd = Command::new(&exe);
        cmd.args(child.argv());
        reports.push(play(
            &mut cmd,
            timeout(args.level),
            &subject,
            &crate::interrupt::interrupted,
        ));
        if crate::interrupt::interrupted() {
            break;
        }
    }
    let reports: Vec<ScenarioReport> = reports.into_iter().map(|r| shown(root, r)).collect();
    print_reports(&args, &v.version, &reports);
    ExitCode::from(exit_code(&reports))
}

/// 3 when one was invalid, stale or incomplete, else 1 when one failed,
/// else 0.
fn exit_code(reports: &[ScenarioReport]) -> u8 {
    let any = |v: &[Verdict]| reports.iter().any(|r| v.contains(&r.verdict));
    if any(&[Verdict::Invalid, Verdict::Stale, Verdict::Incomplete]) {
        3
    } else if any(&[Verdict::Failed]) {
        1
    } else {
        0
    }
}

fn print_reports(args: &ScenarioArgs, version: &str, reports: &[ScenarioReport]) {
    if args.json {
        print_json(&json!({
            "id": args.id,
            "version": version,
            "level": args.level.as_str(),
            "reports": reports,
        }));
        return;
    }
    for r in reports {
        let run = r
            .run_id
            .as_deref()
            .map(|id| format!(", run {id}"))
            .unwrap_or_default();
        println!(
            "{} ({}): {}{run}",
            r.scenario,
            r.level.as_str(),
            r.verdict.as_str()
        );
        if let Some(why) = &r.reason {
            println!("  {why}");
        }
        for c in r.failed_checks() {
            println!(
                "  failed {}: expected {}, observed {}",
                c.check, c.expected, c.observed
            );
        }
        for a in &r.answers.missing {
            println!("  missing answer: {}", a.describe());
        }
        for a in &r.answers.extra {
            println!("  unused answer: {}", a.describe());
        }
        for t in &r.templates_empty {
            println!(
                "  empty template: {} visit {}: {} ({})",
                t.node, t.visit, t.reference, t.why
            );
        }
        if r.decision_requests > 0 {
            println!("  decision requests: {}", r.decision_requests);
        }
    }
}

/// `apb eval <id> --scenarios`: every scenario of the version with its
/// latest result per level, and whether it is stale now.
pub(crate) fn list_cmd(root: &Path, id: &str, version: Option<&str>, json_out: bool) -> ExitCode {
    let v = match load_version(root, id, version) {
        Ok(v) => v,
        Err((code, msg)) => return refuse(json_out, &code, msg),
    };
    let all = apb_core::scenario::load_all(&v.dir);
    let latest = store::latest(root, id);
    let rows: Vec<serde_json::Value> = all
        .scenarios
        .iter()
        .map(|s| {
            let last = |level: ScenarioLevel| {
                latest.get(&(s.name.clone(), level)).map(|l| {
                    json!({
                        "verdict": l.verdict,
                        "run_id": l.run_id,
                        "at_ms": l.at_ms,
                        "version": l.version,
                        "current": l.scenario_digest == s.digest && l.playbook_digest == v.digest,
                    })
                })
            };
            json!({
                "scenario": s.name,
                "title": s.scenario.title,
                "stale": apb_core::scenario::staleness(&s.scenario, &v.version, &v.digest),
                "quick": last(ScenarioLevel::Quick),
                "smoke": last(ScenarioLevel::Smoke),
            })
        })
        .collect();
    let issues: Vec<String> = all
        .issues
        .iter()
        .map(|i| format!("{}: {}", i.code, i.message))
        .collect();
    if json_out {
        print_json(&json!({ "id": id, "version": v.version, "scenarios": rows, "issues": issues }));
        return ExitCode::SUCCESS;
    }
    if rows.is_empty() && issues.is_empty() {
        println!("`{id}` has no scenarios");
    }
    for row in &rows {
        let cell = |level: &str| {
            row[level]
                .get("verdict")
                .and_then(|v| v.as_str())
                .map(|verdict| {
                    let older = if row[level]["current"] == json!(true) {
                        ""
                    } else {
                        " (older)"
                    };
                    format!("{verdict}{older}")
                })
                .unwrap_or_else(|| "never run".into())
        };
        let stale = row["stale"]
            .as_str()
            .map(|why| format!(", stale: {why}"))
            .unwrap_or_default();
        println!(
            "{}: quick {}, smoke {}{stale}",
            row["scenario"].as_str().unwrap_or_default(),
            cell("quick"),
            cell("smoke")
        );
    }
    for i in &issues {
        println!("invalid: {i}");
    }
    ExitCode::SUCCESS
}

/// The child: plays the scenario it was handed in scenario mode and prints
/// its report as JSON. Anything it cannot play is still a report.
pub(crate) fn child_cmd(a: ChildArgs) -> ExitCode {
    let root = match a.root.canonicalize() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("__scenario-run: {}: {e}", a.root.display());
            return ExitCode::from(2);
        }
    };
    if let Err(e) = store::enter(a.level, &root) {
        eprintln!("__scenario-run: cannot enter scenario mode: {e}");
        return ExitCode::from(2);
    }
    let report = match child_request(&root, &a) {
        Ok(req) => {
            let done = store::execute(&req);
            if let Err(e) = &done.stored {
                eprintln!("__scenario-run: the result was not stored: {e}");
            }
            done.report
        }
        Err(report) => *report,
    };
    match serde_json::to_string(&report) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("__scenario-run: {e}");
            ExitCode::from(2)
        }
    }
}

/// The request the child plays, after re-reading the version and the
/// scenario: a digest that moved since the parent checked them, or a
/// scenario that no longer fits, is `invalid` and nothing runs.
fn child_request(root: &Path, a: &ChildArgs) -> Result<DriveRequest, Box<ScenarioReport>> {
    let unread =
        |why: String| Box::new(ScenarioReport::not_run(&a.subject(), Verdict::Invalid, why));
    let v = load_version(root, &a.playbook, Some(&a.version)).map_err(|(_, m)| unread(m))?;
    let s = apb_core::scenario::load(&v.dir, &a.scenario).map_err(unread)?;
    if s.digest != a.scenario_digest || v.digest != a.playbook_digest {
        return Err(unread(
            "the scenario or the version changed after it was checked: run it again".into(),
        ));
    }
    let problems = errors(&s, &v.playbook);
    if !problems.is_empty() {
        return Err(unread(problems.join("; ")));
    }
    Ok(DriveRequest {
        root: root.to_path_buf(),
        playbook_id: a.playbook.clone(),
        version: v.version,
        playbook_digest: v.digest,
        playbook: v.playbook,
        scenario: s,
        level: a.level,
        max_decision_requests: a.max_decision_requests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const PLAYBOOK: &str = "schema: 2\nid: pb\nname: pb\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: work }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";

    const GREEN: &str = "schema: 1\nid: green\nanswers:\n  work:\n    - { visit: 1, attempts: [ { status: succeeded, output: ok } ] }\nexpect: { finish: done }\n";

    fn project(scenarios: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        apb_core::registry::init_project(root).unwrap();
        let pb = root.join(".apb/playbooks/pb");
        std::fs::create_dir_all(pb.join("1.0.0")).unwrap();
        std::fs::write(pb.join("1.0.0/playbook.yaml"), PLAYBOOK).unwrap();
        std::fs::write(pb.join("current"), "1.0.0").unwrap();
        let sc = apb_core::scenario::scenarios_dir(&pb);
        std::fs::create_dir_all(&sc).unwrap();
        for (name, body) in scenarios {
            std::fs::write(sc.join(format!("{name}.yaml")), body).unwrap();
        }
        dir
    }

    fn args(level: ScenarioLevel) -> ScenarioArgs {
        ScenarioArgs {
            id: "pb".into(),
            version: None,
            level,
            scenarios: Vec::new(),
            max_decision_requests: None,
            allow_stale: false,
            yes: false,
            json: true,
        }
    }

    fn subject() -> Subject {
        Subject {
            level: ScenarioLevel::Quick,
            scenario: "green".into(),
            scenario_digest: "sha256:s".into(),
            playbook: "pb".into(),
            version: "1.0.0".into(),
            playbook_digest: "sha256:pb".into(),
        }
    }

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    /// A scenario run that outlives its timeout is stopped with its whole
    /// process group (a program it left behind included) and reads
    /// `incomplete`; one that ends prints the report the parent returns;
    /// one that ends without a report is `incomplete` with its error.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_child_is_bounded_and_its_report_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild.pid");
        let started = Instant::now();
        let r = play(
            &mut sh(&format!("sleep 30 & echo $! > {}; wait", pidfile.display())),
            Duration::from_millis(500),
            &subject(),
            &|| false,
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(r.verdict, Verdict::Incomplete);
        assert!(r.reason.unwrap().contains("did not end within"));
        let pid = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .to_string();
        // Gone, or a zombie waiting to be reaped: no longer running.
        let running = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|s| s.rsplit_once(") ").map(|(_, rest)| rest.starts_with('Z')))
                .is_some_and(|zombie| !zombie)
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !running(),
            "the scenario run's own child survived the timeout"
        );

        let report = ScenarioReport::not_run(&subject(), Verdict::Passed, "x".into());
        let text = serde_json::to_string(&report)
            .unwrap()
            .replace('\'', "'\\''");
        let r = play(
            &mut sh(&format!("echo '{text}'")),
            Duration::from_secs(10),
            &subject(),
            &|| false,
        );
        assert_eq!(r, report);

        // A child that ends and leaves a program holding its pipes: the
        // group is killed, the pipes close, and the report is read at once.
        let started = Instant::now();
        let r = play(
            &mut sh(&format!("sleep 20 & echo '{text}'")),
            Duration::from_secs(60),
            &subject(),
            &|| false,
        );
        assert_eq!(r, report);
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "a program the child left behind kept the parent waiting"
        );

        let r = play(
            &mut sh("echo boom >&2; exit 2"),
            Duration::from_secs(10),
            &subject(),
            &|| false,
        );
        assert_eq!(r.verdict, Verdict::Incomplete);
        assert!(r.reason.unwrap().contains("boom"));
    }

    /// The exit code: 3 when one could not be trusted to have played
    /// (invalid, stale, incomplete), else 1 when one failed, else 0.
    #[test]
    fn the_exit_code_ranks_unplayed_over_failed() {
        let with = |vs: &[Verdict]| {
            let reports: Vec<ScenarioReport> = vs
                .iter()
                .map(|v| ScenarioReport::not_run(&subject(), *v, String::new()))
                .collect();
            exit_code(&reports)
        };
        assert_eq!(with(&[Verdict::Passed, Verdict::Passed]), 0);
        assert_eq!(with(&[Verdict::Passed, Verdict::Failed]), 1);
        assert_eq!(with(&[Verdict::Failed, Verdict::Incomplete]), 3);
        assert_eq!(with(&[Verdict::Stale]), 3);
        assert_eq!(with(&[Verdict::Invalid, Verdict::Passed]), 3);
    }

    /// What is refused before any scenario run starts: a scenario that does
    /// not fit the version, a stale one (reported, neither run nor held to
    /// the version), a name outside the grammar and a request
    /// cap at quick. None of them leaves a scenario run behind.
    #[test]
    fn refusals_come_before_any_run() {
        let bad = GREEN.replace("work:", "nowhere:");
        let p = project(&[("green", &bad)]);
        assert_eq!(
            scenario_cmd(p.path(), args(ScenarioLevel::Quick)),
            ExitCode::from(2)
        );

        // Stale, and written for another version's nodes: it is reported
        // stale and never played or held to this version.
        let stale = format!("versions: \">=2.0.0\"\n{bad}");
        let p2 = project(&[("green", &stale)]);
        assert_eq!(
            scenario_cmd(p2.path(), args(ScenarioLevel::Quick)),
            ExitCode::from(3)
        );

        let p3 = project(&[("green", GREEN)]);
        let capped = ScenarioArgs {
            max_decision_requests: Some(5),
            ..args(ScenarioLevel::Quick)
        };
        assert_eq!(scenario_cmd(p3.path(), capped), ExitCode::from(2));
        let named = ScenarioArgs {
            scenarios: vec!["../green".into()],
            ..args(ScenarioLevel::Quick)
        };
        assert_eq!(scenario_cmd(p3.path(), named), ExitCode::from(2));

        for p in [&p, &p2, &p3] {
            assert!(
                !store::runs_root(p.path()).exists(),
                "a refused scenario ran"
            );
        }
    }

    /// The child plays only what the parent checked: a scenario edited in
    /// between is `invalid` and nothing is driven.
    #[test]
    fn the_child_refuses_a_scenario_that_changed_after_the_check() {
        let p = project(&[("green", GREEN)]);
        let root = p.path();
        let v = load_version(root, "pb", None).unwrap();
        let s = apb_core::scenario::load(&v.dir, "green").unwrap();
        let child = ChildArgs {
            root: root.to_path_buf(),
            playbook: "pb".into(),
            version: v.version.clone(),
            scenario: "green".into(),
            level: ScenarioLevel::Quick,
            max_decision_requests: None,
            scenario_digest: s.digest.clone(),
            playbook_digest: v.digest.clone(),
        };
        let req = child_request(root, &child).unwrap();
        assert_eq!(req.scenario.digest, s.digest);

        std::fs::write(
            apb_core::scenario::scenarios_dir(&v.dir).join("green.yaml"),
            GREEN.replace("output: ok", "output: changed"),
        )
        .unwrap();
        let refused = *child_request(root, &child).unwrap_err();
        assert_eq!(refused.verdict, Verdict::Invalid);
        assert!(
            refused
                .reason
                .unwrap()
                .contains("changed after it was checked")
        );
    }
}
