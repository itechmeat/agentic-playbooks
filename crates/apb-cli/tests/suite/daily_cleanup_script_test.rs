//! `scripts/daily-cleanup.sh` on a git that cannot list worktrees with
//! `--porcelain -z` (older than 2.36): the script warns, skips the worktree
//! steps and still runs the rest to its end.

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
}
