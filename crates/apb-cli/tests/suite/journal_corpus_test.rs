//! Journal corpus: released `apb` binaries and this one, run against each
//! other's journals for real (the journal compatibility rules in
//! `CLAUDE.md`, "Conventions").
//!
//! The in-binary tests (`apb-engine`'s `journal_compat_test.rs`) check what
//! this build does with a journal shape; only a released binary can show what
//! an older apb does with a journal this build wrote, and the other way
//! round. Two released readers matter: `v1.0.0-beta.1` and `v0.25.0`.
//!
//! The old binaries come from `APB_OLD_BINARIES`, a directory holding
//! `<tag>/apb`, built by `scripts/old-apb.sh <tag> <dir>`. Without them each
//! case prints one line and passes; with `APB_CORPUS_REQUIRED=1` (CI's
//! `journal-corpus` job) a missing binary fails it.
//!
//! Every case gets its own project and its own temporary `HOME` and
//! `XDG_CONFIG_HOME`, shared by every binary of the case: the old binaries'
//! registry and installation key writes stay in the case, and each binary
//! sees the same installation key when it checks a run's origin stamp.
//! `APB_CONFIG_DIR` is removed (the old binaries resolve the config dir from
//! `XDG_CONFIG_HOME` like this one does), so nothing reaches the real config
//! dir either way.
//!
//! The writers run the script-only playbooks of
//! `tests/fixtures/journal-corpus/` (a script, a sub-playbook node, a
//! `human_review` gate), so no agent is needed. A writer's state is saved
//! once and every operation of a reader runs on a fresh copy of it, in
//! order, in the same project path: the run's journal names its execution
//! root, so a copy elsewhere would not be the same run.
//!
//! Rows 1 to 6, 9, 10 and 11 are runs real writers produced; rows 7 and 8
//! are synthetic journals that test readers and resumers only, and rows 12
//! and 13 apply a synthetic patch (no binary takes a patch from the command
//! line), as does row 14; each says so in its name.
//!
//! Rows 10 to 13 cover edge addresses: a bounded duplicate pair, judge edges
//! and patched runs, asserting the hops, the counted traversals and the
//! nodes reached explicitly. Row 14 covers the definition digests every
//! drive journals (`run_started`, `run_resumed`, `patch_applied`) across a
//! crashed patch, a resume and a later patch. The parts of rows 13, 14 and
//! D1 to D6 and D8 that need no old binary run without one (D7's writer is
//! beta.1, D9 needs none at all).
//!
//! Rows 3 and 4 start an `agent_launch: ask` playbook through the launch
//! refusal's nonce, so the manifest holds a launch record; rows 6 and 9
//! resume a run an old binary started, so `run_resumed` holds one. The old
//! readers read and resume those records; they do not enforce them.
//!
//! Rows D1 to D8 cover deferred retries (`retry_delay`): the new writer runs
//! `corpus-deferred` with a stub agent (`APB_AGENT_CMD`) that fails its
//! first calls, and leaves a deferral open behind a sibling's checkpoint and
//! its guard (D1), released (D2), paused (D3), cut inside the guard's write
//! (D4, synthetic), crashed before any checkpoint (D5) or closed (D6, D8);
//! in D7 beta.1 starts the run and the new binary resumes it; in D9 the new
//! binary's driver dies during the delay and the new binary resumes it.
//!
//! Rows D11 to D13 cover the read-only node and unattended mode, with
//! their playbooks inline: the old binaries load a playbook with every
//! newer field (D11); an unattended run with `review_decided.decided_by`
//! and `read_only_checked` is read, stopped and resumed by them and stays
//! unattended for the new binary (D12); `read_only_violated` before its
//! `node_finished` is skipped and routed the same (D13). The new-binary
//! part of each runs without the old binaries.
//!
//! Row E1 covers result snapshots: the new binary stamps `node_finished`
//! and `list_item_settled` with `result` and stores the snapshots under
//! `results/`, and a synthetic `artifacts_committed` carries `attempt` and
//! `attempts`; both old binaries read the run, approve and resume it with
//! the new binary's routing and stop it, and the new binary still reads
//! every snapshot verified afterwards.
//!
//! Rows X1 and X2 cover execution identity: the new binary runs
//! Rows X1 and X2 cover execution identity: the new binary runs
//! `corpus-exec` with a stub agent, whose primary reply the
//! `success_check` rejects, so the fallback is entered with its reason; a
//! `--now` pause stops the closing answer's composer, and a synthetic host
//! transition joins the journal. Both old binaries read, resume and stop
//! that run, routing as the new binary does (X1). In X2 beta.1 writes a
//! fallback run with no recorded position or reason, and the new binary's
//! `apb stats` and `apb runs` read it as not recorded, never reclassified.
//! Rows E2 to E8 cover the run guard of approvals and person-only gates
//! (`run_guard`, written after every checkpoint while the definition uses
//! `approves`, `person_only` or `requires_approval`), with their playbook
//! inline. E2: a run waiting at a bound person-only gate; the old binaries
//! read it, refuse to resume it and stop it, and their `apb review` leaves
//! an unstamped entry the new drive never consumes. E3: the run paused at
//! that gate; old resumes refuse, the new one continues. E4: two sibling
//! checkpoints before the gate, each followed by the guard; old resumes
//! refuse after each. E5 pins a stated limit, not a guarantee: after an old
//! binary stopped a guarded run, its own `run_aborted` is the last
//! checkpoint, and its second resume continues past the person-only gate
//! and runs `ship`; the new binary reads the run as it ended, with no
//! person named as the decider. E6 (synthetic fault): the guard's
//! line after the last checkpoint is torn; every resume refuses. E7
//! (synthetic journal shape): a guard ended by `run_guard []` after a
//! patch, and the next checkpoint carries no guard; beta.1 then resumes it
//! with the new binary's routing. No supervisor patch writes this shape
//! (it may not drop a guarded feature); the row pins how every binary
//! reads an ended guard. E8 (synthetic): a `run_guard` naming a
//! feature no apb knows; the new binary refuses to resume and stops it.
//!
//! Row E9 covers evidence for node outputs: a definition with `evidence`
//! carries a `run_guard` naming `evidence` after `run_provenance`; both old
//! binaries read the run, refuse to resume it and stop it.
//!
//! Row S1 covers scenario runs: a quick scenario run the new binary plays
//! (`apb eval --level quick`) lives under `.apb/scenario-runs/` with
//! `origin: scenario` in its manifest; neither old binary lists it, shows
//! it or resumes it.
//!
//! Rows 3 and 4 run with a real notification subscription the new writer
//! takes at start: a filtered end-of-run notice (`finished:failed`) and the
//! `host_task` transition, sent through a connector of the case's config
//! dir to a local HTTP sink the test counts. An old reader's resume must
//! send nothing; a positive control (the same state with a plain `finished`
//! subscription) shows the sink, the target and its trust can carry a
//! notice, so "nothing sent" is not vacuous.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common::RunGuard;

const BETA1: &str = "v1.0.0-beta.1";
const V025: &str = "v0.25.0";

/// The writer playbook and the sub-playbook it calls.
const PLAYBOOKS: [&str; 2] = ["corpus", "corpus-child"];
const PLAYBOOK: &str = "corpus";

/// Longest wait for anything a case waits on: a command, a journal record.
const DEADLINE: Duration = Duration::from_secs(60);

/// A type no apb knows: a record a newer apb might write.
const FUTURE_TYPE: &str = "corpus_future_record";

/// The notification sink connector (`tests/fixtures/journal-corpus-sink`).
const SINK: &str = "corpus-sink";

/// The subscription rows 3 and 4 start with, as the journal names it.
const FILTERED_ON: [&str; 2] = ["finished:failed", "host_task"];

// ---------------------------------------------------------------- the sink

/// An HTTP service on `127.0.0.1:0` that answers every request with a sent
/// message and counts the requests.
struct Sink {
    base_url: String,
    count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Sink {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, stopped) = (count.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => return,
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                drain_request(&mut stream);
                seen.fetch_add(1, Ordering::SeqCst);
                let body = r#"{"id":"m-1"}"#;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        Sink {
            base_url,
            count,
            stop,
            handle: Some(handle),
        }
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Reads one request's head and body.
fn drain_request(stream: &mut std::net::TcpStream) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).to_lowercase();
    let length: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while raw.len() < head_end + length {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
    }
}

// ---------------------------------------------------------------- binaries

/// The released binary of `tag`, or `None` (with one line on stderr) when the
/// corpus is not required and the binary is not there.
fn old_binary(tag: &str, case: &Case) -> Option<PathBuf> {
    let required = std::env::var("APB_CORPUS_REQUIRED").is_ok_and(|v| v == "1");
    let dir = std::env::var_os("APB_OLD_BINARIES").filter(|d| !d.is_empty());
    let found = dir
        .map(|d| PathBuf::from(d).join(tag).join("apb"))
        .filter(|p| p.is_file());
    let Some(bin) = found else {
        assert!(
            !required,
            "APB_CORPUS_REQUIRED=1 but no {tag} binary under APB_OLD_BINARIES \
             (build it with scripts/old-apb.sh {tag} <dir>)"
        );
        eprintln!(
            "journal corpus: {} skipped, no {tag} binary (set APB_OLD_BINARIES)",
            case.name
        );
        return None;
    };
    // In the case's own home and bounded like every other command: nothing
    // an old binary does at start reaches the real config dir.
    let out = case.run(&bin, &["--version"]);
    assert!(
        out.ok(),
        "{}: {} --version: {out:?}",
        case.name,
        bin.display()
    );
    assert_eq!(
        out.stdout.trim(),
        format!("apb {}", tag.trim_start_matches('v')),
        "{} is not the {tag} binary",
        bin.display()
    );
    Some(bin)
}

/// Both released readers, beta.1 and 0.25.0, or `None` when either is
/// missing (see [`old_binary`]): the part of a row that needs them is
/// skipped, the rest of the row still runs.
fn old_readers(case: &Case) -> Option<(PathBuf, PathBuf)> {
    Some((old_binary(BETA1, case)?, old_binary(V025, case)?))
}

fn new_binary() -> PathBuf {
    crate::common::apb_bin().to_path_buf()
}

// ---------------------------------------------------------------- one case

/// What a command printed and how it ended.
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

