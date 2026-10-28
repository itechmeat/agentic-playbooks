//! The publish step end to end, over a local bare repository reached by
//! `file://`: the plan, the consent bound to its nonce, the push, the
//! ledger, every drift refusal, the tree's config and a planted shadow never
//! acting, the operator's URL rewrites refused, and the two unknown twins (a
//! sleeping `pre-receive` and a sleeping `post-receive` in the bare
//! repository's own hooks), which are never pushed again.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::publish::url::PushUrl;
use apb_core::registry::init_project;
use apb_engine::RunResult;
use apb_engine::consent::RunConsent;
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventLog, EventPayload, read_all};
use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
use apb_engine::review::{ReviewCommand, post_decision, read_reviews_after};
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;

use crate::common::bare_remote::{Fixture, make_executable, remote_ref, url_of};
use crate::common::{self, EnvVar};

const DEADLINE: Duration = Duration::from_secs(30);
const STEP: Duration = Duration::from_millis(20);

/// Commits `work.txt` with the run's trailer.
const WORK: &str = r#"printf '%s' "$APB_RUN_ID" > work.txt
git add work.txt
git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false \
  commit -q -m "work of the run" -m "Apb-Run: $APB_RUN_ID"
"#;

/// The playbook: `start -> work -> {mid} -> publish -> done`.
fn playbook(mid: &[&str], publish_extra: &str) -> String {
    let mut nodes = vec![
        "  - { id: start, type: start }".to_string(),
        "  - { id: work, type: script, script: \"scripts/work.sh\", runner: sh }".to_string(),
    ];
    let mut chain = vec!["start", "work"];
    for (i, _) in mid.iter().enumerate() {
        nodes.push(format!(
            "  - {{ id: mid{i}, type: script, script: \"scripts/mid{i}.sh\", runner: sh }}"
        ));
    }
    let mids: Vec<String> = (0..mid.len()).map(|i| format!("mid{i}")).collect();
    chain.extend(mids.iter().map(String::as_str));
    chain.extend(["publish", "done"]);
    nodes.push(format!(
        "  - {{ id: publish, type: publish, from: work, remote: origin, branch: \"apb/out\"{publish_extra} }}"
    ));
    nodes.push("  - { id: done, type: finish, outcome: success }".to_string());
    let edges: Vec<String> = chain
        .windows(2)
        .map(|w| format!("  - {{ from: {}, to: {} }}", w[0], w[1]))
        .collect();
    format!(
        "schema: 2\nid: pub\nname: Publish\nversion: 1.0.0\nnodes:\n{}\nedges:\n{}\n",
        nodes.join("\n"),
        edges.join("\n")
    )
}

fn seed(f: &Fixture, work: &str, mid: &[&str], publish_extra: &str) {
    init_project(&f.root).unwrap();
    let dir = f.root.join(".apb/playbooks/pub/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), playbook(mid, publish_extra)).unwrap();
    fs::write(dir.join("scripts/work.sh"), work).unwrap();
    for (i, body) in mid.iter().enumerate() {
        fs::write(dir.join(format!("scripts/mid{i}.sh")), body).unwrap();
    }
    fs::write(f.root.join(".apb/playbooks/pub/current"), "1.0.0").unwrap();
}

/// The person's start consent: a publish node implies `irreversible`.
fn opts() -> RunOptions {
    RunOptions {
        consent: Some(RunConsent::irreversible("cli")),
        ..Default::default()
    }
}

fn start(root: &Path) -> mpsc::Receiver<RunResult> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let res = run(&root, "pub", None, opts()).unwrap();
        let _ = tx.send(res);
    });
    rx
}

fn finished(rx: &mpsc::Receiver<RunResult>) -> RunResult {
    match rx.recv_timeout(DEADLINE) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the run did not end within {DEADLINE:?}"),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the run thread panicked before reporting")
        }
    }
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(started.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(STEP);
    }
}

fn run_dir(root: &Path) -> PathBuf {
    poll("the run dir", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_dir())
    })
}

/// The plan's fields once it is journaled.
struct Planned {
    after: String,
    remote_before: Option<String>,
    commits: Vec<(String, bool)>,
    outside: bool,
    digest: String,
    nonce: String,
}

fn plans(events: &[Event]) -> Vec<Planned> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::PublishPlanned {
                after,
                remote_before,
                commits,
                outside_push_suspected,
                plan_digest,
                nonce,
                ..
            } => Some(Planned {
                after: after.clone(),
                remote_before: remote_before.clone(),
                commits: commits
                    .iter()
                    .map(|c| (c.commit.subject.clone(), c.this_run))
                    .collect(),
                outside: *outside_push_suspected,
                digest: plan_digest.clone(),
                nonce: nonce.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Waits until the run asked about its `n`-th plan.
fn asked(dir: &Path, n: usize) -> Planned {
    poll("a publish request", || {
        let events = read_all(dir).ok()?;
        let requests = events
            .iter()
            .filter(|e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "publish"))
            .count();
        (requests >= n).then(|| plans(&events).into_iter().nth(n - 1))?
    })
}

fn decide(dir: &Path, decision: &str, nonce: Option<&str>) -> Result<u64, EngineError> {
    post_decision(
        dir,
        ReviewCommand {
            node: "publish".into(),
            decision: decision.into(),
            note: String::new(),
        },
        nonce.map(str::to_string),
    )
}

fn count(events: &[Event], want: impl Fn(&EventPayload) -> bool) -> usize {
    events.iter().filter(|e| want(&e.payload)).count()
}

fn intents(events: &[Event]) -> usize {
    count(
        events,
        |p| matches!(p, EventPayload::SendIntentRecorded { node, .. } if node == "publish"),
    )
}

fn settlements(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SendIntentSettled {
                outcome, source, ..
            } => Some((outcome.clone(), source.clone())),
            _ => None,
        })
        .collect()
}

