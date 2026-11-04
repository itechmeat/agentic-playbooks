//! `scripts/daily-cleanup.sh`: on a git that cannot list worktrees with
//! `--porcelain -z` (older than 2.36) the script warns, skips the worktree
//! steps (the prune included) and still runs the rest to its end; a merged worktree goes only
//! when every apb run in it has ended and its busy lock is not held (a lock
//! parked by a paused run holds only while that run is paused).

use std::path::Path;
use std::process::Command;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

#[test]
fn cleanup_finishes_when_git_cannot_list_worktrees() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("scripts")).expect("scripts dir");
    std::fs::copy(
        repo_root().join("scripts/daily-cleanup.sh"),
        repo.join("scripts/daily-cleanup.sh"),
    )
    .expect("copy script");
    let real_git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("find git")
            .stdout,
    )
    .expect("utf8");
    let real_git = real_git.trim();
    let ok = Command::new(real_git)
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .expect("git init");
    assert!(ok.success());

    // A git that refuses `worktree list`, as an old one refuses `-z`.
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let shim = bin.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = list ] && exit 129; done\nexec {real_git} \"$@\"\n"
        ),
    )
    .expect("write shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let cache = dir.path().join("cache");
    let tmp = dir.path().join("tmp");
    std::fs::create_dir_all(&tmp).expect("tmp dir");
    let out = Command::new("bash")
        .arg(repo.join("scripts/daily-cleanup.sh"))
        .args(["--force", "--dry-run"])
        .env("PATH", path)
        .env("XDG_CACHE_HOME", &cache)
        .env("TMPDIR", &tmp)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run script");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "script failed: {}\nstdout: {stdout}\nstderr: {stderr}",
        out.status
    );
    assert!(
        stderr.contains("worktree cleanup and sweep are skipped"),
        "{stderr}"
    );
    assert!(stdout.contains("daily cleanup: done"), "{stdout}");
    // No listing, no prune: it would drop the records of a worktree whose
    // path is only unavailable for now.
    assert!(!stdout.contains("worktree prune"), "{stdout}");
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository whose branch `feat` is merged into `origin/main` and checked
/// out in a linked worktree, with apb's runtime paths ignored as in this
/// repository. `prepare` fills the worktree's `.apb/`; the script's dry run
/// output is returned.
fn dry_run_with(prepare: impl FnOnce(&Path)) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("scripts")).expect("scripts dir");
    std::fs::copy(
        repo_root().join("scripts/daily-cleanup.sh"),
        repo.join("scripts/daily-cleanup.sh"),
    )
    .expect("copy script");
    std::fs::write(
        repo.join(".gitignore"),
        ".apb/runs/\n.apb/scenario-runs/\n.apb/workdir.lock\n.apb/workspace.local\n.apb/memory/\n",
    )
    .expect("gitignore");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let wt = dir.path().join("wt");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );
    std::fs::write(wt.join("work.txt"), "work\n").expect("work");
    git(&wt, &["add", "work.txt"]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    git(
        &repo,
        &["update-ref", "refs/remotes/origin/main", "refs/heads/feat"],
    );
    std::fs::create_dir_all(wt.join(".apb")).expect(".apb");
    std::fs::write(wt.join(".apb/workspace.local"), "id\n").expect("workspace");
    prepare(&wt);

    let cache = dir.path().join("cache");
    let tmp = dir.path().join("tmp");
    std::fs::create_dir_all(&tmp).expect("tmp dir");
    let out = Command::new("bash")
        .arg(repo.join("scripts/daily-cleanup.sh"))
        .args(["--force", "--dry-run"])
        .current_dir(&repo)
        .env("XDG_CACHE_HOME", &cache)
        .env("TMPDIR", &tmp)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("run script");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "script failed: {}\nstdout: {stdout}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn run_journal(wt: &Path, dir: &str, id: &str, lines: &[&str]) {
    let run = wt.join(".apb").join(dir).join(id);
    std::fs::create_dir_all(&run).expect("run dir");
    std::fs::write(run.join("events.jsonl"), lines.join("\n") + "\n").expect("journal");
}

const STARTED: &str = r#"{"seq":0,"ts":1,"type":"run_started","playbook":"p","version":"1.0.0"}"#;
const FINISHED: &str = r#"{"seq":9,"ts":9,"type":"run_finished","outcome":"succeeded"}"#;

fn removes(out: &str) -> bool {
    out.contains("remove merged worktree")
}

