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
//! Row G1 runs a definition whose list block carries `pilot:`: beta.1
//! cannot parse it and refuses to start the playbook; 0.25.0 parses it,
//! drops the pilot and runs every item (recorded: its list block does not
//! deny unknown fields yet); both refuse to resume a run waiting at its
//! pilot gate, appending nothing either way.
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
//! refuse after each. E5: the limit the manifest guard closes: a guarded
//! run's manifest carries `execution.mode: cli+guard`, so after an old
//! binary stopped it, its resume still refuses (naming the execution
//! mode, not `run_guard`) and writes nothing; the new binary reads the
//! run as it ended, with no person named as the decider. E6 (synthetic fault): the guard's
//! line after the last checkpoint is torn; every resume refuses. E7
//! (synthetic journal shape): a guard ended by `run_guard []` after a
//! patch, and the next checkpoint carries no guard; the new binary reads
//! an ended guard as before, while the manifest guard still makes beta.1
//! and 0.25.0 refuse to resume it and stop it. No supervisor patch writes
//! this shape (it may not drop a guarded feature); the row pins how every
//! binary reads an ended guard. E8 (synthetic): a `run_guard` naming a
//! feature no apb knows; the new binary refuses to resume and stops it.
//!
//! Row E9 covers evidence for node outputs: a definition with `evidence`
//! carries a `run_guard` naming `evidence` after `run_provenance`; both old
//! binaries read the run, refuse to resume it and stop it.
//!
//! Row V1 covers eval doubles: a doubled eval run's directory (a manifest
//! with account names only, `run.yaml` doubles, the `eval_doubles`
//! provenance and guard) on which each old binary's `apb connector call`
//! fails, sends nothing to a live service and runs no `{{cmd:...}}` helper.
//!
//! Row M1 covers MCP tool connectors: the new binary installs an MCP
//! connector whose program is the scripted stub, approves its tree and
//! account and records the account's pins in-process (no binary approves
//! an MCP connector without a terminal), and starts a playbook whose review
//! gate comes before a node granted one MCP function. Each old binary reads
//! that run; approves and resumes it, where the node's stub agent calls the
//! function through that old binary, which cannot load the snapshot, so the
//! call fails closed: no server starts and no send intent is journaled;
//! and refuses a new start with `connector_unresolved`. The whole row runs
//! in a child test process, so the in-process config dir is its own.
//!
//! Row S1 covers scenario runs: a quick scenario run the new binary plays
//! (`apb eval --level quick`) lives under `.apb/scenario-runs/` with
//! `origin: scenario` in its manifest; neither old binary lists it, shows
//! it or resumes it.
//!
//! Rows W1 and W2 pin the journal of a run that never parks on a timer
//! wait (the wait park change). W1: a script node, then a webhook wait
//! that times out after 1 s and falls through to the failure finish; its
//! journal is main's event-type sequence exactly, with no `run_guard`, no
//! `run_paused` and no control entry, and both old binaries read the run.
//! W2: the same with a timer wait of 1 s (under the release threshold, so
//! it never parks); its journal with every `run_guard` line removed is
//! main's sequence, and (added with the change) the guard lines it does
//! carry name exactly `wait_park` and stand right after `run_provenance`
//! and every checkpoint. The literal type sequences never change.
//!
//! Rows I1 to I3 pin an inbox wait against the old binaries (the inbox
//! waits change). I1: an inbox wait over an empty `echo-hooks` inbox times
//! out after 1 s and never parks; its journal, with every `run_guard` line
//! removed, is the timer wait's sequence with the wait's start line before
//! `wait_started` (`kind` `inbox`) and its end line before `wait_timeout`,
//! and both old binaries read the finished run (the new lines skipped and
//! counted) and refuse to resume it with a definition parse error. I2: a
//! long wait parks over its empty inbox; a stored delivery its `types`
//! pass and its `match` refuse wakes it through the park's waker, and the
//! resumed visit parks again over the examined delivery, leaving the
//! progress line before `run_paused`. The journal cut after the progress
//! line and before `run_paused` gives both old binaries a strict read they
//! refuse, and their stop still goes through (`run_aborted` appended).
//! I3: the same cut after the end pair, before
//! `node_finished`, with the same two results. The writer uses the engine
//! test suite's `echo-hooks` fixture connector, installed and approved in
//! the case's config dir; the deliveries are stored where the binary under
//! test reads them.
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

impl Drop for Case {
    fn drop(&mut self) {
        // A killed driver's read-only scripts copy stays behind in the
        // project: its write bits go back before the directory drops, so
        // the case leaves nothing behind under the target dir.
        writable_dirs(self.dir.path());
    }
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
        writable_dirs(&self.runs());
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

/// The case's temp directory goes away whole, even where a killed driver
/// left a read-only scripts copy in its project: the write bits are given
/// back before the drop, so nothing stays behind under the target dir.
#[test]
fn a_case_leaves_no_read_only_leftovers_behind() {
    let path;
    {
        let case = Case::new("cleanup row");
        path = case.dir.path().to_path_buf();
        let ro = case.root().join(".apb/runs/row/scripts");
        fs::create_dir_all(&ro).unwrap();
        fs::write(ro.join("look.sh"), "echo hi").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    }
    assert!(!path.exists(), "the case left {} behind", path.display());
}

/// The cleanup walk never panics on a directory it cannot list (it runs
/// inside `Case::drop`, where a panic aborts the whole test process), and
/// it restores read, write and search for the owner.
#[cfg(unix)]
#[test]
fn the_cleanup_walk_never_panics_on_an_unreadable_directory() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let blind = tmp.path().join("blind");
    fs::create_dir_all(blind.join("inner")).unwrap();
    // Write and search, no read: `read_dir` fails on it.
    fs::set_permissions(&blind, fs::Permissions::from_mode(0o311)).unwrap();
    writable_dirs(tmp.path());
    let mode = fs::symlink_metadata(&blind).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o700,
        0o700,
        "the owner keeps read, write and search: {mode:o}"
    );
    fs::set_permissions(&blind, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Gives every directory under `path` its owner read, write and search
/// bits: a run that may skip list items holds a read-only scripts copy
/// while it can execute. The walk runs inside `Case::drop` too, so an
/// error anywhere never panics: a directory it cannot list is left as it
/// is.
fn writable_dirs(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        let mode = meta.permissions().mode();
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o700));
        if let Ok(entries) = fs::read_dir(path) {
            for e in entries.flatten() {
                writable_dirs(&e.path());
            }
        }
    }
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
/// The number of unknown events a reader's `apb runs` output reports
/// (`<n> unknown event...`), 0 when it reports none.
fn unknown_count(stdout: &str) -> usize {
    let Some(at) = stdout.find(" unknown event") else {
        return 0;
    };
    stdout[..at]
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a count before `unknown event`")
}

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
    /// journal a newer apb wrote, naming `kind`, and writes nothing. For
    /// `cli+guard` the run's journal guard names first on a binary that
    /// checks the journal before the manifest mode, so `run_guard` also
    /// counts.
    fn resume_refused(&self, reader: &Path, run: &str, kind: &str) {
        self.restore();
        let before = self.journal_bytes(run);
        let ran = self.run(reader, &["resume", run]);
        let named =
            ran.stderr.contains(kind) || (kind == "cli+guard" && ran.stderr.contains("run_guard"));
        assert!(
            !ran.ok() && ran.stderr.contains("written by a newer apb") && named,
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
        let manifest = fs::read_to_string(self.run_dir(&run).join("manifest.yaml")).unwrap();
        assert!(
            manifest.contains("mode: cli+guard"),
            "{}: the guarded run's manifest carries the guard suffix: {manifest}",
            self.name
        );
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
        case.resume_refused(&reader, &run, "cli+guard");

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
        case.resume_refused(&reader, &run, "cli+guard");
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
        case.resume_refused(reader, &run, "cli+guard");
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
        case.resume_refused(reader, &run, "cli+guard");
    }
}

