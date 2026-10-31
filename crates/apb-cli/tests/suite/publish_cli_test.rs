//! `apb publish` through the real binary, with `tests/fixtures/fake-git.sh`
//! first on PATH (per spawned process only): it logs every git call and can
//! make a push hang before or after the remote updated. The remote is a
//! local bare repository over `file://`; every git call reads an isolated
//! HOME and no system config, so nothing touches the network or the
//! operator's git config.
//!
//! What it proves: a push whose result is unknown is never repeated by a
//! resume, a person's `apb intent resolve --not-sent` lets a retry ask again
//! and the approval then pushes exactly once; a push that reached the
//! remote is confirmed by the read-only reconcile with no second push; and
//! `apb publish approve` is refused inside a run step.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

use crate::common::{RunGuard, apb_std};

const FAKE_GIT: &str = include_str!("../fixtures/fake-git.sh");
const DEADLINE: Duration = Duration::from_secs(60);

const PLAYBOOK: &str = r#"schema: 2
id: pub
name: Publish
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/work.sh", runner: sh }
  - { id: publish, type: publish, from: work, remote: origin, branch: "apb/out", timeout_seconds: 2 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: publish }
  - { from: publish, to: done }
"#;

const WORK: &str = r#"printf '%s' "$APB_RUN_ID" > work.txt
git add work.txt
git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false \
  commit -q -m "work of the run" -m "Apb-Run: $APB_RUN_ID"
"#;

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    bare: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    real_git: PathBuf,
}

/// The real git, found on the test process's PATH.
fn real_git() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    std::env::split_paths(&path)
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH")
        .canonicalize()
        .unwrap()
}

impl Project {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let p = Project {
            root: base.join("tree"),
            bare: base.join("remote.git"),
            home: base.join("home"),
            bin: base.join("bin"),
            log: base.join("git.log"),
            real_git: real_git(),
            _tmp: tmp,
        };
        for d in [&p.root, &p.home, &p.bin] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(p.home.join(".gitconfig"), "").unwrap();
        let fake = p.bin.join("git");
        fs::write(&fake, FAKE_GIT).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();

        p.git(&p.root, &["init", "-q", "-b", "main"]);
        fs::write(p.root.join(".gitignore"), ".apb/\n").unwrap();
        p.git(&p.root, &["add", ".gitignore"]);
        p.git(&p.root, &["commit", "-q", "-m", "base"]);
        p.git(&base, &["init", "-q", "--bare", "remote.git"]);
        let url = format!("file://{}", p.bare.display());
        p.git(&p.root, &["remote", "add", "origin", &url]);
        p.git(&p.root, &["push", "-q", "origin", "main"]);