fn output(events: &[Event]) -> String {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "publish" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn approving_the_current_plan_pushes_exactly_it() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    // A local commit from before the run rides along and is flagged.
    f.commit("old.txt", "local before the run");
    seed(&f, WORK, &[], "");
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let plan = asked(&dir, 1);

    let subjects: Vec<(&str, bool)> = plan.commits.iter().map(|(s, t)| (s.as_str(), *t)).collect();
    assert_eq!(
        subjects,
        [("work of the run", true), ("local before the run", false)]
    );
    assert_eq!(plan.remote_before, None);
    assert!(!plan.outside);
    assert!(plan.nonce.starts_with("publish-"));
    let events = read_all(&dir).unwrap();
    let request = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewRequested {
                node,
                options,
                recommendation,
                plan_digest,
                instruction,
                ..
            } if node == "publish" => Some((
                options.clone(),
                recommendation.clone(),
                plan_digest.clone(),
                instruction.clone(),
            )),
            _ => None,
        })
        .unwrap();
    assert_eq!(request.0, ["publish", "skip"]);
    assert!(request.1.is_none(), "no recommendation is asked for");
    assert_eq!(request.2.as_deref(), Some(plan.digest.as_str()));
    assert!(request.3.contains(&plan.nonce), "{}", request.3);
    // Nothing is ever posted automatically.
    assert!(read_reviews_after(&dir, None).unwrap().is_empty());

    // A stale nonce and a missing one are refused; a run step is refused.
    let stale = decide(
        &dir,
        "publish",
        Some("publish-00000000000000000000000000000000"),
    );
    assert!(matches!(stale, Err(EngineError::Conflict(ref m)) if m.contains(&plan.nonce)));
    assert!(matches!(
        decide(&dir, "publish", None),
        Err(EngineError::Invalid(_))
    ));
    assert!(matches!(
        decide(&dir, "merge", Some(&plan.nonce)),
        Err(EngineError::Invalid(_))
    ));
    {
        let _step = EnvVar::set("APB_RUN_ID", "some-run");
        let refused = decide(&dir, "publish", Some(&plan.nonce));
        assert!(
            matches!(refused, Err(EngineError::Invalid(ref m)) if m.starts_with("publishing is refused")),
            "{refused:?}"
        );
    }
    assert!(read_reviews_after(&dir, None).unwrap().is_empty());

    decide(&dir, "publish", Some(&plan.nonce)).unwrap();
    // The same consent twice is a conflict, queued or already applied.
    assert!(matches!(
        decide(&dir, "publish", Some(&plan.nonce)),
        Err(EngineError::Conflict(_))
    ));
    assert_eq!(finished(&rx).outcome, RunStatus::Succeeded);

    let events = read_all(&dir).unwrap();
    assert_eq!(
        f.remote_ref("apb/out").as_deref(),
        Some(plan.after.as_str())
    );
    assert_eq!(intents(&events), 1);
    assert_eq!(
        settlements(&events),
        [("confirmed".to_string(), "response".to_string())]
    );
    assert_eq!(output(&events), "published");
    assert!(events.iter().any(|e| matches!(&e.payload,
        EventPayload::ReviewDecided { decision, plan_digest: Some(d), .. }
            if decision == "publish" && *d == plan.digest)));
    // The pin came before the first node acted.
    let pinned = events
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::PublishTargetPinned { url: Some(u), .. } if *u == f.url()))
        .expect("the target is pinned");
    let work_started = events
        .iter()
        .position(
            |e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "work"),
        )
        .unwrap();
    assert!(pinned < work_started);
}

#[test]
fn a_publishing_playbook_needs_the_start_consent() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    seed(&f, WORK, &[], "");
    let refused = run(&f.root, "pub", None, RunOptions::default());
    assert!(
        matches!(refused, Err(EngineError::Invalid(ref m)) if m.contains("node publish")),
        "{refused:?}"
    );
}

#[test]
fn nothing_to_publish_finishes_without_asking() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    seed(&f, "true\n", &[], "");
    let res = run(&f.root, "pub", None, opts()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert_eq!(output(&events), "nothing_to_publish");
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::ReviewRequested { .. }
        )),
        0
    );
    assert_eq!(f.remote_ref("apb/out"), None);
}

#[test]
fn a_remote_that_moved_after_the_plan_is_asked_about_again() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    seed(&f, WORK, &[], "");
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let first = asked(&dir, 1);
    // The remote branch appears (a fast-forward base) after the plan.
    let base = f.git(&f.root, &["rev-parse", "HEAD~1"]);
    f.git(
        &f.root,
        &[
            "push",
            "-q",
            "origin",
            &format!("{base}:refs/heads/apb/out"),
        ],
    );
    decide(&dir, "publish", Some(&first.nonce)).unwrap();
    let second = asked(&dir, 2);
    assert_ne!(second.nonce, first.nonce);
    assert_eq!(second.remote_before.as_deref(), Some(base.as_str()));
    assert_eq!(
        intents(&read_all(&dir).unwrap()),
        0,
        "no push without consent to the new plan"
    );
    assert_eq!(f.remote_ref("apb/out").as_deref(), Some(base.as_str()));
    // The old nonce no longer answers anything.
    assert!(matches!(
        decide(&dir, "publish", Some(&first.nonce)),
        Err(EngineError::Conflict(_))
    ));
    decide(&dir, "publish", Some(&second.nonce)).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Succeeded);
    assert_eq!(
        f.remote_ref("apb/out").as_deref(),
        Some(second.after.as_str())
    );
    assert_eq!(intents(&read_all(&dir).unwrap()), 1);
}

#[test]
fn a_node_that_changes_the_remote_url_is_refused() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    let other = f.other_bare("other.git");
    let set_url = format!("git remote set-url origin '{}'\n", url_of(&other));
    seed(&f, WORK, &[&set_url], "");
    let res = run(&f.root, "pub", None, opts()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(
        output(&events).starts_with("publish_refused: remote_changed"),
        "{}",
        output(&events)
    );
    assert_eq!(remote_ref(&other, "apb/out"), None);
    assert_eq!(f.remote_ref("apb/out"), None);
    assert_eq!(intents(&events), 0);
}

#[test]
fn a_later_commit_is_refused_as_head_moved() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    let later = "printf x > later.txt && git add later.txt && git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m later\n";
    seed(&f, WORK, &[later], "");
    let res = run(&f.root, "pub", None, opts()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(
        output(&events).starts_with("publish_refused: head_moved"),
        "{}",
        output(&events)
    );
}

