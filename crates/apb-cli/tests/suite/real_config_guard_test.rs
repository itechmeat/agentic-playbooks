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

/// The global config dir the binary would resolve with no `APB_CONFIG_DIR`,
/// mirroring `apb_core::config::config_dir`'s fallbacks.
fn real_config_dir() -> Option<PathBuf> {
    let non_empty = |k| std::env::var(k).ok().filter(|v: &String| !v.is_empty());
    non_empty("XDG_CONFIG_HOME")
        .map(|x| PathBuf::from(x).join("apb"))
        .or_else(|| non_empty("HOME").map(|h| PathBuf::from(h).join(".config/apb")))
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
    if let Some(real) = real_config_dir() {
        assert_ne!(real, crate::common::sandbox_config_dir());
        assert!(
            !registered_paths(&real.join("projects.json")).contains(&project),
            "{project} leaked into the real registry {}",
            real.display()
        );
    }
}
