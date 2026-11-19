//! The list pilot and the early stop: a visit runs its first items, then
//! stops at a gate on the list node (`continue`, `revise` with a note,
//! `stop`), and a visit whose `stop_when` holds ends early with its rest
//! pending. The gate is an ordinary review request the decision channel
//! answers, named with its request, consumed only from an entry stamped
//! with it. The stub agent stands in for the steps; no model is called.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use apb_engine::control::Control;
use apb_engine::decision_channel::DecisionChannel;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
use apb_engine::review::{ReviewCommand, post_decision_via};
use apb_engine::run_view::RunView;
use apb_engine::scheduler::{
    RunMode, RunOptions, RunResult, drive_prepared, post_supervisor_command,
    prepare_supervised_background, resume, run,
};
use apb_engine::state::RunStatus;

use crate::list_run_test::{
    DEADLINE, Env, events, inline_items, last_state, plan, playbook, poll_until, runs_of, seed,
    settled, stub, work_finishes,
};

/// An output check every item passes when its `blocking` is `0`.
const CHECK: &str =
    "item_outputs: [blocking]\ncheck: { output: { field: blocking, equals: \"0\" } }";

/// The stub's side effect hook that leaves one item's send outcome unknown:
/// `unknown_send <mode>` records the intents and fails the attempt once.
const APPEND: &str = r#"J="$APB_RUN_DIR/events.jsonl"
command -v perl >/dev/null || { echo 'this test needs perl' 1>&2; exit 99; }
add() { perl -e 'use Fcntl qw(:flock); open(my $w, ">>", $ARGV[0]) or die; flock($w, LOCK_EX) or die; open(my $r, "<", $ARGV[0]) or die; my $s = -1; while (<$r>) { $s = $1 if /^\{"seq":(\d+),/ } printf $w "{\"seq\":%d,\"ts\":1,%s}\n", $s + 1, $ARGV[1]; close $w;' "$J" "$1"; }
unknown_send() { add "\"type\":\"send_intent_recorded\",\"intent\":\"si-$I\",\"nonce\":\"n-$I\",\"node\":\"work\",\"item\":\"$I\",\"attempt\":$APB_ATTEMPT,\"connector\":\"smtp\",\"function\":\"send_email\",\"account\":\"main\",\"mode\":\"$1\",\"label\":\"mail\",\"pid\":0,\"deadline_ms\":0"; add "\"type\":\"send_intent_settled\",\"intent\":\"si-$I\",\"outcome\":\"unknown\",\"source\":\"response\",\"detail\":\"timeout\""; }
"#;

fn pilot_list(count: usize, tail: &str) -> String {
    format!(
        "items: {{ inline: {} }}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\n{CHECK}\n{tail}",
        inline_items(count)
    )
}

/// The whole item makes its check fail once and pass on run `n`.
fn fails_once(root: &Path, item: &str, on: u32) {
    plan(root, item, None, "1");
    plan(root, item, Some(on), "0");
}

fn run_in_background(root: &Path) -> mpsc::Receiver<RunResult> {
    run_in_background_opts(root, RunOptions::default())
}

fn run_in_background_opts(root: &Path, opts: RunOptions) -> mpsc::Receiver<RunResult> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let res = match run(&root, "lst", None, opts) {
            Ok(res) => res,
            Err(e) => panic!("the run errored: {e}"),
        };
        let _ = tx.send(res);
    });
    rx
}

fn latest_run_dir(root: &Path) -> PathBuf {
    poll_until("run dir to appear", || {
        let runs = root.join(".apb/runs");
        let entry = fs::read_dir(&runs).ok()?.filter_map(|e| e.ok()).next()?;
        Some(entry.path())
    })
}

fn wait_result(rx: &mpsc::Receiver<RunResult>) -> RunResult {
    rx.recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("run did not finish within {DEADLINE:?}"))
}

fn run_id_of(run_dir: &Path) -> String {
    run_dir.file_name().unwrap().to_string_lossy().into_owned()
}

/// The pending review of `work`, once the pilot gate waits.
fn pending(run_dir: &Path) -> apb_engine::progress::PendingReview {
    poll_until("pending pilot review", || {
        let view = RunView::load(run_dir, &run_id_of(run_dir)).ok()?;
        view.progress?.pending_review.filter(|p| p.node == "work")
    })
}

fn open_seq(run_dir: &Path) -> Option<u64> {
    read_all(run_dir)
        .ok()
        .and_then(|events| apb_engine::review::open_request(&events, "work").map(|(seq, _)| seq))
}

/// Posts `decision` naming the open request, the way every surface does.
fn post_named(run_dir: &Path, decision: &str, note: &str) -> Result<u64, apb_engine::EngineError> {
    post_decision_via(
        run_dir,
        ReviewCommand {
            node: "work".into(),
            decision: decision.into(),
            note: note.into(),
        },
        None,
        open_seq(run_dir),
        DecisionChannel::Terminal,
    )
}

fn wait_pending_then(root: &Path, run_dir: &Path) {
    poll_until("the pilot gate's request", || {
        read_all(run_dir)
            .ok()?
            .iter()
            .any(|e| {
                matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "work")
            })
            .then_some(())
    });
    let _ = pending(run_dir);
    let _ = root;
}

fn decide(root: &Path, run_dir: &Path, decision: &str, note: &str) {
    wait_pending_then(root, run_dir);
    post_named(run_dir, decision, note).unwrap();
    let _ = root;
}

/// Every `list_pilot` of `work`, in journal order.
fn pilots(events: &[Event]) -> Vec<(u32, Vec<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListPilot { node, round, items } if node == "work" => {
                Some((*round, items.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Every `list_stopped` of `work`.
fn stops(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListStopped {
                node,
                reason,
                detail,
            } if node == "work" => Some((reason.clone(), detail.clone())),
            _ => None,
        })
        .collect()
}

fn requests_of(events: &[Event]) -> usize {
    events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "work"),
        )
        .count()
}

/// A supervised run driven on a thread the test joins, so nothing of the
/// drive outlives the test's environment.
struct Supervised {
    id: String,
    run_dir: PathBuf,
    driver: Option<std::thread::JoinHandle<()>>,
}

impl Supervised {
    fn finish(mut self, root: &Path, want: RunStatus) {
        wait_status(root, &self.id, want);
        if let Some(d) = self.driver.take() {
            d.join().expect("the driver does not panic");
        }
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        if let Some(d) = self.driver.take() {
            let _ = apb_engine::control::post_control(
                &self.run_dir,
                Control::Abort {
                    reason: "the test ended early".into(),
                    by: None,
                    graceful: false,
                },
            );
            let _ = d.join();
        }
    }
}

fn supervised(root: &Path) -> Supervised {
    let prepared = prepare_supervised_background(
        root,
        "lst",
        None,
        RunOptions {
            mode: RunMode::Supervised,
            ..RunOptions::default()
        },
    )
    .unwrap();
    let id = prepared.run_id().to_string();
    let run_dir = root.join(".apb/runs").join(&id);
    let root = root.to_path_buf();
    let driver = std::thread::spawn(move || {
        let _ = drive_prepared(&root, prepared);
    });
    Supervised {
        id,
        run_dir,
        driver: Some(driver),
    }
}