#[test]
fn a_url_the_operators_config_rewrites_further_is_refused() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let other = f.other_bare("other.git");
    let mid = "file:///nowhere/mid.git";
    // The operator's own (isolated) config rewrites the remote to `mid`,
    // which `remote get-url` pins, and `mid` on to another repository,
    // which only the push itself would apply.
    fs::write(
        f.home.join(".gitconfig"),
        format!(
            "[url \"{mid}\"]\n\tinsteadOf = {}\n[url \"{}\"]\n\tinsteadOf = {mid}\n",
            f.url(),
            url_of(&other)
        ),
    )
    .unwrap();
    let _home = f.isolate();
    seed(&f, WORK, &[], "");
    let res = finished(&start(&f.root));
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(
        output(&events).starts_with("publish_refused: url_rewritten"),
        "{}",
        output(&events)
    );
    assert_eq!(remote_ref(&other, "apb/out"), None);
}

#[test]
fn an_operators_push_rewrite_is_refused() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let other = f.other_bare("other.git");
    // An explicit push URL is pinned as written (`remote get-url --push`
    // rewrites only a URL it derives), and `ls-remote --get-url` applies
    // `insteadOf` only; a push to the raw URL applies `pushInsteadOf` too,
    // so this rule would send the push to `other` while every listing
    // still named the pinned URL.
    f.git(&f.root, &["config", "remote.origin.pushurl", &f.url()]);
    fs::write(
        f.home.join(".gitconfig"),
        format!(
            "[url \"{}\"]\n\tpushInsteadOf = {}\n",
            url_of(&other),
            f.url()
        ),
    )
    .unwrap();
    let _home = f.isolate();
    seed(&f, WORK, &[], "");
    let res = finished(&start(&f.root));
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(
        output(&events).starts_with("publish_refused: url_rewritten"),
        "{}",
        output(&events)
    );
    assert_eq!(remote_ref(&other, "apb/out"), None);
    assert_eq!(f.remote_ref("apb/out"), None);
}

/// Approves the run's first plan and returns the finished run and plan.
fn approve(f: &Fixture) -> (RunResult, Planned) {
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let plan = asked(&dir, 1);
    decide(&dir, "publish", Some(&plan.nonce)).unwrap();
    (finished(&rx), plan)
}

#[test]
fn a_rewrite_the_tree_adds_mid_run_never_moves_the_push() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    let other = f.other_bare("other.git");
    // An explicit push URL is never rewritten by `remote get-url --push`,
    // so the pin and the plan read the pinned URL; a push from the tree's
    // own git directory would still apply the tree's `pushInsteadOf`.
    f.git(&f.root, &["config", "remote.origin.pushurl", &f.url()]);
    let rewrite = format!(
        "git config 'url.{}.pushInsteadOf' '{}'\n",
        url_of(&other),
        f.url()
    );
    seed(&f, WORK, &[&rewrite], "");
    let (res, plan) = approve(&f);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        f.remote_ref("apb/out").as_deref(),
        Some(plan.after.as_str())
    );
    assert_eq!(remote_ref(&other, "apb/out"), None);
}

#[test]
fn a_shadow_planted_ahead_of_the_push_is_never_used() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    let other = f.other_bare("other.git");
    // A node that runs outside the sandbox plants a shadow where an earlier
    // apb kept it, with a rule that would redirect the push.
    let plant = format!(
        "s=\".apb/runs/$APB_RUN_ID/publish/shadow.git\"\ngit init -q --bare \"$s\"\ngit --git-dir=\"$s\" config 'url.{}.pushInsteadOf' '{}'\n",
        url_of(&other),
        f.url()
    );
    seed(&f, WORK, &[&plant], "");
    let (res, plan) = approve(&f);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        f.remote_ref("apb/out").as_deref(),
        Some(plan.after.as_str())
    );
    assert_eq!(remote_ref(&other, "apb/out"), None);
    // Every shadow apb made is gone again; the planted one is untouched.
    let shadows: Vec<String> =
        fs::read_dir(f.root.join(".apb/runs").join(&res.run_id).join("publish"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
    assert_eq!(shadows, ["shadow.git"]);
}

#[test]
fn a_file_remote_inside_the_tree_is_refused_and_its_hooks_never_run() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    // A bare repository an agent could write, with a hook that would run
    // outside any sandbox when apb's push reached it.
    let nested = f.root.join("nested.git");
    f.git(&f.root, &["init", "-q", "--bare", "nested.git"]);
    let marker = f.root.parent().unwrap().join("hook-ran");
    let hook = nested.join("hooks/pre-receive");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    common::write_sync(&hook, &format!("#!/bin/sh\ntouch '{}'\n", marker.display()));
    make_executable(&hook);
    f.git(
        &f.root,
        &["config", "remote.origin.pushurl", &url_of(&nested)],
    );
    seed(&f, WORK, &[], "");
    let res = finished(&start(&f.root));
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(
        output(&events).starts_with("publish_refused: remote_writable"),
        "{}",
        output(&events)
    );
    assert!(!marker.exists(), "the remote's hook ran");
    assert_eq!(remote_ref(&nested, "apb/out"), None);
    assert_eq!(intents(&events), 0);
}