/// Row E5: the limit the manifest guard closes. A guarded run's manifest
/// carries `execution.mode: cli+guard`, so an old binary that stops the
/// new binary's guarded run waiting at the person-only gate appends its
/// own `run_aborted` with no guard after it, and its resume still refuses
/// (naming the execution mode, with "written by a newer apb") and writes
/// nothing, before and after its `apb review` appends an unstamped entry.
/// The new binary reads the run as it ended, with no person named as the
/// decider, and its doctor names the entry unconsumed.
#[test]
fn e5_old_stop_then_old_resume_still_refuses_the_manifest_guard() {
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
        for attempt in ["first", "second"] {
            if attempt == "second" {
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
            }
            let before = case.journal_bytes(&run);
            let ran = case.run(&reader, &["resume", &run]);
            assert!(
                !ran.ok()
                    && ran.stderr.contains("cli+guard")
                    && ran.stderr.contains("written by a newer apb"),
                "{name}: the {attempt} resume still refuses, naming the mode: {ran:?}"
            );
            assert_eq!(
                case.journal_bytes(&run),
                before,
                "{name}: the {attempt} resume wrote nothing"
            );
        }

        // The new binary reads the run as it ended.
        let shown = case.run(&new, &["runs", &run]);
        assert!(
            shown.ok() && shown.stdout.contains("aborted") && !shown.stdout.contains("person"),
            "{name}: the new binary reads the run aborted, no person named: {shown:?}"
        );
        let doctor = case.run(&new, &["doctor", "--run", &run]);
        assert!(
            doctor.stdout.contains("aborted")
                && doctor.stdout.contains(
                    "1 decision(s) in the review channel this apb's drive never takes: #0 for `gate` (no request stamp)"
                )
                && !doctor.stdout.contains("person"),
            "{name}: the new doctor: {doctor:?}"
        );
    }
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
    let old: Vec<_> = old_readers(&case)
        .map(|(b, v)| [b, v])
        .into_iter()
        .flatten()
        .collect();
    for reader in std::iter::once(&new).chain(old.iter()) {
        case.read(reader, &run);
        case.restore();
        let before = case.journal_bytes(&run);
        let ran = case.run(reader, &["resume", &run]);
        // The new binary accepts the manifest guard, so its refusal must
        // name the torn tail; an old binary may refuse on the manifest
        // guard before it reads the journal at all, so either refusal
        // counts there.
        let torn = ran
            .stderr
            .contains("resume failed: yaml error: EOF while parsing a string");
        let manifest_guard = ran.stderr.contains("cli+guard");
        let refused = if reader == &new {
            torn
        } else {
            torn || manifest_guard
        };
        assert!(
            ran.code == Some(2) && refused,
            "row E6: {} refuses the torn tail: {ran:?}",
            label(reader)
        );
        assert_eq!(
            case.journal_bytes(&run),
            before,
            "row E6: {} wrote nothing",
            label(reader)
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
/// checkpoint carries no guard, and it approves and resumes it with its
/// own routing. The manifest guard still stands, so beta.1 and 0.25.0
/// refuse to resume it (naming `cli+guard`) and still stop it.
#[test]
fn e7_synthetic_patch_drops_the_guard_old_binaries_still_refuse_the_manifest_guard() {
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
        case.resume_refused(&reader, &run, "cli+guard");
        case.stop(&reader, &run);
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

/// Row F1's playbook: `work` is an agent step whose script `success_check`
/// passes (in the bare form every binary parses), `edit` rewrites the file
/// it checked, and the gate holds the run.
const F1_FRESHNESS: &str = r#"schema: 2
id: corpus-fresh
name: Journal corpus evidence freshness
version: 1.0.0
defaults:
  profile: corpus
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "WORK", max_retries: 0, success_check: "scripts/check.sh" }
  - { id: edit, type: script, script: "scripts/edit.sh", runner: sh }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: edit }
  - { from: edit, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

/// Row F1: evidence freshness. The new binary binds `work`'s passing check
/// (`evidence_bound` before its `node_finished`) and waits at the gate.
/// Both old binaries read the run, skipping the record with the count
/// shown, and resume it with the new binary's routing; the new binary's
/// own resume journals no `evidence_observed` at the finish, since the bare
/// form declares no scope to fingerprint (the in-binary compatibility test
/// covers that event's skip). The new binary then
/// reads a run an old binary finished: the bound check, which `edit` ran
/// after, is not shown as a plain pass.
#[test]
fn f1_evidence_records_old_binaries_skip_them_and_route_the_same() {
    let new = new_binary();
    let mut case = Case::new("row F1");
    case.seed_inline(
        &new,
        "corpus-fresh",
        F1_FRESHNESS,
        &[
            ("check.sh", "grep -q A lib.txt\n"),
            ("edit.sh", "printf 'B\\n' > lib.txt\necho ok\n"),
        ],
    );
    let stub = case.dir.path().join("fresh-agent.sh");
    fs::write(&stub, "#!/bin/sh\nprintf 'A\\n' > lib.txt\necho ok\n").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    case.env.retain(|(k, _)| *k != "APB_AGENT_CMD");
    case.env.push(("APB_AGENT_CMD", stub.display().to_string()));
    let run = case.start_inline(&new, "corpus-fresh", &[]);
    let journal = case.wait_journal(&run, "the gate", |j| {
        j.iter().any(|e| e["type"] == "review_requested")
    });
    case.kill_driver(&run);
    case.save();
    let bound = journal
        .iter()
        .position(|e| e["type"] == "evidence_bound" && e["node"] == "work")
        .unwrap_or_else(|| panic!("row F1: {}", summary(&journal)));
    let finished_at = journal
        .iter()
        .position(|e| e["type"] == "node_finished" && e["node"] == "work")
        .expect("row F1: work finished");
    assert!(bound < finished_at, "row F1: {}", summary(&journal));

    let from = journal.len();
    let ran = case.read(&new, &run);
    assert!(!ran.stdout.contains("unknown event"), "row F1: {ran:?}");
    case.approve_and_resume(&new, &run);
    let after = case.journal(&run);
    assert!(
        !after[from..]
            .iter()
            .any(|e| e["type"] == "evidence_observed"),
        "row F1: an unscoped check is not observed at the finish: {}",
        summary(&after)
    );
    let expected = routing(&after, from);
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        let ran = case.read(&reader, &run);
        assert!(
            ran.stdout.contains("unknown event"),
            "row F1: {} skips the evidence record: {ran:?}",
            label(&reader)
        );
        case.approve_and_resume(&reader, &run);
        assert_eq!(
            routing(&case.journal(&run), from),
            expected,
            "row F1: {} routes as the new binary",
            label(&reader)
        );
        let ran = case.run(&new, &["runs", &run]);
        assert!(
            ran.stdout.contains("success check: work")
                && ran.stdout.contains("[not known to be current"),
            "row F1: the new binary reads the run {} finished: {ran:?}",
            label(&reader)
        );
        case.stop(&reader, &run);
    }
}

/// Row G1's playbook: a script list over five inline items with a pilot of
/// two. The script answers each item and changes nothing.
const G1_PILOT: &str = r#"schema: 2
id: corpus-pilot
name: Journal corpus list pilot
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: work
    type: script
    script: "scripts/item.sh"
    runner: sh
    list:
      items: { inline: [ { id: i1 }, { id: i2 }, { id: i3 }, { id: i4 }, { id: i5 } ] }
      batch: { size: 2, parallel: 1, retries: 0 }
      pilot: { items: 2 }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: ok }
"#;

const G1_ITEM_SH: &str = "echo 'item done'\n";

/// Row G1: a definition whose list block carries `pilot:` is one an older
/// apb cannot parse (`ListSpec` denies unknown fields), so each old binary
/// refuses to start such a playbook (no run directory is created) and
/// refuses to resume a run waiting at its pilot gate, appending nothing to
/// the journal. The new binary resumes it, decides `continue` naming the
/// request, and the visit runs to its end.
#[test]
fn g1_list_pilot_old_binaries_refuse_start_and_resume() {
    let new = new_binary();
    let mut case = Case::new("row G1 (new)");
    case.seed_inline(&new, "corpus-pilot", G1_PILOT, &[("item.sh", G1_ITEM_SH)]);
    let ran = case.run(&new, &["validate", "corpus-pilot"]);
    assert!(ran.ok(), "row G1: the new binary validates: {ran:?}");
    let run = case.start_inline(&new, "corpus-pilot", &[]);
    let journal = case.wait_journal(&run, "the pilot gate", |j| {
        j.iter()
            .any(|e| e["type"] == "review_requested" && e["node"] == "work")
    });
    assert_run_guard_follows_last_checkpoint(&journal, "row G1", &["list_pilot"]);
    case.kill_driver(&run);
    case.save();

    for (tag, name) in [(BETA1, "row G1 (beta.1)"), (V025, "row G1 (0.25.0)")] {
        let mut case = Case::new(name);
        let Some(old) = old_binary(tag, &case) else {
            continue;
        };
        // Before-running-alone G1: v1.0.0-beta.1 and later older binaries
        // reject `pilot` at parse (their list block denies unknown fields)
        // and create no run directory. v0.25.0's list block does not deny
        // unknown fields yet, so it starts the playbook, drops `pilot:`
        // and runs every item without a pilot gate: the standing contract
        // of an older binary meeting a new playbook field (GUARDRAILS.md
        // "Older apb binaries").
        case.seed_inline(&old, "corpus-pilot", G1_PILOT, &[("item.sh", G1_ITEM_SH)]);
        let ran = case.run(&old, &["run", "corpus-pilot"]);
        if tag == BETA1 {
            assert!(!ran.ok(), "{name}: beta.1 refuses to start: {ran:?}");
            let created = fs::read_dir(case.runs())
                .map(|d| d.flatten().count())
                .unwrap_or(0);
            assert_eq!(created, 0, "{name}: no run directory exists");
        } else {
            assert!(
                ran.ok(),
                "{name}: 0.25.0 parses and starts the playbook: {ran:?}"
            );
            let run_dir = case.runs();
            let journal: Vec<Value> = fs::read_dir(&run_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .find_map(|e| {
                    let p = e.path().join("events.jsonl");
                    fs::read_to_string(p).ok().map(|t| {
                        t.lines()
                            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                            .collect()
                    })
                })
                .unwrap_or_default();
            assert!(
                !journal
                    .iter()
                    .any(|e| e["type"] == "review_requested" || e["type"] == "list_pilot"),
                "{name}: 0.25.0 ran the visit without a pilot gate: {}",
                summary(&journal)
            );
        }

        // Resume: the writer's run waits at its pilot gate; either old
        // binary refuses it and the journal stays what it was.
        case.seed_inline(&new, "corpus-pilot", G1_PILOT, &[("item.sh", G1_ITEM_SH)]);
        let run = case.start_inline(&new, "corpus-pilot", &[]);
        case.wait_journal(&run, "the pilot gate", |j| {
            j.iter()
                .any(|e| e["type"] == "review_requested" && e["node"] == "work")
        });
        case.kill_driver(&run);
        case.save();
        case.restore();
        let run_dir = case.runs().join(&run);
        let before = fs::read(run_dir.join("events.jsonl")).unwrap();
        let ran = case.run(&old, &["resume", &run]);
        assert!(
            !ran.ok(),
            "{name}: the old binary refuses to resume: {ran:?}"
        );
        let after = fs::read(run_dir.join("events.jsonl")).unwrap();
        assert_eq!(before, after, "{name}: the old binary appended nothing");
    }

    // The new binary resumes on a fresh copy, decides continue naming the
    // request, and the visit runs to its end.
    let mut case = Case::new("row G1 (new resume)");
    case.seed_inline(&new, "corpus-pilot", G1_PILOT, &[("item.sh", G1_ITEM_SH)]);
    let run = case.start_inline(&new, "corpus-pilot", &[]);
    case.wait_journal(&run, "the pilot gate", |j| {
        j.iter()
            .any(|e| e["type"] == "review_requested" && e["node"] == "work")
    });
    let seq = case
        .journal(&run)
        .iter()
        .rev()
        .find(|e| e["type"] == "review_requested" && e["node"] == "work")
        .map(|e| e["seq"].clone())
        .expect("the request seq");
    case.kill_driver(&run);
    case.save();
    case.restore();
    let ran = case.run(
        &new,
        &[
            "review",
            &run,
            "work",
            "--decision",
            "continue",
            "--request",
            &format!("review:{seq}"),
        ],
    );
    assert!(ran.ok(), "row G1: the new binary decides: {ran:?}");
    let ran = case.run(&new, &["resume", &run]);
    assert!(ran.ok(), "row G1: the new binary resumes: {ran:?}");
    let journal = case.journal(&run);
    assert_eq!(
        last_of(&journal, "run_finished").map(|e| e["outcome"].clone()),
        Some(Value::from("succeeded")),
        "row G1: {}",
        summary(&journal)
    );
    let output = journal
        .iter()
        .rev()
        .find(|e| e["type"] == "node_finished" && e["node"] == "work")
        .and_then(|e| e["output"].as_str().map(str::to_string))
        .expect("the list node's output");
    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["passed"], Value::from(5), "row G1: {output}");
    assert_eq!(output["needs_look"], Value::from(0), "row G1: {output}");
}