fn wait_status(root: &Path, id: &str, want: RunStatus) {
    poll_until(&format!("run status {want:?}"), || {
        let ev = read_all(&root.join(".apb/runs").join(id)).ok()?;
        (apb_engine::state::RunState::fold(&ev).run_status == want).then_some(())
    });
}

fn retry(root: &Path, id: &str) {
    post_supervisor_command(
        root,
        id,
        Control::Retry {
            node: "work".into(),
            prompt_override: None,
        },
    )
    .unwrap();
}

/// A supervised run's failed `node_finished` of `work` and its wake.
fn wait_failed(root: &Path, id: &str, n: usize) {
    poll_until(&format!("failure {n} of work and its wake"), || {
        let ev = read_all(&root.join(".apb/runs").join(id)).ok()?;
        let failed = work_finishes(&ev).iter().filter(|s| *s == "failed").count();
        let wakes = ev
            .iter()
            .filter(
                |e| matches!(&e.payload, EventPayload::WakeRaised { node, .. } if node == "work"),
            )
            .count();
        (failed >= n && wakes >= n).then_some(())
    })
}

/// The pilot gate of the run waits: exactly the slice ran and nothing is
/// decided.
#[test]
fn a_pilot_runs_its_slice_then_waits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let _ = pending(&run_dir);
    let ev = events(root, &run_id_of(&run_dir));
    assert_eq!(
        pilots(&ev),
        vec![(1, vec!["i1".into(), "i2".into(), "i3".into()])]
    );
    assert_eq!(runs_of(root, "i1"), 1);
    assert_eq!(runs_of(root, "i3"), 1);
    assert_eq!(runs_of(root, "i4"), 0, "nothing outside the slice ran");
    assert_eq!(runs_of(root, "i5"), 0);
    assert_eq!(requests_of(&ev), 1);
    assert!(
        !ev.iter()
            .any(|e| matches!(&e.payload, EventPayload::ReviewDecided { .. })),
        "nothing decided yet"
    );
    // The request carries the three pilot options.
    let options = ev
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewRequested { node, options, .. } if node == "work" => {
                Some(options.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(options, ["continue", "revise", "stop"]);
    // The rest runs when a person answers continue, naming the request.
    decide(root, &run_dir, "continue", "");
    let res = wait_result(&rx);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(runs_of(root, "i4"), 1);
    assert_eq!(runs_of(root, "i5"), 1);
}

/// The pilot gate's decision must name its request, so every command it
/// shows names it: the journaled instruction and the pending block's
/// mechanics carry `--request review:<seq>` of the open request, and the
/// decision that names it, as printed, is accepted.
#[test]
fn the_pilot_instruction_names_its_request() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let pr = pending(&run_dir);
    let seq = open_seq(&run_dir).expect("the open request");
    let named = format!("--request review:{seq}");
    let journaled = read_all(&run_dir)
        .unwrap()
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewRequested {
                node, instruction, ..
            } if node == "work" && e.seq == seq => Some(instruction.clone()),
            _ => None,
        })
        .expect("the journaled request");
    for text in [&journaled, &pr.instruction, &pr.how_to_decide] {
        assert!(text.contains(&named), "{text}");
        assert!(text.contains(&format!("request `{seq}`")), "{text}");
    }
    assert_eq!(pr.request, Some(seq));
    // The printed request is the one the channel takes.
    let printed: u64 = pr
        .how_to_decide
        .split("--request review:")
        .nth(1)
        .and_then(|t| t.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|d| d.parse().ok())
        .expect("a request seq in the printed command");
    post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "work".into(),
            decision: "continue".into(),
            note: String::new(),
        },
        None,
        Some(printed),
        DecisionChannel::Terminal,
    )
    .expect("the printed command is accepted");
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// `continue` runs the rest and the run succeeds with every item passed.
#[test]
fn continue_runs_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    decide(root, &run_dir, "continue", "");
    let res = wait_result(&rx);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, res.run_id.as_str());
    let output = ev
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "work" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["passed"], serde_json::json!(5));
    assert_eq!(output["needs_look"], serde_json::json!(0));
    assert!(output.get("stopped").is_none(), "no early stop: {output}");
}

/// `revise` reruns every slice item with the reviewer's note in its
/// prompt, the rest of the visit included.
#[test]
fn revise_reruns_the_slice_with_the_note() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    decide(root, &run_dir, "revise", "keep the greeting short");
    // The revised slice settles and the gate asks again.
    poll_until("the second request", || {
        read_all(&run_dir)
            .ok()
            .and_then(|ev| (requests_of(&ev) >= 2).then_some(()))
    });
    post_named(&run_dir, "continue", "").unwrap();
    let res = wait_result(&rx);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, res.run_id.as_str());
    // The slice of the next round names all three ids.
    assert_eq!(
        pilots(&ev),
        vec![
            (1, vec!["i1".into(), "i2".into(), "i3".into()]),
            (2, vec!["i1".into(), "i2".into(), "i3".into()]),
        ]
    );
    // Every slice item ran twice, the rest once, and every later prompt
    // ends with the reviewer note.
    for item in ["i1", "i2", "i3"] {
        assert_eq!(runs_of(root, item), 2, "{item}");
    }
    assert_eq!(runs_of(root, "i4"), 1);
    assert_eq!(runs_of(root, "i5"), 1);
    let prompts: Vec<String> = crate::list_run_test::invocations(root)
        .into_iter()
        .map(|i| i.prompt)
        .collect();
    for item in ["i1", "i2", "i3", "i4", "i5"] {
        let with_note = prompts
            .iter()
            .filter(|p| p.contains(&format!("\"id\":\"{item}\"")))
            .filter(|p| p.contains("## reviewer note") && p.contains("keep the greeting short"))
            .count();
        assert!(
            with_note >= 1,
            "{item}: no prompt carries the reviewer note ({with_note} of the item's prompts)"
        );
    }
    // The note names the channel the decision came through and claims no
    // person.
    let noted: Vec<&String> = prompts
        .iter()
        .filter(|p| p.contains("## reviewer note"))
        .collect();
    assert!(
        noted.iter().all(|p| p.contains("decided through terminal")),
        "{noted:?}"
    );
    assert!(
        noted.iter().all(|p| !p.contains("A person read")),
        "{noted:?}"
    );
}

/// `stop` ends the visit: the node fails with `stopped` in its output and
/// the items outside the slice stay pending.
#[test]
fn stop_fails_the_node_with_the_rest_pending() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    decide(root, &run_dir, "stop", "");
    let res = wait_result(&rx);
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(root, res.run_id.as_str());
    let output = ev
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "work" => {
                Some(output.clone())
            }
            _ => None,
        })
        .expect("work finished");
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["stopped"]["reason"], serde_json::json!("pilot"));
    assert!(
        output["stopped"]["detail"]
            .as_str()
            .unwrap()
            .contains("did not run"),
        "{output}"
    );
    assert_eq!(stops(&ev).len(), 1);
    assert_eq!(runs_of(root, "i4"), 0, "the rest never ran");
    assert_eq!(runs_of(root, "i5"), 0);
    let ran: Vec<String> = settled(&ev).into_iter().map(|s| s.item).collect();
    assert!(
        !ran.contains(&"i4".to_string()) && !ran.contains(&"i5".to_string()),
        "the rest has no settlement: {ran:?}"
    );
}