#[test]
fn a_push_under_a_stale_lease_is_not_sent() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    let base = f.git(&f.root, &["rev-parse", "HEAD"]);
    let next = f.commit("n.txt", "next");
    f.git(
        &f.root,
        &[
            "push",
            "-q",
            "origin",
            &format!("{base}:refs/heads/apb/out"),
        ],
    );
    let url = PushUrl::parse(&f.url()).unwrap();
    let common_dir = f.root.join(".git");
    let run_dir = f.root.parent().unwrap().join("run");
    fs::create_dir_all(&run_dir).unwrap();
    let remote =
        apb_engine::publish::push::Remote::open(&run_dir, &url, &common_dir, &[], None).unwrap();
    let stale = remote.push(
        "apb/out",
        &next,
        Some(&next),
        Duration::from_secs(30),
        &apb_core::scrub::Scrubber::default(),
    );
    assert!(
        matches!(stale, apb_core::publish::porcelain::Outcome::NotSent(_)),
        "{stale:?}"
    );
    // An empty lease refuses an existing ref too.
    let absent = remote.push(
        "apb/out",
        &next,
        None,
        Duration::from_secs(30),
        &apb_core::scrub::Scrubber::default(),
    );
    assert!(
        matches!(absent, apb_core::publish::porcelain::Outcome::NotSent(_)),
        "{absent:?}"
    );
    assert_eq!(f.remote_ref("apb/out").as_deref(), Some(base.as_str()));
    // The exact lease pushes.
    let ok = remote.push(
        "apb/out",
        &next,
        Some(&base),
        Duration::from_secs(30),
        &apb_core::scrub::Scrubber::default(),
    );
    assert_eq!(ok, apb_core::publish::porcelain::Outcome::Confirmed);
    assert_eq!(f.remote_ref("apb/out").as_deref(), Some(next.as_str()));
}

/// Runs to a consent, approves it, and returns the run once it ended.
fn approve_and_finish(f: &Fixture) -> (String, PathBuf, Planned) {
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let plan = asked(&dir, 1);
    decide(&dir, "publish", Some(&plan.nonce)).unwrap();
    let res = finished(&rx);
    assert_eq!(res.outcome, RunStatus::Failed);
    (res.run_id, dir, plan)
}

#[test]
fn a_push_stopped_before_the_remote_updated_is_unknown_and_never_repeated() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    f.remote_hook("pre-receive", "sleep 30");
    seed(&f, WORK, &[], ", timeout_seconds: 2");
    let (run_id, dir, _plan) = approve_and_finish(&f);
    let events = read_all(&dir).unwrap();
    assert!(
        output(&events).starts_with("publish_unknown"),
        "{}",
        output(&events)
    );
    assert!(output(&events).contains("apb intent resolve"));
    assert_eq!(
        settlements(&events),
        [("unknown".to_string(), "response".to_string())]
    );
    assert_eq!(f.remote_ref("apb/out"), None);

    // Re-entering reads the remote back and never pushes again.
    let again = resume(&f.root, &run_id, Some("publish")).unwrap();
    assert_eq!(again.outcome, RunStatus::Failed);
    let events = read_all(&dir).unwrap();
    assert_eq!(intents(&events), 1);
    assert!(output(&events).starts_with("publish_unknown"));
    assert_eq!(f.remote_ref("apb/out"), None);
}

#[test]
fn a_push_stopped_after_the_remote_updated_is_confirmed_by_the_reconcile() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    f.remote_hook("post-receive", "sleep 30");
    seed(&f, WORK, &[], ", timeout_seconds: 2");
    let (run_id, dir, plan) = approve_and_finish(&f);
    assert!(output(&read_all(&dir).unwrap()).starts_with("publish_unknown"));
    assert_eq!(
        f.remote_ref("apb/out").as_deref(),
        Some(plan.after.as_str())
    );

    let again = resume(&f.root, &run_id, Some("publish")).unwrap();
    assert_eq!(again.outcome, RunStatus::Succeeded);
    let events = read_all(&dir).unwrap();
    assert_eq!(intents(&events), 1);
    assert_eq!(output(&events), "published");
    assert_eq!(
        settlements(&events),
        [
            ("unknown".to_string(), "response".to_string()),
            ("confirmed".to_string(), "reconcile".to_string()),
        ]
    );
}

#[test]
fn an_unspent_consent_pushes_an_identical_plan_once_after_a_crash() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    seed(&f, WORK, &[], "");
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let plan = asked(&dir, 1);
    let run_id = dir.file_name().unwrap().to_string_lossy().into_owned();
    request_pause(
        &f.root,
        &run_id,
        &PauseRequest {
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Paused);
    // The drive journaled the consent and died before recording the push.
    let mut log = EventLog::open(&dir).unwrap();
    log.append(EventPayload::ReviewDecided {
        node: "publish".into(),
        decision: "publish".into(),
        note: String::new(),
        plan_digest: Some(plan.digest.clone()),
    })
    .unwrap();
    drop(log);
    let res = resume(&f.root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&dir).unwrap();
    assert_eq!(intents(&events), 1);
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::ReviewRequested { .. }
        )),
        1,
        "the identical plan is not asked about again"
    );
    assert_eq!(
        f.remote_ref("apb/out").as_deref(),
        Some(plan.after.as_str())
    );
}

#[test]
fn a_push_its_recorder_left_running_is_reconciled_only_after_its_deadline() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    seed(&f, WORK, &[], "");
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let plan = asked(&dir, 1);
    let run_id = dir.file_name().unwrap().to_string_lossy().into_owned();
    request_pause(
        &f.root,
        &run_id,
        &PauseRequest {
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Paused);
    // The drive journaled the consent and the push, then died; the push
    // runs in its own session and is still on its way to the remote.
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let pid = gone.id();
    gone.wait().unwrap();
    // A wide margin: the orphaned push lands about 1 s in, well before
    // the deadline, so a loaded machine does not turn this into a race.
    let deadline_ms = apb_core::clock::now_ms_u64() + 6_000;
    let mut log = EventLog::open(&dir).unwrap();
    log.append(EventPayload::ReviewDecided {
        node: "publish".into(),
        decision: "publish".into(),
        note: String::new(),
        plan_digest: Some(plan.digest.clone()),
    })
    .unwrap();
    log.append(EventPayload::SendIntentRecorded {
        item: None,
        intent: "intent-orphaned-push".into(),
        nonce: "orphaned".into(),
        node: "publish".into(),
        attempt: None,
        attempt_seq: None,
        connector: "@git".into(),
        function: "push".into(),
        account: f.url(),
        mode: "ref".into(),
        label: "ref:apb/out".into(),
        pid,
        deadline_ms,
        replay_of: None,
        resend_of: None,
    })
    .unwrap();
    drop(log);
    let res = std::thread::scope(|s| {
        // The orphaned push lands after the resume started.
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(1_000));
            f.git(
                &f.root,
                &[
                    "push",
                    "-q",
                    "origin",
                    &format!("{}:refs/heads/apb/out", plan.after),
                ],
            );
        });
        resume(&f.root, &run_id, None).unwrap()
    });
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&dir).unwrap();
    assert_eq!(intents(&events), 1, "apb never pushed again");
    assert_eq!(
        settlements(&events),
        [("confirmed".to_string(), "reconcile".to_string())]
    );
    // The remote was read back only once the deadline had passed.
    let settled = events
        .iter()
        .find(|e| matches!(e.payload, EventPayload::SendIntentSettled { .. }))
        .unwrap();
    assert!(
        settled.ts >= u128::from(deadline_ms),
        "settled before the deadline"
    );
}