        let out = p.apb(&["init"], None);
        assert!(out.status.success(), "{}", text(&out.stderr));
        let dir = p.root.join(".apb/playbooks/pub/1.0.0");
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
        fs::write(dir.join("scripts/work.sh"), WORK).unwrap();
        fs::write(p.root.join(".apb/playbooks/pub/current"), "1.0.0").unwrap();
        p
    }

    /// The real git, isolated from the operator's config, for the setup.
    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new(&self.real_git)
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_CONFIG_GLOBAL", self.home.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
        text(&out.stdout).trim().to_string()
    }

    /// `apb <args>` in the tree, the fake git first on PATH.
    fn command(&self, args: &[&str], push: Option<&str>) -> Command {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![self.bin.clone()];
        dirs.extend(std::env::split_paths(&path));
        let mut cmd = apb_std();
        cmd.args(args)
            .current_dir(&self.root)
            .env("PATH", std::env::join_paths(dirs).unwrap())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("FAKE_GIT_LOG", &self.log)
            .env("FAKE_GIT_REAL", &self.real_git)
            .env_remove("FAKE_GIT_PUSH");
        if let Some(mode) = push {
            cmd.env("FAKE_GIT_PUSH", mode);
        }
        cmd
    }

    fn apb(&self, args: &[&str], push: Option<&str>) -> Output {
        self.command(args, push).output().unwrap()
    }

    /// How many pushes the fake git saw.
    fn pushes(&self) -> usize {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.split_whitespace().any(|w| w == "push"))
            .count()
    }

    fn remote_ref(&self) -> Option<String> {
        let out = Command::new(&self.real_git)
            .arg("--git-dir")
            .arg(&self.bare)
            .args(["rev-parse", "--verify", "-q", "refs/heads/apb/out"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join(".gitconfig"))
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| text(&out.stdout).trim().to_string())
    }

    /// Starts the playbook detached with the start consent; returns the run.
    fn start(&self, push: Option<&str>) -> String {
        let refused = self.apb(&["run", "pub", "--detach"], push);
        let stderr = text(&refused.stderr);
        let at = stderr.find("consent_nonce: ").expect(&stderr) + "consent_nonce: ".len();
        let consent: String = stderr[at..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let out = self.apb(
            &[
                "run",
                "pub",
                "--detach",
                &format!("--confirm-irreversible={consent}"),
            ],
            push,
        );
        let stdout = text(&out.stdout);
        assert!(out.status.success(), "{stdout} {}", text(&out.stderr));
        stdout
            .lines()
            .find_map(|l| l.strip_prefix("run started: "))
            .expect(&stdout)
            .trim()
            .to_string()
    }

    /// Waits until `apb publish show` prints a nonce to consent with.
    fn nonce(&self, run_id: &str) -> String {
        poll("a plan waiting for consent", || {
            let out = self.apb(&["publish", "show", run_id], None);
            text(&out.stdout).lines().find_map(|l| {
                l.trim()
                    .strip_prefix("nonce:")
                    .map(|n| n.trim().to_string())
            })
        })
    }

    fn approve(&self, run_id: &str, nonce: &str) {
        let out = self.apb(&["publish", "approve", run_id, "--nonce", nonce], None);
        assert!(out.status.success(), "{}", text(&out.stderr));
    }

    fn events(&self, run_id: &str) -> String {
        fs::read_to_string(
            self.root
                .join(".apb/runs")
                .join(run_id)
                .join("events.jsonl"),
        )
        .unwrap_or_default()
    }

    /// Waits until the run journaled its `n`-th `run_finished`.
    fn finished(&self, run_id: &str, n: usize) {
        poll("the run to finish", || {
            (self
                .events(run_id)
                .matches("\"type\":\"run_finished\"")
                .count()
                >= n)
                .then_some(())
        });
    }

    fn runs(&self, run_id: &str) -> String {
        text(&self.apb(&["runs", run_id], None).stdout)
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Kills a foreground `apb` child if the test fails before it ends.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn an_unknown_push_is_never_repeated_and_a_resolved_retry_pushes_once() {
    let p = Project::new();
    let run_id = p.start(Some("hang"));
    let _guard = RunGuard::new(&p.root, &run_id);
    let nonce = p.nonce(&run_id);

    // Only a person consents: inside a run step the approval is refused.
    let inside = p
        .command(&["publish", "approve", &run_id, "--nonce", &nonce], None)
        .env("APB_RUN_ID", "some-run")
        .output()
        .unwrap();
    assert_eq!(inside.status.code(), Some(2));
    assert!(
        text(&inside.stderr).contains("publishing is refused"),
        "{}",
        text(&inside.stderr)
    );
    // `apb review --nonce` reaches the same check: a stale nonce names the plan.
    let stale = p.apb(
        &[
            "review",
            &run_id,
            "publish",
            "--decision",
            "publish",
            "--nonce",
            "publish-00000000000000000000000000000000",
        ],
        None,
    );
    assert_eq!(stale.status.code(), Some(2));
    assert!(
        text(&stale.stderr).contains(&nonce),
        "{}",
        text(&stale.stderr)
    );
    assert_eq!(p.pushes(), 0);

    p.approve(&run_id, &nonce);
    p.finished(&run_id, 1);
    assert_eq!(p.pushes(), 1);
    assert_eq!(p.remote_ref(), None);
    let detail = p.runs(&run_id);
    assert!(
        detail.contains("publish publish: publish_unknown"),
        "{detail}"
    );
    assert!(detail.contains("needs a look"), "{detail}");

    // A resume reads the remote back and never pushes again.
    let resumed = p.apb(&["resume", &run_id, "--from-node", "publish"], None);
    assert!(text(&resumed.stdout).contains("failed"), "{resumed:?}");
    p.finished(&run_id, 2);
    assert_eq!(p.pushes(), 1, "a resume never repeats an unknown push");

    // The person checked the remote: nothing arrived.
    let show = text(&p.apb(&["publish", "show", &run_id], None).stdout);
    let intent = show
        .lines()
        .find_map(|l| l.trim().strip_prefix("pushed:"))
        .and_then(|l| l.split_whitespace().next())
        .expect(&show)
        .to_string();
    assert!(show.contains("apb intent resolve"), "{show}");
    let resolved = p.apb(&["intent", "resolve", &run_id, &intent, "--not-sent"], None);
    assert!(resolved.status.success(), "{}", text(&resolved.stderr));

    // A retry asks again, and the approval pushes exactly once.
    let child = p
        .command(&["resume", &run_id, "--from-node", "publish"], None)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(child);
    let again = p.nonce(&run_id);
    assert_eq!(again, nonce, "the identical plan carries the same nonce");
    p.approve(&run_id, &again);
    let status = poll("the resumed run to exit", || child.0.try_wait().unwrap());
    assert!(status.success());
    assert_eq!(p.pushes(), 2, "exactly one more push");
    let after = p.git(&p.root, &["rev-parse", "HEAD"]);
    assert_eq!(p.remote_ref().as_deref(), Some(after.as_str()));
    assert!(
        p.runs(&run_id).contains("publish publish: published"),
        "{}",
        p.runs(&run_id)
    );
}

#[test]
fn a_push_that_reached_the_remote_is_confirmed_without_a_second_push() {
    let p = Project::new();
    let run_id = p.start(Some("hang_after"));
    let _guard = RunGuard::new(&p.root, &run_id);
    let nonce = p.nonce(&run_id);
    p.approve(&run_id, &nonce);
    p.finished(&run_id, 1);
    assert_eq!(p.pushes(), 1);
    let after = p.git(&p.root, &["rev-parse", "HEAD"]);
    assert_eq!(p.remote_ref().as_deref(), Some(after.as_str()));
    assert!(p.runs(&run_id).contains("publish_unknown"));

    let resumed = p.apb(&["resume", &run_id, "--from-node", "publish"], None);
    assert!(resumed.status.success(), "{}", text(&resumed.stderr));
    assert_eq!(p.pushes(), 1, "the reconcile reads, it never pushes");
    let detail = p.runs(&run_id);
    assert!(detail.contains("publish publish: published"), "{detail}");
    assert!(detail.contains("confirmed"), "{detail}");
}