/// A clean slice under `then: continue` goes on without asking anybody.
#[test]
fn a_clean_pilot_with_then_continue_asks_nobody() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(5, "pilot: { items: 3, then: continue }"),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let res = crate::list_run_test::run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, res.run_id.as_str());
    assert_eq!(pilots(&ev).len(), 1);
    assert_eq!(requests_of(&ev), 0, "no review request");
    assert_eq!(runs_of(root, "i5"), 1);
    // The surfaces read the slice as continued, never as waiting.
    let fold = apb_engine::list::ListFold::of(&ev, "work").unwrap();
    let view = apb_engine::list::view::PilotView::of(&fold).unwrap();
    assert_eq!(view.state, "continued");
}

/// A visit with no item outside the slice that finished clean under
/// `then: continue` reads as continued, not as a pilot still running.
#[test]
fn a_finished_clean_pilot_with_no_rest_reads_as_continued() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let extra = "  - id: emit\n    type: script\n    script: scripts/emit.sh\n    runner: sh\n    outputs: { fields: [messages] }\n";
    let list = format!(
        "items: {{ from: {{ node: emit, field: messages }}, max_items: 10 }}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\n{CHECK}\npilot: {{ items: 3, then: continue }}"
    );
    seed(
        root,
        &playbook(&list, extra, "emit"),
        &[(
            "emit.sh",
            "echo '{\"messages\":[{\"id\":\"i1\"},{\"id\":\"i2\"}]}'\n",
        )],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let res = crate::list_run_test::run_list(root);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, res.run_id.as_str());
    assert_eq!(pilots(&ev).len(), 1);
    assert_eq!(requests_of(&ev), 0, "no review request");
    let fold = apb_engine::list::ListFold::of(&ev, "work").unwrap();
    let view = apb_engine::list::view::PilotView::of(&fold).unwrap();
    assert_eq!(view.rest, 0);
    assert_eq!(view.state, "continued");
}

/// A rejected slice item retries inside the slice: the gate opens only
/// after its second run settles, and the slice still holds the same items.
#[test]
fn a_rejected_pilot_item_retries_inside_the_slice() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &format!(
                "items: {{ inline: {} }}\nbatch: {{ size: 2, parallel: 1, retries: 1 }}\n{CHECK}\npilot: {{ items: 3 }}",
                inline_items(5)
            ),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    fails_once(root, "i2", 2);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let _ = pending(&run_dir);
    let ev = events(root, &run_id_of(&run_dir));
    assert_eq!(runs_of(root, "i2"), 2, "the rejected item ran again");
    assert_eq!(
        pilots(&ev),
        vec![(1, vec!["i1".into(), "i2".into(), "i3".into()])],
        "the slice still holds three ids"
    );
    let ran: Vec<String> = settled(&ev).into_iter().map(|s| s.item).collect();
    assert!(
        !ran.contains(&"i4".to_string()) && !ran.contains(&"i5".to_string()),
        "{ran:?}"
    );
    decide(root, &run_dir, "continue", "");
    let res = wait_result(&rx);
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// A run paused at the pilot gate and resumed does not rerun its slice
/// when the gate is answered continue.
#[test]
fn a_resume_after_the_pilot_continues_without_rerunning_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let run_id = run_id_of(&run_dir);
    let _ = pending(&run_dir);
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            reason: Some("at the gate".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    // The slice ran exactly once before the pause.
    assert_eq!(runs_of(root, "i1"), 1);
    let (tx, resumed) = mpsc::channel();
    let root2 = root.to_path_buf();
    let id = run_id.clone();
    std::thread::spawn(move || {
        let _ = tx.send(resume(&root2, &id, None).unwrap());
    });
    decide(root, &run_dir, "continue", "");
    let res = wait_result(&resumed);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(runs_of(root, "i1"), 1, "no slice item ran twice");
    assert_eq!(runs_of(root, "i5"), 1);
}

/// A run paused inside the slice finishes the slice before it asks: no
/// item outside the slice has run when the request opens.
#[test]
fn a_resume_inside_the_slice_finishes_the_slice_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    // i3 holds on its worker until the test removes the file, after the
    // pause landed: no wall-clock window.
    let hook = r#"if [ "$I" = i3 ]; then while [ -f "$D/hold-i3" ]; do sleep 0.05; done; fi"#;
    let _env = Env::set(&stub(root, hook), &[]);
    let hold = crate::list_run_test::stub_dir(root).join("hold-i3");
    fs::write(&hold, "").unwrap();
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let run_id = run_id_of(&run_dir);
    // i3 is on the worker: post a `--now` pause under it.
    poll_until("i3 to start", || {
        stub_dir_has(root, "started-i3").then_some(())
    });
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            reason: Some("inside the slice".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Now,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    fs::remove_file(&hold).unwrap();
    assert_eq!(runs_of(root, "i4"), 0);
    let (tx, resumed) = mpsc::channel();
    let root2 = root.to_path_buf();
    let id = run_id.clone();
    std::thread::spawn(move || {
        let _ = tx.send(resume(&root2, &id, None).unwrap());
    });
    // The gate opens while i4 and i5 are still unrun: the slice finished
    // first.
    wait_pending_then(root, &run_dir);
    assert_eq!(runs_of(root, "i4"), 0, "the slice finished first");
    decide(root, &run_dir, "continue", "");
    let res = wait_result(&resumed);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(runs_of(root, "i4"), 1);
}

/// Whether the stub marked `name` under its directory.
fn stub_dir_has(root: &Path, name: &str) -> bool {
    crate::list_run_test::stub_dir(root).join(name).exists()
}

/// A supervisor retry of a visit that failed after its pilot was continued
/// reopens the visit and runs no new pilot.
#[test]
fn a_supervisor_retry_keeps_the_decided_pilot() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(5, "pilot: { items: 3 }\non_needs_look: fail"),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    fails_once(root, "i4", 2);
    let run = supervised(root);
    let id = run.id.clone();
    wait_pending_then(root, &run.run_dir);
    // The gate is answered continue; the rest runs and i4 fails the node.
    decide(root, &run.run_dir, "continue", "");
    wait_failed(root, &id, 1);
    let ev = events(root, &id);
    assert_eq!(requests_of(&ev), 1);
    let ev = events(root, &id);
    assert_eq!(pilots(&ev).len(), 1, "one slice so far");
    // The supervisor retries the node: the reopened visit keeps the
    // decision and runs no new slice.
    retry(root, &id);
    run.finish(root, RunStatus::Succeeded);
    let ev = events(root, &id);
    assert_eq!(pilots(&ev).len(), 1, "no new pilot on the reopen");
    assert_eq!(
        last_state(&ev, "i4").state,
        "passed",
        "the retried item passed"
    );
    assert_eq!(runs_of(root, "i1"), 1, "the passed slice is kept");
}