#[test]
fn a_push_made_outside_the_step_is_flagged_and_skip_pushes_nothing() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    let work = format!("{WORK}git push -q origin HEAD:refs/heads/side\n");
    seed(&f, &work, &[], "");
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let plan = asked(&dir, 1);
    assert!(plan.outside);
    let events = read_all(&dir).unwrap();
    let instruction = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewRequested { instruction, .. } => Some(instruction.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        instruction.contains("already on the remote"),
        "{instruction}"
    );
    // A skip needs no nonce.
    decide(&dir, "skip", None).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Succeeded);
    let events = read_all(&dir).unwrap();
    assert_eq!(output(&events), "skipped");
    assert_eq!(intents(&events), 0);
    assert_eq!(f.remote_ref("apb/out"), None);
}

#[test]
fn the_run_surfaces_show_the_gate_the_plan_and_the_outcome() {
    use apb_engine::run_view::RunView;
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    seed(&f, WORK, &[], "");
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let plan = asked(&dir, 1);
    let run_id = dir.file_name().unwrap().to_string_lossy().into_owned();

    // The pending gate carries the exact plan and how to consent to it.
    let view = RunView::load(&dir, &run_id).unwrap();
    let pending = view
        .progress
        .as_ref()
        .and_then(|p| p.pending_review.clone())
        .expect("a pending publish gate");
    assert_eq!(pending.node, "publish");
    assert_eq!(pending.options, ["publish", "skip"]);
    assert!(pending.how_to_decide.contains(&plan.nonce));
    let shown = pending.publish.expect("the plan is shown");
    assert_eq!(shown.nonce, plan.nonce);
    assert_eq!(shown.after, plan.after);
    assert_eq!(shown.url, f.url());
    let waited = apb_engine::run_wait::wait_run_with(
        &f.root,
        &run_id,
        Duration::from_secs(10),
        Duration::from_millis(50),
        Duration::from_millis(20),
    )
    .unwrap();
    assert_eq!(waited.review_next(), apb_engine::run_wait::PUBLISH_NEXT);

    // The fit report names the target.
    let fit = apb_engine::fit::assess(
        &f.root,
        &apb_core::scope::PlaybookRef {
            origin: apb_core::scope::Origin::Project { workspace_id: None },
            id: "pub".into(),
            version: None,
        },
    )
    .unwrap();
    let target = fit
        .setup
        .iter()
        .find(|i| i.code == "publish_target")
        .expect("a publish target item");
    assert!(target.detail.contains(&f.url()), "{}", target.detail);
    assert_eq!(target.severity, apb_engine::fit::FitSeverity::Note);

    decide(&dir, "publish", Some(&plan.nonce)).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Succeeded);
    let view = RunView::load(&dir, &run_id).unwrap();
    let publish = view.publish();
    assert_eq!(publish.len(), 1);
    let push = publish[0].push.as_ref().unwrap();
    assert_eq!(push.state, apb_engine::publish::view::PushState::Confirmed);
    assert_eq!(
        (push.after.as_str(), push.branch.as_str()),
        (plan.after.as_str(), "apb/out")
    );
    assert!(!publish[0].awaiting_consent);
    let outcome = view.outcome(&dir);
    assert!(
        outcome
            .done
            .iter()
            .any(|l| l.starts_with("publish: published") && l.contains("apb/out")),
        "{:?}",
        outcome.done
    );
    assert!(outcome.needs_look.is_none());
    let html = apb_engine::report_html::render(&f.root, &dir).unwrap();
    assert!(html.contains("Publish"));
}

#[test]
fn an_unknown_push_needs_a_look() {
    use apb_engine::run_view::RunView;
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    f.remote_hook("pre-receive", "sleep 30");
    seed(&f, WORK, &[], ", timeout_seconds: 1");
    let (run_id, dir, _) = approve_and_finish(&f);
    let view = RunView::load(&dir, &run_id).unwrap();
    assert!(view.publish()[0].needs_a_look());
    let look = view.outcome(&dir).needs_look.expect("needs a look");
    assert!(look.line().contains("unknown"), "{}", look.line());
}

#[test]
fn a_nonce_on_a_human_review_gate_is_refused() {
    let _env = common::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/rev/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("playbook.yaml"),
        "schema: 2\nid: rev\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/rev/current"), "1.0.0").unwrap();
    let (tx, rx) = mpsc::channel();
    let r = root.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(run(&r, "rev", None, RunOptions::default()).unwrap());
    });
    let run = run_dir(root);
    poll("the review request", || {
        read_all(&run)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::ReviewRequested { .. }))
            .then_some(())
    });
    let cmd = ReviewCommand {
        node: "gate".into(),
        decision: "approve".into(),
        note: String::new(),
    };
    assert!(matches!(
        post_decision(&run, cmd.clone(), Some("publish-x".into())),
        Err(EngineError::Invalid(_))
    ));
    post_decision(&run, cmd, None).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Succeeded);
}

