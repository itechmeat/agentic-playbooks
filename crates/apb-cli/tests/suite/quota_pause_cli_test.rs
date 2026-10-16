//! Issue #200 acceptance 5, through the real `apb` binary: a stub agent
//! answers its first call with a 429 and a short `Retry-After`. The run
//! pauses `quota (<agent>/<model>)` with an `until`, the driver exits and
//! leaves the waker behind, and at `until` the waker probes the executor and
//! resumes the run, which finishes the node. The node never fails.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::pause::PauseBy;

use crate::common::RunGuard;

const DEADLINE: Duration = Duration::from_secs(60);

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

const PLAYBOOK: &str = r#"
schema: 1
id: quotacli
name: Quota CLI
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: work, to: failed, condition: { type: node_status, node: work, equals: failure } }
"#;

fn seed(root: &Path) -> String {
    crate::common::apb_std()
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let vdir = root.join(".apb/playbooks/quotacli/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/quotacli/current"), "1.0.0").unwrap();
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
    // Call 1 (the node): the field-report quota error with a one-second
    // Retry-After; every later call (the probe, the node again) answers.
    let stub = root.join("agent.sh");
    let calls = root.join("calls");
    let mut f = fs::File::create(&stub).unwrap();
    use std::io::Write as _;
    write!(
        f,
        "#!/bin/sh\nprintf x >> '{c}'\nn=$(wc -c < '{c}' | tr -d ' ')\nif [ \"$n\" -eq 1 ]; then echo '429 FreeUsageLimitError: Rate limit exceeded. Please try again later. Retry-After: 1' 1>&2; exit 1; fi\necho done\n",
        c = calls.display()
    )
    .unwrap();
    f.sync_all().unwrap();
    drop(f);
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    stub.to_string_lossy().into_owned()
}

fn work_finishes(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, status, .. } if node == "work" => {
                Some(status.clone())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_quota_error_pauses_and_the_waker_probes_and_resumes_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = seed(root);
    let out = crate::common::apb_std()
        .args(["run", "quotacli", "--detach"])
        .env("APB_AGENT_CMD", &stub)
        .env("APB_WAKER_POLL_MS", "50")
        .env("APB_INFRA_BACKOFF_MS", "20,20")
        .env("APB_QUOTA_BACKOFF_MS", "1000")
        .current_dir(root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let _guard = RunGuard::new(root, &run_id);
    let run_dir = root.join(".apb/runs").join(&run_id);

    let events = poll("the run to finish", || {
        let ev = read_all(&run_dir).ok()?;
        ev.iter()
            .any(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .then_some(ev)
    });
    let (paused_at, until) = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunPaused {
                reason, by, until, ..
            } => {
                assert_eq!(reason, "quota (claude/haiku)");
                assert_eq!(*by, Some(PauseBy::Engine));
                Some((e.ts, until.expect("a quota pause has an until")))
            }
            _ => None,
        })
        .expect("the run paused on quota");
    assert!(
        u128::from(until) > paused_at,
        "until lies ahead of the pause"
    );
    let probe_at = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::SupervisorAction { action, detail, .. } if action == "quota_probe" => {
                assert!(detail.starts_with("passed"), "{detail}");
                Some(e.ts)
            }
            _ => None,
        })
        .expect("the waker probed the executor");
    let resumed = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunResumed { by, .. } => Some((e.ts, *by)),
            _ => None,
        })
        .expect("the waker resumed the run");
    assert_eq!(resumed.1, Some(PauseBy::Engine));
    assert!(probe_at >= u128::from(until), "the probe runs at until");
    assert!(resumed.0 >= probe_at, "the probe comes before the resume");
    assert_eq!(work_finishes(&events), vec!["succeeded".to_string()]);
    assert!(matches!(
        events.last().map(|e| &e.payload),
        Some(EventPayload::RunFinished { outcome }) if outcome == "succeeded"
    ));
}
