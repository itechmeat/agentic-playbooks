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
//! Rows 1 to 6 are runs real writers produced; rows 7 and 8 are synthetic
//! journals that test readers and resumers only, and say so in their names.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
            .stdin(Stdio::null());
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

    // ----------------------------------------------------- writers

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

/// Row 3: the new binary writes a run waiting at review; beta.1 reads,
/// resumes to the end and stops.
#[test]
fn row3_new_writer_beta1_reader() {
    let mut case = Case::new("row 3");
    let Some(beta1) = old_binary(BETA1, &case) else {
        return;
    };
    let run = case.write_waiting_at_review(&new_binary());
    case.read_resume_stop(&beta1, &run);
}

/// Row 4: the same state as row 3, read, resumed and stopped by 0.25.0.
#[test]
fn row4_new_writer_v025_reader() {
    let mut case = Case::new("row 4");
    let Some(v025) = old_binary(V025, &case) else {
        return;
    };
    let run = case.write_waiting_at_review(&new_binary());
    case.read_resume_stop(&v025, &run);
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