#[test]
fn a_url_with_credentials_is_refused_and_never_journaled() {
    let _env = common::env_lock();
    // User info over https, and a scp-like URL whose `user:secret@` git
    // would read as the host `user` with the secret in the path.
    for with_userinfo in [
        "https://apbuser:Zq7wXv3Lm9Rt2Yp8@example.invalid/o/r.git",
        "apbuser:Zq7wXv3Lm9Rt2Yp8@example.invalid:o/r.git",
    ] {
        let f = Fixture::new();
        let _home = f.isolate();
        f.git(
            &f.root,
            &["remote", "set-url", "--push", "origin", with_userinfo],
        );
        seed(&f, WORK, &[], "");
        let res = run(&f.root, "pub", None, opts()).unwrap();
        assert_eq!(res.outcome, RunStatus::Failed);
        let dir = f.root.join(".apb/runs").join(&res.run_id);
        let events = read_all(&dir).unwrap();
        assert!(events.iter().any(|e| matches!(&e.payload,
            EventPayload::PublishTargetPinned { url: None, refusal: Some(r), .. } if r == "url_refused")));
        assert!(output(&events).starts_with("publish_refused: url_refused"));
        let journal = fs::read_to_string(dir.join("events.jsonl")).unwrap();
        assert!(
            !journal.contains("Zq7wXv3Lm9Rt2Yp8") && !journal.contains("apbuser"),
            "the journal holds the URL's user info"
        );
        // The fit report blocks without showing the URL either.
        let fit = apb_engine::fit::assess(
            &f.root,
            &apb_core::scope::PlaybookRef {
                origin: apb_core::scope::Origin::Project { workspace_id: None },
                id: "pub".into(),
                version: None,
            },
        )
        .unwrap();
        let target = fit
            .setup
            .iter()
            .find(|i| i.code == "publish_target")
            .unwrap();
        assert_eq!(target.severity, apb_engine::fit::FitSeverity::Blocking);
        assert!(
            !target.detail.contains("apbuser"),
            "the fit item shows the URL's user info"
        );
    }
}

/// Like [`WORK`], but each visit appends, so a re-run commits again.
const WORK_APPEND: &str = r#"printf '%s\n' "$APB_RUN_ID" >> work.txt
git add work.txt
git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false \
  commit -q -m "work of the run" -m "Apb-Run: $APB_RUN_ID"
"#;

#[test]
fn a_supervisor_retry_withdraws_the_publish_request_and_its_nonce() {
    use apb_engine::control::{Control, post_control};
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    seed(&f, WORK_APPEND, &[], "");
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let first = asked(&dir, 1);
    post_control(
        &dir,
        Control::Retry {
            node: "work".into(),
            prompt_override: None,
        },
    )
    .unwrap();
    let second = asked(&dir, 2);
    let events = read_all(&dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload,
            EventPayload::ReviewWithdrawn { node, reason } if node == "publish" && reason.contains("work"))),
        "the open publish request is withdrawn"
    );
    assert_ne!(second.after, first.after, "the re-run made a new commit");
    assert_ne!(second.nonce, first.nonce);
    // The withdrawn plan's nonce answers nothing, and nothing was pushed.
    assert!(matches!(
        decide(&dir, "publish", Some(&first.nonce)),
        Err(EngineError::Conflict(_))
    ));
    assert_eq!(intents(&events), 0);
    assert_eq!(f.remote_ref("apb/out"), None);
    decide(&dir, "publish", Some(&second.nonce)).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Succeeded);
    assert_eq!(
        f.remote_ref("apb/out").as_deref(),
        Some(second.after.as_str())
    );
    assert_eq!(intents(&read_all(&dir).unwrap()), 1);
}

/// `yaml` with an agent node `look` ahead of `first`: decision models are
/// set up only for a run with an agent, so without one nothing could ask.
fn with_agent(yaml: &str, first: &str) -> String {
    let edge = format!("  - {{ from: start, to: {first} }}");
    let head = "version: 1.0.0\nnodes:\n";
    assert!(yaml.contains(head) && yaml.contains(&edge), "{yaml}");
    yaml.replace(
        head,
        "version: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: look, type: agent_task, prompt: \"Look around\" }\n",
    )
    .replace(
        &edge,
        &format!("  - {{ from: start, to: look }}\n  - {{ from: look, to: {first} }}"),
    )
}

#[test]
fn an_enforced_review_triage_never_decides_a_publish_gate() {
    use apb_decide::testing::{StubResponse, StubServer};
    use serde_json::json;
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    // A provider that answers with full confidence, an enforce ceiling, an
    // enforced review_triage and a stored threshold.
    let answer = json!({"decision": {"type": "choice", "choice": "needs_changes",
        "probabilities": {"approve": 0.0, "needs_changes": 1.0}, "confidence": 1.0}});
    let server = StubServer::start_with_fallback(
        vec![],
        StubResponse::json(
            200,
            json!({"model": "jev-1.13.0", "answers": answer,
                "usage": {"input_tokens": 1, "output_tokens": 1}})
            .to_string(),
        ),
    );
    let cfg = tempfile::tempdir().unwrap();
    fs::write(
        cfg.path().join("decisions.yaml"),
        format!(
            "version: 1\nmode: enforce\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{}\", model: jev-1.13.0 }}\nuses:\n  review_triage: {{ mode: enforce }}\n",
            server.base_url
        ),
    )
    .unwrap();
    apb_core::decision_thresholds::set_threshold_in(
        cfg.path(),
        "review_triage",
        "stub",
        "jev-1.13.0",
        0.1,
    )
    .unwrap();
    let _cfg = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let agent = f.root.parent().unwrap().join("agent.sh");
    common::write_sync(&agent, "#!/bin/sh\necho 'Looked: one blocking defect.'\n");
    make_executable(&agent);
    let _agent = EnvVar::set("APB_AGENT_CMD", &agent);
    seed(&f, WORK, &[], "");
    common::seed_main(&f.root);
    let yaml = with_agent(&playbook(&[], ""), "work");
    fs::write(f.root.join(".apb/playbooks/pub/1.0.0/playbook.yaml"), yaml).unwrap();
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let plan = asked(&dir, 1);
    // A recommendation is asked for before the request is declared, and an
    // automatic post would land first and make this skip a conflict.
    decide(&dir, "skip", Some(&plan.nonce)).unwrap();
    assert_eq!(finished(&rx).outcome, RunStatus::Succeeded);
    let events = read_all(&dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(&e.payload,
            EventPayload::NodeFinished { node, .. } if node == "look")),
        "the run has an agent, so decision models are set up"
    );
    assert_eq!(
        count(&events, |p| matches!(p, EventPayload::DecisionMade { .. })),
        0,
        "no decision model is asked about a publish gate"
    );
    assert!(server.requests().is_empty());
    assert_eq!(intents(&events), 0);
    assert_eq!(f.remote_ref("apb/out"), None);

    // The control: the same configuration decides a human_review gate that
    // allows it, so the stub and the thresholds above are live.
    let gate = f.root.join(".apb/playbooks/gate/1.0.0");
    fs::create_dir_all(&gate).unwrap();
    let gate_yaml = "schema: 2\nid: gate\nname: Gate\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: g, type: human_review, options: [approve, needs_changes], auto_decide: { allow: [needs_changes] } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: g }\n  - { from: g, to: done }\n";
    fs::write(gate.join("playbook.yaml"), with_agent(gate_yaml, "g")).unwrap();
    fs::write(f.root.join(".apb/playbooks/gate/current"), "1.0.0").unwrap();
    let control = run(&f.root, "gate", None, RunOptions::default()).unwrap();
    assert_eq!(control.outcome, RunStatus::Succeeded);
    let events = read_all(&f.root.join(".apb/runs").join(&control.run_id)).unwrap();
    assert!(events.iter().any(|e| matches!(&e.payload,
        EventPayload::ReviewDecided { decision, note, .. } if decision == "needs_changes" && note.starts_with("auto:"))));
    assert_eq!(server.requests().len(), 1);
}

