//! `scripts/third-party-notices.sh --check` tells a stale file apart from a
//! script that could not run: exit 3 only when the generated notices differ
//! from the committed file, exit 1 for every other failure, a tool that
//! itself exits 3 included. CI's lint job lets exit 3 pass as a warning on a
//! Dependabot pull request, so a failure to run must never read as 3.
//!
//! The real script runs in a scratch repository with stand-ins for `cargo`
//! and `bun` on the PATH, each failure injected through an environment
//! variable. A `grep` or `diff` that cannot do its work is put first on the
//! PATH for one run: a tool error must never read as a verdict.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CARGO: &str = r#"#!/bin/sh
case "$1" in
  fetch) exit "${STUB_FETCH_RC:-0}" ;;
  about)
    out=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --output-file ]; then out="$2"; fi
      shift
    done
    printf 'crate-a 1.0.0 MIT\n\nUsed by:\n  crate-a 1.0.0\n' >"$out"
    if [ -n "${STUB_ABOUT_LOCKED:-}" ]; then
      locked="${out%/*}/locked"
      mkdir "$locked" && : >"$locked/f" && chmod 500 "$locked"
    fi
    exit "${STUB_ABOUT_RC:-0}"
    ;;
  *) exit 99 ;;
esac
"#;

const BUN: &str = r#"#!/bin/sh
case "$1" in
  install) exit "${STUB_INSTALL_RC:-0}" ;;
  scripts/third-party-notices.ts)
    printf '%s\n' "${STUB_WEB_TEXT:-web-package 1.0.0 MIT}"
    exit "${STUB_VITE_RC:-0}"
    ;;
  *) exit 99 ;;
