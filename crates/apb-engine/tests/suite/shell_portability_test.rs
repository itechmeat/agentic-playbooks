//! Regression guard: shell snippets in the workspace's tests must run on
//! macOS too. Two engine tests once edited a file from a fake agent with
//! `sed -i 's/.../'`: GNU sed (Linux) accepts that, BSD sed (macOS) reads the
//! script as `-i`'s backup suffix and fails, so the release gate on the macOS
//! runners failed while every Linux leg passed. Edit with `sed ... > f.tmp &&
//! mv f.tmp f` instead.

use std::path::{Path, PathBuf};

/// GNU-only spellings, built with `concat!` so this file does not match
/// itself.
const GNU_ONLY: [&str; 2] = [concat!("sed", " -i"), concat!("sed", " --in-place")];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_test_uses_gnu_only_sed_in_place() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for krate in std::fs::read_dir(&crates).unwrap().flatten() {
        rust_files(&krate.path().join("tests"), &mut files);
    }
    assert!(!files.is_empty(), "no test sources found under {crates:?}");
    let mut offenders = Vec::new();
    for file in files {
        let src = std::fs::read_to_string(&file).unwrap();
        for (n, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if GNU_ONLY.iter().any(|g| line.contains(g)) {
                offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "in-place sed is GNU-only (BSD sed on macOS parses it differently); \
         write to a temp file and rename instead:\n{}",
        offenders.join("\n")
    );
}
