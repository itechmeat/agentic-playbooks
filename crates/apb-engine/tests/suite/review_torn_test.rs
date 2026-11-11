//! A torn last line of `reviews.jsonl` (design D4): a decision posted at
//! the wrong moment does not fail a run.

use std::io::Write;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::review::{ReviewCommand, post_review, read_reviews_after};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar, env_lock};

/// One whole line as `post_review` writes it, for the torn tail.
fn whole_line() -> String {
    let scratch = tempfile::tempdir().unwrap();
    post_review(
        scratch.path(),
        ReviewCommand {
            node: "gate".into(),
            decision: "approved".into(),
            note: "a".into(),
        },
    )
    .unwrap();
    serde_json::to_string(&read_reviews_after(scratch.path(), None).unwrap()[0]).unwrap()
}

/// `start -> gate -> done`, the gate waits for a person.
const WITH_GATE: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: gate, type: human_review, title: Ship it }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: s, to: gate }\n  - { from: gate, to: done }\n";

#[test]
fn a_gate_waits_through_a_torn_line() {
    let _lock = env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/p/1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), WITH_GATE).unwrap();
    std::fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
    common::seed_main(root);

    let thread = {
        let root = root.to_path_buf();
        std::thread::spawn(move || run(&root, "p", None, RunOptions::default()))
    };

    // The gate's request is journaled; the channel file appears only when
    // a poster writes its first line.
    let start = Instant::now();
    let run_dir = loop {
        let found = std::fs::read_dir(root.join(".apb/runs"))
            .ok()
            .and_then(|rd| {
                rd.filter_map(Result::ok).map(|e| e.path()).find(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("p-"))
                })
            });
        let Some(d) = found else {
            if thread.is_finished() {
                panic!(
                    "the run ended before its gate waited; result: {:?}",
                    thread.join()
                );
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timed out waiting for the run directory"
            );
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        let requested = apb_engine::event::read_all_lossy_tail(&d)
            .ok()
            .is_some_and(|ev| {
                ev.iter().any(|e| {
                    matches!(
                        &e.payload,
                        apb_engine::event::EventPayload::ReviewRequested { node, .. } if node == "gate"
                    )
                })
            });
        if requested {
            break d;
        }
        if thread.is_finished() {
            panic!(
                "the run ended before its gate waited; journal: {}",
                std::fs::read_to_string(d.join("events.jsonl")).unwrap_or_default()
            );
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for the review request"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let reviews = run_dir.join("reviews.jsonl");

    // Half of a valid decision line, written without the lock.
    let line = whole_line();
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&reviews)
            .unwrap();
        let b = line.as_bytes();
        f.write_all(&b[..b.len() / 2]).unwrap();
    }
    // Several polls later the run is still waiting, not failed.
    std::thread::sleep(Duration::from_millis(1500));
    let events = apb_engine::event::read_all(&run_dir).unwrap();
    assert!(
        !apb_engine::state::RunState::fold(&events)
            .run_status
            .is_terminal(),
        "a torn last line must not end the run"
    );

    // The rest of the line and its newline land; the gate consumes it.
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&reviews)
            .unwrap();
        f.write_all(format!("{}\n", &line[line.len() / 2..]).as_bytes())
            .unwrap();
    }
    let result = thread.join().unwrap().expect("the run failed");
    assert_eq!(result.outcome, RunStatus::Succeeded);
}
