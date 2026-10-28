//! The isolation sandbox end to end, with a stub agent under a temp `HOME`
//! (never the operator's). Every enforcement claim here is gated on a
//! machine that can enforce it (`common::sandbox_or_skip`); each has a
//! refusal twin that runs everywhere: where the claim cannot be enforced,
//! `full` refuses and nothing runs.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use apb_core::registry::init_project;
use apb_engine::event::{AttemptIsolation, Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar};

/// A project tree (a git repository with one commit), a temp home with
/// credentials to steal, and a stub agent outside both.
struct Fixture {
    _dirs: Vec<tempfile::TempDir>,
    root: PathBuf,
    home: PathBuf,
    stub: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn fixture(isolation: &str, stub_body: &str) -> Fixture {
    let (tree, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fixture_at(tree, home, isolation, stub_body)
}

/// A temp directory outside `/tmp` (a credential location an extra path
/// may not reach): for a tree or a home an extra path names.
fn outside_tmp() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

/// [`fixture`] with the tree and the home in the given directories.
fn fixture_at(
    tree: tempfile::TempDir,
    home: tempfile::TempDir,
    isolation: &str,
    stub_body: &str,
) -> Fixture {
    let (root, h) = (tree.path().to_path_buf(), home.path().to_path_buf());
    fixture_in(&root, &h, vec![tree, home], isolation, stub_body)
}

/// [`fixture`] with the tree at `root` and the home at `h`, keeping `dirs`.
fn fixture_in(
    root: &Path,
    h: &Path,
    mut dirs: Vec<tempfile::TempDir>,
    isolation: &str,
    stub_body: &str,
) -> Fixture {
    let bin = tempfile::tempdir().unwrap();
    let root = root.to_path_buf();
    init_project(&root).unwrap();
    let pb = format!(
        "schema: 2\nid: iso\nname: Iso\nversion: 1.0.0\ndefaults: {{ profile: main }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: agent_task, prompt: \"do\", max_retries: 1, isolation: {isolation} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    );
    let dir = root.join(".apb/playbooks/iso/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), pb).unwrap();
    fs::write(root.join(".apb/playbooks/iso/current"), "1.0.0").unwrap();
    let prof = root.join(".apb/profiles/main");
    fs::create_dir_all(&prof).unwrap();
    fs::write(
        prof.join("profile.yaml"),
        "name: main\ndescription: t\nexecutor:\n  agent: claude\n  model: haiku\nskills:\n  - cs\n",
    )
    .unwrap();
    fs::write(prof.join("SOUL.md"), "").unwrap();
    fs::create_dir_all(root.join(".agents/skills/cs")).unwrap();
    fs::write(root.join(".agents/skills/cs/SKILL.md"), "skill").unwrap();
    fs::write(
        root.join(".gitignore"),
        ".apb/\n.probe-*\nprobe.txt\nenv.txt\n",
    )
    .unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", ".gitignore", ".agents"]);
    git(&root, &["commit", "-q", "-m", "base"]);
    fs::create_dir_all(h.join(".ssh")).unwrap();
    fs::write(h.join(".ssh/id"), "fake key material").unwrap();
    fs::create_dir_all(h.join(".claude")).unwrap();
    fs::write(h.join(".claude/.credentials.json"), "{\"fake\":1}").unwrap();
    fs::write(
        h.join(".gitconfig"),
        "[user]\n\tname = Sandbox Test\n\temail = sandbox@example.invalid\n",
    )
    .unwrap();
    let stub = bin.path().join("agent.sh");
    common::write_sync(&stub, &format!("#!/bin/bash\n{stub_body}\n"));
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    Fixture {
        home: h.to_path_buf(),
        root,
        stub,
        _dirs: {
            dirs.push(bin);
            dirs
        },
    }
}

/// The environment of one sandboxed run: the stub, the temp home, and the
/// operator variables the sandbox must keep out.
fn env(f: &Fixture) -> Vec<EnvVar> {
    vec![
        EnvVar::set("APB_AGENT_CMD", &f.stub),
        EnvVar::set("HOME", &f.home),
        EnvVar::set("SANDBOX_CANARY_TOKEN", "x"),
        EnvVar::set("SSH_AUTH_SOCK", "/tmp/no-agent.sock"),
        EnvVar::set("DBUS_SESSION_BUS_ADDRESS", "unix:path=/tmp/no-bus"),
        EnvVar::remove("APB_SANDBOX"),
    ]
}

fn probe(root: &Path) -> std::collections::BTreeMap<String, String> {
    fs::read_to_string(root.join("probe.txt"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn events(root: &Path, run_id: &str) -> Vec<Event> {
    read_all(&root.join(".apb/runs").join(run_id)).unwrap()
}

fn isolation_of(events: &[Event]) -> Vec<Option<AttemptIsolation>> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptStarted { isolation, .. } => Some(isolation.clone()),
            _ => None,
        })
        .collect()
}

fn node_output(events: &[Event], node: &str) -> Option<(String, String)> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::NodeFinished {
            node: n,
            status,
            output,
            ..
        } if n == node => Some((status.to_string(), output.clone())),
        _ => None,
    })
}

/// What the stub records: each claim as `key=value` in `probe.txt` of the
/// tree.
const PROBES: &str = r#"out=probe.txt; : > "$out"; r() { echo "$1=$2" >> "$out"; }
if cat "$HOME/.ssh/id" >/dev/null 2>&1; then r ssh read; else r ssh denied; fi
if echo x >> .git/config 2>/dev/null; then r gitconfig written; else r gitconfig ro; fi
if touch .git/hooks/x 2>/dev/null; then r hooks written; else r hooks ro; fi
if ls .apb/runs/*/events.jsonl >/dev/null 2>&1; then r journal visible; else r journal hidden; fi
if (echo hi > "/dev/tcp/127.0.0.1/$(cat .probe-port)") 2>/dev/null; then r tcp reached; else r tcp blocked; fi
if kill -0 "$(cat .probe-pid)" 2>/dev/null; then r kill sent; else r kill denied; fi
if "$(cat .probe-exe)" __test-abstract-connect "$(cat .probe-abstract)"; then r abstract reached; else r abstract blocked; fi
if cat .agents/skills/cs/SKILL.md >/dev/null 2>&1; then r skill read; else r skill missing; fi
if echo x >> .agents/skills/cs/SKILL.md 2>/dev/null; then r skillwrite written; else r skillwrite ro; fi
if printf '{"refreshed":1}' > "$HOME/.claude/.credentials.json" 2>/dev/null; then r cred_inplace ok; else r cred_inplace failed; fi
printf x > "$HOME/.claude/.cred.tmp"
if mv "$HOME/.claude/.cred.tmp" "$HOME/.claude/.credentials.json" 2>/dev/null; then r cred_rename ok; else r cred_rename failed; fi
env | cut -d= -f1 | sort > env.txt
echo work > work.txt
if git add work.txt && git commit -q -m "sandboxed commit"; then r commit ok; else r commit failed; fi
printf '{"status":"success","outputs":{"verdict":"sandboxed"}}' > "$APB_STATUS_FILE"
echo done"#;

/// Listeners the stub probes: a TCP port on the host's loopback and an
/// abstract unix socket, plus the files that name them in the tree.
struct Listeners {
    _tcp: std::net::TcpListener,
    _abstract: std::os::unix::net::UnixListener,
}

fn listen(root: &Path) -> Listeners {
    use std::os::linux::net::SocketAddrExt as _;
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let name = format!("apb-sandbox-test-{}", std::process::id());
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    let abs = std::os::unix::net::UnixListener::bind_addr(&addr).unwrap();
    fs::write(
        root.join(".probe-port"),
        tcp.local_addr().unwrap().port().to_string(),
    )
    .unwrap();
    fs::write(root.join(".probe-pid"), std::process::id().to_string()).unwrap();
    fs::write(root.join(".probe-abstract"), &name).unwrap();
    fs::write(
        root.join(".probe-exe"),
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .as_bytes(),
    )
    .unwrap();
    Listeners {
        _tcp: tcp,
        _abstract: abs,
    }
}

#[test]
fn full_with_no_network_enforces_the_contract() {
    let _l = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let f = fixture("{ level: full, network: none }", PROBES);
    let _env = env(&f);
    let _net = listen(&f.root);
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    let ev = events(&f.root, &res.run_id);
    let p = probe(&f.root);
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "{p:?} {:?}",
        node_output(&ev, "work")
    );
    for (k, v) in [
        ("ssh", "denied"),
        ("gitconfig", "ro"),
        ("hooks", "ro"),
        ("journal", "hidden"),
        ("tcp", "blocked"),
        ("kill", "denied"),
        ("abstract", "blocked"),
        ("skill", "read"),
        ("skillwrite", "ro"),
        ("commit", "ok"),
    ] {
        assert_eq!(p.get(k).map(String::as_str), Some(v), "{k}: {p:?}");
    }
    // Only allowlisted variables reach the agent.
    let names = fs::read_to_string(f.root.join("env.txt")).unwrap();
    for gone in [
        "SANDBOX_CANARY_TOKEN",
        "SSH_AUTH_SOCK",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        assert!(
            !names.lines().any(|n| n == gone),
            "{gone} reached the agent"
        );
    }
    for kept in [
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "APB_STATUS_FILE",
        "HOME",
        "PATH",
    ] {
        assert!(names.lines().any(|n| n == kept), "{kept} missing");
    }
    // The status file verdict is read back, and published where the
    // retro and grade readers look for it.
    let (status, output) = node_output(&ev, "work").unwrap();
    assert_eq!(status, "succeeded");
    assert!(output.contains("sandboxed"), "{output}");
    let published = fs::read_dir(
        f.root
            .join(".apb/runs")
            .join(&res.run_id)
            .join("agent-status"),
    )
    .unwrap()
    .filter_map(|e| fs::read_to_string(e.unwrap().path()).ok())
    .any(|t| t.contains("sandboxed"));
    assert!(published, "the boxed status file was published");
    // Provenance recorded the commit made inside the sandbox.
    let committed = ev.iter().find_map(|e| match &e.payload {
        EventPayload::ArtifactsCommitted { commits, .. } => Some(commits.clone()),
        _ => None,
    });
    assert_eq!(committed.unwrap()[0].subject, "sandboxed commit");
    // The repository's control files are unchanged, nothing was planted.
    assert!(
        !fs::read_to_string(f.root.join(".git/config"))
            .unwrap()
            .ends_with("x\n")
    );
    assert!(!f.root.join(".git/hooks/x").exists());
    // The mount points apb created in the tree are gone again.
    assert!(
        !f.root.join(".claude").exists(),
        "a mount point was left behind"
    );
    // The record names the backend, the holds and the limits.
    let iso = isolation_of(&ev)
        .pop()
        .flatten()
        .expect("an isolation record");
    assert_eq!(
        (iso.level.as_str(), iso.backend.as_str()),
        ("full", "bwrap")
    );
    assert_eq!(iso.network, "none");
    for h in [
        "network.none",
        "files.git_control_read_only",
        "credentials.env_allowlist",
    ] {
        assert!(iso.holds.iter().any(|x| x == h), "{h}: {iso:?}");
    }
    assert!(iso.limits.iter().any(|x| x == "user.host_uid"));
    // The env allowlist was forced on a profile that inherits.
    assert!(ev.iter().any(|e| matches!(
        &e.payload,
        EventPayload::AgentEnvApplied { forced_by: Some(f), .. } if f == "isolation"
    )));
    // claude's login file: a write in place reaches it, a rename onto it
    // fails (it is a single-file bind), which is why the login is bound
    // read-write and an env token is preferred when one is set.
    assert_eq!(p.get("cred_inplace").map(String::as_str), Some("ok"));
    assert_eq!(p.get("cred_rename").map(String::as_str), Some("failed"));
    assert_eq!(
        fs::read_to_string(f.home.join(".claude/.credentials.json")).unwrap(),
        "{\"refreshed\":1}"
    );
}

/// The refusal twin, on every machine: with the sandbox off, `full`
/// refuses at the gate every launch surface calls and again at prepare,
/// before anything of the run is written or spawned.
#[test]
fn full_refuses_where_the_sandbox_is_unavailable() {
    let _l = common::env_lock();
    let f = fixture(
        "{ level: full, network: none }",
        "touch \"$(dirname \"$0\")/ran\"; echo done",
    );
    let _env = env(&f);
    let _off = EnvVar::set("APB_SANDBOX", "off");
    let wref = apb_core::scope::PlaybookRef {
        id: "iso".into(),
        version: None,
        origin: apb_core::scope::Origin::Project { workspace_id: None },
    };
    let refusal = apb_engine::gate::check_run(&f.root, &wref, true, false).unwrap_err();
    assert_eq!(refusal["policy"], "isolation_unavailable", "{refusal}");
    assert_eq!(refusal["nodes"][0]["node"], "work");
    assert_eq!(refusal["nodes"][0]["reasons"][0]["code"], "disabled");
    let err = run(&f.root, "iso", None, RunOptions::default()).unwrap_err();
    assert!(err.to_string().contains("isolation_unavailable"), "{err}");
    let runs = f.root.join(".apb/runs");
    assert!(
        fs::read_dir(&runs).map_or(true, |mut d| d.next().is_none()),
        "a run was written"
    );
    assert!(!f.stub.with_file_name("ran").exists(), "the agent ran");
}

/// `best_effort` without a sandbox still runs: today's scratch directory,
/// the forced allowlist, and the `no_sandbox` limit on the record.
#[test]
fn best_effort_without_the_sandbox_runs_in_a_scratch_directory() {
    let _l = common::env_lock();
    let f = fixture(
        "{ level: best_effort, network: none }",
        "pwd > \"$(dirname \"$0\")/cwd\"; env | cut -d= -f1 > \"$(dirname \"$0\")/env\"; echo done",
    );
    let _env = env(&f);
    let _off = EnvVar::set("APB_SANDBOX", "off");
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let cwd = fs::read_to_string(f.stub.with_file_name("cwd")).unwrap();
    assert!(cwd.trim().ends_with("work/work/1"), "{cwd}");
    let names = fs::read_to_string(f.stub.with_file_name("env")).unwrap();
    assert!(!names.lines().any(|n| n == "SANDBOX_CANARY_TOKEN"));
    let ev = events(&f.root, &res.run_id);
    let iso = isolation_of(&ev)
        .pop()
        .flatten()
        .expect("an isolation record");
    assert_eq!(iso.backend, "none");
    // The node asked for no network, but without a sandbox it had the
    // host's: the record says what the attempt really had.
    assert_eq!(iso.network, "open");
    assert!(iso.limits.iter().any(|l| l == "no_sandbox"));
    assert!(iso.fallbacks.iter().any(|r| r.starts_with("disabled")));
}

/// `network: open` with the Landlock scope: the host's loopback is
/// reachable (a stated limit), a host abstract socket is not.
#[test]
fn the_open_network_reaches_loopback_but_not_abstract_sockets() {
    let _l = common::env_lock();
    if !common::sandbox_or_skip(true) {
        return;
    }
    let f = fixture("{ level: full, network: open }", PROBES);
    let _env = env(&f);
    let _net = listen(&f.root);
    // Control: outside the sandbox the probe reaches the socket.
    let control = Command::new(std::env::current_exe().unwrap())
        .arg("__test-abstract-connect")
        .arg(fs::read_to_string(f.root.join(".probe-abstract")).unwrap())
        .status()
        .unwrap();
    assert!(control.success(), "the abstract socket probe works");
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    let p = probe(&f.root);
    assert_eq!(res.outcome, RunStatus::Succeeded, "{p:?}");
    assert_eq!(p.get("tcp").map(String::as_str), Some("reached"), "{p:?}");
    assert_eq!(
        p.get("abstract").map(String::as_str),
        Some("blocked"),
        "{p:?}"
    );
    let iso = isolation_of(&events(&f.root, &res.run_id))
        .pop()
        .flatten()
        .unwrap();
    assert!(iso.holds.iter().any(|h| h == "network.scoped"));
    assert!(iso.limits.iter().any(|l| l == "network.open_any_host"));
}

/// The check before every attempt fails closed: the sandbox goes away
/// between two attempts, and the retry refuses instead of running the agent
/// without it.
#[test]
fn a_retry_refuses_when_the_sandbox_went_away() {
    let _l = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let body = "n=$(cat .probe-n 2>/dev/null || echo 0); n=$((n+1)); echo $n > .probe-n\n\
                touch .probe-running; while [ -f .probe-gate ]; do sleep 0.05; done; exit 1";
    let f = fixture("{ level: full, network: none }", body);
    let _env = env(&f);
    fs::write(f.root.join(".probe-gate"), "").unwrap();
    let root = f.root.clone();
    let worker = std::thread::spawn(move || run(&root, "iso", None, RunOptions::default()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !f.root.join(".probe-running").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the first attempt never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // `set_var` while the worker thread runs: Rust's own env reads are
    // serialized with it, and the engine reads `APB_SANDBOX` only through
    // `std::env`. A libc-level read elsewhere in the process (none is known
    // here) would race it; the probe offers no other switch, by design.
    let _off = EnvVar::set("APB_SANDBOX", "off");
    fs::remove_file(f.root.join(".probe-gate")).unwrap();
    let res = worker.join().unwrap().unwrap();
    assert_ne!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(f.root.join(".probe-n")).unwrap().trim(),
        "1"
    );
    let ev = events(&f.root, &res.run_id);
    assert_eq!(isolation_of(&ev).len(), 1, "only the first attempt started");
    let (_, output) = node_output(&ev, "work").unwrap();
    assert!(output.starts_with("isolation_unavailable: "), "{output}");
}

/// A host-mode run's agent steps run in the host agent, outside any sandbox
/// apb could start: prepare, which knows the execution mode, refuses a
/// `full` node there even where the gate (which does not) let it through.
#[test]
fn a_host_mode_run_refuses_a_full_node_at_prepare() {
    use apb_core::execution::{ExecutionMode, ExecutionRequest};
    let _l = common::env_lock();
    let f = fixture("{ level: full, network: none }", "echo done");
    let _env = env(&f);
    let opts = RunOptions {
        execution: ExecutionRequest {
            mode: Some(ExecutionMode::Host),
            client: Some("test-host".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let err = run(&f.root, "iso", None, opts).unwrap_err().to_string();
    assert!(err.contains("isolation_unavailable"), "{err}");
    assert!(err.contains("host_mode"), "{err}");
}

/// Without `isolation` (and with `none`) nothing changes: the agent runs in
/// the tree with apb's whole environment, no isolation record, no forced
/// allowlist and nothing under `sandbox/`, even where the sandbox works.
#[test]
fn a_node_without_isolation_runs_as_before() {
    let _l = common::env_lock();
    for iso in ["none", "{ level: none }"] {
        let f = fixture(
            iso,
            "pwd > \"$(dirname \"$0\")/cwd\"; env | cut -d= -f1 > \"$(dirname \"$0\")/env\"; echo done",
        );
        let _env = env(&f);
        let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
        assert_eq!(res.outcome, RunStatus::Succeeded);
        let cwd = fs::read_to_string(f.stub.with_file_name("cwd")).unwrap();
        assert_eq!(
            Path::new(cwd.trim()).canonicalize().unwrap(),
            f.root.canonicalize().unwrap()
        );
        let names = fs::read_to_string(f.stub.with_file_name("env")).unwrap();
        assert!(
            names.lines().any(|n| n == "SANDBOX_CANARY_TOKEN"),
            "inherit"
        );
        let ev = events(&f.root, &res.run_id);
        assert_eq!(isolation_of(&ev), vec![None]);
        assert!(
            !ev.iter()
                .any(|e| matches!(e.payload, EventPayload::AgentEnvApplied { .. }))
        );
        let run_dir = f.root.join(".apb/runs").join(&res.run_id);
        assert!(!run_dir.join("sandbox").exists());
        assert!(!run_dir.join("work").exists());
    }
}

/// A stub that plants files redirecting git (a `commondir` into a
/// directory with its own config, a `config.worktree`) while
/// `.probe-plant` exists, then reports success.
const PLANT: &str = r#"if [ -f .probe-plant ]; then
mkdir -p pwn; printf '[core]\n\trepositoryformatversion = 0\n[core]\n\tfsmonitor = /bin/false\n' > pwn/config
echo "$PWD/pwn" > .git/commondir
printf '[core]\n\tfsmonitor = /bin/false\n' > .git/config.worktree
fi
echo work > work.txt
git add work.txt && git commit -q -m "sandboxed commit"
printf '{"status":"success"}' > "$APB_STATUS_FILE"
echo done"#;

/// Files that redirect git, planted in a sandbox, are put back and fail the
/// attempt, so neither apb's git nor the operator's ever follows them; the
/// twin is the same stub without planting, which succeeds.
#[test]
fn a_planted_git_redirection_is_put_back_and_fails_the_attempt() {
    let _l = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let f = fixture("{ level: full, network: none }", PLANT);
    let _env = env(&f);
    fs::write(f.root.join(".probe-plant"), "").unwrap();
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(&f.root, &res.run_id);
    let (_, output) = node_output(&ev, "work").unwrap();
    assert!(output.contains("redirects git"), "{output}");
    assert!(
        output.contains(".git/commondir` appeared (removed)"),
        "{output}"
    );
    assert!(!f.root.join(".git/commondir").exists());
    assert!(!f.root.join(".git/config.worktree").exists());
    assert!(apb_core::git::Repo::locate(&f.root).is_ok());

    fs::remove_file(f.root.join(".probe-plant")).unwrap();
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// An extra path that resolves into a credential location is refused at
/// spawn, whatever its spelling (here a link in the home to `~/.ssh`): the
/// agent never runs. The twin is a plain directory, which is bound.
#[test]
fn an_extra_path_through_a_link_into_a_credential_location_is_refused() {
    let _l = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let body = "cat \"$HOME/keys/id\" > leaked.txt 2>/dev/null; cat \"$HOME/tools/t\" > tools.txt; echo done";
    let iso = |p: &str| format!("{{ level: full, network: none, read_only: [{p}] }}");
    let f = fixture_at(outside_tmp(), outside_tmp(), &iso("~/keys"), body);
    std::os::unix::fs::symlink(f.home.join(".ssh"), f.home.join("keys")).unwrap();
    let _env = env(&f);
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(&f.root, &res.run_id);
    let (_, output) = node_output(&ev, "work").unwrap();
    assert!(
        output.starts_with("isolation_unavailable: sandbox_setup"),
        "{output}"
    );
    assert!(output.contains("credential location `~/.ssh`"), "{output}");
    assert!(!f.root.join("leaked.txt").exists(), "the agent ran");

    let f = fixture_at(outside_tmp(), outside_tmp(), &iso("~/tools"), body);
    fs::create_dir_all(f.home.join("tools")).unwrap();
    fs::write(f.home.join("tools/t"), "tool").unwrap();
    let _env = env(&f);
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(f.root.join("tools.txt")).unwrap(),
        "tool"
    );
}

/// A writable extra that is the tree itself never re-opens what the view
/// protects in it: the git config stays read-only and the journal hidden.
#[test]
fn a_writable_extra_over_the_tree_keeps_its_protections() {
    let _l = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let tree = outside_tmp();
    let path = tree.path().canonicalize().unwrap();
    let f = fixture_at(
        tree,
        tempfile::tempdir().unwrap(),
        &format!(
            "{{ level: full, network: none, writable: ['{}'] }}",
            path.display()
        ),
        PROBES,
    );
    let _env = env(&f);
    let _net = listen(&f.root);
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    let p = probe(&f.root);
    assert_eq!(res.outcome, RunStatus::Succeeded, "{p:?}");
    for (k, v) in [("gitconfig", "ro"), ("hooks", "ro"), ("journal", "hidden")] {
        assert_eq!(p.get(k).map(String::as_str), Some(v), "{k}: {p:?}");
    }
}

/// A profile that grants no skills sees none of the tree's own: an empty
/// directory covers the tree's skill directories, read-only. The twin is
/// `full_with_no_network_enforces_the_contract`, whose granted skill is read.
#[test]
fn ungranted_skills_of_the_tree_stay_out_of_the_sandbox() {
    let _l = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let body = r#"if cat .agents/skills/cs/SKILL.md >/dev/null 2>&1; then echo read > skill.txt; else echo missing > skill.txt; fi
if echo x >> .agents/skills/new.md 2>/dev/null; then echo written >> skill.txt; else echo ro >> skill.txt; fi
echo done"#;
    let f = fixture("{ level: full, network: none }", body);
    fs::write(
        f.root.join(".apb/profiles/main/profile.yaml"),
        "name: main\ndescription: t\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    let _env = env(&f);
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(f.root.join("skill.txt")).unwrap(),
        "missing\nro\n"
    );
    assert!(
        f.root.join(".agents/skills/cs/SKILL.md").is_file(),
        "the tree's skill is kept"
    );
}

/// A stub that plants a submodule: its own repository with a clean filter
/// its `.gitattributes` names, added to the parent's index as a gitlink.
const PLANT_SUBMODULE: &str = r#"root="$PWD"
mkdir -p sub && cd sub && git init -q
printf '#!/bin/sh\ntouch "%s/ran"\ncat\n' "$root" > evil.sh && chmod +x evil.sh
echo '* filter=x' > .gitattributes && echo one > f.txt
git add . && git commit -q -m sub
git config filter.x.clean "$PWD/evil.sh"
cd "$root" && git add sub && echo two > sub/f.txt
printf '{"status":"success"}' > "$APB_STATUS_FILE"
echo done"#;

/// A submodule planted in the sandbox loses its `.git` after the attempt,
/// which fails: neither apb's git nor the operator's ever enters it. The
/// control is the planted filter itself, live before the guard ran (the
/// stub's own `git add` in the parent does not reach it).
#[test]
fn a_planted_submodule_loses_its_git_directory_and_fails_the_attempt() {
    let _l = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let f = fixture("{ level: full, network: none }", PLANT_SUBMODULE);
    let _env = env(&f);
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(&f.root, &res.run_id);
    let (_, output) = node_output(&ev, "work").unwrap();
    // The first attempt added the gitlink; the retry found it and planted
    // the `.git` again.
    assert!(output.contains("`sub/.git` appeared (removed)"), "{output}");
    assert!(!f.root.join("sub/.git").exists());
    let _ = fs::remove_file(f.root.join("ran"));
    // The operator's own git over the tree: the submodule has no git
    // directory left to enter.
    git(&f.root, &["status", "--porcelain"]);
    assert!(
        !f.root.join("ran").exists(),
        "a plain git ran the planted filter"
    );
}

/// A project rooted at a directory that holds both credentials and
/// persistence locations (`~/.config`) is refused at spawn under `full`,
/// also when it is a writable extra: the agent never runs.
#[test]
fn a_tree_at_the_operators_config_directory_is_refused() {
    let _l = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let home = outside_tmp();
    let h = home.path().to_path_buf();
    let tree = h.join(".config");
    fs::create_dir_all(tree.join("gh")).unwrap();
    fs::write(tree.join("gh/hosts.yml"), "token: fake").unwrap();
    let f = fixture_in(
        &tree,
        &h,
        vec![home],
        "{ level: full, network: none, writable: [~/.config] }",
        "cat gh/hosts.yml > leaked.txt; echo done",
    );
    let _env = env(&f);
    let res = run(&f.root, "iso", None, RunOptions::default());
    // V114 refuses the writable `~/.config` before the run starts.
    let err = res.unwrap_err().to_string();
    assert!(err.contains("V114") && err.contains("~/.config/"), "{err}");
    assert!(!f.root.join("leaked.txt").exists(), "the agent ran");
    // Without the extra, the tree itself is refused at spawn.
    let pb = f.root.join(".apb/playbooks/iso/1.0.0/playbook.yaml");
    let text = fs::read_to_string(&pb).unwrap();
    fs::write(&pb, text.replace(", writable: [~/.config]", "")).unwrap();
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(&f.root, &res.run_id);
    let (_, output) = node_output(&ev, "work").unwrap();
    assert!(
        output.starts_with("isolation_unavailable: sandbox_setup"),
        "{output}"
    );
    assert!(output.contains("the tree"), "{output}");
    assert!(!f.root.join("leaked.txt").exists(), "the agent ran");
}

/// A sandboxed `success_check` that leaves the repository in a state apb
/// cannot put back (a git directory replaced by a link) fails the node at
/// once, with no retry. The twin is
/// `a_planted_git_redirection_is_put_back_and_fails_the_attempt`, whose
/// restorable change is retried.
#[test]
fn a_success_check_that_breaks_the_repository_fails_the_node_at_once() {
    let _l = common::env_lock();
    if !common::sandbox_or_skip(false) {
        return;
    }
    let f = fixture(
        "{ level: full, network: none }",
        "printf '{\"status\":\"success\"}' > \"$APB_STATUS_FILE\"; echo done",
    );
    let dir = f.root.join(".apb/playbooks/iso/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(
        dir.join("scripts/check.sh"),
        "mkdir -p .git/objects/info && mv .git/objects/info .git/objects/moved && ln -s /tmp .git/objects/info\n",
    )
    .unwrap();
    let pb = dir.join("playbook.yaml");
    let text = fs::read_to_string(&pb).unwrap();
    fs::write(
        &pb,
        text.replace(
            "max_retries: 1,",
            "max_retries: 1, success_check: scripts/check.sh,",
        ),
    )
    .unwrap();
    let _env = env(&f);
    let res = run(&f.root, "iso", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(&f.root, &res.run_id);
    let (_, output) = node_output(&ev, "work").unwrap();
    assert!(output.contains("replaced by a link"), "{output}");
    assert_eq!(
        isolation_of(&ev).len(),
        1,
        "no retry on a tree apb could not restore"
    );
}