/// Seeds the playbook `id` at 1.0.0 from `yaml`.
fn seed_playbook(root: &Path, id: &str, yaml: &str) {
    let dir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    fs::write(dir.join("scripts/work.sh"), WORK).unwrap();
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
}

#[test]
fn a_child_of_a_publish_only_run_is_held_to_it() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    let marker = f.root.parent().unwrap().join("agent-ran");
    let agent = f.root.parent().unwrap().join("agent.sh");
    common::write_sync(
        &agent,
        &format!("#!/bin/sh\ntouch '{}'\necho done\n", marker.display()),
    );
    make_executable(&agent);
    let _agent = EnvVar::set("APB_AGENT_CMD", &agent);
    seed(&f, WORK, &[], "");
    common::seed_main(&f.root);
    // The parent is publish_only; its sub-playbook runs an agent outside
    // any sandbox, which could push with the operator's credentials.
    let parent = playbook(&[], "")
        .replace("version: 1.0.0\n", "version: 1.0.0\npublish_only: true\n")
        .replace(
            "  - { id: start, type: start }\n",
            "  - { id: start, type: start }\n  - { id: sub, type: playbook, playbook: child }\n",
        )
        .replace(
            "  - { from: start, to: work }",
            "  - { from: start, to: sub }\n  - { from: sub, to: work }",
        );
    seed_playbook(&f.root, "pub", &parent);
    seed_playbook(
        &f.root,
        "child",
        "schema: 2\nid: child\nname: Child\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: look, type: agent_task, prompt: \"Look around\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: look }\n  - { from: look, to: done }\n",
    );
    let rx = start(&f.root);
    let dir = run_dir(&f.root);
    let plan = asked(&dir, 1);
    decide(&dir, "skip", Some(&plan.nonce)).unwrap();
    finished(&rx);
    let events = read_all(&dir).unwrap();
    let sub = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "sub" => {
                Some(output.clone())
            }
            _ => None,
        })
        .expect("the sub-playbook node finished");
    assert!(
        sub.contains("publish_only") && sub.contains("`look`"),
        "{sub}"
    );
    assert!(!marker.exists(), "the child's unsandboxed agent ran");
    assert_eq!(intents(&events), 0);
}

#[test]
fn a_child_started_after_its_parent_acted_pins_nothing() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    // The parent's own script runs first and could have pointed the remote
    // anywhere before the child starts.
    seed_playbook(
        &f.root,
        "parent",
        "schema: 2\nid: parent\nname: Parent\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: prep, type: script, script: \"scripts/prep.sh\", runner: sh }\n  - { id: sub, type: playbook, playbook: pub }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: prep }\n  - { from: prep, to: sub }\n  - { from: sub, to: done }\n",
    );
    fs::write(
        f.root.join(".apb/playbooks/parent/1.0.0/scripts/prep.sh"),
        "true\n",
    )
    .unwrap();
    seed_playbook(&f.root, "pub", &playbook(&[], ""));
    let root = f.root.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "parent", None, opts()).unwrap());
    });
    let res = finished(&rx);
    assert_eq!(res.outcome, RunStatus::Failed);
    let child = fs::read_dir(f.root.join(".apb/runs"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("pub-"))
        .expect("the child run");
    let events = read_all(&child).unwrap();
    assert!(
        output(&events).starts_with("publish_refused: no_pin"),
        "{}",
        output(&events)
    );
    assert_eq!(
        count(&events, |p| matches!(
            p,
            EventPayload::PublishTargetPinned { .. }
        )),
        0
    );
    assert_eq!(f.remote_ref("apb/out"), None);
}

#[test]
fn a_run_step_cannot_consent_even_without_the_runs_playbook() {
    let _env = common::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    // A run directory whose playbook snapshot is missing.
    let dir = tmp.path().join("pub-1");
    fs::create_dir_all(&dir).unwrap();
    let _step = EnvVar::set("APB_RUN_ID", "some-run");
    let refused = decide(
        &dir,
        "publish",
        Some("publish-00000000000000000000000000000000"),
    );
    assert!(
        matches!(refused, Err(EngineError::Invalid(ref m)) if m.starts_with("publishing is refused")),
        "{refused:?}"
    );
    assert!(read_reviews_after(&dir, None).unwrap().is_empty());
}

