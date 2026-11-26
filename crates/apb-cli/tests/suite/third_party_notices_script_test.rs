//! `scripts/third-party-notices.sh --check` tells a stale file apart from a
//! script that could not run: exit 3 only when the generated notices differ
//! from the committed file, exit 1 for every other failure, a tool that
//! itself exits 3 included. CI's lint job lets exit 3 pass as a warning on a
//! Dependabot pull request, so a failure to run must never read as 3.
//!
//! The real script runs in a scratch repository with stand-ins for `cargo`
//! and `bun` on the PATH, each failure injected through an environment
//! variable.

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
        let path = format!(
            "{}:{}",
            self.dir.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
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
        assert!(left.is_empty(), "the work dir is removed on exit: {left:?}");
        out
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