// ------------------------------------------------- wait park: never parks

/// Row W1's playbook: a script node, then a webhook wait with
/// `timeout_seconds: 1` and no signal, routed on by its fallback to the
/// failure finish (the shape of `WF_TIMEOUT` in the engine's
/// `wait_test.rs`). A run of this shape never parks.
const W1_WEBHOOK_TIMEOUT: &str = r#"schema: 1
id: corpus-wait-webhook
name: Journal corpus webhook timeout
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/go.sh", runner: sh }
  - { id: wait, type: wait, wait_for: { type: webhook, key: ci }, timeout_seconds: 1 }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: wait }
  - { from: wait, to: ok, condition: { type: node_status, node: wait, equals: success } }
  - { from: wait, to: no, fallback: true }
"#;

/// Row W2's playbook: the same with a timer wait of 1 s (under
/// `release_after`), so it never parks either.
const W2_SHORT_TIMER: &str = r#"schema: 1
id: corpus-wait-timer
name: Journal corpus short timer wait
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/go.sh", runner: sh }
  - { id: wait, type: wait, wait_for: { type: timer, seconds: 1 }, timeout_seconds: 60 }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: wait }
  - { from: wait, to: ok }
"#;

const GO_SH: &str = "echo go\n";

/// The journal's sequence of event `type`s.
fn types_of(journal: &[Value]) -> Vec<&str> {
    journal.iter().filter_map(|e| e["type"].as_str()).collect()
}

/// Row W1: a run that never parks writes the journal main writes, and the
/// old binaries read the run. The literal below is the unchanged tree's
/// sequence, by construction.
#[test]
fn w1_a_run_without_a_timer_wait_journals_as_main() {
    let new = new_binary();
    let mut case = Case::new("row W1");
    case.seed_inline(
        &new,
        "corpus-wait-webhook",
        W1_WEBHOOK_TIMEOUT,
        &[("go.sh", GO_SH)],
    );
    let ran = case.run(&new, &["run", "corpus-wait-webhook"]);
    assert_eq!(ran.code, Some(1), "row W1: the run fails: {ran:?}");
    let run = only_run_id(&case);
    let journal = case.journal(&run);
    assert!(
        !journal.iter().any(|e| e["type"] == "run_guard"),
        "row W1: no run_guard: {}",
        summary(&journal)
    );
    assert!(
        !journal.iter().any(|e| e["type"] == "run_paused"),
        "row W1: no run_paused: {}",
        summary(&journal)
    );
    assert!(
        !journal.iter().any(|e| e["type"] == "run_resumed"),
        "row W1: no run_resumed: {}",
        summary(&journal)
    );
    assert!(
        control_file_is_empty(&case.run_dir(&run)),
        "row W1: no control entry"
    );
    assert_eq!(
        types_of(&journal),
        W1_TYPES.to_vec(),
        "row W1: actual sequence: {:?}",
        types_of(&journal)
    );
    case.save();

    for (tag, name) in [(BETA1, "row W1 (beta.1)"), (V025, "row W1 (0.25.0)")] {
        let Some(old) = old_binary(tag, &case) else {
            continue;
        };
        case.restore();
        let ran = case.run(&old, &["runs", &run]);
        assert!(
            ran.ok() && ran.stdout.contains("failed"),
            "{name}: {tag} reads the run: {ran:?}"
        );
    }
}

/// Whether the run's `control.jsonl` holds no entry: absent or empty. Any
/// other read failure fails the row (a file that does not read is no proof
/// that nothing was posted).
fn control_file_is_empty(run_dir: &Path) -> bool {
    match fs::read_to_string(run_dir.join("control.jsonl")) {
        Ok(text) => text.trim().is_empty(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => panic!("control.jsonl does not read: {e}"),
    }
}

/// Row W2: a short timer wait adds only its guard. The journal with every
/// `run_guard` line removed is the unchanged tree's sequence, by
/// construction, and the guard lines it carries name exactly `wait_park`
/// and stand right after `run_provenance` and every checkpoint.
#[test]
fn w2_a_short_timer_wait_adds_only_its_guard() {
    let new = new_binary();
    let mut case = Case::new("row W2");
    // The one-second timer never parks, whatever the caller's environment
    // says about the release threshold.
    case.env
        .push(("APB_RELEASE_AFTER_MS", NO_RELEASE.to_string()));
    case.seed_inline(
        &new,
        "corpus-wait-timer",
        W2_SHORT_TIMER,
        &[("go.sh", GO_SH)],
    );
    let ran = case.run(&new, &["run", "corpus-wait-timer"]);
    assert!(ran.ok(), "row W2: the run succeeds: {ran:?}");
    let run = only_run_id(&case);
    let journal = case.journal(&run);
    assert!(
        !journal.iter().any(|e| e["type"] == "run_paused"),
        "row W2: no run_paused: {}",
        summary(&journal)
    );
    assert!(
        !journal.iter().any(|e| e["type"] == "run_resumed"),
        "row W2: no run_resumed: {}",
        summary(&journal)
    );
    assert!(
        control_file_is_empty(&case.run_dir(&run)),
        "row W2: no control entry"
    );
    let without_guards: Vec<Value> = journal
        .iter()
        .filter(|e| e["type"] != "run_guard")
        .cloned()
        .collect();
    assert_eq!(
        types_of(&without_guards),
        W2_TYPES.to_vec(),
        "row W2: actual sequence: {:?}",
        types_of(&without_guards)
    );
    w2_guard_lines(&journal);
}

/// Row W1's literal journal, the unchanged tree's sequence.
const W1_TYPES: &[&str] = &[
    "run_started",
    "run_provenance",
    "node_started",
    "node_finished",
    "edge_traversed",
    "node_started",
    "node_finished",
    "edge_traversed",
    "node_started",
    "wait_started",
    "wait_timeout",
    "node_finished",
    "edge_traversed",
    "node_finished",
    "run_finished",
];
/// Row W2's literal journal without its guard lines, the unchanged tree's
/// sequence.
const W2_TYPES: &[&str] = &[
    "run_started",
    "run_provenance",
    "node_started",
    "node_finished",
    "edge_traversed",
    "node_started",
    "node_finished",
    "edge_traversed",
    "node_started",
    "wait_started",
    "wait_signalled",
    "node_finished",
    "edge_traversed",
    "node_finished",
    "run_finished",
];

/// The guard assertions of row W2, added with the wait park change: every
/// `run_guard` names exactly `wait_park` and stands right after
/// `run_provenance` or right after a checkpoint, and nowhere else.
fn w2_guard_lines(journal: &[Value]) {
    let is_checkpoint = |t: &str| {
        matches!(
            t,
            "node_finished" | "run_paused" | "run_finished" | "run_aborted"
        )
    };
    let guards: Vec<usize> = journal
        .iter()
        .enumerate()
        .filter_map(|(i, e)| (e["type"] == "run_guard").then_some(i))
        .collect();
    let expected: Vec<usize> = journal
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let t = e["type"].as_str()?;
            (t == "run_provenance" || is_checkpoint(t)).then_some(i + 1)
        })
        .collect();
    assert_eq!(
        guards,
        expected,
        "row W2: guard lines right after run_provenance and every checkpoint: {}",
        summary(journal)
    );
    for e in journal.iter().filter(|e| e["type"] == "run_guard") {
        assert_eq!(
            e["requires"],
            Value::from(["wait_park"]),
            "row W2: a guard names exactly wait_park: {e}"
        );
    }
}

