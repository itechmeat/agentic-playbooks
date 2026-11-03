//! Regression guard: a test run must never write the developer's real global
//! config dir (`~/.config/apb`). It once did: every `apb` command spawned
//! inside a seeded tempdir auto-registered that tempdir in the real
//! `projects.json`, about fifty `/tmp/.tmp*` entries per `cargo test`.
//!
//! Two checks: every spawn of the binary in this crate's tests goes through
//! the sandboxing helpers in `common.rs`, and those helpers really keep a
//! registration out of the real config dir.

use std::path::{Path, PathBuf};

/// Spellings that spawn (or name, for spawning) the binary under test while
/// bypassing `common.rs`. Built with `concat!` so this file does not match
/// itself.
const BYPASSES: [&str; 2] = [concat!("CARGO_BIN_EXE", "_apb"), concat!("cargo", "_bin(")];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_apb_spawn_goes_through_the_sandbox_helper() {
    let tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut files = Vec::new();
    rust_files(&tests_dir, &mut files);
    let mut offenders = Vec::new();
    for file in files {
        if file.ends_with("suite/common.rs") {
            continue;
        }
        let src = std::fs::read_to_string(&file).unwrap();
        for (n, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if BYPASSES.iter().any(|b| line.contains(b)) {
                offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "spawn `apb` through crate::common::apb()/apb_std() (they sandbox \
         APB_CONFIG_DIR), not directly:\n{}",
        offenders.join("\n")
    );
}

/// The real global config dirs, as the suite's isolation hook recorded them
/// before it redirected `XDG_CONFIG_HOME` (`apb_core::config::test_isolation`).
fn real_config_dirs() -> Vec<PathBuf> {
    std::env::var_os(apb_core::config::test_isolation::FORBIDDEN_ENV)
        .map(|l| std::env::split_paths(&l).collect())
        .unwrap_or_default()
}

fn registered_paths(projects_json: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(projects_json) else {
        return Vec::new();
    };
    let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
    doc["entries"]
        .as_object()
        .map(|m| {
            m.values()
                .filter_map(|e| e["path"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The registry stores canonical paths, so the expected entry is the
/// canonical project path, not the spelling the command ran under. On macOS
/// the temp dir itself sits behind a symlink (`/var` -> `/private/var`); on
/// unix the project is entered through a symlink here so a comparison against
/// the raw spelling fails on every platform, not only on macOS.
#[test]
fn sandboxed_apb_registers_into_the_sandbox_not_the_real_config() {
    let dir = tempfile::tempdir().unwrap();
    let project = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    #[cfg(unix)]
    let (_links, cwd) = {
        let links = tempfile::tempdir().unwrap();
        let link = links.path().join("project");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        (links, link)
    };
    #[cfg(not(unix))]
    let cwd = dir.path().to_path_buf();
    crate::common::apb()
        .arg("init")
        .current_dir(&cwd)
        .assert()
        .success();
    // Registration is off under CI; force it on so the check means something
    // there too.
    crate::common::apb()
        .arg("list")
        .current_dir(&cwd)
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .assert()
        .success();

    let sandbox = crate::common::sandbox_config_dir().join("projects.json");
    assert!(
        registered_paths(&sandbox).contains(&project),
        "the sandbox registry should hold {project}"
    );
    for real in real_config_dirs() {
        assert_ne!(real, crate::common::sandbox_config_dir());
        assert!(
            !registered_paths(&real.join("projects.json")).contains(&project),
            "{project} leaked into the real registry {}",
            real.display()
        );
    }
}

/// The isolation hook ran in this test process, and a test-build `apb` that
/// still resolves a forbidden (real) config dir fails loudly instead of
/// writing there: the same panic every test process gets from
/// `apb_core::config::config_dir`.
#[test]
fn a_spawn_that_reaches_the_real_config_dir_fails_loudly() {
    assert!(
        !real_config_dirs().is_empty(),
        "the suite's isolation hook did not record the real config dir"
    );
    let forbidden = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let out = crate::common::apb_std()
        .arg("list")
        .current_dir(project.path())
        .env(
            apb_core::config::test_isolation::FORBIDDEN_ENV,
            forbidden.path(),
        )
        .env("APB_CONFIG_DIR", forbidden.path().join("apb"))
        .env_remove("CI")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the leak was not refused: {stderr}");
    assert!(stderr.contains("test isolation"), "{stderr}");
    assert!(
        !forbidden.path().join("apb/projects.json").exists(),
        "the forbidden config dir was written"
    );
}

/// The crates of this workspace whose Cargo.toml depends on `apb-core`.
/// Crates without that dependency cannot install the hook (`isolate_test_config!`
/// comes from apb-core), so they are skipped; `apb-decide` is one.
fn core_dependent_crates() -> Vec<PathBuf> {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates")
        .canonicalize()
        .expect("the workspace crates dir");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&crates_dir).unwrap().flatten() {
        let manifest = entry.path().join("Cargo.toml");
        if entry.path().is_dir()
            && std::fs::read_to_string(&manifest)
                .map(|t| t.contains("apb-core"))
                .unwrap_or(false)
        {
            out.push(entry.path());
        }
    }
    out.sort();
    out
}

/// Roots that install the isolation hook: every top-level `tests/*.rs` file
/// (each is its own test binary) and every lib/bin root of a crate depending
/// on apb-core. A new test binary without the hook would read the developer's
/// real config dir, so the scan refuses it. String scan only: fast and
/// deterministic, no parsing.
#[test]
fn every_test_binary_and_core_user_installs_the_isolation_hook() {
    let mut offenders = Vec::new();
    for crate_dir in core_dependent_crates() {
        let name = crate_dir.file_name().unwrap().to_string_lossy();
        let tests = crate_dir.join("tests");
        if tests.is_dir() {
            for entry in std::fs::read_dir(&tests).unwrap().flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "rs") {
                    let src = std::fs::read_to_string(&path).unwrap();
                    if !src.contains("test_isolation") {
                        offenders.push(format!(
                            "{}: no isolate_test_config hook in a test binary of {name}",
                            path.display()
                        ));
                    }
                }
            }
        }
        for root in [crate_dir.join("src/lib.rs"), crate_dir.join("src/main.rs")] {
            if root.is_file() {
                let src = std::fs::read_to_string(&root).unwrap();
                if !src.contains("test_isolation") {
                    offenders.push(format!(
                        "{}: no isolate_test_config hook in a lib/bin root of {name}",
                        root.display()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "every test binary and every lib/bin root of a crate depending on \
         apb-core must install the config isolation hook \
         (apb_core::isolate_test_config or config::test_isolation::isolate):\n{}",
        offenders.join("\n")
    );
}