/// A new visit under `rerun: not_passed` pilots the items that still need
/// to run, never the carried ones.
#[test]
fn a_new_visit_under_rerun_not_passed_pilots_the_remaining_items() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(
                5,
                "pilot: { items: 3 }\nrerun: not_passed\non_needs_look: fail",
            ),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    fails_once(root, "i4", 2);
    let run = supervised(root);
    let id = run.id.clone();
    wait_pending_then(root, &run.run_dir);
    decide(root, &run.run_dir, "continue", "");
    wait_failed(root, &id, 1);
    let ev = events(root, &id);
    assert_eq!(pilots(&ev).len(), 1);
    // `run_continue_from` starts a new visit of the node: the passed items
    // are carried, the pilot takes the ones that still need to run.
    post_supervisor_command(
        root,
        &id,
        Control::ContinueFrom {
            node: "work".into(),
        },
    )
    .unwrap();
    wait_pending_then(root, &run.run_dir);
    let ev = events(root, &id);
    let started = ev
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::ListStarted { node, .. } if node == "work"))
        .count();
    assert_eq!(started, 2, "a new visit started");
    let rounds = pilots(&ev);
    assert_eq!(rounds.len(), 2, "{rounds:?}");
    // i5 passed the visit before and is carried with the slice items; the
    // new visit pilots what still needs to run.
    assert_eq!(rounds[1].1, vec!["i4".to_string()]);
    assert_eq!(runs_of(root, "i1"), 1, "the carried item did not run again");
    assert_eq!(runs_of(root, "i5"), 1, "the carried item did not run again");
    decide(root, &run.run_dir, "continue", "");
    run.finish(root, RunStatus::Succeeded);
}

/// A drive that died right after a new visit's `list_started`, before
/// its opening steps: the resume completes the carry of the passed items
/// before it selects the pilot slice, so the slice holds only the items
/// that still need to run and no carried item runs again.
#[test]
fn a_crash_before_the_carry_still_carries_before_the_slice() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(
                5,
                "pilot: { items: 3 }\nrerun: not_passed\non_needs_look: fail",
            ),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    fails_once(root, "i4", 2);
    let mut run = supervised(root);
    let id = run.id.clone();
    wait_pending_then(root, &run.run_dir);
    decide(root, &run.run_dir, "continue", "");
    wait_failed(root, &id, 1);
    request_pause(
        root,
        &id,
        &PauseRequest {
            reason: Some("before the crash".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    wait_status(root, &id, RunStatus::Paused);
    run.driver.take().unwrap().join().unwrap();
    // What a drive that died after the new visit's `list_started` leaves:
    // the node started again and the visit opened, nothing carried.
    let ev = events(root, &id);
    let started = ev
        .iter()
        .find_map(|e| match &e.payload {
            p @ EventPayload::ListStarted { node, .. } if node == "work" => Some(p.clone()),
            _ => None,
        })
        .unwrap();
    let mut log = apb_engine::event::EventLog::open(&run.run_dir).unwrap();
    log.append(EventPayload::NodeStarted {
        node: "work".into(),
        attempt: 2,
    })
    .unwrap();
    log.append(started).unwrap();
    drop(log);
    let (tx, rx) = mpsc::channel();
    let (r, i) = (root.to_path_buf(), id.clone());
    std::thread::spawn(move || {
        let _ = tx.send(resume(&r, &i, None));
    });
    wait_pending_then(root, &run.run_dir);
    let ev = events(root, &id);
    let rounds = pilots(&ev);
    assert_eq!(rounds.len(), 2, "{rounds:?}");
    assert_eq!(rounds[1].1, vec!["i4".to_string()], "{rounds:?}");
    assert_eq!(runs_of(root, "i1"), 1, "the carried item did not run again");
    assert_eq!(runs_of(root, "i5"), 1, "the carried item did not run again");
    decide(root, &run.run_dir, "continue", "");
    let res = rx
        .recv_timeout(DEADLINE)
        .expect("the resume ended")
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// The early-stop rule ends the visit between batches: the node fails with
/// `stopped` and the items it never ran stay pending.
#[test]
fn stop_when_ends_the_visit_between_batches_failed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(6, "stop_when: { needs_look_at_least: 2 }"),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    plan(root, "i1", None, "1");
    plan(root, "i2", None, "1");
    let res = crate::list_run_test::run_list(root);
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(root, res.run_id.as_str());
    let (reason, detail) = stops(&ev).last().cloned().expect("a list_stopped");
    assert_eq!(reason, "stop_when");
    assert!(
        detail.contains("2 of 2 settled items need a look"),
        "{detail}"
    );
    let output = ev
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "work" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["stopped"]["reason"], serde_json::json!("stop_when"));
    assert_eq!(runs_of(root, "i3"), 0, "no further batch started");
    let statuses: Vec<(String, String)> = settled(&ev)
        .into_iter()
        .map(|s| (s.item, s.state))
        .collect();
    assert!(
        !statuses.iter().any(|(i, _)| i == "i3"),
        "the unsettled items stay pending: {statuses:?}"
    );
}

/// An unattended run never waits at a pilot gate: `fail` ends the run,
/// `continue` decides continue over a clean slice and fails over a bad
/// one.
#[test]
fn an_unattended_pilot_fails_or_continues_as_declared() {
    // `fail`: the run ends hitl_required.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run(
        root,
        "lst",
        None,
        RunOptions {
            unattended: true,
            ..RunOptions::default()
        },
    )
    .unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(root, res.run_id.as_str());
    assert!(
        ev.iter().any(
            |e| matches!(&e.payload, EventPayload::RunError { reason, .. }
            if reason.contains("hitl_required"))
        ),
        "the run ends hitl_required"
    );
    assert_eq!(requests_of(&ev), 0, "an unattended run never asks");
    assert!(
        !ev.iter()
            .any(|e| matches!(&e.payload, EventPayload::ReviewDecided { .. })),
        "nobody decided"
    );
    drop(_env);

    // `continue` over a clean slice: the engine decides continue and the
    // rest runs.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(5, "pilot: { items: 3, unattended: continue }"),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let res = run(
        root,
        "lst",
        None,
        RunOptions {
            unattended: true,
            ..RunOptions::default()
        },
    )
    .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, res.run_id.as_str());
    let decided = ev
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::ReviewDecided { node, .. } if node == "work"))
        .expect("the engine decided");
    match &decided.payload {
        EventPayload::ReviewDecided {
            decision,
            decided_by,
            ..
        } => {
            assert_eq!(decision, "continue");
            assert!(matches!(
                decided_by,
                Some(apb_engine::event::DecidedBy::Unattended)
            ));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(runs_of(root, "i5"), 1);
    drop(_env);

    // `continue` over a slice with an item that needs a look: hitl_required.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(5, "pilot: { items: 3, unattended: continue }"),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    plan(root, "i2", None, "1");
    let res = run(
        root,
        "lst",
        None,
        RunOptions {
            unattended: true,
            ..RunOptions::default()
        },
    )
    .unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(root, res.run_id.as_str());
    assert!(
        ev.iter().any(
            |e| matches!(&e.payload, EventPayload::RunError { reason, .. }
            if reason.contains("hitl_required"))
        ),
        "a bad slice never runs on by itself"
    );
    assert_eq!(runs_of(root, "i4"), 0);
}

/// The channel refuses a decision outside the pilot options.
#[test]
fn the_channel_refuses_an_approve_on_a_pilot_gate() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    wait_pending_then(root, &run_dir);
    let refused = post_named(&run_dir, "approve", "").unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("is a list pilot gate; decide continue, revise or stop"),
        "{refused}"
    );
    assert!(
        !run_dir.join("reviews.jsonl").exists(),
        "a refused decision is not queued"
    );
    decide(root, &run_dir, "continue", "");
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// The pilot gate is a plain review request: no `review_detail` of the
/// list node in the journal, and the pending review carries the slice.
#[test]
fn the_pilot_gate_shows_its_slice() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let pr = pending(&run_dir);
    assert!(
        pr.scope_line.is_none(),
        "a pilot gate shows no work scope: {pr:?}"
    );
    let pilot = pr.pilot.expect("the pending review carries the pilot");
    assert_eq!(pilot.round, 1);
    assert_eq!(
        pilot.items,
        ["i1".to_string(), "i2".to_string(), "i3".to_string()]
    );
    assert_eq!(pilot.passed, 3);
    assert_eq!(pilot.needs_look, 0);
    assert_eq!(pilot.rest, 2);
    assert_eq!(pilot.state, "waiting");
    let ev = read_all(&run_dir).unwrap();
    let instruction = ev
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewRequested {
                node, instruction, ..
            } if node == "work" => Some(instruction.clone()),
            _ => None,
        })
        .expect("the request");
    assert!(
        instruction.contains("Pilot of list `work`"),
        "{instruction}"
    );
    assert!(
        !ev.iter().any(
            |e| matches!(&e.payload, EventPayload::ReviewDetail { node, .. } if node == "work")
        ),
        "the pilot gate journals no review_detail"
    );
    decide(root, &run_dir, "continue", "");
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// A decision made for a pilot request the drive withdrew never answers
/// the request asked after it.
#[test]
fn a_decision_for_a_withdrawn_pilot_request_never_answers_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let run_id = run_id_of(&run_dir);
    let _ = pending(&run_dir);
    let seq_a = open_seq(&run_dir).unwrap();
    // The drive is stopped at the gate; the decision for A lands while it
    // cannot consume it.
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            reason: Some("at the gate".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    post_named(&run_dir, "continue", "").unwrap();
    // A supervisor directive withdraws A (the `review_withdrawn` a
    // `run_continue_from` writes), and the run resumes.
    let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
    log.append(EventPayload::ReviewWithdrawn {
        node: "work".into(),
        reason: "run_continue_from `work`".into(),
    })
    .unwrap();
    drop(log);
    let (tx, resumed) = mpsc::channel();
    let root2 = root.to_path_buf();
    let id = run_id.clone();
    std::thread::spawn(move || {
        let _ = tx.send(resume(&root2, &id, None).unwrap());
    });
    poll_until("the second request", || {
        (read_all(&run_dir).ok().map(|ev| requests_of(&ev) >= 2)?).then_some(())
    });
    let seq_b = open_seq(&run_dir).unwrap();
    assert_ne!(seq_a, seq_b);
    // A decision without a request is refused, naming the open one.
    let none_named = post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "work".into(),
            decision: "continue".into(),
            note: String::new(),
        },
        None,
        None,
        DecisionChannel::Terminal,
    )
    .unwrap_err();
    assert!(
        none_named.to_string().contains("decide_again"),
        "{none_named}"
    );
    // A decision naming A is refused too, and the doctor lists it.
    let naming_a = post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "work".into(),
            decision: "continue".into(),
            note: String::new(),
        },
        None,
        Some(seq_a),
        DecisionChannel::Terminal,
    )
    .unwrap_err();
    assert!(naming_a.to_string().contains("decide_again"), "{naming_a}");
    let report = apb_engine::run_doctor::diagnose_run(root, &run_id).unwrap();
    let reviews = report
        .iter()
        .find(|c| c.subject == "reviews")
        .expect("the doctor lists A's entry");
    assert!(reviews.detail.contains("work"), "{}", reviews.detail);
    // A decision naming B is consumed and the run goes on.
    post_named(&run_dir, "continue", "").unwrap();
    let res = wait_result(&resumed);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, run_id.as_str());
    let decided: Vec<&str> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReviewDecided { decision, .. } => Some(decision.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(decided, vec!["continue"]);
    assert_eq!(runs_of(root, "i4"), 1);
}