// ------------------------------------------------- inbox waits

/// The fixture connector of the inbox rows: the engine test suite's
/// ingest-only fake (`crates/apb-engine/tests/fixtures/connectors/
/// echo-hooks`), installed into the case's config dir like the sink.
const ECHO_HOOKS: &str = "echo-hooks";

/// The one account of the inbox rows.
const ECHO_ACCOUNT: &str = "main";

/// The values of the account's two secret fields. The case env carries
/// them for the gate's secret check, and `<root>/.apb/secrets.env` records
/// them for the storage redactor, as the engine test suite does.
const ECHO_LONG: &str = "abcd1234";
const ECHO_SHORT: &str = "ab12";

/// The inbox rows' playbook: a script node, then an inbox wait on the
/// fixture connector's one account, routed on its timeout to the failure
/// finish (the shape of `plain_yaml` in the engine's `inbox_wait_test.rs`).
/// `extra` carries further `wait_for` keys: the parked row gives the wait
/// a `types` list the stored delivery passes (so the park's waker, which
/// reads only the type, wakes the run) and a `match` condition its body
/// fails (so the resumed visit examines it, does not match, and parks over
/// it). A delivery of another type would not wake the run at all.
fn inbox_yaml(timeout: u64, extra: &str) -> String {
    format!(
        r#"schema: 1
id: corpus-wait-inbox
name: Journal corpus inbox wait
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - {{ id: work, type: script, script: "scripts/go.sh", runner: sh }}
  - id: wait
    type: wait
    wait_for:
      type: inbox
      connector: echo-hooks
      account: main{extra}
    timeout_seconds: {timeout}
  - {{ id: ok, type: finish, outcome: success }}
  - {{ id: no, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: work }}
  - {{ from: work, to: wait }}
  - {{ from: wait, to: ok, condition: {{ type: node_status, node: wait, equals: success }} }}
  - {{ from: wait, to: no, fallback: true }}
"#
    )
}

impl Case {
    /// The `echo-hooks` fixture connector, installed in the case's config
    /// dir with its one account configured and both digests approved by
    /// `binary` (the sink's shape).
    fn install_echo_hooks(&self, binary: &Path) {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../apb-engine/tests/fixtures/connectors/echo-hooks/connector.yaml");
        let dst = self
            .dir
            .path()
            .join("home/.config/apb/connectors")
            .join(ECHO_HOOKS);
        fs::create_dir_all(&dst).unwrap();
        fs::copy(&fixture, dst.join("connector.yaml")).unwrap();
        let accounts = self.root().join(".apb/connector-config");
        fs::create_dir_all(&accounts).unwrap();
        fs::write(
            accounts.join(format!("{ECHO_HOOKS}.yaml")),
            "accounts:\n  - name: main\n    default: true\n    verify_token: \"{{env.APB_IW_LONG}}\"\n    app_secret: \"{{env.APB_IW_SHORT}}\"\n",
        )
        .unwrap();
        fs::write(
            self.root().join(".apb/secrets.env"),
            format!("APB_IW_LONG={ECHO_LONG}\nAPB_IW_SHORT={ECHO_SHORT}\n"),
        )
        .unwrap();
        for args in [
            vec!["connector", "approve", ECHO_HOOKS],
            vec![
                "connector",
                "approve",
                ECHO_HOOKS,
                "--account",
                ECHO_ACCOUNT,
            ],
        ] {
            let ran = self.run(binary, &args);
            assert!(ran.ok(), "{}: {}: {ran:?}", self.name, args.join(" "));
        }
    }

    /// `apb stop <run>` by `reader` on the saved state, over a journal cut
    /// before the checkpoint that would settle its unknown lines: the
    /// stop's first strict read refuses (its warning names the type and
    /// the newer binary), and the stop still goes through, appending
    /// `run_aborted` and nothing else.
    fn stop_after_refused_strict_read(&self, reader: &Path, run: &str, kind: &str) {
        self.restore();
        let before = self.journal_bytes(run);
        let ran = self.run(reader, &["stop", run]);
        assert!(ran.ok(), "{}: {} stop: {ran:?}", self.name, label(reader));
        assert!(
            ran.stderr.contains("written by a newer apb") && ran.stderr.contains(kind),
            "{}: {} refuses the strict read, naming `{kind}`: {ran:?}",
            self.name,
            label(reader)
        );
        assert!(
            ran.stderr.contains("stopping the run anyway"),
            "{}: {} still stops the run: {ran:?}",
            self.name,
            label(reader)
        );
        let journal = self.journal(run);
        assert_eq!(
            journal.last().map(|e| e["type"].clone()),
            Some(Value::from("run_aborted")),
            "{}: {} stop appends run_aborted last: {}",
            self.name,
            label(reader),
            summary(&journal)
        );
        let after = self.journal_bytes(run);
        assert!(
            after.starts_with(&before) && after.len() > before.len(),
            "{}: {} stop only appended: {} records over {} bytes",
            self.name,
            label(reader),
            journal.len(),
            before.len()
        );
        // Exactly one record was appended, and it is `run_aborted`.
        let appended: Vec<Value> = String::from_utf8_lossy(&after[before.len()..])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("an appended record"))
            .collect();
        assert!(
            appended.len() == 1 && appended[0]["type"] == "run_aborted",
            "{}: {} stop appends run_aborted and nothing else: {appended:?}",
            self.name,
            label(reader)
        );
    }
}

/// The case's `echo-hooks` inbox, under the case's config dir: deliveries
/// are stored exactly where the binary under test reads them.
fn echo_inbox(case: &Case) -> apb_core::connector::inbox::Inbox {
    let cfg = case.dir.path().join("home/.config/apb");
    apb_core::connector::inbox::Inbox::at(&cfg.join("connector-inbox"), ECHO_HOOKS, ECHO_ACCOUNT)
        .unwrap()
}

/// Stores one delivery in the case's inbox, the way the webhook listener
/// would.
fn store_delivery(case: &Case, event_type: Option<&str>, body: serde_json::Value) {
    match echo_inbox(case).append_typed(
        &format!("m-{}", apb_core::connector::marker::new_nonce()),
        event_type,
        &body,
    ) {
        Ok(apb_core::connector::inbox::Appended::Stored(_)) => {}
        other => panic!("{}: the delivery was not stored: {other:?}", case.name),
    }
}

/// The guard rules the inbox rows share with row W2: every `run_guard`
/// names exactly `wait_park` and stands right after `run_provenance` or
/// right after a checkpoint, and nowhere else.
fn wait_park_guard_lines(journal: &[Value], row: &str) {
    let guards: Vec<usize> = journal
        .iter()
        .enumerate()
        .filter_map(|(i, e)| (e["type"] == "run_guard").then_some(i))
        .collect();
    let expected: Vec<usize> = journal
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let t = e["type"].as_str()?;
            (t == "run_provenance" || is_checkpoint(e)).then_some(i + 1)
        })
        .collect();
    assert_eq!(
        guards,
        expected,
        "{row}: guard lines right after run_provenance and every checkpoint: {}",
        summary(journal)
    );
    for e in journal.iter().filter(|e| e["type"] == "run_guard") {
        assert_eq!(
            e["requires"],
            Value::from(["wait_park"]),
            "{row}: a guard names exactly wait_park: {e}"
        );
    }
}

/// Row I1's literal journal without its guard lines: the timer wait's
/// sequence with the wait's start line before `wait_started` and its end
/// line before `wait_timeout`, by construction.
const I1_TYPES: &[&str] = &[
    "run_started",
    "run_provenance",
    "node_started",
    "node_finished",
    "edge_traversed",
    "node_started",
    "node_finished",
    "edge_traversed",
    "node_started",
    "inbox_wait_read",
    "wait_started",
    "inbox_wait_read",
    "wait_timeout",
    "node_finished",
    "edge_traversed",
    "node_finished",
    "run_finished",
];

