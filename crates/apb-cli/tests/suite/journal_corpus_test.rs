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
//! binary's driver dies during the delay and the new binary resumes it. D8's
//! `review_decided.decided_by` and `attempt_isolation.tree` belong to later
//! features and are not in these journals yet.
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