/// The early-stop rule waits for the pilot gate: it counts nothing while
/// the slice runs, and judges the items settled outside it afterwards.
#[test]
fn stop_when_waits_for_the_pilot_gate() {
    // The slice's needs-look item does not count and does not stop the
    // visit: two batches run inside the slice, the gate opens, and after
    // continue the rest runs to the end.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(
                5,
                "pilot: { items: 3 }\nstop_when: { needs_look_at_least: 1 }",
            ),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    plan(root, "i1", None, "1");
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    decide(root, &run_dir, "continue", "");
    let res = wait_result(&rx);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, res.run_id.as_str());
    assert_eq!(stops(&ev), vec![], "no list_stopped while the slice ran");
    assert_eq!(runs_of(root, "i5"), 1);
    // Inside the slice: a batch of two, then one, and the gate opens only
    // after the third item settled.
    let gate = ev
        .iter()
        .position(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "work"),
        )
        .expect("the pilot gate");
    let slice_batches: Vec<Vec<String>> = ev[..gate]
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListBatchStarted { node, items, .. } if node == "work" => {
                Some(items.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        slice_batches,
        vec![
            vec!["i1".to_string(), "i2".to_string()],
            vec!["i3".to_string()]
        ]
    );
    assert!(
        ev[..gate].iter().any(|e| matches!(&e.payload,
            EventPayload::ListItemSettled { item, .. } if item == "i3")),
        "i3 settled before the gate"
    );
    drop(_env);

    // A variant whose fourth item is rejected: the rule holds after that
    // batch and the visit ends failed, no further batch admitted.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(
                5,
                "pilot: { items: 3 }\nstop_when: { needs_look_at_least: 1 }",
            ),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    plan(root, "i4", None, "1");
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    decide(root, &run_dir, "continue", "");
    let res = wait_result(&rx);
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(root, res.run_id.as_str());
    let batches = ev
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::ListBatchStarted { node, .. } if node == "work"),
        )
        .count();
    let (reason, _) = stops(&ev).last().cloned().expect("a list_stopped");
    assert_eq!(reason, "stop_when");
    let stopped_at = ev
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::ListStopped { .. }))
        .unwrap();
    assert!(
        ev[..stopped_at]
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::ListBatchStarted { items, .. } if items.contains(&"i4".to_string()))),
        "the rule held after the batch that ran i4"
    );
    assert_eq!(batches, 3, "no batch after the stop: {batches}");
    let output = ev
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "work" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["stopped"]["reason"], serde_json::json!("stop_when"));
}

/// A batch its `--now` pause cut short is honoured before the early-stop
/// rule: the run pauses with no `list_stopped`, and on resume the rule
/// ends the visit before another batch is admitted.
#[test]
fn a_paused_batch_is_honoured_before_stop_when() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(5, "stop_when: { needs_look_at_least: 1 }"),
            "",
            "",
        ),
        &[],
    );
    // i2 holds on its worker until the test removes the file, after the
    // pause landed: no wall-clock window.
    let hook = r#"if [ "$I" = i2 ]; then while [ -f "$D/hold-i2" ]; do sleep 0.05; done; fi"#;
    let _env = Env::set(&stub(root, hook), &[]);
    let hold = crate::list_run_test::stub_dir(root).join("hold-i2");
    fs::write(&hold, "").unwrap();
    plan(root, "i1", None, "1");
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let run_id = run_id_of(&run_dir);
    // i1 settled while i2 is still on the worker: pause now.
    poll_until("i1 to settle", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| {
                matches!(&e.payload, EventPayload::ListItemSettled { item, state, .. }
                if item == "i1" && state == "needs_look")
            })
            .then_some(())
    });
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            reason: Some("mid batch".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Now,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    fs::remove_file(&hold).unwrap();
    let ev = read_all(&run_dir).unwrap();
    assert_eq!(stops(&ev), vec![], "no list_stopped before the resume");
    let i2 = last_state(&ev, "i2");
    assert_eq!(
        i2.state, "interrupted",
        "the unsettled item is pending again"
    );
    // On resume the rule holds before the next batch is admitted.
    let paused_at = ev
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::RunPaused { .. }))
        .unwrap();
    let (tx, resumed) = mpsc::channel();
    let root2 = root.to_path_buf();
    let id = run_id.clone();
    std::thread::spawn(move || {
        let _ = tx.send(resume(&root2, &id, None).unwrap());
    });
    let res = wait_result(&resumed);
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(root, run_id.as_str());
    let resumed_at = ev
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::RunResumed { .. }))
        .unwrap();
    let stopped_at = ev
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::ListStopped { .. }))
        .expect("the stop on resume");
    assert!(
        stopped_at > resumed_at && resumed_at > paused_at,
        "the stop follows the resume"
    );
    assert!(
        !ev[resumed_at..=stopped_at]
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::ListBatchStarted { .. })),
        "no batch was admitted before the stop"
    );
    let (reason, _) = stops(&ev).last().cloned().unwrap();
    assert_eq!(reason, "stop_when");
}