/// Row I1: an inbox wait over an empty inbox times out after 1 s and never
/// parks. Its journal, with every `run_guard` line removed, is the timer
/// wait's sequence with the wait's start line before `wait_started`
/// (`kind` `inbox`) and its end line before `wait_timeout`. Both old
/// binaries read the finished run, counting the lines they skip, and
/// refuse to resume it with a definition parse error, writing nothing.
#[test]
fn i1_inbox_timeout_journals_as_main_old_read_and_refuse_resume() {
    let new = new_binary();
    let mut case = Case::new("row I1");
    // The one-second wait never parks, whatever the caller's environment
    // says about the release threshold.
    case.env
        .push(("APB_RELEASE_AFTER_MS", NO_RELEASE.to_string()));
    case.seed_inline(
        &new,
        "corpus-wait-inbox",
        &inbox_yaml(1, ""),
        &[("go.sh", GO_SH)],
    );
    case.install_echo_hooks(&new);
    let ran = case.run(&new, &["run", "corpus-wait-inbox"]);
    assert_eq!(ran.code, Some(1), "row I1: the run fails: {ran:?}");
    let run = only_run_id(&case);
    let journal = case.journal(&run);
    assert!(
        !journal.iter().any(|e| e["type"] == "run_paused"),
        "row I1: no run_paused: {}",
        summary(&journal)
    );
    assert!(
        !journal.iter().any(|e| e["type"] == "run_resumed"),
        "row I1: no run_resumed: {}",
        summary(&journal)
    );
    assert!(
        control_file_is_empty(&case.run_dir(&run)),
        "row I1: no control entry"
    );
    let without_guards: Vec<Value> = journal
        .iter()
        .filter(|e| e["type"] != "run_guard")
        .cloned()
        .collect();
    assert_eq!(
        types_of(&without_guards),
        I1_TYPES.to_vec(),
        "row I1: actual sequence: {:?}",
        types_of(&without_guards)
    );
    let started = journal
        .iter()
        .find(|e| e["type"] == "wait_started")
        .expect("row I1: the wait started");
    assert_eq!(
        started["kind"],
        Value::from("inbox"),
        "row I1: the wait is an inbox wait: {started}"
    );
    let at = journal
        .iter()
        .position(|e| e["type"] == "inbox_wait_read")
        .expect("row I1: the wait's start line");
    assert_eq!(
        journal[at + 1]["type"],
        Value::from("wait_started"),
        "row I1: the start line stands right before wait_started: {}",
        summary(&journal)
    );
    assert!(
        journal[at].get("end").is_none(),
        "row I1: the start line carries no end: {}",
        journal[at]
    );
    let end_at = journal
        .iter()
        .rposition(|e| e["type"] == "inbox_wait_read")
        .expect("row I1: the wait's end line");
    assert_eq!(
        journal[end_at]["end"],
        Value::from("timeout"),
        "row I1: the visit ended at its timeout: {}",
        journal[end_at]
    );
    assert_eq!(
        journal[end_at + 1]["type"],
        Value::from("wait_timeout"),
        "row I1: the end line stands right before wait_timeout: {}",
        summary(&journal)
    );
    wait_park_guard_lines(&journal, "row I1");
    case.save();

    for (tag, name) in [(BETA1, "row I1 (beta.1)"), (V025, "row I1 (0.25.0)")] {
        let Some(old) = old_binary(tag, &case) else {
            continue;
        };
        case.restore();
        let shown = case.read(&old, &run);
        assert!(
            shown.stdout.contains("unknown event"),
            "{name}: {tag} counts the lines it skips: {shown:?}"
        );
        assert!(
            shown.stdout.contains("failed"),
            "{name}: {tag} reads the failed run: {shown:?}"
        );
        // The count holds both `inbox_wait_read` lines: the same reader over
        // the journal without them counts two fewer (whatever else this
        // release does not know is counted the same in both).
        let saved_journal = case.saved().join(&run).join("events.jsonl");
        let original = fs::read(&saved_journal).unwrap();
        rewrite_journal(&case.saved().join(&run), |records| {
            records
                .into_iter()
                .filter(|e| e["type"] != "inbox_wait_read")
                .collect()
        });
        let without = case.read(&old, &run);
        fs::write(&saved_journal, original).unwrap();
        assert_eq!(
            unknown_count(&shown.stdout),
            unknown_count(&without.stdout) + 2,
            "{name}: {tag} counts each inbox_wait_read line: {shown:?} vs {without:?}"
        );
        case.restore();
        let before = case.journal_bytes(&run);
        let resume = case.run(&old, &["resume", &run]);
        assert!(!resume.ok(), "{name}: {tag} refuses to resume: {resume:?}");
        assert!(
            resume.stderr.contains("unknown variant `inbox`"),
            "{name}: {tag} refuses with the definition parse error: {resume:?}"
        );
        assert_eq!(
            case.journal_bytes(&run),
            before,
            "{name}: {tag} wrote nothing"
        );
    }
}

/// Row I2: a long inbox wait parks over its empty inbox, a stored delivery
/// its `types` pass and its `match` refuses wakes it (the park's waker),
/// and the resumed visit examines it and parks again, leaving the progress
/// line before `run_paused`. The journal cut after the progress line,
/// before `run_paused`, holds unknown lines no checkpoint settles: each
/// old binary refuses that strict read and still stops the run, appending
/// `run_aborted`.
#[test]
fn i2_cut_after_the_progress_line_old_refuse_read_and_stop() {
    let new = new_binary();
    let mut case = Case::new("row I2");
    // The wait parks at once, and the waker wakes within one poll: the
    // first park holds nothing examined, the second leaves the progress
    // line over the examined delivery.
    case.env.push(("APB_RELEASE_AFTER_MS", "1000".to_string()));
    case.env.push(("APB_WAKER_POLL_MS", "100".to_string()));
    case.seed_inline(
        &new,
        "corpus-wait-inbox",
        &inbox_yaml(
            3600,
            "\n      types: [review]\n      match:\n        - { path: kind, equals: signal }",
        ),
        &[("go.sh", GO_SH)],
    );
    case.install_echo_hooks(&new);
    let run = case.start_inline(&new, "corpus-wait-inbox", &[]);
    let first = case.wait_journal(&run, "the first park", |j| {
        j.iter().any(|e| e["type"] == "run_paused")
    });
    assert!(
        first
            .iter()
            .all(|e| e["type"] != "inbox_wait_read" || e["examined"].as_u64() == Some(0)),
        "row I2: the first park holds the start line only: {}",
        summary(&first)
    );
    // The delivery passes the wait's `types` and fails its `match`, so the
    // waker wakes the run and the resumed visit examines it without
    // matching: the second park writes the progress line.
    store_delivery(
        &case,
        Some("review"),
        serde_json::json!({ "kind": "noise" }),
    );
    let journal = case.wait_journal(&run, "the second park", |j| {
        j.iter().filter(|e| e["type"] == "run_paused").count() >= 2
    });
    let paused = journal
        .iter()
        .rposition(|e| e["type"] == "run_paused")
        .expect("row I2: the second park");
    assert_eq!(
        journal[paused - 1]["type"],
        Value::from("inbox_wait_read"),
        "row I2: the progress line stands right before run_paused: {}",
        summary(&journal)
    );
    assert!(
        journal[paused - 1].get("end").is_none(),
        "row I2: the progress line carries no end: {}",
        journal[paused - 1]
    );
    assert!(
        journal[paused - 1]["examined"]
            .as_u64()
            .is_some_and(|n| n >= 1),
        "row I2: the progress line covers the examined delivery: {}",
        journal[paused - 1]
    );
    wait_driver_gone(&case, &run);
    case.forget_driver(&run);
    case.kill_wakers(&run);
    // Re-read once the driver is gone: the snapshot above may have caught
    // the second `run_paused` before the guard line written with it.
    let journal = case.journal(&run);
    let paused = journal
        .iter()
        .rposition(|e| e["type"] == "run_paused")
        .expect("row I2: the second park");
    wait_park_guard_lines(&journal, "row I2");
    // The cut: everything before the second `run_paused`, the progress
    // line last.
    rewrite_journal(&case.run_dir(&run), |records| records[..paused].to_vec());
    let cut = case.journal(&run);
    assert_eq!(
        cut.last().map(|e| e["type"].clone()),
        Some(Value::from("inbox_wait_read")),
        "row I2: the cut journal ends at the progress line: {}",
        summary(&cut)
    );
    case.save();

    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        case.stop_after_refused_strict_read(&reader, &run, "inbox_wait_read");
    }
}

/// Row I3: the same 1 s run as row I1, its journal cut after the end pair
/// (`inbox_wait_read` with its end and `wait_timeout`), before
/// `node_finished`: again each old binary refuses the strict read and
/// still stops the run.
#[test]
fn i3_cut_after_the_end_pair_old_refuse_read_and_stop() {
    let new = new_binary();
    let mut case = Case::new("row I3");
    case.env
        .push(("APB_RELEASE_AFTER_MS", NO_RELEASE.to_string()));
    case.seed_inline(
        &new,
        "corpus-wait-inbox",
        &inbox_yaml(1, ""),
        &[("go.sh", GO_SH)],
    );
    case.install_echo_hooks(&new);
    let ran = case.run(&new, &["run", "corpus-wait-inbox"]);
    assert_eq!(ran.code, Some(1), "row I3: the run fails: {ran:?}");
    let run = only_run_id(&case);
    let journal = case.journal(&run);
    let end_at = journal
        .iter()
        .rposition(|e| e["type"] == "inbox_wait_read")
        .expect("row I3: the wait's end line");
    assert_eq!(
        journal[end_at]["end"],
        Value::from("timeout"),
        "row I3: the visit ended at its timeout: {}",
        journal[end_at]
    );
    assert_eq!(
        journal[end_at + 1]["type"],
        Value::from("wait_timeout"),
        "row I3: the end event follows the end line: {}",
        summary(&journal)
    );
    // The cut: everything through the end pair, `node_finished` dropped.
    rewrite_journal(&case.run_dir(&run), |records| {
        records[..end_at + 2].to_vec()
    });
    let cut = case.journal(&run);
    assert_eq!(
        cut.last().map(|e| e["type"].clone()),
        Some(Value::from("wait_timeout")),
        "row I3: the cut journal ends at the end pair: {}",
        summary(&cut)
    );
    case.save();

    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        case.stop_after_refused_strict_read(&reader, &run, "inbox_wait_read");
    }
}