#[test]
fn a_publish_decision_waits_for_a_readable_journal() {
    let _env = common::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("pub-1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), playbook(&[], "")).unwrap();
    // A journal that cannot be read (a damaged line before the end): the
    // decision could not be stamped with the open plan's nonce, so no drive
    // would ever consume it.
    fs::write(dir.join("events.jsonl"), "not json\n{\"seq\":1}\n").unwrap();
    assert!(read_all(&dir).is_err(), "the journal must not read");
    assert!(matches!(
        decide(&dir, "skip", None),
        Err(EngineError::Conflict(_))
    ));
    assert!(read_reviews_after(&dir, None).unwrap().is_empty());
}

#[test]
fn a_file_url_with_a_host_part_is_refused_at_the_pin() {
    let _env = common::env_lock();
    let f = Fixture::new();
    let _home = f.isolate();
    // git reads everything after `file://` as the path, so this names the
    // relative path `localhost/...` under whatever directory git runs in,
    // a place nothing checked.
    let nested = f.root.join("nested.git");
    f.git(&f.root, &["init", "-q", "--bare", "nested.git"]);
    let url = format!("file://localhost{}", nested.display());
    f.git(&f.root, &["config", "remote.origin.pushurl", &url]);
    seed(&f, WORK, &[], "");
    let res = finished(&start(&f.root));
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(
        output(&events).starts_with("publish_refused: url_refused"),
        "{}",
        output(&events)
    );
    assert_eq!(intents(&events), 0);
}

/// A run directory holding `payloads` as its journal and, when given,
/// `playbook` as its snapshot.
fn bare_run(payloads: Vec<EventPayload>, playbook: Option<&str>) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run-1");
    fs::create_dir_all(&dir).unwrap();
    if let Some(p) = playbook {
        fs::write(dir.join("playbook.yaml"), p).unwrap();
    }
    let mut log = EventLog::open(&dir).unwrap();
    for p in payloads {
        log.append(p).unwrap();
    }
    (tmp, dir)
}

fn requested(node: &str, options: &[&str]) -> EventPayload {
    EventPayload::ReviewRequested {
        node: node.into(),
        options: options.iter().map(|o| o.to_string()).collect(),
        title: None,
        instruction: String::new(),
        prompt: None,
        recommendation: None,
        plan_digest: None,
    }
}

#[test]
fn a_run_step_may_still_decide_a_review_gate_that_offers_publish() {
    let _env = common::env_lock();
    let gate = "schema: 2\nid: g\nname: G\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [publish, hold] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n";
    let (_tmp, dir) = bare_run(vec![requested("gate", &["publish", "hold"])], Some(gate));
    let _step = EnvVar::set("APB_RUN_ID", "some-run");
    let posted = post_decision(
        &dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "publish".into(),
            note: String::new(),
        },
        None,
    );
    assert!(posted.is_ok(), "{posted:?}");
}

#[test]
fn a_skip_without_the_snapshot_is_stamped_from_the_journal() {
    let _env = common::env_lock();
    let nonce = apb_core::publish::nonce("sha256:x");
    let plan = EventPayload::PublishPlanned {
        node: "publish".into(),
        from: "work".into(),
        from_seq: 1,
        branch: "apb/out".into(),
        local_branch: "refs/heads/main".into(),
        after: "a".repeat(40),
        remote_before: None,
        url: "file:///srv/r.git".into(),
        commits: Vec::new(),
        omitted: 0,
        this_run: 0,
        outside_push_suspected: false,
        plan_digest: "sha256:x".into(),
        nonce: nonce.clone(),
    };
    let (_tmp, dir) = bare_run(vec![plan, requested("publish", &["publish", "skip"])], None);
    decide(&dir, "skip", None).unwrap();
    let queued = read_reviews_after(&dir, None).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].nonce.as_deref(), Some(nonce.as_str()));
}

/// Journals a settled push of node `publish` to `branch`.
fn settled_push(
    log: &mut EventLog,
    f: &Fixture,
    id: &str,
    branch: &str,
    outcome: &str,
    source: &str,
    effect_ref: Option<String>,
) {
    log.append(EventPayload::SendIntentRecorded {
        item: None,
        intent: id.into(),
        nonce: id.into(),
        node: "publish".into(),
        attempt: None,
        attempt_seq: None,
        connector: "@git".into(),
        function: "push".into(),
        account: f.url(),
        mode: "ref".into(),
        label: format!("ref:{branch}"),
        pid: 1,
        deadline_ms: 1,
        replay_of: None,
        resend_of: None,
    })
    .unwrap();
    log.append(EventPayload::SendIntentSettled {
        intent: id.into(),
        outcome: outcome.into(),
        source: source.into(),
        detail: None,
        effect_ref,
        by: None,
    })
    .unwrap();
}

#[test]
fn a_confirmed_push_of_this_commit_counts_without_its_effect_reference() {
    // A person's confirmation carries no effect reference, and a long URL
    // and branch push the commit past the reference's cap: neither may
    // read as "another commit was pushed".
    for effect_ref in [
        None,
        Some(format!("file:///{} refs/heads/apb/out ..", "x".repeat(300))),
    ] {
        let _env = common::env_lock();
        let f = Fixture::new();
        let _home = f.isolate();
        seed(&f, WORK, &[], "");
        let rx = start(&f.root);
        let dir = run_dir(&f.root);
        let plan = asked(&dir, 1);
        let run_id = dir.file_name().unwrap().to_string_lossy().into_owned();
        request_pause(
            &f.root,
            &run_id,
            &PauseRequest {
                by: Some(PauseBy::Operator),
                mode: PauseMode::Drain,
                ..PauseRequest::default()
            },
        )
        .unwrap();
        assert_eq!(finished(&rx).outcome, RunStatus::Paused);
        // This plan's push is confirmed in the ledger; a later push to
        // another branch was not sent, so the drive plans again and the
        // ledger answers that the push already went out.
        let mut log = EventLog::open(&dir).unwrap();
        settled_push(
            &mut log,
            &f,
            "intent-sent",
            "apb/out",
            "confirmed",
            "person",
            effect_ref,
        );
        settled_push(
            &mut log,
            &f,
            "intent-other",
            "apb/else",
            "not_sent",
            "response",
            None,
        );
        log.append(EventPayload::ReviewDecided {
            node: "publish".into(),
            decision: "publish".into(),
            note: String::new(),
            plan_digest: Some(plan.digest.clone()),
        })
        .unwrap();
        drop(log);
        let res = resume(&f.root, &run_id, None).unwrap();
        let events = read_all(&dir).unwrap();
        assert_eq!(res.outcome, RunStatus::Succeeded, "{}", output(&events));
        assert_eq!(output(&events), "published");
        assert_eq!(intents(&events), 2, "nothing was pushed again");
    }
}