/// A crash after `list_stopped` leaves the node to finish failed with the
/// same stop, running no item.
#[test]
fn a_crash_after_list_stopped_ends_the_node_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(4, "stop_when: { needs_look_at_least: 1 }"),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    plan(root, "i1", None, "1");
    let res = crate::list_run_test::run_list(root);
    assert_eq!(res.outcome, RunStatus::Failed);
    let run_id = res.run_id.clone();
    let run_dir = root.join(".apb/runs").join(&run_id);
    // The crash cut: the journal ends at `list_stopped`.
    let ev = read_all(&run_dir).unwrap();
    let cut = ev
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::ListStopped { .. }))
        .unwrap();
    let kept: String = ev[..=cut]
        .iter()
        .map(|e| serde_json::to_string(e).unwrap() + "\n")
        .collect();
    fs::write(run_dir.join("events.jsonl"), &kept).unwrap();
    assert_eq!(runs_of(root, "i2"), 1, "the first batch ran before the cut");
    let (tx, resumed) = mpsc::channel();
    let root2 = root.to_path_buf();
    let id = run_id.clone();
    std::thread::spawn(move || {
        let _ = tx.send(resume(&root2, &id, None).unwrap());
    });
    let res = wait_result(&resumed);
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = read_all(&run_dir).unwrap();
    assert_eq!(stops(&ev).len(), 1, "no second list_stopped");
    let output = ev
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "work" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap();
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["stopped"]["reason"], serde_json::json!("stop_when"));
    assert_eq!(runs_of(root, "i2"), 1, "no item ran after the cut");
    assert_eq!(runs_of(root, "i3"), 0);
}

/// A crash after `list_pilot` runs the same slice: no second slice is
/// journaled.
#[test]
fn a_crash_after_list_pilot_runs_the_same_slice() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let run_id = run_id_of(&run_dir);
    let _ = pending(&run_dir);
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            reason: Some("at the gate".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    // The crash cut: the journal ends right after the slice's `list_pilot`.
    let ev = read_all(&run_dir).unwrap();
    let cut = ev
        .iter()
        .position(|e| matches!(&e.payload, EventPayload::ListPilot { .. }))
        .unwrap();
    let kept: String = ev[..=cut]
        .iter()
        .map(|e| serde_json::to_string(e).unwrap() + "\n")
        .collect();
    fs::write(run_dir.join("events.jsonl"), &kept).unwrap();
    let (tx, resumed) = mpsc::channel();
    let root2 = root.to_path_buf();
    let id = run_id.clone();
    std::thread::spawn(move || {
        let _ = tx.send(resume(&root2, &id, None).unwrap());
    });
    wait_pending_then(root, &run_dir);
    let ev = read_all(&run_dir).unwrap();
    assert_eq!(
        pilots(&ev).len(),
        1,
        "the resumed drive wrote no second list_pilot"
    );
    assert_eq!(runs_of(root, "i4"), 0, "the same three ids ran");
    for item in ["i1", "i2", "i3"] {
        assert_eq!(runs_of(root, item), 2, "{item} ran again after the cut");
    }
    decide(root, &run_dir, "continue", "");
    let res = wait_result(&resumed);
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// Two decisions naming the same open pilot request while nothing drives
/// the run (it is paused at the gate, so no drive consumes the first in
/// between): the second is refused as already queued, and the resumed
/// run journals exactly one `review_decided`.
#[test]
fn a_second_decision_for_the_same_pilot_request_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let run_id = run_id_of(&run_dir);
    wait_pending_then(root, &run_dir);
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            reason: Some("at the gate".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let seq = open_seq(&run_dir).unwrap();
    post_named(&run_dir, "continue", "").unwrap();
    let second = post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "work".into(),
            decision: "continue".into(),
            note: String::new(),
        },
        None,
        Some(seq),
        DecisionChannel::Terminal,
    )
    .unwrap_err();
    assert!(
        second
            .to_string()
            .contains("a decision is already queued for the request of node `work`"),
        "{second}"
    );
    let res = resume(root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, res.run_id.as_str());
    let decided = ev
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::ReviewDecided { node, .. } if node == "work"),
        )
        .count();
    assert_eq!(decided, 1, "exactly one review_decided");
}

/// A run step never decides its own run's pilot gate.
#[test]
fn a_run_step_cannot_decide_its_own_pilot() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let run_id = run_id_of(&run_dir);
    wait_pending_then(root, &run_dir);
    // Nothing drives the run while this process poses as its step: the
    // drive is paused at the gate first, so no drive thread reads the
    // step variable.
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            reason: Some("at the gate".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let seq = open_seq(&run_dir).unwrap();
    let _own = crate::common::EnvVar::set("APB_RUN_ID", &run_id);
    let err = post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "work".into(),
            decision: "continue".into(),
            note: String::new(),
        },
        None,
        Some(seq),
        DecisionChannel::Terminal,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("run_step_own_run") || err.to_string().contains("own run"),
        "{err}"
    );
    drop(_own);
    post_named(&run_dir, "continue", "").unwrap();
    assert_eq!(
        resume(root, &run_id, None).unwrap().outcome,
        RunStatus::Succeeded
    );
}

/// A pilot gate takes no field values: a post carrying them is refused
/// and nothing is queued; the same post without them is accepted.
#[test]
fn a_post_with_fields_on_a_pilot_gate_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    wait_pending_then(root, &run_dir);
    let seq = open_seq(&run_dir).unwrap();
    let cmd = ReviewCommand {
        node: "work".into(),
        decision: "continue".into(),
        note: String::new(),
    };
    let refused = apb_engine::review::post(
        &run_dir,
        apb_engine::review::DecisionPost {
            cmd: cmd.clone(),
            nonce: None,
            request: Some(seq),
            channel: DecisionChannel::Terminal,
            fields: [("amount".to_string(), serde_json::json!(1))].into(),
            passkey: None,
        },
    )
    .unwrap_err();
    assert!(refused.to_string().contains("takes no fields"), "{refused}");
    assert!(
        !run_dir.join("reviews.jsonl").exists(),
        "the refused post wrote nothing"
    );
    post_decision_via(&run_dir, cmd, None, Some(seq), DecisionChannel::Terminal).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// A pilot under a send-once function: `revise` never reruns an item whose