/// Row V1's playbook (eval doubles): one agent step bound to the
/// `mock-tracker` fixture connector.
const V1_DOUBLED: &str = "schema: 2\nid: corpus-doubled\nname: Journal corpus doubled eval\nversion: 1.0.0\ndefaults: { profile: corpus }\neffects: [network, external]\nnodes:\n  - { id: start, type: start }\n  - { id: look, type: agent_task, prompt: LOOK, connectors: [{ name: mock-tracker, accounts: [main], functions: [list_items] }] }\n  - { id: ok, type: finish, outcome: success }\nedges:\n  - { from: start, to: look }\n  - { from: look, to: ok }\n";

const V1_CASE: &str = "schema: 1\nid: doubled\ninstruction: look\nfixture: { dir: fixtures/base }\nconnectors:\n  mock-tracker:\n    list_items:\n      - answer: { status: 200, body: { items: [] } }\nrepeat: 1\n";

/// Row V1 (eval doubles; named V1 because row E1 is the result snapshots'
/// row): the new binary runs a doubled eval case, whose stored run
/// directory has a manifest with `doubled_accounts` and no account record,
/// `run.yaml` doubles, and `run_provenance.eval_doubles` with the
/// `eval_doubles` guard. Each old binary then runs `apb connector call` in
/// that run against an account whose base URL is a live service and whose
/// token comes from a `{{cmd:...}}` helper: the call fails, the service
/// received nothing, and the helper never ran.
#[test]
fn rowv1_doubled_run_old_connector_call_sends_nothing() {
    let new = new_binary();
    let mut case = Case::new("row V1");
    let ran = case.run(&new, &["init"]);
    assert!(ran.ok(), "row V1: apb init: {ran:?}");
    seed_corpus_profile(&case.root());
    let service = crate::http_service::Service::spawn(Default::default());
    let marker = case.dir.path().join("helper-ran");
    let cfg = case.dir.path().join("home/.config/apb");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../apb-engine/tests/fixtures/connectors/mock-tracker/connector.yaml");
    fs::create_dir_all(cfg.join("connectors/mock-tracker")).unwrap();
    fs::copy(&fixture, cfg.join("connectors/mock-tracker/connector.yaml")).unwrap();
    fs::create_dir_all(cfg.join("connector-config")).unwrap();
    fs::write(
        cfg.join("connector-config/mock-tracker.yaml"),
        format!(
            "accounts:\n  - name: main\n    default: true\n    base_url: {}\n    token: \"{{{{cmd:touch {}}}}}\"\n",
            service.base_url,
            marker.display()
        ),
    )
    .unwrap();
    for args in [
        vec!["connector", "approve", "mock-tracker"],
        vec!["connector", "approve", "mock-tracker", "--account", "main"],
    ] {
        let ran = case.run(&new, &args);
        assert!(ran.ok(), "row V1: {}: {ran:?}", args.join(" "));
    }
    let pdir = case.root().join(".apb/playbooks/corpus-doubled");
    fs::create_dir_all(pdir.join("1.0.0")).unwrap();
    fs::write(pdir.join("1.0.0/playbook.yaml"), V1_DOUBLED).unwrap();
    fs::write(pdir.join("current"), "1.0.0").unwrap();
    fs::create_dir_all(pdir.join("evals/fixtures/base")).unwrap();
    fs::write(pdir.join("evals/doubled.yaml"), V1_CASE).unwrap();
    fs::write(pdir.join("evals/fixtures/base/a.txt"), "a\n").unwrap();
    let stub = case.dir.path().join("doubled-agent.sh");
    fs::write(
        &stub,
        "#!/bin/sh\nprintf 'done\\n```yaml\\nstatus: success\\nsummary: done\\n```\\n'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    case.env.push(("APB_AGENT_CMD", stub.display().to_string()));
    let ran = case.run(&new, &["eval", "corpus-doubled", "--yes", "--json"]);
    assert!(ran.ok(), "row V1: apb eval: {ran:?}");
    let v: Value = serde_json::from_str(&ran.stdout).unwrap();
    let rep = &v["result"]["cases"][0]["repetitions"][0];
    let run_dir = PathBuf::from(rep["run_dir"].as_str().expect("row V1: a stored run"));
    let manifest: Value =
        serde_json::to_value(apb_engine::manifest::read(&run_dir).unwrap().unwrap()).unwrap();
    let conn = &manifest["connectors"][0];
    assert_eq!(conn["accounts"], serde_json::json!([]), "row V1: {conn}");
    assert_eq!(
        conn["doubled_accounts"][0]["name"], "main",
        "row V1: {conn}"
    );
    let settings = fs::read_to_string(run_dir.join("run.yaml")).unwrap();
    assert!(settings.contains("doubles:"), "row V1: {settings}");
    let journal = journal_at(&run_dir);
    assert!(
        journal
            .iter()
            .any(|e| e["type"] == "run_provenance" && e["eval_doubles"] == true),
        "row V1: the provenance says doubled"
    );
    assert!(
        journal.iter().any(|e| e["type"] == "run_guard"
            && e["requires"]
                .as_array()
                .is_some_and(|r| r.iter().any(|f| f == "eval_doubles"))),
        "row V1: the guard names eval_doubles: {journal:?}"
    );
    assert!(!marker.exists(), "row V1: the new binary ran no helper");
    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        let mut cmd = case.command(
            &reader,
            &[
                "connector",
                "call",
                "mock-tracker",
                "list_items",
                "--args",
                "{\"q\":\"x\"}",
            ],
        );
        let out = cmd
            .env("APB_RUN_DIR", &run_dir)
            .env("APB_NODE_ID", "look")
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "row V1: {} the call fails: {}{}",
            label(&reader),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            said.contains("is not in the connector snapshot"),
            "row V1: {} fails on the account: {said}",
            label(&reader)
        );
        assert!(
            service.bodies("/items").is_empty(),
            "row V1: {} sent nothing",
            label(&reader)
        );
        assert!(!marker.exists(), "row V1: {} ran no helper", label(&reader));
    }
}

/// Row M1's connector: design D1's example with the stub as its program.
fn m1_connector(command: &str) -> String {
    format!(
        r#"name: tracker-mcp
version: 0.1.0
mcp:
  transport: stdio
  command: {command}
  args: ["--workspace", "{{{{account.workspace}}}}"]
  env:
    TRACKER_TOKEN: "{{{{secret.token}}}}"
account_fields:
  - {{ name: workspace, required: true }}
  - {{ name: token, required: true, secret: true }}
identity: {{ function: whoami, path: id }}
functions:
  - name: whoami
    description: The account the server acts as
    read_only: true
    mcp: {{ tool: whoami }}
  - name: list_comments
    description: Recent comments of one issue
    read_only: true
    mcp: {{ tool: list_comments }}
  - name: create_comment
    description: Add a comment to an issue
    mcp: {{ tool: create_comment }}
    args_schema:
      type: object
      properties: {{ issue: {{ type: string }}, body: {{ type: string }} }}
      required: [issue, body]
    idempotency:
      mode: marker
      arg: body
      format: html_comment
      reconcile:
        function: list_comments
        items: comments
        text: body
        author: author.id
        item_ref: url
"#
    )
}

/// Row M1's playbook: a review gate before a node granted one MCP function.
const M1_MCP: &str = "schema: 2\nid: corpus-mcp\nname: Journal corpus MCP\nversion: 1.0.0\neffects: [network, external]\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approved, rejected] }\n  - { id: look, type: agent_task, prompt: LOOK, profile: corpus, connectors: [{ name: tracker-mcp, functions: [list_comments] }] }\n  - { id: ok, type: finish, outcome: success }\n  - { id: no, type: finish, outcome: failure }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: look, condition: { type: review_status, equals: approved } }\n  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }\n  - { from: look, to: ok }\n";

#[test]
fn rowm1_mcp_call_old_binaries_fail_closed() {
    apb_core::process_mode::testing::in_plain_child(
        "journal_corpus_test::rowm1_mcp_call_old_binaries_fail_closed",
        rowm1_body,
    );
}