esac
"#;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    /// A scratch repository whose committed notices are current.
    fn current() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("scripts")).expect("scripts dir");
        std::fs::create_dir_all(root.join("web")).expect("web dir");
        std::fs::copy(
            repo_root().join("scripts/third-party-notices.sh"),
            root.join("scripts/third-party-notices.sh"),
        )
        .expect("copy script");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        for (name, body) in [("cargo", CARGO), ("bun", BUN)] {
            let path = bin.join(name);
            std::fs::write(&path, body).expect("write stub");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        std::fs::create_dir_all(dir.path().join("tmp")).expect("tmp dir");
        let repo = Self { dir };
        let out = repo.run(&[], &[]);
        assert_eq!(out.status.code(), Some(0), "write: {}", stderr(&out));
        repo
    }

    fn notices(&self) -> PathBuf {
        self.dir.path().join("repo/THIRD-PARTY-NOTICES.txt")
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.run_in(None, args, env)
    }

    /// A directory holding a stand-in for `tool` that exits `rc` without
    /// doing its work, as a tool that cannot run does.
    fn stand_in(&self, tool: &str, rc: i32) -> PathBuf {
        let fail = self.dir.path().join("fail");
        std::fs::create_dir_all(&fail).expect("fail dir");
        let path = fail.join(tool);
        let body = format!("#!/bin/sh\necho \"{tool}: stand-in failure\" >&2\nexit {rc}\n");
        std::fs::write(&path, body).expect("write stand-in");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        fail
    }

    /// Runs the script with `tool` replaced on the PATH by its stand-in.
    fn run_failing(&self, args: &[&str], tool: &str, rc: i32) -> Output {
        let fail = self.stand_in(tool, rc);
        let out = self.run_in(Some(&fail), args, &[]);
        std::fs::remove_dir_all(&fail).expect("remove fail dir");
        out
    }

    fn run_in(&self, first: Option<&Path>, args: &[&str], env: &[(&str, &str)]) -> Output {
        let (out, left) = self.run_leaving(first, args, env);
        assert!(left.is_empty(), "the work dir is removed on exit: {left:?}");
        out
    }

    /// Runs the script and returns what it left in its temporary directory.
    fn run_leaving(
        &self,
        first: Option<&Path>,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> (Output, Vec<PathBuf>) {
        let mut path = format!(
            "{}:{}",
            self.dir.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        if let Some(first) = first {
            path = format!("{}:{path}", first.display());
        }
        let tmp = self.dir.path().join("tmp");
        let mut cmd = Command::new("bash");
        cmd.arg(self.dir.path().join("repo/scripts/third-party-notices.sh"))
            .args(args)
            .env("PATH", path)
            .env("TMPDIR", &tmp);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run script");
        let left: Vec<_> = std::fs::read_dir(&tmp)
            .expect("read tmp")
            .map(|e| e.expect("entry").path())
            .collect();
        (out, left)
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_current_file_passes_and_a_stale_one_exits_3() {
    let repo = Repo::current();
    let out = repo.run(&["--check"], &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("up to date"));

    let mut text = std::fs::read_to_string(repo.notices()).expect("read notices");
    text.push_str("an edit by hand\n");
    std::fs::write(repo.notices(), text).expect("write notices");
    let out = repo.run(&["--check"], &[]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("THIRD-PARTY-NOTICES.txt is stale"));
}

#[test]
fn a_check_that_cannot_run_exits_1_even_when_a_tool_exits_3() {
    let repo = Repo::current();
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("cargo fetch fails", &[("STUB_FETCH_RC", "3")]),
        ("cargo-about fails", &[("STUB_ABOUT_RC", "3")]),
        ("bun install fails", &[("STUB_INSTALL_RC", "1")]),
        ("bun install exits 3", &[("STUB_INSTALL_RC", "3")]),
        ("the Vite build fails", &[("STUB_VITE_RC", "3")]),
        ("bun is missing", &[("STUB_INSTALL_RC", "127")]),
        (
            "a local path in the output",
            &[("STUB_WEB_TEXT", "/home/someone/web/node_modules/x")],
        ),
    ];
    for (what, env) in cases {
        let out = repo.run(&["--check"], env);
        assert_eq!(out.status.code(), Some(1), "{what}: {}", stderr(&out));
    }

    let out = repo.run(&["--bogus"], &[]);
    assert_eq!(out.status.code(), Some(1), "a bad argument");

    // A committed file that cannot be compared is not a stale one.
    std::fs::remove_file(repo.notices()).expect("remove notices");
    let out = repo.run(&["--check"], &[]);
    assert_eq!(out.status.code(), Some(1), "missing: {}", stderr(&out));
}

/// Exit 1, never 3: the error of a tool is not read as a stale file.
fn assert_cannot_run(out: &Output, what: &str) {
    assert_eq!(out.status.code(), Some(1), "{what}: {}", stderr(out));
    assert!(!stderr(out).contains("is stale"), "{what}: {}", stderr(out));
}

#[test]
fn a_grep_or_diff_that_fails_exits_1_never_3() {
    let repo = Repo::current();
    let current = std::fs::read_to_string(repo.notices()).expect("read notices");

    // grep exits 1 for no match and 2 when it could not search; diff exits
    // 1 when the files differ and 2 when it could not compare them. Only
    // the 1s are verdicts.
    assert_cannot_run(&repo.run_failing(&["--check"], "grep", 2), "grep, current");
    assert_cannot_run(&repo.run_failing(&["--check"], "diff", 2), "diff, current");

    let stale = format!("{current}an edit by hand\n");
    std::fs::write(repo.notices(), &stale).expect("write notices");
    assert_cannot_run(&repo.run_failing(&["--check"], "grep", 2), "grep, stale");
    assert_cannot_run(&repo.run_failing(&["--check"], "diff", 2), "diff, stale");

    // A write whose path check could not run leaves the committed file alone.
    assert_cannot_run(&repo.run_failing(&[], "grep", 2), "grep, write");
    let left = std::fs::read_to_string(repo.notices()).expect("read notices");
    assert_eq!(left, stale, "the committed file is not rewritten");
}

/// The temporary directory goes on every exit, also when a tool left a
/// directory in it without write permission.
#[test]
fn a_directory_a_tool_left_read_only_is_still_removed() {
    let repo = Repo::current();
    let out = repo.run(&["--check"], &[("STUB_ABOUT_LOCKED", "1")]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

/// A cleanup that fails is reported and fails a check that passed, but
/// never replaces the verdict of one that did not: stale stays 3 and a
/// check that could not run stays 1.
#[test]
fn a_cleanup_that_fails_never_hides_the_verdict() {
    let repo = Repo::current();
    let rm = repo.stand_in("rm", 1);
    let current = std::fs::read_to_string(repo.notices()).expect("read notices");
    let stale = format!("{current}an edit by hand\n");
    for (what, notices, env, code) in [
        ("current", &current, &[][..], 1),
        ("stale", &stale, &[][..], 3),
        ("could not run", &current, &[("STUB_FETCH_RC", "1")][..], 1),
    ] {
        std::fs::write(repo.notices(), notices).expect("write notices");
        let (out, left) = repo.run_leaving(Some(&rm), &["--check"], env);
        assert_eq!(out.status.code(), Some(code), "{what}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("cannot remove the temporary directory"),
            "{what}: {}",
            stderr(&out)
        );
        assert_eq!(left.len(), 1, "{what}: {left:?}");
        for dir in left {
            std::fs::remove_dir_all(dir).expect("remove the left directory");
        }
    }
    std::fs::remove_dir_all(&rm).expect("remove fail dir");
}