/// send is unknown. It keeps its needs-look record, stays in the slice as
/// such, and the gate counts it.
#[test]
fn revise_never_reruns_an_item_with_an_unknown_send() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let hook = format!(
        "{APPEND}if [ \"$I\" = i2 ] && [ \"$N\" = 1 ]; then unknown_send none; echo 'send timed out' 1>&2; exit 1; fi"
    );
    let _env = Env::set(&stub(root, &hook), &[]);
    let run = supervised(root);
    let id = run.id.clone();
    wait_pending_then(root, &run.run_dir);
    let ev = events(root, &id);
    let i2 = last_state(&ev, "i2");
    assert_eq!(
        (i2.state.as_str(), i2.reason.as_deref()),
        ("needs_look", Some("unknown_send"))
    );
    // Revise: i1 and i3 run again, i2 is never interrupted.
    decide(root, &run.run_dir, "revise", "try again");
    poll_until("the slice of round 2 to settle and the gate to ask", || {
        let ev = read_all(&run.run_dir).ok()?;
        (requests_of(&ev) >= 2).then_some(())
    });
    let ev = events(root, &id);
    assert_eq!(
        pilots(&ev),
        vec![
            (1, vec!["i1".into(), "i2".into(), "i3".into()]),
            (2, vec!["i1".into(), "i2".into(), "i3".into()]),
        ]
    );
    let revised = |item: &str| {
        settled(&ev).iter().any(|s| {
            s.item == item
                && s.state == "interrupted"
                && s.reason.as_deref() == Some("pilot_revise")
        })
    };
    assert!(revised("i1"), "i1 was interrupted for the revise");
    assert!(revised("i3"), "i3 was interrupted for the revise");
    assert_eq!(
        runs_of(root, "i2"),
        1,
        "the unknown-send item never ran again"
    );
    let attempts_of = |item: &str| {
        ev.iter()
            .filter(|e| {
                matches!(&e.payload, EventPayload::AttemptStarted { node, item: Some(i), .. }
                if node == "work" && i == item)
            })
            .count()
    };
    assert_eq!(attempts_of("i2"), 1, "no attempt for the unknown-send item");
    let intents = ev
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::SendIntentRecorded { item, .. } if item.as_deref() == Some("i2")))
        .count();
    assert_eq!(intents, 1, "one intent, never a second");
    // The gate after round 2 counts i2 as needing a look.
    let pr = pending(&run.run_dir);
    let pilot = pr.pilot.expect("the pending review carries the pilot");
    assert_eq!(pilot.round, 2);
    assert_eq!(pilot.needs_look, 1);
    assert_eq!(
        runs_of(root, "i2"),
        1,
        "the unknown-send item never ran again"
    );
    // A person resolves the send and answers continue; the visit ends with
    // i2 needing a look (its rerun is a retry's or a new visit's business).
    apb_engine::connector::intent::resolve_by_person(&run.run_dir, "si-i2", true, None, "tester")
        .unwrap();
    decide(root, &run.run_dir, "continue", "");
    run.finish(root, RunStatus::Succeeded);
    assert_eq!(runs_of(root, "i2"), 1, "one send, never a second attempt");
}