fn rowm1_body() {
    use apb_core::connector::mcp::digest::{pin_digest, tool_digest};
    use apb_core::connector::mcp::pins::{PinStore, ServerPin, ToolPin};
    use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
    use apb_mcp_client::testing::{StubEra, StubServer};

    let new = new_binary();
    let mut case = Case::new("row M1");
    case.seed_inline(&new, "corpus-mcp", M1_MCP, &[]);
    let cfg = case.dir.path().join("home/.config/apb");
    let stub = StubServer::write(&case.dir.path().join("stub"), StubEra::Modern).unwrap();
    let tools = serde_json::json!([
        { "name": "whoami", "inputSchema": { "type": "object" } },
        { "name": "list_comments", "inputSchema": { "type": "object" } },
        { "name": "create_comment", "inputSchema": { "type": "object" } }
    ]);
    stub.set_tools(&tools).unwrap();
    let program = fs::canonicalize(stub.program()).unwrap();
    fs::create_dir_all(cfg.join("connectors/tracker-mcp")).unwrap();
    fs::write(
        cfg.join("connectors/tracker-mcp/connector.yaml"),
        m1_connector(&program.to_string_lossy()),
    )
    .unwrap();
    fs::create_dir_all(cfg.join("connector-config")).unwrap();
    fs::write(
        cfg.join("connector-config/tracker-mcp.yaml"),
        "accounts:\n  - name: work\n    default: true\n    workspace: acme\n    token: \"{{env.CORPUS_MCP_TOKEN}}\"\n",
    )
    .unwrap();
    case.env
        .push(("CORPUS_MCP_TOKEN", "corpus-mcp-token-6612".to_string()));
    // The approvals and the pins, written in this child process's own
    // config dir, which is the case's.
    // SAFETY: this child process runs this one test.
    unsafe { std::env::set_var("APB_CONFIG_DIR", &cfg) };
    let tree = apb_core::connector::store::load("tracker-mcp")
        .unwrap()
        .digest;
    let accounts = apb_core::connector::config::load_merged(&case.root(), "tracker-mcp").unwrap();
    let account = apb_core::connector::config::account_digest(&accounts[0]);
    let mut trust = TrustStore::load();
    trust
        .approve_kind(
            &tree,
            "tracker-mcp",
            Kind::Connector,
            OriginKind::LocallyApproved,
        )
        .unwrap();
    trust
        .approve_kind(
            &account,
            &account_trust_id("tracker-mcp", "work"),
            Kind::ConnectorAccount,
            OriginKind::LocallyApproved,
        )
        .unwrap();
    let program_s = program.to_string_lossy().to_string();
    let program_digest = apb_core::content::sha256_hex(&fs::read(&program).unwrap());
    let pins: std::collections::BTreeMap<String, ToolPin> = tools
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_string(),
                ToolPin {
                    digest: tool_digest(t),
                    description: String::new(),
                    members: Default::default(),
                },
            )
        })
        .collect();
    let digests = pins
        .iter()
        .map(|(k, v)| (k.clone(), v.digest.clone()))
        .collect();
    PinStore::record(
        "tracker-mcp",
        "work",
        ServerPin {
            pin_digest: pin_digest(&tree, &account, &program_s, &program_digest, &digests),
            connector_digest: tree,
            account_digest: account,
            command: program_s.clone(),
            program: program_s,
            program_digest,
            tools: pins,
            approved_at_ms: 1,
        },
    )
    .unwrap();
    // The node's agent calls the function through `CORPUS_APB` and keeps
    // what it answered.
    let d = case.dir.path().display().to_string();
    let agent = case.dir.path().join("mcp-agent.sh");
    fs::write(
        &agent,
        format!(
            "#!/bin/sh\n\"$CORPUS_APB\" connector call tracker-mcp list_comments --args '{{}}' > '{d}/call.out' 2>&1\necho $? > '{d}/call.code'\necho ok\n"
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    case.env.retain(|(k, _)| *k != "APB_AGENT_CMD");
    case.env
        .push(("APB_AGENT_CMD", agent.display().to_string()));

    let run = case.start_inline(&new, "corpus-mcp", &[]);
    case.wait_journal(&run, "the review gate", |j| {
        j.iter().any(|e| e["type"] == "review_requested")
    });
    case.kill_driver(&run);
    case.save();
    assert!(
        case.run_dir(&run)
            .join("connectors/tracker-mcp.yaml")
            .exists(),
        "row M1: the run snapshotted the MCP connector"
    );
    assert!(
        stub.starts().is_empty(),
        "row M1: the writer started no server"
    );

    let Some((beta1, v025)) = old_readers(&case) else {
        return;
    };
    for reader in [beta1, v025] {
        case.restore();
        let ran = case.run(&reader, &["runs", &run]);
        assert!(ran.ok(), "row M1: {} runs: {ran:?}", label(&reader));

        case.env.retain(|(k, _)| *k != "CORPUS_APB");
        case.env.push(("CORPUS_APB", reader.display().to_string()));
        let _ = fs::remove_file(case.dir.path().join("call.code"));
        case.approve_and_resume(&reader, &run);
        let code = fs::read_to_string(case.dir.path().join("call.code"))
            .unwrap_or_else(|_| panic!("row M1: {} the node ran its call", label(&reader)));
        let said = fs::read_to_string(case.dir.path().join("call.out")).unwrap_or_default();
        assert_ne!(
            code.trim(),
            "0",
            "row M1: {} the call fails: {said}",
            label(&reader)
        );
        assert!(
            said.contains("snapshot is invalid"),
            "row M1: {} cannot load the snapshot: {said}",
            label(&reader)
        );
        assert!(
            stub.starts().is_empty(),
            "row M1: {} started no server",
            label(&reader)
        );
        assert!(
            !case
                .journal(&run)
                .iter()
                .any(|e| e["type"] == "send_intent_recorded"),
            "row M1: {} journaled no send intent",
            label(&reader)
        );

        case.restore();
        let ran = case.run(&reader, &["run", "corpus-mcp", "--detach"]);
        assert!(
            !ran.ok(),
            "row M1: {} refuses a new start: {ran:?}",
            label(&reader)
        );
        assert!(
            format!("{}{}", ran.stdout, ran.stderr).contains("connector_unresolved"),
            "row M1: {} refuses with connector_unresolved: {ran:?}",
            label(&reader)
        );
        assert!(
            stub.starts().is_empty(),
            "row M1: {} started no server",
            label(&reader)
        );
    }
}

// ---------------------------------------------- list skip and session fork

/// Row L1's playbook: a script list whose items a later run skips, then a
/// gate the run waits at, so a checkpoint and its run guard follow the
/// skip sets.
const L1_SKIP: &str = r#"schema: 2
id: corpus-skip
name: Journal corpus list skip
version: 1.0.0
nodes:
  - { id: start, type: start }
  - id: work
    type: script
    script: "scripts/item.sh"
    runner: sh
    list:
      items: { inline: [ { id: i1 }, { id: i2 } ] }
      batch: { size: 2, parallel: 1, retries: 0 }
      skip: { passed_in: previous_success }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

/// The item script of row L1: it answers each item and changes nothing.
const L1_ITEM_SH: &str = "echo 'item done'\n";

/// The new binary's run of row L1 waiting at its gate after the list.
fn l1_waiting_at_gate(case: &mut Case, new: &Path) -> String {
    case.seed_inline(new, "corpus-skip", L1_SKIP, &[("item.sh", L1_ITEM_SH)]);
    let run = case.start_inline(new, "corpus-skip", &[]);
    let journal = case.wait_journal(&run, "the gate", |j| {
        j.iter()
            .any(|e| e["type"] == "review_requested" && e["node"] == "gate")
    });
    assert!(
        journal.iter().any(|e| e["type"] == "list_skip_read"),
        "{}: the run journals its skip set: {}",
        case.name,
        summary(&journal)
    );
    assert_run_guard_follows_last_checkpoint(&journal, case.name, &["list_skip"]);
    case.kill_driver(&run);
    run
}

/// Row L1: a definition with `list.skip` is one beta.1 cannot parse
/// (its list block denies unknown fields): it refuses to start it and
/// creates no run directory. 0.25.0 accepts unknown fields: it drops the
/// skip (it predates list mode, so the whole list block) and runs the step,
/// skipping nothing. Neither resumes a run of the new binary,
/// whose guard names `list_skip`, and neither appends to its journal.
#[test]
fn l1_list_skip_old_binaries_refuse_start_and_resume() {
    let new = new_binary();
    let mut case = Case::new("row L1 (new)");
    case.seed_inline(&new, "corpus-skip", L1_SKIP, &[("item.sh", L1_ITEM_SH)]);
    let ran = case.run(&new, &["validate", "corpus-skip"]);
    assert!(ran.ok(), "row L1: the new binary validates: {ran:?}");

    for (tag, name) in [(BETA1, "row L1 (beta.1)"), (V025, "row L1 (0.25.0)")] {
        let mut case = Case::new(name);
        let Some(old) = old_binary(tag, &case) else {
            continue;
        };
        case.seed_inline(&old, "corpus-skip", L1_SKIP, &[("item.sh", L1_ITEM_SH)]);
        let ran = case.run(&old, &["run", "corpus-skip", "--detach"]);
        if tag == BETA1 {
            assert!(!ran.ok(), "{name}: beta.1 refuses to start: {ran:?}");
            let created = fs::read_dir(case.runs())
                .map(|d| d.flatten().count())
                .unwrap_or(0);
            assert_eq!(created, 0, "{name}: no run directory exists");
        } else {
            assert!(
                ran.ok(),
                "{name}: 0.25.0 parses and starts the playbook: {ran:?}"
            );
            let run = ran
                .stdout
                .lines()
                .find_map(|l| l.strip_prefix("run started: "))
                .map(|r| r.trim().to_string())
                .unwrap_or_else(|| panic!("{name}: no run id in {ran:?}"));
            case.guards.push(RunGuard::new(&case.root(), &run));
            let journal = case.wait_journal(&run, "the gate", |j| {
                j.iter()
                    .any(|e| e["type"] == "review_requested" && e["node"] == "gate")
            });
            assert!(
                !journal.iter().any(|e| e["type"] == "list_skip_read"),
                "{name}: 0.25.0 drops the skip: {}",
                summary(&journal)
            );
            // 0.25.0 predates list mode: it drops the whole list block and
            // runs the step once, skipping nothing.
            assert!(
                journal.iter().any(|e| e["type"] == "node_finished"
                    && e["node"] == "work"
                    && e["status"] == "succeeded"),
                "{name}: 0.25.0 runs the step: {}",
                summary(&journal)
            );
            case.kill_driver(&run);
        }

        let run = l1_waiting_at_gate(&mut case, &new);
        case.save();
        case.restore();
        let run_dir = case.runs().join(&run);
        let before = fs::read(run_dir.join("events.jsonl")).unwrap();
        let ran = case.run(&old, &["resume", &run]);
        assert!(
            !ran.ok(),
            "{name}: the old binary refuses to resume: {ran:?}"
        );
        let after = fs::read(run_dir.join("events.jsonl")).unwrap();
        assert_eq!(before, after, "{name}: the old binary appended nothing");
    }
}

/// Row L1's window snapshots, made as the design fixes by a preparation
/// in process, run in a child process of this test binary with an explicit
/// environment ([`l1_window_child`]): nothing in this process changes its
/// own environment. The child prepares the run through
/// `prepare_supervised_background` under the case's project and home,
/// copies the journal while the `PreparedRun` is held (it ends with the skip
/// sets, no checkpoint after them), drives it on a thread until the gate
/// after the list waits, copies it again (a `node_finished` and its run
/// guard follow the sets) and stops it with the new binary. Returns the run
/// id and the two snapshots of the runs directory, before the checkpoint
/// and at the gate.
fn l1_prepared_windows(case: &mut Case, new: &Path) -> (String, PathBuf, PathBuf) {
    case.seed_inline(new, "corpus-skip", L1_SKIP, &[("item.sh", L1_ITEM_SH)]);
    let home = case.dir.path().join("home");
    let pre = case.dir.path().join("saved-pre");
    let gate = case.saved();
    let out = case.dir.path().join("l1-run-id");
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "journal_corpus_test::l1_window_child",
        "--ignored",
        "--nocapture",
    ])
    .env_clear()
    .env("PATH", std::env::var_os("PATH").unwrap_or_default())
    .env("HOME", &home)
    .env("XDG_CONFIG_HOME", home.join(".config"))
    .env("APB_DECISIONS_ALLOW_FAKE", "1")
    .env("APB_L1_ROOT", case.root())
    .env("APB_L1_PRE", &pre)
    .env("APB_L1_GATE", &gate)
    .env("APB_L1_OUT", &out)
    .env("APB_L1_NEW", new)
    .stdin(Stdio::null());
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        cmd.env("TMPDIR", tmp);
    }
    for (k, v) in &case.env {
        cmd.env(k, v);
    }
    let done = cmd.output().unwrap();
    assert!(
        done.status.success(),
        "{}: the window child failed: {}{}",
        case.name,
        String::from_utf8_lossy(&done.stdout),
        String::from_utf8_lossy(&done.stderr)
    );
    let run = fs::read_to_string(&out)
        .unwrap_or_else(|e| panic!("{}: the window child wrote no run id: {e}", case.name));
    let run = run.trim().to_string();
    case.guards.push(RunGuard::new(&case.root(), &run));
    let pre_journal = journal_at(&pre.join(&run));
    assert_eq!(
        pre_journal.last().map(|e| e["type"].clone()),
        Some(Value::from("list_skip_read")),
        "{}: the prepared journal ends with the skip sets: {}",
        case.name,
        summary(&pre_journal)
    );
    let journal = journal_at(&gate.join(&run));
    assert_run_guard_follows_last_checkpoint(&journal, case.name, &["list_skip"]);
    (run, pre, gate)
}