impl std::fmt::Debug for Ran {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exit {:?}\n--- stdout\n{}\n--- stderr\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

/// One corpus case: a project, a shared home for every binary, and the
/// saved writer state its operations start from.
struct Case {
    /// Kills the drivers a case started, however the case ends. Declared
    /// before the directory so they drop first.
    guards: Vec<RunGuard>,
    dir: tempfile::TempDir,
    name: &'static str,
    /// Extra environment every command of the case gets (a stub agent, a
    /// release threshold), in order; a later entry wins.
    env: Vec<(&'static str, String)>,
}

impl Case {
    fn new(name: &'static str) -> Self {
        // Under cargo's target dir, never /tmp: old binaries write their
        // whole config there.
        let parent = Path::new(env!("CARGO_TARGET_TMPDIR"));
        fs::create_dir_all(parent).expect("create CARGO_TARGET_TMPDIR");
        let dir = tempfile::Builder::new()
            .prefix("apb-journal-corpus-")
            .tempdir_in(parent)
            .expect("case dir");
        for sub in ["home/.config", "project", "logs"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        Self {
            guards: Vec::new(),
            dir,
            name,
            env: Vec::new(),
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("project")
    }

    fn runs(&self) -> PathBuf {
        self.root().join(".apb/runs")
    }

    fn run_dir(&self, run: &str) -> PathBuf {
        self.runs().join(run)
    }

    fn saved(&self) -> PathBuf {
        self.dir.path().join("saved-runs")
    }

    /// A command of `bin` in the project, with the case's home and nothing
    /// of the developer's or of an enclosing run.
    fn command(&self, bin: &Path, args: &[&str]) -> Command {
        let home = self.dir.path().join("home");
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .current_dir(self.root())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env_remove("APB_CONFIG_DIR")
            .env_remove("APB_RUN_ID")
            .env_remove("APB_HOST_TASK")
            .env_remove("APB_ATTEMPT_TOKEN")
            // Only a case that writes a decisions.yaml (row 11) uses it.
            .env("APB_DECISIONS_ALLOW_FAKE", "1")
            .stdin(Stdio::null());
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }

    /// Runs a command to its end, bounded by [`DEADLINE`]. Output goes to
    /// files, not pipes: a detached driver must not hold a pipe the test
    /// reads to its end.
    fn run(&self, bin: &Path, args: &[&str]) -> Ran {
        let mut child = self.spawn(bin, args);
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = child.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.child.kill();
                let _ = child.child.wait();
                panic!(
                    "{}: `apb {}` did not end in time",
                    self.name,
                    args.join(" ")
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        child.ran(status.code())
    }

    /// Starts a command and returns at once (a foreground driver the case
    /// kills later).
    fn spawn(&self, bin: &Path, args: &[&str]) -> Spawned {
        let logs = self.dir.path().join("logs");
        let n = fs::read_dir(&logs).unwrap().count();
        let out = logs.join(format!("{n}.out"));
        let err = logs.join(format!("{n}.err"));
        let child = self
            .command(bin, args)
            .stdout(File::create(&out).unwrap())
            .stderr(File::create(&err).unwrap())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        Spawned { child, out, err }
    }

    /// `apb init` by the writer, then the corpus playbooks.
    fn seed(&self, writer: &Path) {
        let ran = self.run(writer, &["init"]);
        assert!(ran.ok(), "{}: apb init: {ran:?}", self.name);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal-corpus");
        for id in PLAYBOOKS {
            let pdir = self.root().join(".apb/playbooks").join(id);
            copy_tree(&fixtures.join(id), &pdir.join("1.0.0"));
            fs::write(pdir.join("current"), "1.0.0").unwrap();
        }
    }

    /// Starts the writer playbook detached and returns its run id.
    fn start(&mut self, writer: &Path) -> String {
        let ran = self.run(writer, &["run", PLAYBOOK, "--detach"]);
        assert!(ran.ok(), "{}: apb run --detach: {ran:?}", self.name);
        let run = ran
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("run started: "))
            .unwrap_or_else(|| panic!("{}: no run id in {ran:?}", self.name))
            .trim()
            .to_string();
        self.guards.push(RunGuard::new(&self.root(), &run));
        run
    }

    /// The run's journal, one JSON value per line, read without any apb
    /// type: the old binaries' records are checked the same way.
    fn journal(&self, run: &str) -> Vec<Value> {
        journal_at(&self.run_dir(run))
    }

    /// Polls until `pred` holds for the run's journal.
    fn wait_journal(&self, run: &str, what: &str, pred: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let start = Instant::now();
        loop {
            let journal = self.journal(run);
            if pred(&journal) {
                return journal;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "{}: timed out waiting for {what}; journal:\n{}",
                self.name,
                fs::read_to_string(self.run_dir(run).join("events.jsonl")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_file(&self, name: &str) {
        let start = Instant::now();
        while !self.root().join(name).exists() {
            assert!(
                start.elapsed() < DEADLINE,
                "{}: timed out waiting for {name}",
                self.name
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Kills the run's driver group (the driver and the script it runs) and
    /// waits until the driver is gone, so the saved state is a lost driver.
    fn kill_driver(&mut self, run: &str) {
        let pid = apb_engine::driver::read_driver_pid(&self.run_dir(run))
            .unwrap_or_else(|| panic!("{}: run {run} has no driver.pid", self.name));
        crate::common::sig::kill_group(pid);
        crate::common::sig::kill_pid(pid);
        let start = Instant::now();
        while crate::common::sig::alive(pid) {
            assert!(
                start.elapsed() < DEADLINE,
                "{}: driver {pid} did not die",
                self.name
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        self.forget_driver(run);
    }

    /// Drops the guard of `run` once its driver is gone: the saved state
    /// keeps a `driver.pid` naming the dead process, whose pid may belong to
    /// another process by the time the case ends.
    fn forget_driver(&mut self, run: &str) {
        let dir = self.run_dir(run);
        let (stale, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.guards)
            .into_iter()
            .partition(|g| g.guards(&dir));
        self.guards = keep;
        // Not dropped: a guard's drop signals the pid in `driver.pid`.
        stale.into_iter().for_each(std::mem::forget);
    }

    /// Saves every run directory (the run and its child runs) as the
    /// writer left them.
    fn save(&self) {
        copy_tree(&self.runs(), &self.saved());
    }

    /// Puts the saved runs back, so the next operation starts from the
    /// writer's state and nothing an earlier operation wrote.
    fn restore(&self) {
        fs::remove_dir_all(self.runs()).unwrap();
        copy_tree(&self.saved(), &self.runs());
    }

    // ----------------------------------------------------- operations

    /// `apb runs <id>`: the reader reads the whole journal.
    fn read(&self, reader: &Path, run: &str) -> Ran {
        self.restore();
        let ran = self.run(reader, &["runs", run]);
        assert!(ran.ok(), "{}: {} runs: {ran:?}", self.name, label(reader));
        assert!(
            ran.stdout.contains(run) && ran.stdout.contains("work"),
            "{}: {} runs shows the run and its nodes: {ran:?}",
            self.name,
            label(reader)
        );
        ran
    }

    /// Approves the gate, then `apb resume`, which drives the run to its
    /// end in the foreground.
    fn approve_and_resume(&self, reader: &Path, run: &str) {
        self.restore();
        self.approve_and_resume_restored(reader, run);
    }

    /// [`Case::approve_and_resume`] on the runs as they are now.
    fn approve_and_resume_restored(&self, reader: &Path, run: &str) {
        let ran = self.run(reader, &["review", run, "gate", "--decision", "approved"]);
        assert!(ran.ok(), "{}: {} review: {ran:?}", self.name, label(reader));
        let ran = self.run(reader, &["resume", run]);
        assert!(ran.ok(), "{}: {} resume: {ran:?}", self.name, label(reader));
        let journal = self.journal(run);
        assert_eq!(
            last_of(&journal, "run_finished").map(|e| e["outcome"].clone()),
            Some(Value::from("succeeded")),
            "{}: {} resumed the run to a success",
            self.name,
            label(reader)
        );
        assert!(
            journal
                .iter()
                .any(|e| e["type"] == "node_finished" && e["node"] == "ok"),
            "{}: the approved branch ran",
            self.name
        );
    }

    /// `apb stop <id>` on a run no driver drives: it ends with `run_aborted`.
    fn stop(&self, reader: &Path, run: &str) {
        self.restore();
        let ran = self.run(reader, &["stop", run]);
        assert!(ran.ok(), "{}: {} stop: {ran:?}", self.name, label(reader));
        let journal = self.journal(run);
        assert_eq!(
            journal.last().map(|e| e["type"].clone()),
            Some(Value::from("run_aborted")),
            "{}: {} stop appends run_aborted last",
            self.name,
            label(reader)
        );
    }

    /// Read, resume to the end, stop: each on its own copy.
    fn read_resume_stop(&self, reader: &Path, run: &str) {
        let ran = self.read(reader, run);
        assert!(
            !ran.stdout.contains("unknown event"),
            "{}: {} skipped nothing: {ran:?}",
            self.name,
            label(reader)
        );
        self.approve_and_resume(reader, run);
        self.stop(reader, run);
    }

    /// The sink connector, installed in the case's config dir with an
    /// account pointing at `sink`, both approved by `binary`.
    fn install_sink(&self, binary: &Path, sink: &Sink) {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal-corpus-sink");
        let dst = self
            .dir
            .path()
            .join("home/.config/apb/connectors")
            .join(SINK);
        fs::create_dir_all(&dst).unwrap();
        fs::copy(fixture.join("connector.yaml"), dst.join("connector.yaml")).unwrap();
        let accounts = self.root().join(".apb/connector-config");
        fs::create_dir_all(&accounts).unwrap();
        fs::write(
            accounts.join(format!("{SINK}.yaml")),
            format!(
                "accounts:\n  - name: me\n    default: true\n    base_url: {}\n",
                sink.base_url
            ),
        )
        .unwrap();
        for args in [
            vec!["connector", "approve", SINK],
            vec!["connector", "approve", SINK, "--account", "me"],
        ] {
            let ran = self.run(binary, &args);
            assert!(ran.ok(), "{}: {}: {ran:?}", self.name, args.join(" "));
        }
    }

    /// The `on` names of the run's `notify_set`, read without any apb type.
    fn notify_on(journal: &[Value]) -> Option<Vec<String>> {
        journal.iter().find(|e| e["type"] == "notify_set").map(|e| {
            e["on"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect()
        })
    }

    /// Restores the saved runs with the subscription's names replaced by
    /// `on`, approves the gate and resumes to the end with `reader`; how
    /// many notices reached the sink meanwhile.
    fn resume_with_subscription(
        &self,
        reader: &Path,
        run: &str,
        on: &[&str],
        sink: &Sink,
    ) -> usize {
        self.restore();
        rewrite_journal(&self.run_dir(run), |records| {
            records
                .into_iter()
                .map(|mut r| {
                    if r["type"] == "notify_set" {
                        r["on"] = Value::from(on.to_vec());
                    }
                    r
                })
                .collect()
        });
        let before = sink.count();
        self.approve_and_resume_restored(reader, run);
        sink.count() - before
    }

    /// Declares `agent_launch: <policy>` in the seeded writer playbook.
    fn declare_launch(&self, policy: &str) {
        let path = self
            .root()
            .join(".apb/playbooks")
            .join(PLAYBOOK)
            .join("1.0.0/playbook.yaml");
        let yaml = fs::read_to_string(&path).unwrap();
        let yaml = yaml.replacen(
            "version: 1.0.0\n",
            &format!("version: 1.0.0\nagent_launch: {policy}\n"),
            1,
        );
        assert!(yaml.contains("agent_launch:"), "{}: declared", self.name);
        fs::write(path, yaml).unwrap();
    }

    // ----------------------------------------------------- writers

    /// [`Case::write_waiting_at_review`] by the new binary, with the run's
    /// own subscription: the end-of-run notice only for `failed`, and host
    /// tasks, through the sink. The fixture checks the exact journal names
    /// and that the writer sent nothing.
    fn write_notified_waiting_at_review(&mut self, sink: &Sink) -> String {
        let new = new_binary();
        self.seed(&new);
        self.install_sink(&new, sink);
        let via = format!("--notify-via={SINK}/me:notify_person");
        let mut args = vec![
            "run",
            PLAYBOOK,
            "--detach",
            "--notify-on=finished,host_task",
            "--notify-finished=failed",
            &via,
            "--notify-to=42",
        ];
        // The writer playbook asks before an agent launches it: the start
        // without a terminal is refused with the launch nonce, and the
        // retry that echoes it is admitted with a `cli_flag` record.
        self.declare_launch("ask");
        let refused = self.run(&new, &args);
        assert!(
            !refused.ok() && refused.stderr.contains("launch_requires_confirmation"),
            "{}: an `ask` start without a terminal is refused: {refused:?}",
            self.name
        );
        let confirm = launch_flags(&refused.stderr)
            .unwrap_or_else(|| panic!("{}: no launch flags in {refused:?}", self.name));
        args.extend(confirm.iter().map(String::as_str));
        let ran = self.run(&new, &args);
        assert!(
            ran.ok(),
            "{}: apb run --detach --notify: {ran:?}",
            self.name
        );
        let run = ran
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("run started: "))
            .unwrap_or_else(|| panic!("{}: no run id in {ran:?}", self.name))
            .trim()
            .to_string();
        self.guards.push(RunGuard::new(&self.root(), &run));
        let journal = self.wait_journal(&run, "the review gate", |j| {
            j.iter().any(|e| e["type"] == "review_requested")
        });
        assert_eq!(
            Self::notify_on(&journal),
            Some(FILTERED_ON.iter().map(|s| s.to_string()).collect()),
            "{}: the new writer journals the exact names",
            self.name
        );
        let manifest = fs::read_to_string(self.run_dir(&run).join("manifest.yaml")).unwrap();
        assert!(
            manifest.contains("launch:")
                && manifest.contains("policy: ask")
                && manifest.contains("confirmation: cli_flag"),
            "{}: the new writer records the launch authorization: {manifest}",
            self.name
        );
        let nonce = confirm
            .iter()
            .find_map(|f| f.strip_prefix("--confirm-launch="))
            .unwrap_or_else(|| panic!("{}: no nonce in {confirm:?}", self.name));
        assert!(
            !manifest.contains(nonce),
            "{}: the record holds no nonce: {manifest}",
            self.name
        );
        self.kill_driver(&run);
        assert_eq!(sink.count(), 0, "{}: the writer sent nothing", self.name);
        self.save();
        run
    }

    /// The writer runs the playbook to its review gate; the state saved is
    /// that run waiting at the gate with its driver gone.
    fn write_waiting_at_review(&mut self, writer: &Path) -> String {
        self.seed(writer);
        let run = self.start(writer);
        self.wait_journal(&run, "the review gate", |j| {
            j.iter().any(|e| e["type"] == "review_requested")
        });
        self.kill_driver(&run);
        self.save();
        run
    }

    /// The writer runs the corpus fixture `id` to its review gate; the
    /// state saved is that run waiting at the gate with its driver gone.
    fn write_playbook_waiting_at_review(&mut self, writer: &Path, id: &str) -> String {
        self.write_playbook_waiting_at_review_with(writer, id, |_| {})
    }

    /// [`Case::write_playbook_waiting_at_review`] with `prepare` run on the
    /// project after the seed and before the start.
    fn write_playbook_waiting_at_review_with(
        &mut self,
        writer: &Path,
        id: &str,
        prepare: impl FnOnce(&Path),
    ) -> String {
        self.seed(writer);
        prepare(&self.root());
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal-corpus");
        let pdir = self.root().join(".apb/playbooks").join(id);
        copy_tree(&fixtures.join(id), &pdir.join("1.0.0"));
        fs::write(pdir.join("current"), "1.0.0").unwrap();
        let ran = self.run(writer, &["run", id, "--detach"]);
        assert!(ran.ok(), "{}: apb run {id} --detach: {ran:?}", self.name);
        let run = ran
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("run started: "))
            .unwrap_or_else(|| panic!("{}: no run id in {ran:?}", self.name))
            .trim()
            .to_string();
        self.guards.push(RunGuard::new(&self.root(), &run));
        self.wait_journal(&run, "the review gate", |j| {
            j.iter().any(|e| e["type"] == "review_requested")
        });
        self.kill_driver(&run);
        self.save();
        run
    }

    /// The writer's driver is killed while the script node runs: the
    /// journal has an open attempt and the run looks live.
    fn write_killed_mid_node(&mut self, writer: &Path) -> String {
        self.seed(writer);
        fs::write(self.root().join("hold"), "").unwrap();
        let run = self.start(writer);
        self.wait_file("work.started");
        self.wait_journal(&run, "the work node to start", |j| {
            j.iter()
                .any(|e| e["type"] == "node_started" && e["node"] == "work")
        });
        self.kill_driver(&run);
        fs::remove_file(self.root().join("hold")).unwrap();
        let journal = self.journal(&run);
        assert!(
            !journal
                .iter()
                .any(|e| e["type"] == "node_finished" && e["node"] == "work"),
            "{}: the work node is still open",
            self.name
        );
        self.save();
        run
    }
}

/// A command started by [`Case::spawn`].
struct Spawned {
    child: std::process::Child,
    out: PathBuf,
    err: PathBuf,
}

impl Spawned {
    fn ran(&self, code: Option<i32>) -> Ran {
        Ran {
            code,
            stdout: fs::read_to_string(&self.out).unwrap_or_default(),
            stderr: fs::read_to_string(&self.err).unwrap_or_default(),
        }
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn label(bin: &Path) -> String {
    if bin == crate::common::apb_bin() {
        "new apb".to_string()
    } else {
        bin.parent()
            .and_then(|p| p.file_name())
            .map(|t| format!("apb {}", t.to_string_lossy()))
            .unwrap_or_else(|| bin.display().to_string())
    }
}

fn journal_at(run_dir: &Path) -> Vec<Value> {
    let Ok(raw) = fs::read_to_string(run_dir.join("events.jsonl")) else {
        return Vec::new();
    };
    // A live writer may be finishing its last line: only whole lines count.
    raw.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn last_of<'a>(journal: &'a [Value], kind: &str) -> Option<&'a Value> {
    journal.iter().rev().find(|e| e["type"] == kind)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_tree(&src, &dst);
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&src).unwrap(), &dst).unwrap();
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

/// Rewrites the run's journal: `edit` gets the records and returns the new
/// list; `seq` is renumbered in order, as one writer would have.
fn rewrite_journal(run_dir: &Path, edit: impl FnOnce(Vec<Value>) -> Vec<Value>) {
    let records = edit(journal_at(run_dir));
    let mut text = String::new();
    for (seq, mut record) in records.into_iter().enumerate() {
        record["seq"] = Value::from(seq);
        text.push_str(&serde_json::to_string(&record).unwrap());
        text.push('\n');
    }
    fs::write(run_dir.join("events.jsonl"), text).unwrap();
}

/// The `--confirm-launch`, `--launch-version` and `--launch-choice` flags a
/// launch refusal prints.
fn launch_flags(stderr: &str) -> Option<Vec<String>> {
    ["--confirm-launch=", "--launch-version=", "--launch-choice="]
        .into_iter()
        .map(|flag| {
            let at = stderr.find(flag)?;
            let value: String = stderr[at + flag.len()..]
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            (!value.is_empty()).then(|| format!("{flag}{value}"))
        })
        .collect()
}

/// The `launch` of the latest `run_resumed`, read without any apb type.
fn resumed_launch(journal: &[Value]) -> Option<Value> {
    last_of(journal, "run_resumed").and_then(|e| e.get("launch").cloned())
}

fn future_record(ts: &Value) -> Value {
    serde_json::json!({ "seq": 0, "ts": ts, "type": FUTURE_TYPE, "node": "work" })
}

// ---------------------------------------------------------------- rows

/// Row 1: 0.25.0 writes a run waiting at review; the new binary reads it,
/// approves and resumes it to its end, and stops it.
#[test]
fn row1_v025_writer_new_reader() {
    let mut case = Case::new("row 1");
    let Some(v025) = old_binary(V025, &case) else {
        return;
    };
    let run = case.write_waiting_at_review(&v025);
    case.read_resume_stop(&new_binary(), &run);
}

/// Row 2: beta.1 writes a run waiting at review; the new binary reads,
/// resumes to the end and stops.
#[test]
fn row2_beta1_writer_new_reader() {
    let mut case = Case::new("row 2");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let run = case.write_waiting_at_review(&beta1);
    case.read_resume_stop(&new_binary(), &run);
}

/// Row 3: the new binary writes a run waiting at review, with its own
/// subscription (`finished:failed`, `host_task`); beta.1 reads, resumes to
/// the end and stops. beta.1 drops both names it does not know, so its
/// resume to a success sends nothing; with a plain `finished` the same
/// resume sends one notice (the positive control).
#[test]
fn row3_new_writer_beta1_reader() {
    let mut case = Case::new("row 3");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let sink = Sink::spawn();
    let run = case.write_notified_waiting_at_review(&sink);
    let ran = case.read(&beta1, &run);
    assert!(
        !ran.stdout.contains("unknown event"),
        "row 3: beta.1 knows every record: {ran:?}"
    );
    assert_eq!(
        case.resume_with_subscription(&beta1, &run, &FILTERED_ON, &sink),
        0,
        "row 3: beta.1 sends no notice for a filtered subscription"
    );
    case.stop(&beta1, &run);
    assert_eq!(
        case.resume_with_subscription(&beta1, &run, &["finished"], &sink),
        1,
        "row 3: positive control, beta.1 sends the plain end-of-run notice"
    );
}

/// Row 4: the same state as row 3, read, resumed and stopped by 0.25.0,
/// which has no notifications: it skips `notify_set` as an unknown record
/// before a checkpoint, and its resume sends nothing. The positive control
/// is the new binary's resume of the plain `finished` copy.
#[test]
fn row4_new_writer_v025_reader() {
    let mut case = Case::new("row 4");
    let Some(v025) = old_binary(V025, &case) else {
        return;
    };
    let sink = Sink::spawn();
    let run = case.write_notified_waiting_at_review(&sink);
    let ran = case.read(&v025, &run);
    assert!(
        ran.stdout.contains("unknown event"),
        "row 4: 0.25.0 skips the subscription record and says so: {ran:?}"
    );
    assert_eq!(
        case.resume_with_subscription(&v025, &run, &FILTERED_ON, &sink),
        0,
        "row 4: 0.25.0 sends nothing"
    );
    case.stop(&v025, &run);
    assert_eq!(
        case.resume_with_subscription(&new_binary(), &run, &["finished"], &sink),
        1,
        "row 4: positive control, the new binary sends the plain end-of-run notice"
    );
}

/// Row 5: the new binary's driver is killed mid-node (an open attempt, a
/// run that looks live); beta.1 and 0.25.0 each stop it.
#[test]
fn row5_new_writer_killed_mid_node_old_binaries_stop() {
    let mut case = Case::new("row 5");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let Some(v025) = old_binary(V025, &case) else {
        return;
    };
    let run = case.write_killed_mid_node(&new_binary());
    case.stop(&beta1, &run);
    case.stop(&v025, &run);
}

/// Row 6: one journal, two writers. beta.1 starts the run and pauses it
/// (drain) while the script node runs; the new binary resumes it to the
/// review gate. Before beta.1 reads, the journal holds beta.1's records
/// exactly as beta.1 left them (start to pause), then the new binary's
/// (`run_resumed` and every record after it).
#[test]
fn row6_beta1_starts_new_resumes_beta1_reads() {
    let mut case = Case::new("row 6");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let new = new_binary();
    case.seed(&beta1);
    fs::write(case.root().join("hold"), "").unwrap();
    let run = case.start(&beta1);
    case.wait_file("work.started");
    let ran = case.run(&beta1, &["pause", &run]);
    assert!(ran.ok(), "row 6: beta.1 pause: {ran:?}");
    fs::remove_file(case.root().join("hold")).unwrap();
    case.wait_journal(&run, "beta.1 to journal the pause", |j| {
        j.last().is_some_and(|e| e["type"] == "run_paused")
    });
    let driver = apb_engine::driver::read_driver_pid(&case.run_dir(&run));
    if let Some(pid) = driver {
        let start = Instant::now();
        while crate::common::sig::alive(pid) {
            assert!(start.elapsed() < DEADLINE, "row 6: beta.1's driver exits");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // Both views of what beta.1 left, taken once its driver is gone, from
    // one read of the file.
    let beta1_text = fs::read_to_string(case.run_dir(&run).join("events.jsonl")).unwrap();
    let by_beta1: Vec<Value> = beta1_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        by_beta1[0]["type"], "run_started",
        "row 6: beta.1 started it"
    );
    assert_eq!(
        by_beta1.last().map(|e| e["type"].clone()),
        Some(Value::from("run_paused")),
        "row 6: beta.1's last record is its pause"
    );

    // The new binary resumes in the foreground; it parks at the gate and is
    // killed there, a lost driver like the other rows' writers.
    let resume = case.spawn(&new, &["resume", &run]);
    case.wait_journal(&run, "the new binary to reach the review gate", |j| {
        j.iter().any(|e| e["type"] == "review_requested")
    });
    drop(resume);
    // `driver.pid` names the resume just killed and reaped.
    case.forget_driver(&run);

    let text = fs::read_to_string(case.run_dir(&run).join("events.jsonl")).unwrap();
    assert!(
        text.starts_with(&beta1_text),
        "row 6: beta.1's records are kept as it wrote them"
    );
    let journal = case.journal(&run);
    let added: Vec<&str> = journal[by_beta1.len()..]
        .iter()
        .filter_map(|e| e["type"].as_str())
        .collect();
    assert_eq!(
        added.first(),
        Some(&"run_resumed"),
        "row 6: the new binary's first record is its resume: {added:?}"
    );
    // beta.1 recorded no launch authorization, so the new binary's resume
    // checks one and journals it on `run_resumed`, persisting the child pins
    // it checked; beta.1 reads both below.
    let launch = resumed_launch(&journal).expect("row 6: the resume records its launch");
    assert_eq!(launch["kind"], "resume", "row 6: {launch}");
    assert_eq!(launch["policy"], "allow", "row 6: {launch}");
    for kind in [
        "node_started",
        "child_run_started",
        "node_finished",
        "review_requested",
    ] {
        assert!(
            added.contains(&kind),
            "row 6: the new binary wrote {kind}: {added:?}"
        );
    }
    case.save();

    let ran = case.read(&beta1, &run);
    assert!(
        !ran.stdout.contains("unknown event"),
        "row 6: beta.1 knows every record of the mixed journal: {ran:?}"
    );
    // `sub` ran under the new binary only: its row reads succeeded only if
    // beta.1 read the new binary's records.
    let sub_line = ran
        .stdout
        .lines()
        .find(|l| l.split_whitespace().next() == Some("sub"))
        .unwrap_or_else(|| panic!("row 6: beta.1 lists the sub node: {ran:?}"));
    assert!(
        sub_line.contains("succeeded"),
        "row 6: beta.1 reads the new binary's records too: {ran:?}"
    );
}

/// Row 9: 0.25.0 writes a run waiting at review, without a launch record
/// or child pins; the new binary approves and resumes it to its end, which
/// journals `run_resumed.launch` and persists the pins it checked; 0.25.0
/// then reads the whole journal without skipping anything.
#[test]
fn row9_v025_writer_new_resumes_v025_reads_the_launch_record() {
    let mut case = Case::new("row 9");
    let Some(v025) = old_binary(V025, &case) else {
        return;
    };
    let run = case.write_waiting_at_review(&v025);
    case.approve_and_resume(&new_binary(), &run);
    let launch = resumed_launch(&case.journal(&run))
        .expect("row 9: the new binary's resume records its launch");
    assert_eq!(launch["kind"], "resume", "row 9: {launch}");
    case.save();
    let ran = case.read(&v025, &run);
    assert!(
        !ran.stdout.contains("unknown event"),
        "row 9: 0.25.0 reads `run_resumed` with its launch record: {ran:?}"
    );
}

/// Row 7 (synthetic, readers and resumers only): a record of a type no apb
/// knows after the last checkpoint. The new binary and beta.1 read the run
/// and show the count, refuse to resume it, and stop it.
#[test]
fn row7_synthetic_unknown_record_after_last_checkpoint() {
    let mut case = Case::new("row 7");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let run = case.write_waiting_at_review(&new_binary());
    rewrite_journal(&case.saved().join(&run), |mut records| {
        let ts = records.last().unwrap()["ts"].clone();
        records.push(future_record(&ts));
        records
    });
    for reader in [new_binary(), beta1] {
        let ran = case.read(&reader, &run);
        assert!(
            ran.stdout.contains("1 unknown event"),
            "row 7: {} shows the count: {ran:?}",
            label(&reader)
        );

        case.restore();
        let ran = case.run(&reader, &["resume", &run]);
        assert!(
            !ran.ok(),
            "row 7: {} refuses to resume: {ran:?}",
            label(&reader)
        );
        assert!(
            ran.stderr.contains(FUTURE_TYPE),
            "row 7: {} names the record it does not know: {ran:?}",
            label(&reader)
        );
        assert!(
            last_of(&case.journal(&run), "run_resumed").is_none(),
            "row 7: {} journaled no resume",
            label(&reader)
        );

        case.stop(&reader, &run);
    }
}

/// Row 8 (synthetic, resumers only): a record of a type no apb knows, then
/// a checkpoint written after it by another node (the `sub` node's
/// `node_finished`). Today's rule accepts any later checkpoint, so both the
/// new binary and beta.1 resume the run to its end. This pins that
/// behaviour; a record an execution decision reads needs its own refusal.
#[test]
fn row8_synthetic_unknown_record_then_checkpoint_of_another_node() {
    let mut case = Case::new("row 8");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let run = case.write_waiting_at_review(&new_binary());
    rewrite_journal(&case.saved().join(&run), |mut records| {
        let at = records
            .iter()
            .position(|e| e["type"] == "node_finished" && e["node"] == "sub")
            .expect("the sub node finished");
        let ts = records[at]["ts"].clone();
        records.insert(at, future_record(&ts));
        records
    });
    for reader in [new_binary(), beta1] {
        case.approve_and_resume(&reader, &run);
    }
}

// ------------------------------------------------- edge addresses (17a)

/// The `edge_traversed` records `from -> to` as (uncounted, edges).
fn hops_of(journal: &[Value], from: &str, to: &str) -> Vec<(bool, Value)> {
    journal
        .iter()
        .filter(|e| e["type"] == "edge_traversed" && e["from"] == from && e["to"] == to)
        .map(|e| {
            (
                e["uncounted"].as_bool().unwrap_or(false),
                e.get("edges").cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

/// Counted traversals `from -> to`: neither a policy hop nor `uncounted`.
fn counted(journal: &[Value], from: &str, to: &str) -> usize {
    journal
        .iter()
        .filter(|e| {
            e["type"] == "edge_traversed"
                && e["from"] == from
                && e["to"] == to
                && !e["uncounted"].as_bool().unwrap_or(false)
                && !e["via_policy"].as_bool().unwrap_or(false)
        })
        .count()
}

/// How many times `node` finished.
fn finished(journal: &[Value], node: &str) -> usize {
    journal
        .iter()
        .filter(|e| e["type"] == "node_finished" && e["node"] == node)
        .count()
}

/// Row 10: the new binary writes a run through a bounded duplicate pair
/// (`work -> dup` twice, `max_traversals: 1`) and leaves it at the review
/// gate. Its journal names both addresses on one decision record and on the
/// one counted record. beta.1 and 0.25.0 each read it, resume it to the end
/// and stop it; after the resume the pair was still counted once, `dup` ran
/// once and the run reached `ok`.
#[test]
fn row10_new_writer_bounded_duplicate_pair_old_binaries() {
    let mut case = Case::new("row 10");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let Some(v025) = old_binary(V025, &case) else {
        return;
    };
    let run = case.write_playbook_waiting_at_review(&new_binary(), "corpus-routes");
    let both = serde_json::json!(["e:work>dup#1", "e:work>dup#2"]);
    assert_eq!(
        hops_of(&case.journal(&run), "work", "dup"),
        vec![(true, both.clone()), (false, both)],
        "row 10: one decision record and one counted record, both naming the pair"
    );
    for reader in [beta1, v025] {
        case.read_resume_stop(&reader, &run);
        case.approve_and_resume(&reader, &run);
        let journal = case.journal(&run);
        assert_eq!(
            counted(&journal, "work", "dup"),
            1,
            "row 10: {}",
            label(&reader)
        );
        assert_eq!(finished(&journal, "dup"), 1, "row 10: {}", label(&reader));
        assert_eq!(finished(&journal, "ok"), 1, "row 10: {}", label(&reader));
        assert_eq!(finished(&journal, "no"), 0, "row 10: {}", label(&reader));
    }
}

/// Row 12: beta.1 writes a run through the bounded duplicate pair (no
/// addresses, no definition digests); the new binary's route reads it as
/// not established: the definition is unknown, the hops resolve to no edge
/// and nothing is drawn as not taken.
#[test]
fn row12_beta1_writer_new_route_reads_not_established() {
    let mut case = Case::new("row 12");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let run = case.write_playbook_waiting_at_review(&beta1, "corpus-routes");
    let shown = case.run(&new_binary(), &["runs", &run]);
    assert!(shown.ok(), "row 12: {shown:?}");
    let out = &shown.stdout;
    assert!(out.contains("route:"), "row 12: {out}");
    assert!(
        out.contains("not established: segment 0"),
        "row 12: the definition beta.1 ran is unknown: {out}"
    );
    assert!(
        out.contains("hop work -> dup at seq") && out.contains("not established"),
        "row 12: {out}"
    );
    assert!(!out.contains("not taken"), "row 12: {out}");
}

const JUDGE_DECISIONS: &str = "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      edge_0: { type: noul, noul: 0.1 }\n      edge_1: { type: noul, noul: 0.9 }\nuses:\n  judge_edge: { mode: enforce }\n";

/// Row 11: the new binary routes `work` on two judge edges answered by a
/// fake provider (`fixit` no, `gate` yes; neither edge is taken without an
/// answer) and leaves the run at the review gate. Its judge-edge decision
/// names the edges' addresses. beta.1 and 0.25.0 each read, resume to the
/// end and stop: the routing out of `work` is not asked again, `fixit`
/// never runs and the run reaches `ok`.
#[test]
fn row11_new_writer_judge_edges_old_binaries() {
    let mut case = Case::new("row 11");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let Some(v025) = old_binary(V025, &case) else {
        return;
    };
    let profile = case.root().join(".apb/profiles/corpus");
    let decisions = case.dir.path().join("home/.config/apb/decisions.yaml");
    fs::create_dir_all(decisions.parent().unwrap()).unwrap();
    fs::write(&decisions, JUDGE_DECISIONS).unwrap();
    let run = case.write_playbook_waiting_at_review_with(&new_binary(), "corpus-judge", |root| {
        fs::create_dir_all(root.join(".apb/profiles/corpus")).unwrap();
        fs::write(
            root.join(".apb/profiles/corpus/profile.yaml"),
            "name: corpus\ndescription: never started\nexecutor:\n  agent: claude-code\n  model: haiku\n",
        )
        .unwrap();
        fs::write(root.join(".apb/profiles/corpus/SOUL.md"), "").unwrap();
    });
    assert!(profile.join("profile.yaml").is_file());
    // The old binaries never ask again; no provider is configured for them.
    fs::remove_file(&decisions).unwrap();
    let journal = case.journal(&run);
    let decision = journal
        .iter()
        .find(|e| e["type"] == "decision_made" && e["use_site"] == "judge_edge")
        .unwrap_or_else(|| panic!("row 11: the judge edges were asked"));
    assert_eq!(
        decision["edge_addresses"],
        serde_json::json!({"edge_0": "e:work>fixit#1", "edge_1": "e:work>gate#1"})
    );
    assert_eq!(
        hops_of(&journal, "work", "gate"),
        vec![(true, serde_json::json!(["e:work>gate#1"]))]
    );
    for reader in [beta1, v025] {
        case.read_resume_stop(&reader, &run);
        case.approve_and_resume(&reader, &run);
        let journal = case.journal(&run);
        assert_eq!(
            hops_of(&journal, "work", "gate").len(),
            1,
            "row 11: {}",
            label(&reader)
        );
        assert_eq!(finished(&journal, "fixit"), 0, "row 11: {}", label(&reader));
        assert!(
            !journal.iter().any(|e| e["type"] == "attempt_started"),
            "row 11: {}: no agent started",
            label(&reader)
        );
        assert_eq!(finished(&journal, "ok"), 1, "row 11: {}", label(&reader));
    }
}

/// The routes playbook with `patched` (a condition node) between the
/// approved gate and `ok`, as version 1.0.1.
fn patched_routes() -> String {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/journal-corpus/corpus-routes/playbook.yaml");
    let yaml = fs::read_to_string(fixtures).unwrap();
    let yaml = yaml
        .replace("version: 1.0.0", "version: 1.0.1")
        .replace(
            "  - { id: ok, type: finish, outcome: success }",
            "  - { id: patched, type: condition }\n  - { id: ok, type: finish, outcome: success }",
        )
        .replace(
            "  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }",
            "  - { from: gate, to: patched, condition: { type: review_status, equals: approved } }\n  - { from: patched, to: ok }",
        );
    assert!(yaml.contains("to: patched"));
    yaml
}

/// Applies a patch to the saved run the way a binary writes one: the new
/// snapshot replaces `playbook.yaml`, then `patch_applied` and
/// `run_migrated` are journaled. `with_definition`: the new binary stores
/// the definition first and journals its digest; an older one does neither.
fn synthetic_patch(case: &Case, run: &str, with_definition: bool) {
    synthetic_patch_to(case, run, &patched_routes(), "1.0.0", with_definition);
}

/// [`synthetic_patch`] to `yaml` (whose version is in the yaml) from the
/// version `from`.
fn synthetic_patch_to(case: &Case, run: &str, yaml: &str, from: &str, with_definition: bool) {
    let dir = case.saved().join(run);
    let to = yaml
        .lines()
        .find_map(|l| l.strip_prefix("version: "))
        .unwrap()
        .trim()
        .to_string();
    let definition =
        with_definition.then(|| apb_engine::run_definitions::store(&dir, yaml.as_bytes()).unwrap());
    fs::write(dir.join("playbook.yaml"), yaml).unwrap();
    rewrite_journal(&dir, |mut records| {
        let ts = records.last().unwrap()["ts"].clone();
        let mut applied = serde_json::json!({
            "type": "patch_applied", "ts": ts, "version": to,
            "classification": "improvement", "continue_from": "gate"
        });
        if let Some(d) = &definition {
            applied["definition"] = Value::from(d.clone());
        }
        records.push(applied);
        records.push(serde_json::json!({
            "type": "run_migrated", "ts": ts, "from_version": from,
            "to_version": to, "continue_from": "gate"
        }));
        records
    });
}

/// Row 12 (synthetic patch): the new binary's run waits at the gate, then
/// a patch is applied as the new binary writes one (definition stored,
/// digest journaled). beta.1 and 0.25.0 read it, resume it on the current
/// `playbook.yaml` (the run passes `patched`) and stop it.
#[test]
fn row12_synthetic_new_patch_old_binaries_run_the_current_definition() {
    let mut case = Case::new("row 12");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let Some(v025) = old_binary(V025, &case) else {
        return;
    };
    let run = case.write_playbook_waiting_at_review(&new_binary(), "corpus-routes");
    synthetic_patch(&case, &run, true);
    for reader in [beta1, v025] {
        case.read_resume_stop(&reader, &run);
        case.approve_and_resume(&reader, &run);
        let journal = case.journal(&run);
        assert_eq!(
            finished(&journal, "patched"),
            1,
            "row 12: {}",
            label(&reader)
        );
        assert_eq!(
            counted(&journal, "work", "dup"),
            1,
            "row 12: {}",
            label(&reader)
        );
    }
}

/// Row 13 (synthetic patch): the new binary starts the run, a patch is
/// applied as beta.1 writes one (no stored definition, no digest), and the
/// new binary resumes it: it executes the current `playbook.yaml` (the run
/// passes `patched`) and records a `definition_mismatch` against that
/// patch. beta.1 reads the result.
#[test]
fn row13_synthetic_old_patch_new_resume_records_the_mismatch() {
    let mut case = Case::new("row 13");
    let run = case.write_playbook_waiting_at_review(&new_binary(), "corpus-routes");
    synthetic_patch(&case, &run, false);
    case.approve_and_resume(&new_binary(), &run);
    let journal = case.journal(&run);
    assert_eq!(
        finished(&journal, "patched"),
        1,
        "row 13: the current definition ran"
    );
    let sidecar = fs::read_to_string(case.run_dir(&run).join("diagnostics.jsonl")).unwrap();
    assert!(
        sidecar.contains("\"definition_mismatch\"") && sidecar.contains("patch_without_definition"),
        "row 13: the mismatch is recorded: {sidecar}"
    );
    // Only the read needs the old binary; everything above runs without it.
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let ran = case.run(&beta1, &["runs", &run]);
    assert!(ran.ok(), "row 13: beta.1 reads the resumed run: {ran:?}");
}

/// The digest each drive of `run` journaled, in journal order: the start's,
/// every resume's and every patch's (`null` where none was journaled).
fn journaled_definitions(journal: &[Value]) -> Vec<(String, Value)> {
    journal
        .iter()
        .filter(|e| {
            matches!(
                e["type"].as_str(),
                Some("run_started" | "run_resumed" | "patch_applied")
            )
        })
        .map(|e| {
            (
                e["type"].as_str().unwrap().to_string(),
                e["definition"].clone(),
            )
        })
        .collect()
}

/// Row 14 (synthetic patches): the new binary starts the run on A and it
/// waits at the gate; a patch to B stores B and replaces the snapshot, then
/// "crashes" before `patch_applied`; the new binary resumes it, executing
/// B, and parks at the gate again; a later patch to C is applied as the new
/// binary writes one; the diagnostics sidecar is deleted. The journal alone
/// says the resumed segment ran B, never A. beta.1 and 0.25.0 then read,
/// resume and stop the journal with the new `definition` fields.
#[test]
fn row14_synthetic_crashed_patch_resume_patch_definitions_from_the_journal() {
    use apb_engine::run_definitions::{DefinitionsInForce, Resolved, digest};
    let mut case = Case::new("row 14");
    let new = new_binary();
    let run = case.write_playbook_waiting_at_review(&new, "corpus-routes");
    let a = digest(&fs::read(case.run_dir(&run).join("playbook.yaml")).unwrap());
    // The crashed patch, on the live run.
    let b_yaml = patched_routes();
    let b = apb_engine::run_definitions::store(&case.run_dir(&run), b_yaml.as_bytes()).unwrap();
    fs::write(case.run_dir(&run).join("playbook.yaml"), &b_yaml).unwrap();
    // The new binary resumes it in the foreground; it waits at the gate
    // (the request stands, so nothing more is journaled) and is killed there.
    let before = case.journal(&run).len();
    let resume = case.spawn(&new, &["resume", &run]);
    case.wait_journal(&run, "the resume to journal run_resumed", |j| {
        j.len() > before && j[before..].iter().any(|e| e["type"] == "run_resumed")
    });
    drop(resume);
    case.forget_driver(&run);
    case.save();
    // The later patch to C, as the new binary writes one.
    let c_yaml = b_yaml
        .replace("version: 1.0.1", "version: 1.0.2")
        .replace("patched", "later");
    synthetic_patch_to(&case, &run, &c_yaml, "1.0.1", true);
    let c = digest(c_yaml.as_bytes());
    let _ = fs::remove_file(case.saved().join(&run).join("diagnostics.jsonl"));
    case.restore();

    let journal = case.journal(&run);
    assert_eq!(
        journaled_definitions(&journal),
        vec![
            ("run_started".to_string(), Value::from(a.clone())),
            ("run_resumed".to_string(), Value::from(b.clone())),
            ("patch_applied".to_string(), Value::from(c.clone())),
        ],
        "row 14: every drive journaled what it executed"
    );
    let events = apb_engine::event::read_all(&case.run_dir(&run)).unwrap();
    let defs = DefinitionsInForce::load(&case.run_dir(&run), &events);
    let known: Vec<Option<String>> = defs
        .segments
        .iter()
        .map(|s| match &s.resolved {
            Resolved::Known { digest, .. } => Some(digest.clone()),
            Resolved::Unknown(_) => None,
        })
        .collect();
    assert_eq!(
        known,
        vec![Some(a), Some(b), Some(c)],
        "row 14: the resumed segment reads B, without the sidecar"
    );

    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let Some(v025) = old_binary(V025, &case) else {
        return;
    };
    for reader in [beta1, v025] {
        case.read_resume_stop(&reader, &run);
        case.approve_and_resume(&reader, &run);
        let journal = case.journal(&run);
        assert_eq!(finished(&journal, "later"), 1, "row 14: {}", label(&reader));
        assert_eq!(
            finished(&journal, "patched"),
            0,
            "row 14: {}",
            label(&reader)
        );
    }
}

// ------------------------------------------------- deferred retries (19a)

/// The writer playbook of rows D1 to D8 (`corpus-deferred`).
const DEFERRED: &str = "corpus-deferred";

/// A release threshold no row reaches: the writer's driver keeps the run.
const NO_RELEASE: &str = "3600000";

/// This binary's version, as it records it on `run_provenance` and
/// `run_resumed`.
const THIS_APB: &str = env!("CARGO_PKG_VERSION");

impl Case {
    /// Seeds the deferred writer with the new binary: the fixture with its
    /// `retry_delay` set to `delay` and `max_retries` to `retries`, the
    /// profile it names and a stub agent that fails its first `fails` calls.
    /// A call `n` waits while `agent.hold.<n>` exists in the case directory.
    fn seed_deferred(&mut self, delay: &str, retries: u32, fails: u32, writer: &Path) {
        self.seed(writer);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal-corpus");
        let pdir = self.root().join(".apb/playbooks").join(DEFERRED);
        copy_tree(&fixtures.join(DEFERRED), &pdir.join("1.0.0"));
        fs::write(pdir.join("current"), "1.0.0").unwrap();
        let yaml_path = pdir.join("1.0.0/playbook.yaml");
        let yaml = fs::read_to_string(&yaml_path)
            .unwrap()
            .replace("initial: 10m", &format!("initial: {delay}"))
            .replace("max_retries: 1", &format!("max_retries: {retries}"));
        fs::write(&yaml_path, yaml).unwrap();
        let profile = self.root().join(".apb/profiles/corpus");
        fs::create_dir_all(&profile).unwrap();
        fs::write(
            profile.join("profile.yaml"),
            "name: corpus\ndescription: the stub agent\nexecutor:\n  agent: claude-code\n  model: haiku\n",
        )
        .unwrap();
        fs::write(profile.join("SOUL.md"), "").unwrap();
        let d = self.dir.path().display().to_string();
        let stub = self.dir.path().join("agent.sh");
        fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf x >> '{d}/calls'\nn=$(wc -c < '{d}/calls' | tr -d ' ')\n: > '{d}/call.'$n\nwhile [ -f '{d}/agent.hold.'$n ]; do sleep 0.05; done\nif [ \"$n\" -le {fails} ]; then echo 'the agent made a mistake' 1>&2; exit 1; fi\necho ok\n"
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        self.env.push(("APB_AGENT_CMD", stub.display().to_string()));
        self.env.push(("APB_WAKER_POLL_MS", "100".to_string()));
    }

    /// Starts the deferred writer detached with `work` held, and waits for
    /// `flaky`'s retry to be deferred. Returns the run id.
    fn start_deferred(&mut self, writer: &Path) -> String {
        fs::write(self.root().join("hold"), "").unwrap();
        let ran = self.run(writer, &["run", DEFERRED, "--detach"]);
        assert!(ran.ok(), "{}: apb run {DEFERRED}: {ran:?}", self.name);
        let run = ran
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("run started: "))
            .unwrap_or_else(|| panic!("{}: no run id in {ran:?}", self.name))
            .trim()
            .to_string();
        self.guards.push(RunGuard::new(&self.root(), &run));
        self.wait_journal(&run, "flaky's retry to be deferred", |j| {
            j.iter()
                .any(|e| e["type"] == "retry_deferred" && e["node"] == "flaky")
        });
        run
    }

    /// Lets `work` finish while the deferral is open and waits for its
    /// `node_finished` and the guard written with it.
    fn finish_sibling(&self, run: &str) {
        fs::remove_file(self.root().join("hold")).unwrap();
        self.wait_journal(run, "work's finish and its guard", |j| {
            j.iter().any(|e| e["type"] == "deferred_retry_guard")
        });
    }

    /// The new binary writes a run whose deferral is open with a sibling's
    /// `node_finished` and its guard after it; the driver is then killed
    /// (D1, D4).
    fn write_deferral_open(&mut self) -> String {
        let new = new_binary();
        self.env
            .push(("APB_RELEASE_AFTER_MS", NO_RELEASE.to_string()));
        self.seed_deferred("10m", 1, 1, &new);
        let run = self.start_deferred(&new);
        self.finish_sibling(&run);
        self.kill_driver(&run);
        let journal = self.journal(&run);
        assert_guard_follows_last_checkpoint(&journal, self.name);
        self.save();
        run
    }

    /// The journal bytes of `run` now.
    fn journal_bytes(&self, run: &str) -> Vec<u8> {
        fs::read(self.run_dir(run).join("events.jsonl")).unwrap()
    }

    /// `apb resume <run>` by `reader` on the saved state is refused as a
    /// journal a newer apb wrote, naming `kind`, and writes nothing.
    fn resume_refused(&self, reader: &Path, run: &str, kind: &str) {
        self.restore();
        let before = self.journal_bytes(run);
        let ran = self.run(reader, &["resume", run]);
        assert!(
            !ran.ok() && ran.stderr.contains("written by a newer apb") && ran.stderr.contains(kind),
            "{}: {} refuses to resume, naming `{kind}`: {ran:?}",
            self.name,
            label(reader)
        );
        assert_eq!(
            self.journal_bytes(run),
            before,
            "{}: {} wrote nothing",
            self.name,
            label(reader)
        );
    }

    /// Kills every `apb __pause-waker` of `run`, so no waker of a saved
    /// state acts on a later copy.
    fn kill_wakers(&self, run: &str) {
        crate::common::kill_wakers(&self.root(), run);
    }
}

/// One line per record: its seq, type, node and status, for a failure
/// message.
fn summary(journal: &[Value]) -> String {
    journal
        .iter()
        .map(|e| format!("{} {} {} {}", e["seq"], e["type"], e["node"], e["status"]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the record is a checkpoint (`EventPayload::is_checkpoint`).
fn is_checkpoint(e: &Value) -> bool {
    matches!(
        e["type"].as_str(),
        Some("node_finished" | "run_paused" | "run_finished" | "run_aborted")
    )
}

/// The record right after the journal's last checkpoint is the guard of
/// the open deferral of `flaky`.
fn assert_guard_follows_last_checkpoint(journal: &[Value], name: &str) {
    let at = journal
        .iter()
        .rposition(is_checkpoint)
        .unwrap_or_else(|| panic!("{name}: a checkpoint"));
    let next = journal.get(at + 1);
    assert_eq!(
        next.map(|e| (e["type"].clone(), e["open"].clone())),
        Some((
            Value::from("deferred_retry_guard"),
            serde_json::json!(["flaky"])
        )),
        "{name}: the guard follows the last checkpoint in the same write: {:?}",
        &journal[at..]
    );
}

/// The nodes that finished after record `from`, with their status, and
/// the hops taken: what a resume routed.
fn routing(journal: &[Value], from: usize) -> Vec<String> {
    journal[from..]
        .iter()
        .filter_map(|e| match e["type"].as_str() {
            Some("node_finished") => Some(format!("{} {}", e["node"], e["status"])),
            Some("edge_traversed") => Some(format!("{} -> {}", e["from"], e["to"])),
            _ => None,
        })
        .collect()
}

/// The project tree of a case as (path, bytes), to prove a read wrote
/// nothing.
fn tree_snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                out.push((path.clone(), Vec::new()));
                stack.push(path);
            } else if kind.is_file() {
                out.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// Row D1: the new binary leaves a deferral open with a sibling's
/// `node_finished` and its guard. beta.1 and 0.25.0 read the run (the
/// unknown records counted), refuse to resume it with the newer-apb
/// message, and stop it; the new binary then reads it aborted. The new
/// doctor names the guarded deferral.
#[test]
fn d1_open_deferral_old_binaries_read_refuse_and_stop() {
    let mut case = Case::new("row D1");
    let run = case.write_deferral_open();
    let new = new_binary();
    case.restore();
    let doctor = case.run(&new, &["doctor", "--run", &run]);
    assert!(
        doctor.stdout.contains("retry 1 of `flaky` deferred") && doctor.stdout.contains("guarded"),
        "row D1: the doctor names the guarded deferral: {doctor:?}"
    );
    // Only the old readers need the old binaries; the doctor runs without.
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        let ran = case.read(&reader, &run);
        assert!(
            ran.stdout.contains("unknown event"),
            "row D1: {} counts what it skipped: {ran:?}",
            label(&reader)
        );
        case.restore();
        let listed = case.run(&reader, &["runs"]);
        assert!(
            listed.ok() && listed.stdout.contains(&run),
            "row D1: {} lists the run: {listed:?}",
            label(&reader)
        );
        case.resume_refused(&reader, &run, "deferred_retry_guard");
        case.stop(&reader, &run);
        let shown = case.run(&new, &["runs", &run]);
        assert!(
            shown.ok() && shown.stdout.contains("aborted"),
            "row D1: the new binary reads the run {} stopped: {shown:?}",
            label(&reader)
        );
    }
}

/// Row D2: the new binary's driver releases the run (nothing but the
/// deferred retry left, due in ten minutes) and exits. An old `apb resume
/// --all` passes over it and writes nothing, and an old `apb resume`
/// refuses it. Once the retry is due, the new `apb resume --all` resumes
/// it: the retry runs, `flaky` succeeds and the run reaches the gate.
#[test]
fn d2_released_run_old_resume_all_writes_nothing_new_resumes_when_due() {
    let mut case = Case::new("row D2");
    let new = new_binary();
    case.seed_deferred("10m", 1, 1, &new);
    let run = case.start_deferred(&new);
    // Read before the release: the release removes `driver.pid` while the
    // departing driver still runs its tail (it starts the release's waker).
    let driver = apb_engine::driver::read_driver_pid(&case.run_dir(&run));
    assert!(driver.is_some(), "row D2: the driver wrote its pid");
    fs::remove_file(case.root().join("hold")).unwrap();
    case.wait_journal(&run, "the driver to release the run", |j| {
        j.last().is_some_and(|e| e["type"] == "drive_released")
    });
    if let Some(pid) = driver {
        let start = Instant::now();
        while crate::common::sig::alive(pid) {
            assert!(start.elapsed() < DEADLINE, "row D2: the driver exits");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    case.forget_driver(&run);
    case.kill_wakers(&run);
    case.save();
    // The old readers' part needs them; the new resume below runs without.
    for reader in old_readers(&case)
        .map(|(b, v)| [b, v])
        .into_iter()
        .flatten()
    {
        case.restore();
        let before = case.journal_bytes(&run);
        let ran = case.run(&reader, &["resume", "--all"]);
        assert!(
            !ran.stdout.contains("resumed"),
            "row D2: {} resumes nothing: {ran:?}",
            label(&reader)
        );
        assert_eq!(
            case.journal_bytes(&run),
            before,
            "row D2: {} wrote nothing",
            label(&reader)
        );
        case.resume_refused(&reader, &run, "drive_released");
    }
    // Due now: the release's wake moved to the deferral's own time, the
    // delay down to its floor (what is left of it the drive waits out).
    rewrite_journal(&case.saved().join(&run), |records| {
        let at = records
            .iter()
            .find(|e| e["type"] == "retry_deferred")
            .map(|e| e["ts"].as_u64().unwrap())
            .unwrap();
        records
            .into_iter()
            .map(|mut r| {
                if r["type"] == "retry_deferred" {
                    r["delay_ms"] = Value::from(1000);
                }
                if r["type"] == "drive_released" {
                    r["wake_at_ms"] = Value::from(at);
                }
                r
            })
            .collect()
    });
    case.restore();
    let ran = case.run(&new, &["resume", "--all"]);
    assert!(
        ran.ok() && ran.stdout.contains("resumed (retry_due)"),
        "row D2: new resume --all: {ran:?}"
    );
    case.guards.push(RunGuard::new(&case.root(), &run));
    let journal = case.wait_journal(&run, "the resumed run to reach the gate", |j| {
        j.iter().any(|e| e["type"] == "review_requested")
    });
    case.kill_driver(&run);
    let resumed = journal
        .iter()
        .position(|e| e["type"] == "run_resumed")
        .unwrap_or_else(|| panic!("row D2: resumed: {journal:?}"));
    assert_eq!(
        journal[resumed]["apb"],
        Value::from(THIS_APB),
        "row D2: the resume records its binary"
    );
    let tries: Vec<&Value> = journal
        .iter()
        .filter(|e| e["type"] == "attempt_started" && e["node"] == "flaky")
        .collect();
    assert_eq!(tries.len(), 2, "row D2: the deferred retry ran once");
    assert!(
        journal.iter().any(|e| e["type"] == "node_finished"
            && e["node"] == "flaky"
            && e["status"] == "succeeded"),
        "row D2: flaky succeeded on its retry: {}",
        summary(&journal)
    );
}

/// Row D3: an operator pause while the deferral is open (`run_paused` and
/// its guard). Old resumes refuse; the new resume continues what is left of
/// the delay (it releases the run again, the wake no earlier than the
/// deferral's own due time) and starts no attempt early.
#[test]
fn d3_paused_with_open_deferral_old_refuse_new_continues_the_delay() {
    let mut case = Case::new("row D3");
    let new = new_binary();
    case.env
        .push(("APB_RELEASE_AFTER_MS", NO_RELEASE.to_string()));
    case.seed_deferred("10m", 1, 1, &new);
    let run = case.start_deferred(&new);
    case.finish_sibling(&run);
    let ran = case.run(&new, &["pause", &run]);
    assert!(ran.ok(), "row D3: pause: {ran:?}");
    let journal = case.wait_journal(&run, "the pause and its guard", |j| {
        j.iter().any(|e| e["type"] == "run_paused")
            && j.last()
                .is_some_and(|e| e["type"] == "deferred_retry_guard")
    });
    assert_guard_follows_last_checkpoint(&journal, "row D3");
    if let Some(pid) = apb_engine::driver::read_driver_pid(&case.run_dir(&run)) {
        let start = Instant::now();
        while crate::common::sig::alive(pid) {
            assert!(start.elapsed() < DEADLINE, "row D3: the driver exits");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    case.forget_driver(&run);
    case.save();
    // The old readers' part needs them; the new resume below runs without.
    for reader in old_readers(&case)
        .map(|(b, v)| [b, v])
        .into_iter()
        .flatten()
    {
        case.resume_refused(&reader, &run, "deferred_retry_guard");
    }
    // The new resume, with the ordinary release threshold.
    case.env.retain(|(k, _)| *k != "APB_RELEASE_AFTER_MS");
    case.restore();
    let before = case.journal(&run).len();
    let ran = case.run(&new, &["resume", &run]);
    // A resume that releases the run again ends with exit 4, as a wait
    // that left the run to its waker.
    assert!(
        ran.code == Some(4) && ran.stdout.contains("released"),
        "row D3: new resume: {ran:?}"
    );
    case.kill_wakers(&run);
    let journal = case.journal(&run);
    let added: Vec<&str> = journal[before..]
        .iter()
        .filter_map(|e| e["type"].as_str())
        .collect();
    assert_eq!(added.first(), Some(&"run_resumed"), "row D3: {added:?}");
    assert!(
        !added.contains(&"attempt_started"),
        "row D3: no attempt before the delay is over: {added:?}"
    );
    let deferred = journal
        .iter()
        .find(|e| e["type"] == "retry_deferred")
        .unwrap();
    let due = deferred["ts"].as_u64().unwrap() + deferred["delay_ms"].as_u64().unwrap();
    let released = last_of(&journal, "drive_released")
        .unwrap_or_else(|| panic!("row D3: the resume released the run: {added:?}"));
    assert!(
        released["wake_at_ms"].as_u64().unwrap() >= due,
        "row D3: the remaining delay continues: {released} against due {due}"
    );
}

/// Row D4 (synthetic fault): the guard's line of D1's two-line write is cut
/// short, as a crash inside that one write leaves it. Every binary's resume
/// refuses the torn tail and writes nothing; the read-only `apb runs` of
/// all three reads the rest; the new doctor names the torn line.
#[test]
fn d4_synthetic_torn_guard_every_resume_refuses_reads_go_on() {
    let mut case = Case::new("row D4");
    let run = case.write_deferral_open();
    let path = case.saved().join(&run).join("events.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    // The crash stops the write inside the guard's line: nothing after it
    // reached the file.
    let guard = text
        .find("\"type\":\"deferred_retry_guard\"")
        .expect("row D4: the guard");
    let line = text[..guard].rfind('\n').unwrap() + 1;
    let end = line + text[line..].find('\n').unwrap();
    let torn = &text[..line + (end - line) / 2];
    fs::write(&path, torn).unwrap();
    let lines = torn.lines().count();
    let new = new_binary();
    // The new binary always; the old ones when they are here.
    let old = old_readers(&case)
        .map(|(b, v)| [b, v])
        .into_iter()
        .flatten();
    for reader in std::iter::once(new.clone()).chain(old) {
        case.read(&reader, &run);
        case.restore();
        let before = case.journal_bytes(&run);
        let ran = case.run(&reader, &["resume", &run]);
        assert!(
            !ran.ok(),
            "row D4: {} refuses the torn tail: {ran:?}",
            label(&reader)
        );
        assert_eq!(
            case.journal_bytes(&run),
            before,
            "row D4: {} wrote nothing",
            label(&reader)
        );
    }
    case.restore();
    let doctor = case.run(&new, &["doctor", "--run", &run]);
    assert!(
        doctor
            .stdout
            .contains(&format!("line {lines} is a write that did not finish")),
        "row D4: the doctor names the torn line: {doctor:?}"
    );
}

/// Row D5: the new binary's driver dies right after `retry_deferred`, with
/// no checkpoint after it (`work` still runs). The old resumes refuse
/// under the existing rule, naming `retry_deferred`.
#[test]
fn d5_crash_after_retry_deferred_old_resumes_refuse() {
    let mut case = Case::new("row D5");
    let new = new_binary();
    case.env
        .push(("APB_RELEASE_AFTER_MS", NO_RELEASE.to_string()));
    case.seed_deferred("10m", 1, 1, &new);
    let run = case.start_deferred(&new);
    case.kill_driver(&run);
    fs::remove_file(case.root().join("hold")).unwrap();
    let journal = case.journal(&run);
    let checkpoint = journal.iter().rposition(is_checkpoint).unwrap();
    let deferred = journal
        .iter()
        .position(|e| e["type"] == "retry_deferred")
        .unwrap();
    assert!(deferred > checkpoint, "row D5: no checkpoint after it");
    case.save();
    // Only the old resumes need the old binaries.
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        case.resume_refused(&reader, &run, "retry_deferred");
        case.stop(&reader, &run);
    }
}

/// Row D9: the new binary's driver is killed while a retry waits out its
/// delay, and the new binary resumes the run: the delayed attempt is the
/// node's last (`max_retries: 1`, so two attempts in all), never a fresh
/// retry budget. No old binary is needed.
#[test]
fn d9_killed_during_the_delay_new_resume_keeps_max_retries() {
    let mut case = Case::new("row D9");
    let new = new_binary();
    case.env
        .push(("APB_RELEASE_AFTER_MS", NO_RELEASE.to_string()));
    // The stub agent fails every call.
    case.seed_deferred("3s", 1, 100, &new);
    let run = case.start_deferred(&new);
    case.kill_driver(&run);
    assert!(
        !case
            .journal(&run)
            .iter()
            .any(|e| e["type"] == "retry_started"),
        "row D9: killed during the delay"
    );
    fs::remove_file(case.root().join("hold")).unwrap();
    // The resume parks at the review gate once both branches ended; it is
    // killed there.
    let resume = case.spawn(&new, &["resume", &run]);
    let journal = case.wait_journal(&run, "the review gate after flaky's end", |j| {
        j.iter().any(|e| e["type"] == "review_requested")
    });
    drop(resume);
    case.forget_driver(&run);
    let of_flaky = |kind: &str| {
        journal
            .iter()
            .filter(|e| e["type"] == kind && e["node"] == "flaky")
            .count()
    };
    assert_eq!(
        of_flaky("attempt_started"),
        2,
        "row D9: one attempt and its one retry:\n{}",
        summary(&journal)
    );
    assert_eq!(of_flaky("retry_deferred"), 1, "row D9: one deferral");
    let calls = fs::read_to_string(case.dir.path().join("calls")).unwrap_or_default();
    assert_eq!(calls.len(), 2, "row D9: the agent ran twice");
    let finished = last_of(&journal, "node_finished")
        .filter(|e| e["node"] == "flaky")
        .map(|e| e["status"].clone());
    assert_eq!(
        finished,
        Some(Value::from("failed")),
        "row D9: {}",
        summary(&journal)
    );
}

/// Rows D6 and D8: the new binary runs the deferral to its end (`flaky`
/// fails, waits three seconds while `work` finishes, then succeeds), so
/// the journal holds `retry_deferred`, the guard after `work`'s finish,
/// `flaky`'s finish with no guard, `attempt_started.exec`,
/// `run_provenance.apb`, and a new `run_resumed.apb`. beta.1 and 0.25.0
/// read it, approve and resume it to the end with the same routing as the
/// new binary, and stop it. After beta.1's resume the new `apb doctor
/// --run` lists both binaries and a downgrade, and writes nothing.
#[test]
fn d6_d8_closed_deferral_old_binaries_resume_with_the_same_routing() {
    let mut case = Case::new("row D6");
    let new = new_binary();
    case.env
        .push(("APB_RELEASE_AFTER_MS", NO_RELEASE.to_string()));
    case.seed_deferred("3s", 1, 1, &new);
    let run = case.start_deferred(&new);
    case.finish_sibling(&run);
    case.wait_journal(&run, "the gate", |j| {
        j.iter().any(|e| e["type"] == "review_requested")
    });
    case.kill_driver(&run);
    // A resume by the new binary, so `run_resumed.apb` is in the journal:
    // it parks at the gate again and is killed there.
    let before = case.journal(&run).len();
    let resume = case.spawn(&new, &["resume", &run]);
    case.wait_journal(&run, "the new resume", |j| {
        j.len() > before && j[before..].iter().any(|e| e["type"] == "run_resumed")
    });
    drop(resume);
    case.forget_driver(&run);
    case.save();

    let journal = case.journal(&run);
    let guard = journal
        .iter()
        .rposition(|e| e["type"] == "deferred_retry_guard")
        .expect("row D6: a guard");
    assert_eq!(journal[guard - 1]["type"], "node_finished");
    assert_eq!(journal[guard - 1]["node"], "work");
    let flaky_done = journal
        .iter()
        .position(|e| e["type"] == "node_finished" && e["node"] == "flaky")
        .expect("row D6: flaky finished");
    assert!(
        flaky_done > guard,
        "row D6: the deferral closed after the guard"
    );
    assert_ne!(
        journal.get(flaky_done + 1).map(|e| e["type"].clone()),
        Some(Value::from("deferred_retry_guard")),
        "row D6: no guard once the deferral closed"
    );
    assert_eq!(
        last_of(&journal, "run_provenance").map(|e| e["apb"].clone()),
        Some(Value::from(THIS_APB))
    );
    assert_eq!(
        last_of(&journal, "run_resumed").map(|e| e["apb"].clone()),
        Some(Value::from(THIS_APB))
    );
    assert!(
        journal
            .iter()
            .filter(|e| e["type"] == "attempt_started" && e["node"] == "flaky")
            .all(|e| e["exec"].is_object()),
        "row D8: every attempt records its chain step"
    );

    let from = journal.len();
    case.approve_and_resume(&new, &run);
    let expected = routing(&case.journal(&run), from);
    // Everything above runs on the new binary alone; the old readers'
    // resumes and the doctor after beta.1's need the old binaries.
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1.clone(), v025] {
        let ran = case.read(&reader, &run);
        assert!(
            ran.stdout.contains("unknown event"),
            "row D6: {} skips the deferral records: {ran:?}",
            label(&reader)
        );
        case.approve_and_resume(&reader, &run);
        assert_eq!(
            routing(&case.journal(&run), from),
            expected,
            "row D6: {} routes as the new binary",
            label(&reader)
        );
        case.stop(&reader, &run);
    }

    // E10: after beta.1's resume, the doctor names both and writes nothing.
    case.approve_and_resume(&beta1, &run);
    let before = tree_snapshot(&case.root());
    let doctor = case.run(&new, &["doctor", "--run", &run]);
    assert_eq!(
        tree_snapshot(&case.root()),
        before,
        "row D6: the doctor wrote nothing"
    );
    assert!(
        doctor.stdout.contains(&format!(
            "driven by apb {THIS_APB}, resumed by apb {THIS_APB}, an older apb (no version recorded)"
        )) && doctor.stdout.contains("downgrade"),
        "row D6: the doctor lists both binaries: {doctor:?}"
    );
    let shown = case.run(&new, &["runs", &run]);
    assert!(
        shown.stdout.contains(&format!("driven by apb {THIS_APB}")),
        "row D6: the run detail names the binaries: {shown:?}"
    );
}

/// Row D7: beta.1 starts the deferred playbook (it drops `retry_delay`):
/// `flaky` fails and beta.1 retries it at once; its driver dies inside that
/// retry. The new binary resumes it: `flaky` runs again, fails, and its
/// next retry waits the delay. The delay applies to the gaps after the
/// resume only.
#[test]
fn d7_beta1_starts_new_applies_the_delay_to_later_gaps_only() {
    let mut case = Case::new("row D7");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let new = new_binary();
    case.seed_deferred("1s", 3, 3, &beta1);
    fs::write(case.dir.path().join("agent.hold.2"), "").unwrap();
    let ran = case.run(&beta1, &["run", DEFERRED, "--detach"]);
    assert!(ran.ok(), "row D7: beta.1 starts: {ran:?}");
    let run = ran
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .unwrap_or_else(|| panic!("row D7: no run id in {ran:?}"))
        .trim()
        .to_string();
    case.guards.push(RunGuard::new(&case.root(), &run));
    let start = Instant::now();
    while !case.dir.path().join("call.2").exists() {
        assert!(start.elapsed() < DEADLINE, "row D7: beta.1 retries flaky");
        std::thread::sleep(Duration::from_millis(20));
    }
    case.kill_driver(&run);
    fs::remove_file(case.dir.path().join("agent.hold.2")).unwrap();
    let by_beta1 = case.journal(&run);
    assert!(
        !by_beta1.iter().any(|e| e["type"] == "retry_deferred"),
        "row D7: beta.1 does not defer"
    );
    assert_eq!(
        by_beta1
            .iter()
            .filter(|e| e["type"] == "attempt_started" && e["node"] == "flaky")
            .count(),
        2,
        "row D7: beta.1 retried at once"
    );

    let resume = case.spawn(&new, &["resume", &run]);
    let journal = case.wait_journal(&run, "the new resume to reach the gate", |j| {
        j.iter().any(|e| e["type"] == "review_requested")
    });
    drop(resume);
    case.forget_driver(&run);
    let after = &journal[by_beta1.len()..];
    let deferrals: Vec<&Value> = after
        .iter()
        .filter(|e| e["type"] == "retry_deferred")
        .collect();
    assert_eq!(deferrals.len(), 1, "row D7: one delayed gap: {after:?}");
    let d = deferrals[0];
    let due = d["ts"].as_u64().unwrap() + d["delay_ms"].as_u64().unwrap();
    let next = after
        .iter()
        .skip_while(|e| e["type"] != "retry_deferred")
        .find(|e| e["type"] == "attempt_started" && e["node"] == "flaky")
        .expect("row D7: the delayed attempt");
    assert!(
        next["ts"].as_u64().unwrap() >= due,
        "row D7: the delayed attempt waited: {next} against due {due}"
    );
    assert!(
        after.iter().any(|e| e["type"] == "node_finished"
            && e["node"] == "flaky"
            && e["status"] == "succeeded"),
        "row D7: flaky succeeded: {}",
        summary(after)
    );
}

/// The declaration row D10 adds to a finished corpus run's `playbook.yaml`:
/// what a later definition of the playbook would declare.
const D10_OUTPUTS: &str = "\noutputs:\n  work: { from: { node: work } }\n  child: { from: { node: sub } }\n  decided: { from: { node: ok } }\n  rejected: { from: { node: no } }\n  field: { from: { node: work, field: x } }\n";

/// Row D10: the new binary projects `run_outputs` over journals older
/// binaries wrote. beta.1 and 0.25.0 each run the corpus writer to its end
/// (approved at the gate); without the old binaries, the new writer's run
/// with everything an older apb does not journal removed (the definition
/// digests, the binary versions, the stored definitions) stands in for
/// them, so the new-binary half runs everywhere. Each journal projects
/// without an error: nothing declared at first, then the values the
/// journal holds once the run's `playbook.yaml` declares outputs, read
/// against that current file because no definition of the run is
/// established.
#[test]
fn d10_old_journals_project_run_outputs() {
    let new = new_binary();
    let mut case = Case::new("row D10 (old shape)");
    let run = case.write_waiting_at_review(&new);
    case.approve_and_resume(&new, &run);
    rewrite_journal(&case.run_dir(&run), |journal| {
        journal
            .into_iter()
            .map(|mut e| {
                if let Some(o) = e.as_object_mut() {
                    o.remove("definition");
                    o.remove("apb");
                }
                e
            })
            .collect()
    });
    let _ = fs::remove_dir_all(case.run_dir(&run).join("definitions"));
    d10_project(&case, &run);

    for (tag, name) in [(BETA1, "row D10 (beta.1)"), (V025, "row D10 (0.25.0)")] {
        let mut case = Case::new(name);
        let Some(old) = old_binary(tag, &case) else {
            continue;
        };
        let run = case.write_waiting_at_review(&old);
        case.approve_and_resume(&old, &run);
        d10_project(&case, &run);
    }
}

/// The new binary's projection of a finished corpus run, before and after
/// its `playbook.yaml` declares outputs.
fn d10_project(case: &Case, run: &str) {
    let new = new_binary();
    let outputs = |case: &Case| -> Value {
        let ran = case.run(&new, &["runs", run, "--outputs-json"]);
        assert!(ran.ok(), "{}: runs --outputs-json: {ran:?}", case.name);
        serde_json::from_str(&ran.stdout)
            .unwrap_or_else(|e| panic!("{}: not JSON ({e}): {ran:?}", case.name))
    };
    let o = outputs(case);
    assert_eq!(
        o,
        serde_json::json!({ "final": true, "definition": "current", "outputs": [] }),
        "{}: nothing declared",
        case.name
    );
    let ran = case.run(&new, &["runs", run, "--json"]);
    assert!(ran.ok(), "{}: runs --json: {ran:?}", case.name);
    let status: Value = serde_json::from_str(&ran.stdout).unwrap();
    assert!(
        status.get("run_outputs").is_none(),
        "{}: {status}",
        case.name
    );

    let snapshot = case.run_dir(run).join("playbook.yaml");
    let mut yaml = fs::read_to_string(&snapshot).unwrap();
    yaml.push_str(D10_OUTPUTS);
    fs::write(&snapshot, yaml).unwrap();
    let journal = case.journal(run);
    let output_of = |node: &str| {
        journal
            .iter()
            .find(|e| e["type"] == "node_finished" && e["node"] == node)
            .map(|e| e["output"].clone())
            .unwrap_or_else(|| panic!("{}: no node_finished of {node}", case.name))
    };
    let o = outputs(case);
    assert_eq!(o["definition"], "current", "{}: {o:#}", case.name);
    assert_eq!(o["final"], true, "{}: {o:#}", case.name);
    let named = |name: &str| -> &Value {
        o["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == name)
            .unwrap_or_else(|| panic!("{}: no output {name}: {o:#}", case.name))
    };
    assert_eq!(named("work")["value"], output_of("work"), "{}", case.name);
    assert_eq!(named("work")["source"]["execution"]["ordinal"], 1);
    assert_eq!(named("child")["value"], output_of("sub"), "{}", case.name);
    assert_eq!(named("decided")["state"], "value", "{}: {o:#}", case.name);
    assert_eq!(
        named("rejected")["reason"],
        "not_run",
        "{}: {o:#}",
        case.name
    );
    assert_eq!(named("field")["reason"], "not_json_object", "{}", case.name);
    let ran = case.run(&new, &["runs", run]);
    assert!(ran.ok(), "{}: runs: {ran:?}", case.name);
    assert!(
        ran.stdout
            .contains("  outputs (final, current definition):"),
        "{}: {ran:?}",
        case.name
    );
}

// ------------------------------------------- read-only node, unattended mode

/// The stub profile `corpus` an `agent_task` of the rows below names (it
/// never runs: validation only needs it to exist).
fn seed_corpus_profile(root: &Path) {
    let profile = root.join(".apb/profiles/corpus");
    fs::create_dir_all(&profile).unwrap();
    fs::write(
        profile.join("profile.yaml"),
        "name: corpus\ndescription: the stub agent\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(profile.join("SOUL.md"), "").unwrap();
}

impl Case {
    /// `apb init` by the writer, the corpus playbooks, then playbook `id`
    /// (`yaml`, with `scripts` under `scripts/`) and the stub profile.
    fn seed_inline(&mut self, writer: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
        self.seed(writer);
        let vdir = self.root().join(".apb/playbooks").join(id).join("1.0.0");
        fs::create_dir_all(vdir.join("scripts")).unwrap();
        fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
        for (name, body) in scripts {
            fs::write(vdir.join("scripts").join(name), body).unwrap();
        }
        fs::write(
            self.root().join(".apb/playbooks").join(id).join("current"),
            "1.0.0",
        )
        .unwrap();
        seed_corpus_profile(&self.root());
        // The profile's agent never runs; a binary that checks it can start
        // finds a command.
        self.env.retain(|(k, _)| *k != "APB_AGENT_CMD");
        self.env.push(("APB_AGENT_CMD", "/bin/true".to_string()));
    }

    /// Starts `id` detached with `extra` flags and returns its run id.
    fn start_inline(&mut self, writer: &Path, id: &str, extra: &[&str]) -> String {
        let mut args = vec!["run", id, "--detach"];
        args.extend_from_slice(extra);
        let ran = self.run(writer, &args);
        assert!(ran.ok(), "{}: apb {}: {ran:?}", self.name, args.join(" "));
        let run = ran
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("run started: "))
            .unwrap_or_else(|| panic!("{}: no run id in {ran:?}", self.name))
            .trim()
            .to_string();
        self.guards.push(RunGuard::new(&self.root(), &run));
        run
    }
}

/// Row D11's playbook: every playbook field 19c adds or uses (`outputs`, a
/// node's `read_only` and `retry_delay`, a gate's `unattended`) in one
/// definition. `look` is a read-only script that writes a file; `fix`
/// never runs.
const D11_FIELDS: &str = r#"schema: 2
id: corpus-fields
name: Journal corpus newer fields
version: 1.0.0
outputs:
  look: { from: { node: look } }
nodes:
  - { id: start, type: start }
  - { id: look, type: script, script: "scripts/write.sh", runner: sh, read_only: true }
  - { id: gate, type: human_review, options: [approved, rejected], unattended: { decide: approved } }
  - { id: fix, type: agent_task, prompt: "FIX", profile: corpus, max_retries: 1, retry_delay: { initial: 1s } }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: look }
  - { from: look, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: fix, condition: { type: review_status, equals: rejected } }
  - { from: fix, to: no }
"#;

/// A script that changes the tree it runs in (the project root).
const WRITE_SH: &str = "echo written >> written.txt\necho wrote\n";

/// Row D11: beta.1 and 0.25.0 load a playbook with the newer fields: both
/// validate it and run it, ignoring what they do not know (the read-only
/// script writes and succeeds, the gate waits for a person, who approves).
/// The new binary validates the same playbook and fails the read-only
/// script `read_only_dirty`, with its declared output missing.
#[test]
fn d11_old_binaries_load_the_newer_playbook_fields() {
    let new = new_binary();
    let mut case = Case::new("row D11 (new)");
    case.seed_inline(&new, "corpus-fields", D11_FIELDS, &[("write.sh", WRITE_SH)]);
    let ran = case.run(&new, &["validate", "corpus-fields"]);
    assert!(ran.ok(), "row D11: the new binary validates: {ran:?}");
    let ran = case.run(&new, &["run", "corpus-fields", "--unattended"]);
    assert_eq!(ran.code, Some(1), "row D11: the new run fails: {ran:?}");
    let run = only_run_id(&case);
    let journal = case.journal(&run);
    assert!(
        journal.iter().any(|e| e["type"] == "read_only_violated"
            && e["node"] == "look"
            && e["changes"][0]["path"] == "written.txt"),
        "row D11: {}",
        summary(&journal)
    );
    assert!(
        journal.iter().any(|e| e["type"] == "node_finished"
            && e["node"] == "look"
            && e["status"] == "failed"
            && e["output"]
                .as_str()
                .is_some_and(|r| r.starts_with("read_only_dirty: "))),
        "row D11: {}",
        summary(&journal)
    );
    // The failure went on along the unconditional edge, so the run fails
    // at its end naming the node.
    assert_eq!(
        last_of(&journal, "run_error").map(|e| e["node"].clone()),
        Some(Value::from("look")),
        "row D11: {}",
        summary(&journal)
    );
    let ran = case.run(&new, &["runs", &run, "--outputs-json"]);
    let o: Value = serde_json::from_str(&ran.stdout).unwrap();
    assert_eq!(o["outputs"][0]["name"], "look", "row D11: {o:#}");
    assert_eq!(o["outputs"][0]["reason"], "not_succeeded", "row D11: {o:#}");

    for (tag, name) in [(BETA1, "row D11 (beta.1)"), (V025, "row D11 (0.25.0)")] {
        let mut case = Case::new(name);
        let Some(old) = old_binary(tag, &case) else {
            continue;
        };
        case.seed_inline(&old, "corpus-fields", D11_FIELDS, &[("write.sh", WRITE_SH)]);
        let ran = case.run(&old, &["validate", "corpus-fields"]);
        assert!(ran.ok(), "{name}: validates: {ran:?}");
        let run = case.start_inline(&old, "corpus-fields", &[]);
        let journal = case.wait_journal(&run, "the gate", |j| {
            j.iter().any(|e| e["type"] == "review_requested")
        });
        assert!(
            journal.iter().any(|e| e["type"] == "node_finished"
                && e["node"] == "look"
                && e["status"] == "succeeded"),
            "{name}: the read-only script wrote and succeeded: {}",
            summary(&journal)
        );
        assert!(case.root().join("written.txt").is_file(), "{name}");
        case.kill_driver(&run);
        case.save();
        case.approve_and_resume(&old, &run);
    }
}

/// The run id of the case's only run.
fn only_run_id(case: &Case) -> String {
    let ids: Vec<String> = fs::read_dir(case.runs())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "{}: {ids:?}", case.name);
    ids[0].clone()
}

/// Row D12's playbook: a read-only script that only reads, a gate the
/// unattended run decides, a script the harness holds to pause the run,
/// and a second gate with no unattended outcome.
const D12_UNATTENDED: &str = r#"schema: 2
id: corpus-unattended
name: Journal corpus unattended run
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: look, type: script, script: "scripts/look.sh", runner: sh, read_only: true }
  - { id: gate1, type: human_review, options: [approved, rejected], unattended: { decide: approved, note: ci } }
  - { id: work, type: script, script: "scripts/work.sh", runner: sh, expected_duration: 10s }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: look }
  - { from: look, to: gate1 }
  - { from: gate1, to: work, condition: { type: review_status, equals: approved } }
  - { from: gate1, to: no, condition: { type: review_status, equals: rejected } }
  - { from: work, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

const LOOK_SH: &str = "cat playbook.yaml >/dev/null 2>&1; echo looked\n";

/// The `work` script of the corpus writer: `work.started`, then held while
/// `hold` exists.
const WORK_SH: &str = ": > work.started\nwhile [ -f hold ]; do sleep 0.05; done\necho work done\n";

/// The new binary resumes `run` (unattended): the plain gate fails the run
/// `hitl_required`, so the run stayed unattended whoever drove it before.
fn d12_new_resume_stays_unattended(case: &Case, run: &str) {
    let new = new_binary();
    let ran = case.run(&new, &["resume", run]);
    assert_eq!(ran.code, Some(1), "{}: new resume: {ran:?}", case.name);
    let journal = case.journal(run);
    let error = last_of(&journal, "run_error").expect("a run_error");
    assert_eq!(error["node"], "gate", "{}: {error}", case.name);
    assert!(
        error["reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("hitl_required: ")),
        "{}: {error}",
        case.name
    );
    assert_eq!(
        last_of(&journal, "run_finished").map(|e| e["outcome"].clone()),
        Some(Value::from("failed")),
        "{}",
        case.name
    );
}

/// Row D12: the new binary runs `corpus-unattended` unattended: the
/// read-only check is journaled (`read_only_checked`), gate 1 is decided
/// `decided_by: unattended`, and its driver is lost while `work` runs,
/// before gate 2. Both old binaries read it (skipping `read_only_checked`)
/// and stop it; beta.1 resumes it and waits at gate 2 (`apb wait` exits
/// 3); the new binary that resumes it afterwards still runs it
/// unattended. Without the old
/// binaries, the new binary's own resume of the saved state shows the same.
#[test]
fn d12_unattended_run_old_binaries_read_stop_resume_new_stays_unattended() {
    let new = new_binary();
    let mut case = Case::new("row D12");
    case.seed_inline(
        &new,
        "corpus-unattended",
        D12_UNATTENDED,
        &[("look.sh", LOOK_SH), ("work.sh", WORK_SH)],
    );
    fs::write(case.root().join("hold"), "").unwrap();
    let run = case.start_inline(&new, "corpus-unattended", &["--unattended"]);
    case.wait_file("work.started");
    let journal = case.wait_journal(&run, "work to start", |j| {
        j.iter()
            .any(|e| e["type"] == "node_started" && e["node"] == "work")
    });
    case.kill_driver(&run);
    fs::remove_file(case.root().join("hold")).unwrap();
    case.save();
    assert!(
        journal.iter().any(|e| e["type"] == "read_only_checked"
            && e["node"] == "look"
            && e["check"]["tier"] == "detected"),
        "row D12: {}",
        summary(&journal)
    );
    let decided: Vec<&Value> = journal
        .iter()
        .filter(|e| e["type"] == "review_decided")
        .collect();
    assert_eq!(decided.len(), 1, "row D12: {}", summary(&journal));
    assert_eq!(decided[0]["node"], "gate1");
    assert_eq!(decided[0]["decided_by"], "unattended");
    assert!(
        !journal
            .iter()
            .any(|e| e["type"] == "review_requested" && e["node"] == "gate"),
        "row D12: stopped before gate 2"
    );

    // The new binary alone: it reads the run and its resume stays
    // unattended.
    let ran = case.read(&new, &run);
    assert!(!ran.stdout.contains("unknown event"), "row D12: {ran:?}");
    d12_new_resume_stays_unattended(&case, &run);

    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [&beta1, &v025] {
        let ran = case.read(reader, &run);
        assert!(
            ran.stdout.contains("unknown event"),
            "row D12: {} skips read_only_checked: {ran:?}",
            label(reader)
        );
        case.stop(reader, &run);
    }
    case.restore();
    let resume = case.spawn(&beta1, &["resume", &run]);
    case.wait_journal(&run, "beta.1 at gate 2", |j| {
        j.iter()
            .any(|e| e["type"] == "review_requested" && e["node"] == "gate")
    });
    let waited = case.run(&beta1, &["wait", &run, "--timeout", "30"]);
    assert_eq!(waited.code, Some(3), "row D12: beta.1 waits: {waited:?}");
    drop(resume);
    case.forget_driver(&run);
    d12_new_resume_stays_unattended(&case, &run);
}

/// Row D13's playbook: a read-only script that writes, whose failure is
/// routed to the gate.
const D13_VIOLATION: &str = r#"schema: 2
id: corpus-violation
name: Journal corpus read-only violation
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/write.sh", runner: sh, read_only: true }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: clean, type: finish, outcome: success }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: gate, condition: { type: node_status, node: work, equals: failure } }
  - { from: work, to: clean, condition: { type: node_status, node: work, equals: success } }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

/// Row D13: the new binary's read-only script changes the tree, so the run
/// journals `read_only_violated` before `node_finished { failed }` and
/// routes the failure to the gate, where it waits. Both old binaries read
/// it, skipping the two read-only records with the count shown, and
/// resume it with the routing of the new binary.
#[test]
fn d13_read_only_violation_old_binaries_skip_it_and_route_the_same() {
    let new = new_binary();
    let mut case = Case::new("row D13");
    case.seed_inline(
        &new,
        "corpus-violation",
        D13_VIOLATION,
        &[("write.sh", WRITE_SH)],
    );
    let run = case.start_inline(&new, "corpus-violation", &[]);
    let journal = case.wait_journal(&run, "the gate", |j| {
        j.iter().any(|e| e["type"] == "review_requested")
    });
    case.kill_driver(&run);
    case.save();
    let violated = journal
        .iter()
        .position(|e| e["type"] == "read_only_violated")
        .unwrap_or_else(|| panic!("row D13: {}", summary(&journal)));
    let finished_at = journal
        .iter()
        .position(|e| e["type"] == "node_finished" && e["node"] == "work")
        .expect("row D13: work finished");
    assert!(violated < finished_at, "row D13: {}", summary(&journal));
    assert_eq!(journal[finished_at]["status"], "failed");

    let from = journal.len();
    let ran = case.read(&new, &run);
    assert!(!ran.stdout.contains("unknown event"), "row D13: {ran:?}");
    case.approve_and_resume(&new, &run);
    let expected = routing(&case.journal(&run), from);
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        let ran = case.read(&reader, &run);
        assert!(
            ran.stdout.contains("unknown event"),
            "row D13: {} skips the read-only records: {ran:?}",
            label(&reader)
        );
        case.approve_and_resume(&reader, &run);
        assert_eq!(
            routing(&case.journal(&run), from),
            expected,
            "row D13: {} routes as the new binary",
            label(&reader)
        );
        case.stop(&reader, &run);
    }
}

/// Row S1's playbook and scenario: one script node, answered by the
/// scenario, so the scenario run ends at once.
const SCENARIO_PLAYBOOK: &str = "schema: 2\nid: corpus-scenario\nname: corpus scenario\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: scripts/work.sh, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";
const SCENARIO: &str = "schema: 1\nid: green\nanswers:\n  work:\n    - { visit: 1, script: { exit: 0, stdout: ok } }\n";

/// Row S1: a quick scenario run of the new binary is invisible to both
/// released binaries: `apb runs` lists no run, and `apb runs <id>` and
/// `apb resume <id>` do not find it. The new binary's own `apb runs` is the
/// control: it does not list it either, while the scenario run is on disk
/// with `origin: scenario`.
#[test]
fn rows1_new_scenario_run_old_binaries_do_not_see_it() {
    let case = Case::new("row S1");
    let pb = case.root().join(".apb/playbooks/corpus-scenario");
    fs::create_dir_all(pb.join("1.0.0/scripts")).unwrap();
    fs::create_dir_all(pb.join("evals/scenarios")).unwrap();
    fs::write(pb.join("1.0.0/playbook.yaml"), SCENARIO_PLAYBOOK).unwrap();
    fs::write(pb.join("1.0.0/scripts/work.sh"), "exit 99\n").unwrap();
    fs::write(pb.join("current"), "1.0.0").unwrap();
    fs::write(pb.join("evals/scenarios/green.yaml"), SCENARIO).unwrap();
    apb_core::registry::init_project(&case.root()).unwrap();

    let played = case.run(
        &new_binary(),
        &[
            "eval",
            "corpus-scenario",
            "--level",
            "quick",
            "--scenario",
            "green",
            "--json",
        ],
    );
    assert!(played.ok(), "row S1: the scenario plays: {played:?}");
    let report: Value = serde_json::from_str(&played.stdout).unwrap();
    let run = report["reports"][0]["run_id"].as_str().unwrap().to_string();
    let mut readers = Vec::new();
    if let Some((beta1, v025)) = old_readers(&case) {
        readers.extend([beta1, v025]);
    }
    readers.push(new_binary());
    for bin in &readers {
        let listed = case.run(bin, &["runs"]);
        assert!(
            !listed.stdout.contains(&run),
            "row S1: {} runs lists the scenario run: {listed:?}",
            label(bin)
        );
        for args in [vec!["runs", run.as_str()], vec!["resume", run.as_str()]] {
            let ran = case.run(bin, &args);
            assert!(
                !ran.ok() && ran.stderr.contains("not found"),
                "row S1: {} {args:?} found the scenario run: {ran:?}",
                label(bin)
            );
        }
    }
    let manifest = fs::read_to_string(
        case.root()
            .join(".apb/scenario-runs")
            .join(&run)
            .join("manifest.yaml"),
    )
    .unwrap();
    // The playbook id holds the word too: read the field, not the text.
    let m: Value = yaml_serde::from_str(&manifest).unwrap();
    assert_eq!(m["origin"]["kind"], "scenario", "row S1: {manifest}");
    assert_eq!(m["origin"]["scenario"], "green", "row S1: {manifest}");
}

/// Row E1's playbook: a script node and a list node that declare
/// `results:`, then a gate.
const E1_RESULTS: &str = r#"schema: 2
id: corpus-results
name: Journal corpus result snapshots
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/report.sh", runner: sh, results: [ { path: report.md } ] }
  - id: each
    type: script
    script: scripts/item.sh
    runner: sh
    results: []
    list:
      items: { inline: [ { id: a }, { id: b } ] }
      batch: { size: 1, retries: 0 }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: each }
  - { from: each, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

const REPORT_SH: &str = "echo 'the report' > report.md\necho reported\n";
const ITEM_SH: &str = "echo '{\"done\":true}'\n";

/// The snapshot rows the new binary lists for `run`, every one verified.
fn e1_rows_verified(case: &Case, run: &str) -> usize {
    let listed = case.run(&new_binary(), &["results", run, "--json"]);
    assert!(listed.ok(), "{}: results: {listed:?}", case.name);
    let v: Value = serde_json::from_str(&listed.stdout).unwrap();
    let rows = v["rows"].as_array().cloned().unwrap_or_default();
    for row in &rows {
        assert_eq!(row["verified"], "ok", "{}: {row}", case.name);
    }
    rows.len()
}

/// Row E1: result stamps and per-attempt heads are fields older apb
/// ignores. The new binary writes a run with stamps on the work node, on
/// each list item and on the list node, waiting at the gate; a synthetic
/// `artifacts_committed` with `attempt` and `attempts` goes before the list
/// node's settlement. Both old binaries read it (beta.1 without an unknown
/// record; 0.25.0 skips only the list records it predates), approve and resume it routing as the new binary does, and stop it; the
/// snapshots stay readable and verified for the new binary after each.
#[test]
fn rowe1_result_stamps_old_binaries_read_resume_stop() {
    let new = new_binary();
    let mut case = Case::new("row E1");
    case.seed_inline(
        &new,
        "corpus-results",
        E1_RESULTS,
        &[("report.sh", REPORT_SH), ("item.sh", ITEM_SH)],
    );
    let run = case.start_inline(&new, "corpus-results", &[]);
    let journal = case.wait_journal(&run, "the gate", |j| {
        j.iter().any(|e| e["type"] == "review_requested")
    });
    case.kill_driver(&run);
    let stamped = |kind: &str, node: &str| {
        journal
            .iter()
            .filter(|e| e["type"] == kind && e["node"] == node)
            .filter(|e| {
                e["result"]["snapshot_id"]
                    .as_str()
                    .is_some_and(|s| s.len() == 64)
            })
            .count()
    };
    assert_eq!(
        stamped("node_finished", "work"),
        1,
        "row E1: {}",
        summary(&journal)
    );
    assert_eq!(
        stamped("list_item_settled", "each"),
        2,
        "row E1: {}",
        summary(&journal)
    );
    assert_eq!(
        stamped("node_finished", "each"),
        1,
        "row E1: {}",
        summary(&journal)
    );
    rewrite_journal(&case.run_dir(&run), |mut records| {
        let at = records
            .iter()
            .position(|e| e["type"] == "node_finished" && e["node"] == "each")
            .expect("each settled");
        let head = "a".repeat(40);
        records.insert(
            at,
            serde_json::json!({
                "seq": 0, "ts": records[at]["ts"], "type": "artifacts_committed",
                "node": "each", "before": "b".repeat(40), "after": head, "commits": [],
                "attempt": 7, "attempts": [ { "attempt": 7, "head": head } ],
            }),
        );
        records
    });
    case.save();
    let rows = e1_rows_verified(&case, &run);
    assert_eq!(rows, 4, "row E1: work, two items and the list node");

    let from = case.journal(&run).len();
    let ran = case.read(&new, &run);
    assert!(!ran.stdout.contains("unknown event"), "row E1: {ran:?}");
    case.approve_and_resume(&new, &run);
    let expected = routing(&case.journal(&run), from);
    assert_eq!(e1_rows_verified(&case, &run), rows);
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    // 0.25.0 predates list mode: it skips the list records, counted, and
    // nothing else; beta.1 knows them, so any unknown record there would be
    // a stamp read as a record.
    for (reader, knows_lists) in [(beta1, true), (v025, false)] {
        let ran = case.read(&reader, &run);
        assert_eq!(
            ran.stdout.contains("unknown event"),
            !knows_lists,
            "row E1: {} reads the stamps as fields it ignores: {ran:?}",
            label(&reader)
        );
        case.approve_and_resume(&reader, &run);
        assert_eq!(
            routing(&case.journal(&run), from),
            expected,
            "row E1: {} routes as the new binary",
            label(&reader)
        );
        assert_eq!(
            e1_rows_verified(&case, &run),
            rows,
            "row E1: the snapshots survive {}'s resume",
            label(&reader)
        );
        case.stop(&reader, &run);
    }
}

// ------------------------------------------------- execution identity (21a)

/// The writer playbook of rows X1 and X2 (`corpus-exec`).
const EXEC: &str = "corpus-exec";

impl Case {
    /// Seeds `corpus-exec` with `writer`: the fixture, its profile (primary
    /// `haiku`, one fallback `sonnet`) and a stub agent that prints the
    /// `success_check` marker only on `sonnet`. While `compose.hold` exists
    /// in the case directory every call waits.
    fn seed_exec(&mut self, writer: &Path) {
        self.seed(writer);
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal-corpus");
        let pdir = self.root().join(".apb/playbooks").join(EXEC);
        copy_tree(&fixtures.join(EXEC), &pdir.join("1.0.0"));
        fs::write(pdir.join("current"), "1.0.0").unwrap();
        let profile = self.root().join(".apb/profiles").join(EXEC);
        fs::create_dir_all(&profile).unwrap();
        fs::write(
            profile.join("profile.yaml"),
            "name: corpus-exec\ndescription: the stub agent chain\nexecutor:\n  agent: claude-code\n  model: haiku\n  fallbacks:\n    - { agent: claude-code, model: sonnet }\n",
        )
        .unwrap();
        fs::write(profile.join("SOUL.md"), "").unwrap();
        let d = self.dir.path().display().to_string();
        let stub = self.dir.path().join("exec-agent.sh");
        fs::write(
            &stub,
            format!(
                "#!/bin/sh\nwhile [ -f '{d}/compose.hold' ]; do sleep 0.05; done\nfor a in \"$@\"; do\n  if [ \"$a\" = sonnet ]; then echo PASSES; exit 0; fi\ndone\necho done\n"
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        self.env.push(("APB_AGENT_CMD", stub.display().to_string()));
        self.env.push(("APB_PAUSE_GRACE_MS", "200".to_string()));
    }

    /// `writer` runs `corpus-exec` to its gate and the driver is killed:
    /// `work`'s primary reply was rejected, its fallback passed.
    fn write_exec_at_gate(&mut self, writer: &Path) -> String {
        self.seed_exec(writer);
        let run = self.start_inline(writer, EXEC, &[]);
        self.wait_journal(&run, "the gate", |j| {
            j.iter().any(|e| e["type"] == "review_requested")
        });
        self.kill_driver(&run);
        run
    }
}

/// The run of `reader` resumed from the saved state: it ends succeeded,
/// with `ok` composed once.
fn x_resume(case: &Case, reader: &Path, run: &str) {
    case.restore();
    let ran = case.run(reader, &["resume", run]);
    assert!(ran.ok(), "{}: {} resume: {ran:?}", case.name, label(reader));
    let journal = case.journal(run);
    assert_eq!(
        last_of(&journal, "run_finished").map(|e| e["outcome"].clone()),
        Some(Value::from("succeeded")),
        "{}: {} resumed the run to a success: {}",
        case.name,
        label(reader),
        summary(&journal)
    );
}

/// Row X1: the new binary writes an executor chain: `work`'s primary reply
/// is rejected, so its fallback is entered with `check_rejected` on
/// `fallback_triggered` (and `executed_by`) and on the fallback attempt's
/// `exec.entered`. After the gate a `--now` pause stops the composer of
/// `ok`: an `interrupted` attempt with its chain position, no transition,
/// no `node_finished`. A synthetic host transition (`from`/`to` `host`,
/// hint models, `agent_failed`, `executed_by: host`) goes after the real
/// one. Both old binaries read the run without an unknown record, resume
/// it composing again and routing as the new binary does, and stop it.
#[test]
fn rowx1_execution_identity_old_binaries_read_resume_stop() {
    let new = new_binary();
    let mut case = Case::new("row X1");
    let run = case.write_exec_at_gate(&new);
    let journal = case.journal(&run);
    let fb: Vec<_> = journal
        .iter()
        .filter(|e| e["type"] == "fallback_triggered" && e["node"] == "work")
        .collect();
    assert_eq!(fb.len(), 1, "row X1: {}", summary(&journal));
    assert_eq!(fb[0]["reason"], "check_rejected");
    assert_eq!(fb[0]["executed_by"], "cli");
    let entered = journal
        .iter()
        .rfind(|e| e["type"] == "attempt_started" && e["node"] == "work")
        .map(|e| e["exec"]["entered"].clone());
    assert_eq!(
        entered
            .as_ref()
            .map(|e| (e["from_step"].clone(), e["reason"].clone())),
        Some((Value::from(0), Value::from("check_rejected"))),
        "row X1: {}",
        summary(&journal)
    );

    // The composer, paused `--now` while it composes.
    let hold = case.dir.path().join("compose.hold");
    fs::write(&hold, "").unwrap();
    let ran = case.run(&new, &["review", &run, "gate", "--decision", "approved"]);
    assert!(ran.ok(), "row X1: review: {ran:?}");
    let mut resume = case.spawn(&new, &["resume", &run]);
    case.wait_journal(&run, "the composer's attempt", |j| {
        j.iter()
            .any(|e| e["type"] == "attempt_started" && e["node"] == "ok")
    });
    let ran = case.run(&new, &["pause", &run, "--now"]);
    assert!(ran.ok(), "row X1: pause --now: {ran:?}");
    case.wait_journal(&run, "the pause", |j| {
        j.iter().any(|e| e["type"] == "run_paused")
    });
    let start = Instant::now();
    while resume.child.try_wait().unwrap().is_none() {
        assert!(start.elapsed() < DEADLINE, "row X1: the paused driver ends");
        std::thread::sleep(Duration::from_millis(20));
    }
    fs::remove_file(&hold).unwrap();
    let journal = case.journal(&run);
    let of_ok = |kind: &str| {
        journal
            .iter()
            .filter(|e| e["type"] == kind && e["node"] == "ok")
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        of_ok("node_finished").len(),
        0,
        "row X1: {}",
        summary(&journal)
    );
    assert_eq!(
        of_ok("fallback_triggered").len(),
        0,
        "row X1: no transition"
    );
    let started = of_ok("attempt_started");
    assert_eq!(started.len(), 1, "row X1: {}", summary(&journal));
    assert_eq!(started[0]["exec"]["step"], 0, "row X1: {}", started[0]);
    assert_eq!(of_ok("attempt_finished")[0]["status"], "interrupted");

    rewrite_journal(&case.run_dir(&run), |mut records| {
        let at = records
            .iter()
            .position(|e| e["type"] == "fallback_triggered" && e["node"] == "work")
            .expect("work's transition");
        let ts = records[at]["ts"].clone();
        records.insert(
            at + 1,
            serde_json::json!({
                "seq": 0, "ts": ts, "type": "fallback_triggered", "node": "work",
                "from": "host", "to": "host", "from_model": "opus", "to_model": "sonnet",
                "reason": "agent_failed", "executed_by": "host",
            }),
        );
        records
    });
    case.save();

    let from = case.journal(&run).len();
    let ran = case.read(&new, &run);
    assert!(
        ran.stdout.contains("entered from step 0: check_rejected"),
        "row X1: {ran:?}"
    );
    x_resume(&case, &new, &run);
    let journal = case.journal(&run);
    let composed: Vec<_> = journal[from..]
        .iter()
        .filter(|e| e["type"] == "attempt_started" && e["node"] == "ok")
        .collect();
    assert_eq!(composed.len(), 1, "row X1: {}", summary(&journal));
    assert_eq!(
        composed[0]["exec"]["step"], 0,
        "the resume composes from step 0"
    );
    assert!(composed[0]["exec"].get("entered").is_none());
    let expected = routing(&journal, from);
    assert_eq!(expected, ["\"ok\" \"succeeded\""], "row X1");
    case.stop(&new, &run);

    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        let ran = case.read(&reader, &run);
        assert!(
            !ran.stdout.contains("unknown event"),
            "row X1: {} reads the new fields as fields it ignores: {ran:?}",
            label(&reader)
        );
        x_resume(&case, &reader, &run);
        assert_eq!(
            routing(&case.journal(&run), from),
            expected,
            "row X1: {} routes as the new binary",
            label(&reader)
        );
        case.stop(&reader, &run);
    }
}

/// Row X2 (card A9): beta.1 writes a run whose `work` falls back (beta.1
/// journals no chain position and no reason) and resumes it to its end.
/// The new binary's `apb stats` reads it: the transition is `not_recorded`,
/// never reclassified, the step row has no position, and the executor
/// digest comes from the manifest. `apb runs <id>` says `position not
/// recorded`.
#[test]
fn rowx2_beta1_fallback_run_new_report_reads_not_recorded() {
    let mut case = Case::new("row X2");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let new = new_binary();
    let run = case.write_exec_at_gate(&beta1);
    case.save();
    case.approve_and_resume(&beta1, &run);
    let journal = case.journal(&run);
    let fb = journal
        .iter()
        .find(|e| e["type"] == "fallback_triggered" && e["node"] == "work")
        .unwrap_or_else(|| panic!("row X2: beta.1 fell back: {}", summary(&journal)));
    assert!(fb.get("reason").is_none() && fb.get("executed_by").is_none());

    let ran = case.run(&new, &["stats", "--playbook", EXEC, "--json"]);
    assert!(ran.ok(), "row X2: stats: {ran:?}");
    let report: Value = serde_json::from_str(&ran.stdout).unwrap();
    let work = report["versions"][0]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "work")
        .cloned()
        .unwrap_or_else(|| panic!("row X2: work in {report}"));
    let groups = work["executors"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "row X2: {work}");
    let g = &groups[0];
    assert!(
        g["executor"].as_str().is_some_and(|x| !x.is_empty()),
        "row X2: the executor digest comes from the manifest: {g}"
    );
    let steps = g["steps"].as_array().unwrap();
    assert!(
        steps.iter().all(|s| s.get("step").is_none()),
        "row X2: no position recorded: {g}"
    );
    assert_eq!(
        steps
            .iter()
            .map(|s| s["attempts"].as_u64().unwrap())
            .sum::<u64>(),
        2,
        "row X2: {g}"
    );
    let reasons: Vec<_> = g["transitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["reason"].clone(), t["count"].clone()))
        .collect();
    assert_eq!(
        reasons,
        vec![(Value::from("not_recorded"), Value::from(1))],
        "row X2: {g}"
    );

    let ran = case.run(&new, &["runs", &run]);
    assert!(ran.ok(), "row X2: runs: {ran:?}");
    assert!(
        ran.stdout.contains("position not recorded"),
        "row X2: {ran:?}"
    );
}

// ------------------------------------- approvals and person-only gates (20b)

/// Rows E2 to E8's playbook: `work` (held while `hold.work` exists, its
/// report snapshotted) and `side` (held while `hold.side` exists) run side
/// by side into a person-only gate bound to `work`'s results; `ship` needs
/// that gate's approval. Every guarded feature in one definition.
const E_GUARDED: &str = r#"schema: 2
id: corpus-guarded
name: Journal corpus guarded run
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/work.sh", runner: sh, results: [ { path: report.md } ] }
  - { id: side, type: script, script: "scripts/side.sh", runner: sh }
  - { id: gate, type: human_review, options: [approved, rejected], person_only: true, approves: { results: [work] } }
  - { id: ship, type: script, script: "scripts/ship.sh", runner: sh, requires_approval: { gate: gate } }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: start, to: side }
  - { from: work, to: gate }
  - { from: side, to: gate }
  - { from: gate, to: ship, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
  - { from: ship, to: ok }
"#;

const E_WORK_SH: &str = ": > work.started\nwhile [ -f hold.work ]; do sleep 0.05; done\necho 'the report' > report.md\necho work done\n";
const E_SIDE_SH: &str =
    ": > side.started\nwhile [ -f hold.side ]; do sleep 0.05; done\necho side done\n";
const E_SHIP_SH: &str = "echo shipped\n";

/// The features [`E_GUARDED`] uses, as its `run_guard` names them.
const E_FEATURES: [&str; 3] = ["approval_binding", "person_only", "required_approval"];

/// A feature no apb knows (row E8).
const FUTURE_FEATURE: &str = "corpus_future_feature";

impl Case {
    /// Seeds [`E_GUARDED`] with the new binary.
    fn seed_guarded(&mut self) {
        self.seed_inline(
            &new_binary(),
            "corpus-guarded",
            E_GUARDED,
            &[
                ("work.sh", E_WORK_SH),
                ("side.sh", E_SIDE_SH),
                ("ship.sh", E_SHIP_SH),
            ],
        );
    }

    /// The new binary runs [`E_GUARDED`] to its person-only gate; the state
    /// saved is that run waiting at the gate with its driver gone (E2, E5,
    /// E6, E7).
    fn write_guarded_waiting(&mut self) -> String {
        self.seed_guarded();
        let run = self.start_inline(&new_binary(), "corpus-guarded", &[]);
        let journal = self.wait_journal(&run, "the gate", |j| {
            j.iter().any(|e| e["type"] == "review_requested")
        });
        self.kill_driver(&run);
        let provenance = journal
            .iter()
            .position(|e| e["type"] == "run_provenance")
            .expect("run_provenance");
        assert_run_guard_after(&journal, provenance, self.name, &E_FEATURES);
        let requested = last_of(&journal, "review_requested").unwrap();
        assert!(
            requested["binding"]["nonce"].is_string(),
            "{}: the gate's request is bound: {requested}",
            self.name
        );
        assert_run_guard_follows_last_checkpoint(&journal, self.name, &E_FEATURES);
        self.save();
        run
    }

    /// `apb wait <run>` by the new binary: the run waits for a person (3).
    fn waits_for_review(&self, run: &str) {
        let waited = self.run(&new_binary(), &["wait", run, "--timeout", "30"]);
        assert!(
            waited.code == Some(3) && waited.stdout.contains("gate"),
            "{}: the run waits at the gate: {waited:?}",
            self.name
        );
    }
}

/// The record right after the journal's last checkpoint is the run guard
/// naming `requires`.
fn assert_run_guard_follows_last_checkpoint(journal: &[Value], name: &str, requires: &[&str]) {
    let at = journal
        .iter()
        .rposition(is_checkpoint)
        .unwrap_or_else(|| panic!("{name}: a checkpoint"));
    assert_run_guard_after(journal, at, name, requires);
}

/// The record right after record `at` is the run guard naming `requires`.
fn assert_run_guard_after(journal: &[Value], at: usize, name: &str, requires: &[&str]) {
    let next = journal.get(at + 1);
    assert_eq!(
        next.map(|e| (e["type"].clone(), e["requires"].clone())),
        Some((Value::from("run_guard"), Value::from(requires.to_vec()))),
        "{name}: the run guard follows record {at} in the same write: {}",
        summary(&journal[at..])
    );
}

/// Row E2: the new binary's run waits at a bound person-only gate. A
/// person-only gate refuses the new `apb review` without a terminal. Both
/// old binaries read the run (the guard counted as an unknown event),
/// refuse to resume it, naming `run_guard`, and stop it. Their `apb
/// review` appends an entry without a request stamp: the new doctor names
/// it never consumed, and a new resume waits at the gate with no decision.
#[test]
fn e2_person_only_gate_old_binaries_read_refuse_review_unconsumed_stop() {
    let mut case = Case::new("row E2");
    let run = case.write_guarded_waiting();
    let new = new_binary();
    case.restore();
    let refused = case.run(&new, &["review", &run, "gate", "--decision", "approved"]);
    assert!(
        !refused.ok()
            && refused.stderr.contains("is person_only")
            && refused.stderr.contains("cli_unattended"),
        "row E2: the new review without a terminal is refused: {refused:?}"
    );
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        let ran = case.read(&reader, &run);
        assert!(
            ran.stdout.contains("unknown event"),
            "row E2: {} counts the guard it skipped: {ran:?}",
            label(&reader)
        );
        case.resume_refused(&reader, &run, "run_guard");

        // The old review appends an entry; the new drive never takes it.
        case.restore();
        let ran = case.run(&reader, &["review", &run, "gate", "--decision", "approved"]);
        assert!(ran.ok(), "row E2: {} review: {ran:?}", label(&reader));
        let entries = fs::read_to_string(case.run_dir(&run).join("reviews.jsonl")).unwrap();
        assert_eq!(
            entries.lines().count(),
            1,
            "row E2: {} appended one entry: {entries}",
            label(&reader)
        );
        let doctor = case.run(&new, &["doctor", "--run", &run]);
        assert!(
            doctor
                .stdout
                .contains("1 decision(s) in the review channel this apb's drive never takes")
                && doctor.stdout.contains("for `gate` (no request stamp)"),
            "row E2: the new doctor names {}'s entry unconsumed: {doctor:?}",
            label(&reader)
        );
        let before = case.journal(&run).len();
        let resume = case.spawn(&new, &["resume", &run]);
        case.wait_journal(&run, "the new resume", |j| {
            j.len() > before && j[before..].iter().any(|e| e["type"] == "run_resumed")
        });
        case.waits_for_review(&run);
        // Twenty looks of the drive at the gate (it looks every 50 ms).
        std::thread::sleep(Duration::from_millis(1000));
        let journal = case.journal(&run);
        assert!(
            !journal.iter().any(|e| e["type"] == "review_decided"),
            "row E2: the new drive consumed {}'s entry: {}",
            label(&reader),
            summary(&journal)
        );
        drop(resume);
        case.forget_driver(&run);

        case.stop(&reader, &run);
        let shown = case.run(&new, &["runs", &run]);
        assert!(
            shown.ok() && shown.stdout.contains("aborted"),
            "row E2: the new binary reads the run {} stopped: {shown:?}",
            label(&reader)
        );
    }
}

/// Waits until the driver of `run` (its `driver.pid`, if any) is gone.
fn wait_driver_gone(case: &Case, run: &str) {
    if let Some(pid) = apb_engine::driver::read_driver_pid(&case.run_dir(run)) {
        let start = Instant::now();
        while crate::common::sig::alive(pid) {
            assert!(
                start.elapsed() < DEADLINE,
                "{}: the driver exits",
                case.name
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Row E3: an operator pause while the run waits at the person-only gate
/// (`run_paused` and its guard). Both old resumes refuse, naming
/// `run_guard`; the new resume continues: it journals `run_resumed` and
/// waits at the gate again, the guard still standing.
#[test]
fn e3_paused_at_the_gate_old_refuse_new_continues() {
    let mut case = Case::new("row E3");
    let new = new_binary();
    case.seed_guarded();
    let run = case.start_inline(&new, "corpus-guarded", &[]);
    case.wait_journal(&run, "the gate", |j| {
        j.iter().any(|e| e["type"] == "review_requested")
    });
    let ran = case.run(&new, &["pause", &run]);
    assert!(ran.ok(), "row E3: pause: {ran:?}");
    let journal = case.wait_journal(&run, "the pause and its guard", |j| {
        j.iter().any(|e| e["type"] == "run_paused")
            && j.last().is_some_and(|e| e["type"] == "run_guard")
    });
    let paused = journal
        .iter()
        .rposition(|e| e["type"] == "run_paused")
        .unwrap();
    assert_eq!(paused + 2, journal.len(), "row E3: {}", summary(&journal));
    assert_run_guard_after(&journal, paused, "row E3", &E_FEATURES);
    wait_driver_gone(&case, &run);
    case.forget_driver(&run);
    case.save();
    for reader in old_readers(&case)
        .map(|(b, v)| [b, v])
        .into_iter()
        .flatten()
    {
        case.resume_refused(&reader, &run, "run_guard");
    }
    case.restore();
    let before = case.journal(&run).len();
    let resume = case.spawn(&new, &["resume", &run]);
    let journal = case.wait_journal(&run, "the new resume", |j| {
        j.len() > before && j[before..].iter().any(|e| e["type"] == "run_resumed")
    });
    case.waits_for_review(&run);
    drop(resume);
    case.forget_driver(&run);
    let added: Vec<&str> = journal[before..]
        .iter()
        .filter_map(|e| e["type"].as_str())
        .collect();
    assert_eq!(added.first(), Some(&"run_resumed"), "row E3: {added:?}");
    let journal = case.journal(&run);
    assert!(
        !journal[before..]
            .iter()
            .any(|e| e["type"] == "review_decided" || e["type"] == "node_started"),
        "row E3: the resume only waits at the gate: {}",
        summary(&journal[before..])
    );
}

/// Row E4: `work` and `side` run side by side before the gate, so `ship`
/// (`requires_approval`) is not reached. The new binary's driver is lost
/// after `work`'s checkpoint and its guard (`side` still runs); the new
/// binary resumes it, and its driver is lost again after `side`'s
/// checkpoint and its guard. Both old resumes refuse after each, naming
/// `run_guard`.
#[test]
fn e4_two_sibling_checkpoints_old_resumes_refuse_after_each() {
    let mut case = Case::new("row E4");
    let new = new_binary();
    case.seed_guarded();
    for hold in ["hold.work", "hold.side"] {
        fs::write(case.root().join(hold), "").unwrap();
    }
    let run = case.start_inline(&new, "corpus-guarded", &[]);
    case.wait_file("side.started");
    fs::remove_file(case.root().join("hold.work")).unwrap();
    case.wait_journal(&run, "work's checkpoint and its guard", |j| {
        j.iter()
            .position(|e| e["type"] == "node_finished" && e["node"] == "work")
            .is_some_and(|at| j.len() > at + 1)
    });
    case.kill_driver(&run);
    let journal = case.journal(&run);
    let work = journal
        .iter()
        .position(|e| e["type"] == "node_finished" && e["node"] == "work")
        .unwrap();
    assert_run_guard_after(&journal, work, "row E4", &E_FEATURES);
    assert!(
        !journal
            .iter()
            .any(|e| e["type"] == "node_finished" && e["node"] == "side"),
        "row E4: side still runs: {}",
        summary(&journal)
    );
    assert_run_guard_follows_last_checkpoint(&journal, "row E4", &E_FEATURES);
    case.save();
    let old: Vec<PathBuf> = old_readers(&case)
        .map(|(b, v)| vec![b, v])
        .unwrap_or_default();
    for reader in &old {
        case.resume_refused(reader, &run, "run_guard");
    }

    // The new binary resumes it: `side` runs again, held, then finishes.
    case.restore();
    fs::remove_file(case.root().join("side.started")).unwrap();
    let before = case.journal(&run).len();
    let resume = case.spawn(&new, &["resume", &run]);
    case.wait_file("side.started");
    fs::remove_file(case.root().join("hold.side")).unwrap();
    case.wait_journal(&run, "side's checkpoint and its guard", |j| {
        j.iter()
            .position(|e| e["type"] == "node_finished" && e["node"] == "side")
            .is_some_and(|at| j.len() > at + 1)
    });
    drop(resume);
    case.forget_driver(&run);
    let journal = case.journal(&run);
    assert!(
        journal[before..].iter().any(|e| e["type"] == "run_resumed"),
        "row E4: {}",
        summary(&journal)
    );
    let side = journal
        .iter()
        .position(|e| e["type"] == "node_finished" && e["node"] == "side")
        .unwrap();
    assert_run_guard_after(&journal, side, "row E4", &E_FEATURES);
    assert_run_guard_follows_last_checkpoint(&journal, "row E4", &E_FEATURES);
    assert!(
        !journal
            .iter()
            .any(|e| e["type"] == "node_started" && e["node"] == "ship"),
        "row E4: ship is not reached: {}",
        summary(&journal)
    );
    case.save();
    for reader in &old {
        case.resume_refused(reader, &run, "run_guard");
    }
}

/// Row E5 (a stated limit, not a guarantee): the guard holds only while
/// the new binary writes the last checkpoint. An old binary that stops the
/// new binary's guarded run waiting at the person-only gate appends its own
/// `run_aborted` with no guard after it; its first resume only applies that
/// stop again (exit 1, `run_resumed`, `run_aborted`); its `apb review`
/// appends an unstamped entry; and its second resume skips the guards (all
/// before its own last checkpoint), decides the gate with that entry and
/// runs `ship` (exit 0). This pins that sequence for both old binaries, and
/// that the new binary reads the run as it ended without claiming a person
/// decided the gate (the decision carries no channel).
#[test]
fn e5_old_stop_then_old_resume_continues_past_the_guard_stated_limit() {
    let mut case = Case::new("row E5");
    let run = case.write_guarded_waiting();
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    let new = new_binary();
    for reader in [beta1, v025] {
        let name = format!("row E5: {}", label(&reader));
        case.stop(&reader, &run);
        assert_ne!(
            case.journal(&run)
                .iter()
                .rev()
                .nth(1)
                .map(|e| e["type"].clone()),
            Some(Value::from("run_guard")),
            "{name}: no guard follows the old stop's run_aborted either"
        );
        let stopped = case.journal(&run).len();
        let ran = case.run(&reader, &["resume", &run]);
        assert!(
            ran.code == Some(1)
                && ran
                    .stdout
                    .contains("this resume only applied a stop that was still pending"),
            "{name}: the first resume applies the pending stop: {ran:?}"
        );
        let journal = case.journal(&run);
        assert_eq!(
            types_from(&journal, stopped),
            ["run_resumed", "run_aborted"],
            "{name}"
        );

        let ran = case.run(&reader, &["review", &run, "gate", "--decision", "approved"]);
        assert!(ran.ok(), "{name}: review: {ran:?}");
        let entries = fs::read_to_string(case.run_dir(&run).join("reviews.jsonl")).unwrap();
        let entry: Value = serde_json::from_str(entries.trim()).unwrap();
        assert!(
            entries.lines().count() == 1
                && entry["node"] == "gate"
                && entry["request"].is_null()
                && entry["channel"].is_null(),
            "{name}: one unstamped entry: {entries}"
        );

        let before = case.journal(&run).len();
        let ran = case.run(&reader, &["resume", &run]);
        assert!(
            ran.ok()
                && ran.stdout.contains("finished: succeeded")
                && ran
                    .stderr
                    .contains("skipping 4 event(s) of a type this apb")
                && ran.stderr.contains("(run_guard)"),
            "{name}: the second resume continues past the guards: {ran:?}"
        );
        let journal = case.journal(&run);
        let added = types_from(&journal, before);
        assert_eq!(added.first(), Some(&"run_resumed"), "{name}: {added:?}");
        assert_eq!(added.last(), Some(&"run_finished"), "{name}: {added:?}");
        let decided = last_of(&journal, "review_decided").expect("review_decided");
        assert!(
            decided["node"] == "gate"
                && decided["decision"] == "approved"
                && decided.get("via").is_none()
                && decided.get("decided_by").is_none(),
            "{name}: the old decision names no channel and no decider: {decided}"
        );
        assert!(
            journal[before..]
                .iter()
                .any(|e| e["type"] == "node_finished"
                    && e["node"] == "ship"
                    && e["status"] == "succeeded"),
            "{name}: ship ran: {}",
            summary(&journal[before..])
        );

        // The new binary reads the run as it ended.
        let shown = case.run(&new, &["runs", &run]);
        assert!(
            shown.ok()
                && shown
                    .stdout
                    .contains(&format!("{run}\tcorpus-guarded 1.0.0\tsucceeded"))
                && shown.stdout.contains("outcome: Run succeeded")
                && shown
                    .stdout
                    .contains("resumed by an older apb (no version recorded) (2 times)")
                && !shown.stdout.contains("person"),
            "{name}: the new binary reads the run finished, no person named: {shown:?}"
        );
        let doctor = case.run(&new, &["doctor", "--run", &run]);
        assert!(
            doctor.stdout.contains("[ok]   run: folded status: succeeded")
                && doctor.stdout.contains("the resume at seq 20 ran on an older apb")
                && doctor.stdout.contains(
                    "1 decision(s) in the review channel this apb's drive never takes: #0 for `gate` (no request stamp)"
                )
                && !doctor.stdout.contains("person"),
            "{name}: the new doctor: {doctor:?}"
        );
    }
}

/// The record types of `journal` from record `from` on.
fn types_from(journal: &[Value], from: usize) -> Vec<&str> {
    journal[from..]
        .iter()
        .filter_map(|e| e["type"].as_str())
        .collect()
}

/// Row E6 (synthetic fault): the run guard's line that follows the last
/// checkpoint is cut short, as a crash inside that one write leaves it
/// (nothing after it reached the file). Every binary's resume refuses the
/// torn tail and writes nothing; every binary still reads the run.
#[test]
fn e6_synthetic_torn_run_guard_every_resume_refuses() {
    let mut case = Case::new("row E6");
    let run = case.write_guarded_waiting();
    let path = case.saved().join(&run).join("events.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let journal = journal_at(&case.saved().join(&run));
    let at = journal.iter().rposition(is_checkpoint).unwrap();
    // The lines up to the checkpoint, then half of the guard's line.
    let mut line = 0;
    for _ in 0..=at {
        line += text[line..].find('\n').unwrap() + 1;
    }
    let end = line + text[line..].find('\n').unwrap();
    assert!(text[line..end].contains("\"type\":\"run_guard\""), "row E6");
    fs::write(&path, &text[..line + (end - line) / 2]).unwrap();
    let new = new_binary();
    let old = old_readers(&case)
        .map(|(b, v)| [b, v])
        .into_iter()
        .flatten();
    for reader in std::iter::once(new).chain(old) {
        case.read(&reader, &run);
        case.restore();
        let before = case.journal_bytes(&run);
        let ran = case.run(&reader, &["resume", &run]);
        assert!(
            ran.code == Some(2)
                && ran
                    .stderr
                    .contains("resume failed: yaml error: EOF while parsing a string"),
            "row E6: {} refuses the torn tail: {ran:?}",
            label(&reader)
        );
        assert_eq!(
            case.journal_bytes(&run),
            before,
            "row E6: {} wrote nothing",
            label(&reader)
        );
    }
}

/// [`E_GUARDED`] without a guarded feature, as version 1.0.1.
fn e_unguarded() -> String {
    let yaml = E_GUARDED
        .replace("version: 1.0.0", "version: 1.0.1")
        .replace(", person_only: true, approves: { results: [work] }", "")
        .replace(", requires_approval: { gate: gate }", "");
    assert!(
        !yaml.contains("person_only")
            && !yaml.contains("approves")
            && !yaml.contains("requires_approval"),
        "{yaml}"
    );
    yaml
}

/// Row E7 (synthetic journal shape): the run waits at the gate, and a
/// patch to a definition with no guarded feature is written into the
/// journal by hand: `patch_applied`, then `run_guard []` ending the guard,
/// then `run_migrated`. A supervisor's patch is refused this change
/// (`approval_weakened`); the row pins how every binary reads a guard
/// that ended. The new binary resumes it and is paused at the gate: that
/// checkpoint carries no guard. From there beta.1 and 0.25.0 approve and
/// resume it with the new binary's routing.
#[test]
fn e7_synthetic_patch_drops_the_guard_old_binaries_resume_the_same() {
    let mut case = Case::new("row E7");
    let run = case.write_guarded_waiting();
    synthetic_patch_to(&case, &run, &e_unguarded(), "1.0.0", true);
    rewrite_journal(&case.saved().join(&run), |mut records| {
        let at = records
            .iter()
            .rposition(|e| e["type"] == "patch_applied")
            .unwrap();
        let ts = records[at]["ts"].clone();
        records.insert(
            at + 1,
            serde_json::json!({ "seq": 0, "ts": ts, "type": "run_guard", "requires": [] }),
        );
        records
    });
    let new = new_binary();
    case.restore();
    let before = case.journal(&run).len();
    let mut resume = case.spawn(&new, &["resume", &run]);
    case.wait_journal(&run, "the new resume", |j| {
        j.len() > before && j[before..].iter().any(|e| e["type"] == "run_resumed")
    });
    case.waits_for_review(&run);
    let ran = case.run(&new, &["pause", &run]);
    assert!(ran.ok(), "row E7: pause: {ran:?}");
    let start = Instant::now();
    while resume.child.try_wait().unwrap().is_none() {
        assert!(
            start.elapsed() < DEADLINE,
            "row E7: the paused resume exits"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(resume);
    case.forget_driver(&run);
    let journal = case.journal(&run);
    assert_eq!(
        journal.last().map(|e| e["type"].clone()),
        Some(Value::from("run_paused")),
        "row E7: the pause is the last record, no guard after it: {}",
        summary(&journal)
    );
    assert_eq!(
        last_of(&journal, "run_guard").map(|e| e["requires"].clone()),
        Some(serde_json::json!([])),
        "row E7: the latest guard is the patch's empty one"
    );
    case.save();

    let from = journal.len();
    case.approve_and_resume(&new, &run);
    let expected = routing(&case.journal(&run), from);
    assert!(
        expected.contains(&"\"ship\" \"succeeded\"".to_string()),
        "row E7: {expected:?}"
    );
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        case.approve_and_resume(&reader, &run);
        assert_eq!(
            routing(&case.journal(&run), from),
            expected,
            "row E7: {} routes as the new binary",
            label(&reader)
        );
    }
}

/// Row E8 (synthetic): a `run_guard` naming a known feature and one no apb
/// knows, appended after the last checkpoint of the new binary's plain
/// corpus run waiting at review. The new binary reads it with the guard
/// counted as unknown; with the gate approved its resume still refuses,
/// naming the unknown feature, and writes nothing; `apb stop` goes through.
#[test]
fn e8_synthetic_unknown_guard_feature_new_refuses_resume_stop_goes_through() {
    let mut case = Case::new("row E8");
    let new = new_binary();
    let run = case.write_waiting_at_review(&new);
    rewrite_journal(&case.saved().join(&run), |mut records| {
        let ts = records.last().unwrap()["ts"].clone();
        records.push(serde_json::json!({
            "seq": 0, "ts": ts, "type": "run_guard",
            "requires": ["person_only", FUTURE_FEATURE]
        }));
        records
    });
    let ran = case.read(&new, &run);
    assert!(
        ran.stdout.contains("1 unknown event"),
        "row E8: the guard is counted as unknown: {ran:?}"
    );
    case.restore();
    let ran = case.run(&new, &["review", &run, "gate", "--decision", "approved"]);
    assert!(ran.ok(), "row E8: review: {ran:?}");
    let before = case.journal_bytes(&run);
    let ran = case.run(&new, &["resume", &run]);
    assert!(
        !ran.ok()
            && ran.stderr.contains("written by a newer apb")
            && ran.stderr.contains(&format!("run_guard {FUTURE_FEATURE}")),
        "row E8: the new binary refuses to resume: {ran:?}"
    );
    assert_eq!(case.journal_bytes(&run), before, "row E8: wrote nothing");
    case.stop(&new, &run);
}

// ------------------------------------------- evidence for node outputs (20c)

/// Row E9's playbook: `work` (held while `hold.work` exists, its report
/// snapshotted) feeds `read`, an agent_task that reads it through an
/// evidence session; the run never gets that far.
const E9_EVIDENCE: &str = r#"schema: 2
id: corpus-evidence
name: Journal corpus evidence run
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/work.sh", runner: sh, results: [ { path: report.md } ] }
  - { id: read, type: agent_task, prompt: "READ", profile: corpus, evidence: { from: [work], on_invalid: fail } }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: read }
  - { from: read, to: ok }
"#;

/// Row E9: a run whose definition uses `evidence` carries a `run_guard`
/// naming it right after `run_provenance`. With its driver gone while
/// `work` runs, both old binaries read the run (the guard counted as an
/// unknown event), refuse to resume it, naming `run_guard`, and stop it.
#[test]
fn e9_evidence_guard_old_binaries_read_refuse_resume_stop() {
    let mut case = Case::new("row E9");
    let new = new_binary();
    case.seed_inline(
        &new,
        "corpus-evidence",
        E9_EVIDENCE,
        &[("work.sh", E_WORK_SH)],
    );
    fs::write(case.root().join("hold.work"), "").unwrap();
    let run = case.start_inline(&new, "corpus-evidence", &[]);
    let journal = case.wait_journal(&run, "work to start", |j| {
        j.iter()
            .any(|e| e["type"] == "node_started" && e["node"] == "work")
    });
    case.kill_driver(&run);
    let provenance = journal
        .iter()
        .position(|e| e["type"] == "run_provenance")
        .expect("run_provenance");
    assert_run_guard_after(&journal, provenance, "row E9", &["evidence"]);
    case.save();
    // The orphaned script ends on its own; the saved state is the one read.
    fs::remove_file(case.root().join("hold.work")).unwrap();
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        let ran = case.read(&reader, &run);
        assert!(
            ran.stdout.contains("unknown event"),
            "row E9: {} counts the guard it skipped: {ran:?}",
            label(&reader)
        );
        case.resume_refused(&reader, &run, "run_guard");
        case.stop(&reader, &run);
    }
}