/// A crash partway through the revise transition resumes once: the items
/// already interrupted are skipped, exactly one slice of the next round is
/// journaled, and the item with the unknown send is never interrupted.
#[test]
fn a_crash_inside_revise_resumes_the_transition_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let hook = format!(
        "{APPEND}if [ \"$I\" = i2 ] && [ \"$N\" = 1 ]; then unknown_send none; echo 'send timed out' 1>&2; exit 1; fi"
    );
    let _env = Env::set(&stub(root, &hook), &[]);
    let run = supervised(root);
    let id = run.id.clone();
    wait_pending_then(root, &run.run_dir);
    decide(root, &run.run_dir, "revise", "try again");
    poll_until("the second gate", || {
        let ev = read_all(&run.run_dir).ok()?;
        (requests_of(&ev) >= 2).then_some(())
    });
    // The transition ran whole. The crash cut: the journal ends after i1's
    // `pilot_revise` settlement.
    let ev = read_all(&run.run_dir).unwrap();
    let cut = ev
        .iter()
        .position(|e| {
            matches!(&e.payload, EventPayload::ListItemSettled { item, state, reason, .. }
            if item == "i1" && state == "interrupted" && reason.as_deref() == Some("pilot_revise"))
        })
        .expect("i1's revise settlement");
    let kept: String = ev[..=cut]
        .iter()
        .map(|e| serde_json::to_string(e).unwrap() + "\n")
        .collect();
    // The drive must not hold the journal while it is rewritten.
    request_pause(
        root,
        &id,
        &PauseRequest {
            reason: Some("crash point".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    wait_status(root, &id, RunStatus::Paused);
    fs::write(run.run_dir.join("events.jsonl"), &kept).unwrap();
    let (tx, resumed) = mpsc::channel();
    let root2 = root.to_path_buf();
    let id2 = id.clone();
    std::thread::spawn(move || {
        // The paused driver releases the workdir on its way out; the
        // resume waits for it.
        let res = poll_until("the workdir to free for the resume", || {
            match resume(&root2, &id2, None) {
                Ok(r) => Some(r),
                Err(apb_engine::EngineError::WorkdirBusy(_)) => None,
                Err(e) => panic!("resume failed: {e}"),
            }
        });
        let _ = tx.send(res);
    });
    wait_pending_then(root, &run.run_dir);
    let ev = read_all(&run.run_dir).unwrap();
    let revise_settlements = |item: &str| {
        ev.iter()
            .filter(|e| {
                matches!(&e.payload, EventPayload::ListItemSettled { item: i, state, reason, .. }
                if i == item && state == "interrupted" && reason.as_deref() == Some("pilot_revise"))
            })
            .count()
    };
    assert_eq!(revise_settlements("i1"), 1, "no second settlement for i1");
    assert_eq!(revise_settlements("i3"), 1, "i3's settlement written once");
    assert_eq!(
        revise_settlements("i2"),
        0,
        "the unknown-send item is never interrupted"
    );
    assert_eq!(
        pilots(&ev).iter().filter(|(r, _)| *r == 2).count(),
        1,
        "exactly one list_pilot of round 2"
    );
    assert_eq!(runs_of(root, "i2"), 1);
    decide(root, &run.run_dir, "continue", "");
    let r = wait_result(&resumed);
    assert_eq!(r.outcome, RunStatus::Succeeded);
}

/// A scenario answers the pilot gate in the same visit answer as the
/// list's items: a quick scenario run validates and ends succeeded with
/// no missing and no extra answer.
#[test]
fn a_scenario_answers_items_and_the_pilot_gate() {
    use apb_core::scenario::{Scenario, ScenarioLevel};
    use apb_engine::scenario::session::ScenarioPrep;
    use apb_engine::scheduler::run_scenario;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // No item check: the scripted answers pass by themselves, so the
    // pilot's gate is the only boundary of the visit.
    seed(
        root,
        &playbook(
            "items: { inline: [ { id: i1 }, { id: i2 }, { id: i3 }, { id: i4 } ] }\nbatch: { size: 2, parallel: 1, retries: 0 }\npilot: { items: 2 }",
            "",
            "",
        ),
        &[],
    );
    let scenario = "schema: 1\nid: lst\nanswers:\n  work:\n    - visit: 1\n      items: { \"*\": { status: succeeded } }\n      review: { decision: continue }\n";
    let vdir = root.join(".apb/playbooks/lst/1.0.0/evals/scenarios");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("pilot.yaml"), scenario).unwrap();
    let prep = ScenarioPrep {
        scenario: yaml_serde::from_str::<Scenario>(scenario).unwrap(),
        level: ScenarioLevel::Quick,
        scenario_digest: "sha256:pilot".into(),
        max_requests: 20,
        sent: Default::default(),
        external_data: false,
    };
    let quick = run_scenario(root, "lst", None, &prep).unwrap();
    assert_eq!(quick.outcome, RunStatus::Succeeded);
    assert!(quick.halted.is_none(), "{:?}", quick.halted);
    assert!(
        quick.answers.missing.is_empty(),
        "{:?}",
        quick.answers.missing
    );
    assert!(quick.answers.extra.is_empty(), "{:?}", quick.answers.extra);
    // The review answer was used and the rest ran.
    assert!(
        quick
            .answers
            .used
            .iter()
            .any(|a| { a.node.as_deref() == Some("work") && a.part.contains("review") }),
        "{:?}",
        quick.answers.used
    );
    // A scenario run lives under .apb/scenario-runs/.
    let ev = read_all(&quick.run_dir).unwrap();
    // A scenario run's items are scripted: the journal settles all four.
    let states: Vec<String> = settled(&ev).into_iter().map(|s| s.state).collect();
    assert_eq!(states.len(), 4, "{states:?}");
    assert!(states.iter().all(|s| s == "passed"), "{states:?}");
    assert_eq!(pilots(&ev).len(), 1);
}

/// A run of a pilot list is not capturable: its per-item answers are not
/// captured, so the pilot's decision alone would replay with every item
/// missing; the capture names the `list_node` gap and writes nothing.
#[test]
fn a_capture_of_a_pilot_list_names_the_list_gap() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(4, "pilot: { items: 2 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    decide(root, &run_dir, "continue", "");
    let res = wait_result(&rx);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let req = apb_engine::scenario::capture::CaptureRequest {
        name: "pilot".into(),
        nodes: Vec::new(),
    };
    match apb_engine::scenario::capture::capture(root, &run_dir, &req) {
        Err(apb_engine::scenario::capture::CaptureError::NotCapturable(gaps)) => {
            assert!(
                gaps.iter()
                    .any(|g| g.code == "list_node" && g.node.as_deref() == Some("work")),
                "{:?}",
                gaps.iter().map(|g| (g.code, &g.node)).collect::<Vec<_>>()
            );
        }
        Ok(c) => panic!("a pilot list was captured: {}", c.yaml),
        Err(apb_engine::scenario::capture::CaptureError::Engine(e)) => {
            panic!("capture failed: {e}")
        }
    }
}

/// The next action of a run parked at a pilot gate reads the pilot's own
/// `unattended:` outcome.
#[test]
fn a_pilot_gate_reads_its_unattended_outcome() {
    use apb_engine::next_action::{Observation, next_actions};
    // `fail`: the action says an unattended run fails hitl_required.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(4, "pilot: { items: 2 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let _ = pending(&run_dir);
    let obs = Observation::load(&run_dir, &run_id_of(&run_dir)).unwrap();
    let actions = next_actions(&obs);
    let action = actions
        .actions
        .iter()
        .find(|a| a.code == apb_engine::next_action::ActionCode::DecideReview)
        .expect("a decide_review action");
    let outcome = action.unattended.as_ref().expect("an unattended outcome");
    assert!(
        matches!(outcome, apb_engine::next_action::UnattendedOutcome::FailsRun { code }
            if code == apb_engine::run_config::HITL_REQUIRED),
        "{outcome:?}"
    );
    decide(root, &run_dir, "continue", "");
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
    drop(_env);

    // `unattended: continue` over a clean slice: the action says the run
    // decides continue.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(
            &pilot_list(4, "pilot: { items: 2, unattended: continue }"),
            "",
            "",
        ),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    let _ = pending(&run_dir);
    let obs = Observation::load(&run_dir, &run_id_of(&run_dir)).unwrap();
    let actions = next_actions(&obs);
    let action = actions
        .actions
        .iter()
        .find(|a| a.code == apb_engine::next_action::ActionCode::DecideReview)
        .expect("a decide_review action");
    let outcome = action.unattended.as_ref().expect("an unattended outcome");
    assert!(
        matches!(outcome, apb_engine::next_action::UnattendedOutcome::Decides { option }
            if option == "continue"),
        "{outcome:?}"
    );
    decide(root, &run_dir, "continue", "");
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}

/// A quota pause inside the pilot slice ends the visit paused: the pause
/// is journaled, the node is not dispatched again, and nothing outside
/// the slice runs. A slice that drops its batch's end reads as a slice
/// waiting for its gate and dispatches the node again in a loop.
#[test]
fn a_quota_pause_inside_the_slice_pauses_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let cfg = tempfile::tempdir().unwrap();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let quota =
        "429 FreeUsageLimitError: Rate limit exceeded. Please try again later. Retry-After: 1";
    let hook =
        format!("if [ \"$I\" = i2 ] && [ \"$N\" = 1 ]; then echo '{quota}' 1>&2; exit 1; fi");
    let cfg_path = cfg.path().to_string_lossy().into_owned();
    let _env = Env::set(
        &stub(root, &hook),
        &[
            ("APB_QUOTA_BACKOFF_MS", "600000,1200000"),
            ("APB_CONFIG_DIR", &cfg_path),
        ],
    );
    apb_core::run_origin::ensure_key().unwrap();
    let res = run(root, "lst", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let ev = events(root, &res.run_id);
    let paused = ev
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::RunPaused { .. }))
        .count();
    assert_eq!(paused, 1, "the quota pause is journaled once");
    let dispatched = ev
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "work"))
        .count();
    assert_eq!(dispatched, 1, "the node is not dispatched again");
    assert_eq!(requests_of(&ev), 0, "the gate is not asked while paused");
    assert_eq!(runs_of(root, "i2"), 1);
    assert_eq!(runs_of(root, "i4"), 0, "nothing outside the slice ran");
}

/// A pilot gate takes no nonce: a post carrying one is refused and
/// nothing is queued; the same post without it is accepted.
#[test]
fn a_post_with_a_nonce_on_a_pilot_gate_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &playbook(&pilot_list(5, "pilot: { items: 3 }"), "", ""),
        &[],
    );
    let _env = Env::set(&stub(root, ""), &[]);
    let rx = run_in_background(root);
    let run_dir = latest_run_dir(root);
    wait_pending_then(root, &run_dir);
    let err = post_decision_via(
        &run_dir,
        ReviewCommand {
            node: "work".into(),
            decision: "continue".into(),
            note: String::new(),
        },
        Some("n-forged".into()),
        open_seq(&run_dir),
        DecisionChannel::Terminal,
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("a nonce answers a publish node only"),
        "{err}"
    );
    assert!(
        !run_dir.join("reviews.jsonl").is_file()
            || fs::read_to_string(run_dir.join("reviews.jsonl"))
                .unwrap()
                .trim()
                .is_empty(),
        "nothing is queued"
    );
    post_named(&run_dir, "continue", "").unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
}