/// The child half of [`l1_prepared_windows`], run only by it: its whole
/// environment is the one the parent set, and the paths come from it. Run
/// any other way it does nothing.
#[test]
#[ignore = "run by l1_prepared_windows in a child process"]
fn l1_window_child() {
    use apb_engine::scheduler::{RunOptions, drive_prepared, prepare_supervised_background};
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let (Some(root), Some(pre), Some(gate), Some(out), Some(new)) = (
        var("APB_L1_ROOT"),
        var("APB_L1_PRE"),
        var("APB_L1_GATE"),
        var("APB_L1_OUT"),
        var("APB_L1_NEW"),
    ) else {
        return;
    };
    let prepared = prepare_supervised_background(&root, "corpus-skip", None, RunOptions::default())
        .unwrap_or_else(|e| panic!("prepare: {e}"));
    let run = prepared.run_id().to_string();
    let runs = root.join(".apb/runs");
    copy_tree(&runs, &pre);
    let _ = fs::remove_file(pre.join(&run).join("driver.pid"));
    let r = root.clone();
    let driver = std::thread::spawn(move || drive_prepared(&r, prepared));
    let start = Instant::now();
    while !journal_at(&runs.join(&run))
        .iter()
        .any(|e| e["type"] == "review_requested" && e["node"] == "gate")
    {
        assert!(start.elapsed() < DEADLINE, "the gate was not reached");
        std::thread::sleep(Duration::from_millis(20));
    }
    copy_tree(&runs, &gate);
    let _ = fs::remove_file(gate.join(&run).join("driver.pid"));
    let stopped = Command::new(&new)
        .args(["stop", &run])
        .current_dir(&root)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(stopped.status.success(), "stop: {stopped:?}");
    driver
        .join()
        .unwrap()
        .unwrap_or_else(|e| panic!("drive: {e}"));
    fs::write(&out, &run).unwrap();
}

/// Row L1, the pause window: on the snapshot right after the skip sets are
/// written (the journal ends with the sets, no checkpoint after them) and
/// on the snapshot at the gate after the list (a checkpoint and its run
/// guard naming `list_skip` follow them), both made in process, both
/// released readers read the run, refuse to pause it and append nothing,
/// and stop it with `run_aborted`.
#[test]
fn l1_list_skip_old_binaries_stop_but_never_pause() {
    let new = new_binary();
    for (tag, name) in [
        (BETA1, "row L1 window (beta.1)"),
        (V025, "row L1 window (0.25.0)"),
    ] {
        let mut case = Case::new(name);
        let Some(old) = old_binary(tag, &case) else {
            continue;
        };
        let (run, pre, gate) = l1_prepared_windows(&mut case, &new);
        let put_back = |snapshot: &Path| {
            writable_dirs(&case.runs());
            fs::remove_dir_all(case.runs()).unwrap();
            copy_tree(snapshot, &case.runs());
        };
        for (window, snapshot) in [
            ("after the set write", pre.clone()),
            ("at the gate", gate.clone()),
        ] {
            let run_dir = case.runs().join(&run);
            put_back(&snapshot);
            let ran = case.run(&old, &["runs", &run]);
            assert!(ran.ok(), "{name} ({window}): reads the run: {ran:?}");
            put_back(&snapshot);
            let before = fs::read(run_dir.join("events.jsonl")).unwrap();
            let ran = case.run(&old, &["pause", &run]);
            assert!(!ran.ok(), "{name} ({window}): refuses to pause: {ran:?}");
            let after = fs::read(run_dir.join("events.jsonl")).unwrap();
            assert_eq!(before, after, "{name} ({window}): appended nothing");
            put_back(&snapshot);
            let ran = case.run(&old, &["stop", &run]);
            assert!(ran.ok(), "{name} ({window}): stops the run: {ran:?}");
            let journal = case.journal(&run);
            assert_eq!(
                journal.last().map(|e| e["type"].clone()),
                Some(Value::from("run_aborted")),
                "{name} ({window}): stop appends run_aborted last"
            );
        }
    }
}

/// Row F1 (fork)'s playbook: a node that forks its source's session (the object
/// form of `continue_session`).
const F1_FORK: &str = r#"schema: 2
id: corpus-fork
name: Journal corpus session fork
version: 1.0.0
defaults: { profile: corpus }
nodes:
  - { id: start, type: start }
  - { id: assess, type: agent_task, prompt: "Assess." }
  - { id: try_a, type: agent_task, prompt: "Try.", continue_session: { from: assess, mode: fork } }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: assess }
  - { from: assess, to: try_a }
  - { from: try_a, to: ok }
"#;

/// Row F1 (fork): both released readers refuse to validate and to start a
/// playbook with the object form of `continue_session`, and create no
/// run directory; the new binary validates it.
#[test]
fn f1_fork_old_binaries_refuse_the_definition() {
    let new = new_binary();
    let mut case = Case::new("row F1 (new)");
    case.seed_inline(&new, "corpus-fork", F1_FORK, &[]);
    let ran = case.run(&new, &["validate", "corpus-fork"]);
    assert!(ran.ok(), "row F1: the new binary validates: {ran:?}");
    for (tag, name) in [(BETA1, "row F1 (beta.1)"), (V025, "row F1 (0.25.0)")] {
        let mut case = Case::new(name);
        let Some(old) = old_binary(tag, &case) else {
            continue;
        };
        case.seed_inline(&old, "corpus-fork", F1_FORK, &[]);
        let ran = case.run(&old, &["validate", "corpus-fork"]);
        assert!(!ran.ok(), "{name}: refuses to validate: {ran:?}");
        let ran = case.run(&old, &["run", "corpus-fork", "--detach"]);
        assert!(!ran.ok(), "{name}: refuses to start: {ran:?}");
        let created = fs::read_dir(case.runs())
            .map(|d| d.flatten().count())
            .unwrap_or(0);
        assert_eq!(created, 0, "{name}: no run directory exists");
    }
}