#[test]
fn a_merged_worktree_whose_runs_all_finished_is_removed() {
    let out = dry_run_with(|wt| {
        run_journal(
            wt,
            "runs",
            "r1",
            &[
                STARTED,
                r#"{"seq":1,"ts":2,"type":"send_intent_recorded","intent":"i1"}"#,
                r#"{"seq":2,"ts":3,"type":"send_intent_settled","intent":"i1","outcome":"unknown"}"#,
                FINISHED,
                r#"{"seq":10,"ts":10,"type":"send_intent_settled","intent":"i1","outcome":"confirmed","source":"person"}"#,
            ],
        );
        run_journal(
            wt,
            "runs",
            "r2",
            &[STARTED, r#"{"seq":1,"ts":2,"type":"run_aborted"}"#],
        );
        run_journal(wt, "scenario-runs", "s1", &[STARTED, FINISHED]);
        std::fs::create_dir_all(wt.join(".apb/memory")).unwrap();
        std::fs::write(wt.join(".apb/memory/x.json"), "{}").unwrap();
    });
    assert!(removes(&out), "{out}");
}

#[test]
fn a_merged_worktree_with_a_run_that_has_not_ended_is_kept() {
    let paused = r#"{"seq":5,"ts":5,"type":"run_paused","reason":"operator"}"#;
    let resumed = r#"{"seq":10,"ts":10,"type":"run_resumed"}"#;
    let queued = r#"{"seq":1,"ts":2,"type":"run_queued","reason":"busy"}"#;
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("paused", vec![STARTED, paused]),
        (
            "waiting on a gate",
            vec![
                STARTED,
                r#"{"seq":1,"ts":2,"type":"review_requested","node":"g"}"#,
            ],
        ),
        ("queued", vec![STARTED, queued]),
        ("resumed after its end", vec![STARTED, FINISHED, resumed]),
        (
            "an unknown send",
            vec![
                STARTED,
                r#"{"seq":1,"ts":2,"type":"send_intent_recorded","intent":"i1"}"#,
                r#"{"seq":2,"ts":3,"type":"send_intent_settled","intent":"i1","outcome":"unknown"}"#,
                FINISHED,
            ],
        ),
        (
            "an unsettled send",
            vec![
                STARTED,
                r#"{"seq":1,"ts":2,"type":"send_intent_recorded","intent":"i1"}"#,
                FINISHED,
            ],
        ),
        (
            "a torn journal",
            vec![STARTED, FINISHED, r#"{"seq":10,"ts""#],
        ),
    ];
    for (what, lines) in cases {
        let out = dry_run_with(|wt| run_journal(wt, "runs", "r1", &lines));
        assert!(!removes(&out), "{what}: {out}");
        assert!(out.contains("a run that has not ended"), "{what}: {out}");
    }
    // A run directory without a journal yet, and a live scenario run.
    let out = dry_run_with(|wt| std::fs::create_dir_all(wt.join(".apb/runs/r1")).unwrap());
    assert!(!removes(&out), "{out}");
    let out = dry_run_with(|wt| run_journal(wt, "scenario-runs", "s1", &[STARTED]));
    assert!(!removes(&out), "{out}");
}

#[test]
fn a_merged_worktree_whose_busy_lock_is_held_is_kept() {
    let me = std::process::id().to_string();
    let out = dry_run_with(|wt| {
        run_journal(wt, "runs", "r1", &[STARTED, FINISHED]);
        std::fs::write(wt.join(".apb/workdir.lock"), &me).unwrap();
    });
    assert!(!removes(&out), "{out}");
    assert!(out.contains("busy lock"), "{out}");
    // A pid no process holds is a crash leftover.
    let out = dry_run_with(|wt| {
        run_journal(wt, "runs", "r1", &[STARTED, FINISHED]);
        std::fs::write(wt.join(".apb/workdir.lock"), "4000000000").unwrap();
    });
    assert!(removes(&out), "{out}");
}

const PAUSED: &str = r#"{"seq":5,"ts":5,"type":"run_paused","reason":"operator"}"#;

fn park(wt: &Path, id: &str) {
    std::fs::write(wt.join(".apb/workdir.lock"), format!("paused:{id}\n")).unwrap();
}

#[test]
fn a_lock_parked_by_a_run_that_is_still_paused_keeps_the_worktree() {
    // The run is paused: the parked lock holds it.
    let out = dry_run_with(|wt| {
        run_journal(wt, "runs", "r1", &[STARTED, PAUSED]);
        park(wt, "r1");
    });
    assert!(!removes(&out), "{out}");
    assert!(out.contains("busy lock"), "{out}");
    // A journal that does not parse, and a run id that is not a plain name,
    // keep it too.
    let out = dry_run_with(|wt| {
        run_journal(wt, "runs", "r1", &[STARTED, r#"{"seq":1,"#]);
        park(wt, "r1");
    });
    assert!(!removes(&out), "{out}");
    assert!(out.contains("busy lock"), "{out}");
    let out = dry_run_with(|wt| park(wt, "../r1"));
    assert!(!removes(&out), "{out}");
    assert!(out.contains("busy lock"), "{out}");
}

#[test]
fn a_lock_parked_by_a_run_that_is_no_longer_paused_is_stale() {
    let aborted = r#"{"seq":6,"ts":6,"type":"run_aborted"}"#;
    let resumed = r#"{"seq":6,"ts":6,"type":"run_resumed"}"#;
    let finished = r#"{"seq":9,"ts":9,"type":"run_finished","outcome":"succeeded"}"#;
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "finished after its pause",
            vec![STARTED, PAUSED, resumed, finished],
        ),
        ("aborted while paused", vec![STARTED, PAUSED, aborted]),
    ];
    for (what, lines) in cases {
        let out = dry_run_with(|wt| {
            run_journal(wt, "runs", "r1", &lines);
            park(wt, "r1");
        });
        assert!(removes(&out), "{what}: {out}");
    }
    // Its run directory is gone.
    let out = dry_run_with(|wt| park(wt, "r1"));
    assert!(removes(&out), "{out}");
    // Another run that has not ended still keeps the worktree.
    let out = dry_run_with(|wt| {
        run_journal(wt, "runs", "r1", &[STARTED, PAUSED, aborted]);
        run_journal(wt, "runs", "r2", &[STARTED]);
        park(wt, "r1");
    });
    assert!(!removes(&out), "{out}");
    assert!(out.contains("a run that has not ended"), "{out}");
}
