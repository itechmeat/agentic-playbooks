//! `apb suggestions list|allow|reset` against a temp global config dir and a
//! temp project, driving the real binary the way the other CLI suites do.
//!
//! Records are seeded by writing the on-disk store files directly (schema 2
//! shape, `suggestions.json`, mirroring `apb_core::dismiss`'s own `StoreFile`/
//! `SuggestionRecord` field names) rather than by calling
//! `apb_core::dismiss::record_decision` in process: that function resolves
//! the global-scope directory from the `APB_CONFIG_DIR` process env var at
//! call time, and this file's tests run concurrently, in the same test
//! binary, with every other suite in `tests/main.rs` that spawns `apb` -
//! mutating that env var in-process, even briefly and even under a local
//! lock scoped to this file alone, cannot protect those other suites' spawns
//! from observing it. Writing the store files directly needs no process env
//! mutation at all; the spawned `apb` process gets its config dir the same
//! way every other suite's does, via `Command::env`.

use std::path::Path;
use std::process::Command;

fn apb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_apb")
}

fn run(cfg: &Path, cwd: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(apb_bin())
        .args(["suggestions"])
        .args(args)
        .current_dir(cwd)
        .env("APB_CONFIG_DIR", cfg)
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .output()
        .expect("run apb suggestions");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

const MS_PER_DAY: u64 = 24 * 60 * 60 * 1000;

/// Writes one `suggestions.json` (schema 2 shape) directly into `dir`,
/// creating the directory if absent. `dir` is the project's `.apb` for
/// project scope, or the global config dir itself for global scope
/// (`apb_core::dismiss::store_dir`'s exact resolution) - never a subdirectory
/// derived from the process environment.
fn write_store(dir: &Path, records: Vec<serde_json::Value>) {
    std::fs::create_dir_all(dir).unwrap();
    let file = serde_json::json!({ "schema": 2, "records": records });
    std::fs::write(
        dir.join("suggestions.json"),
        serde_json::to_vec_pretty(&file).unwrap(),
    )
    .unwrap();
}

/// One `SuggestionRecord` as JSON, already-active (`snoozed_in_days` days out
/// from now, so `apb_core::dismiss::active` still suppresses it at read time).
fn record(
    pattern: &str,
    synopsis: &str,
    kind: &str,
    declines: u32,
    snoozed_in_days: u64,
) -> serde_json::Value {
    let now = apb_core::clock::now_ms_u64();
    serde_json::json!({
        "pattern": pattern,
        "synopsis": synopsis,
        "kind": kind,
        "declines": declines,
        "snoozed_until_ms": now + snoozed_in_days * MS_PER_DAY,
        "updated_at_ms": now,
    })
}

#[test]
fn list_shows_both_scopes_as_a_table_and_as_json() {
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(proj.path()).unwrap();

    write_store(
        &proj.path().join(".apb"),
        vec![record("code-review-run", "Review a file", "soft", 1, 1)],
    );
    write_store(
        cfg.path(),
        vec![record("never-anywhere", "Never offer this", "hard", 0, 90)],
    );

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["list"]);
    assert!(ok, "list failed: {stderr}");
    assert!(stdout.contains("PATTERN"), "no table header: {stdout}");
    assert!(stdout.contains("code-review-run"), "{stdout}");
    assert!(stdout.contains("never-anywhere"), "{stdout}");
    assert!(stdout.contains("project"), "{stdout}");
    assert!(stdout.contains("global"), "{stdout}");
    assert!(stdout.contains("Review a file"), "{stdout}");

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["list", "--json"]);
    assert!(ok, "list --json failed: {stderr}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("json output");
    let rows = parsed["suggestions"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{parsed}");
    let soft = rows
        .iter()
        .find(|r| r["pattern"] == "code-review-run")
        .unwrap();
    assert_eq!(soft["kind"], "soft");
    assert_eq!(soft["scope"], "project");
    assert_eq!(soft["declines"], 1);
    assert!(soft["snoozed_until"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn allow_removes_a_record_in_the_requested_scope() {
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(proj.path()).unwrap();

    write_store(
        &proj.path().join(".apb"),
        vec![record("in-project", "Project one", "hard", 0, 90)],
    );
    write_store(
        cfg.path(),
        vec![record("in-global", "Global one", "hard", 0, 90)],
    );

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["allow", "in-project"]);
    assert!(ok, "allow failed: {stderr}");
    assert!(stdout.contains("in-project"), "{stdout}");

    // Without --global the global record is untouched, and the command says so.
    let (_out, _err, ok) = run(cfg.path(), proj.path(), &["allow", "in-global"]);
    assert!(
        !ok,
        "removing a global record without --global must not report success"
    );

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["allow", "in-global", "--global"]);
    assert!(ok, "allow --global failed: {stderr}");
    assert!(stdout.contains("in-global"), "{stdout}");

    let (stdout, _err, ok) = run(cfg.path(), proj.path(), &["list", "--json"]);
    assert!(ok);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        parsed["suggestions"].as_array().unwrap().len(),
        0,
        "{parsed}"
    );
}

/// The same pattern can live in both stores, and `allow` only ever removes the
/// scope it was asked for. Promising "the suggestion can be offered again"
/// while the other scope still silences it is simply false, so the command has
/// to say what is left and which follow-up clears it.
#[test]
fn allow_says_when_the_other_scope_still_silences_the_suggestion() {
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(proj.path()).unwrap();

    write_store(
        &proj.path().join(".apb"),
        vec![record("both-scopes", "In both stores", "hard", 0, 90)],
    );
    write_store(
        cfg.path(),
        vec![record("both-scopes", "In both stores", "hard", 0, 90)],
    );

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["allow", "both-scopes"]);
    assert!(ok, "allow failed: {stderr}");
    assert!(
        !stdout.contains("can be offered again"),
        "the global record still silences the offer, so this must not promise a re-offer: {stdout}"
    );
    assert!(
        stdout.contains("global"),
        "the surviving scope must be named: {stdout}"
    );
    assert!(
        stdout.contains("apb suggestions allow both-scopes --global"),
        "the follow-up command must be spelled out: {stdout}"
    );

    // Once the global record is gone too, the offer really is back.
    let (stdout, stderr, ok) = run(
        cfg.path(),
        proj.path(),
        &["allow", "both-scopes", "--global"],
    );
    assert!(ok, "allow --global failed: {stderr}");
    assert!(
        stdout.contains("can be offered again"),
        "nothing suppresses the pattern now: {stdout}"
    );
}

#[test]
fn allow_on_a_fresh_project_leaves_no_apb_dir_behind() {
    // A store-less root: `allow` must report "nothing to remove" without
    // materializing `.apb`, which elsewhere in apb is an adoption marker
    // (registry auto-registration, "already initialized" checks).
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();

    let (_stdout, _stderr, ok) = run(cfg.path(), proj.path(), &["allow", "nope"]);
    assert!(!ok, "allow of an absent pattern must not report success");
    assert!(
        !proj.path().join(".apb").exists(),
        "apb suggestions allow on a fresh root must not create .apb"
    );
}

#[test]
fn reset_clears_soft_records_and_refuses_hard_ones() {
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(proj.path()).unwrap();

    write_store(
        &proj.path().join(".apb"),
        vec![
            record("soft-one", "Soft one", "soft", 1, 1),
            record("soft-two", "Soft two", "soft", 1, 1),
            record("hard-one", "Hard one", "hard", 0, 90),
        ],
    );

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["reset", "hard-one"]);
    assert!(!ok, "resetting a hard record is refused: {stdout} {stderr}");
    assert!(
        stdout.contains("apb suggestions allow") || stderr.contains("apb suggestions allow"),
        "the refusal must point at allow: {stdout} {stderr}"
    );

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["reset", "soft-one"]);
    assert!(ok, "reset failed: {stderr}");
    assert!(stdout.contains("soft-one"), "{stdout}");

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["reset", "--all"]);
    assert!(ok, "reset --all failed: {stderr}");
    assert!(stdout.contains("soft-two"), "{stdout}");

    // Both soft records are inactive now; the hard one still suppresses.
    let (stdout, _err, ok) = run(cfg.path(), proj.path(), &["list", "--json"]);
    assert!(ok);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let rows = parsed["suggestions"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{parsed}");
    assert_eq!(rows[0]["pattern"], "hard-one");
}

#[test]
fn reset_rejects_a_pattern_together_with_all_and_mutates_nothing() {
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(proj.path()).unwrap();

    write_store(
        &proj.path().join(".apb"),
        vec![record("soft-one", "Soft one", "soft", 1, 1)],
    );
    let store_path = proj.path().join(".apb/suggestions.json");
    let before = std::fs::read_to_string(&store_path).unwrap();

    let (stdout, stderr, ok) = run(cfg.path(), proj.path(), &["reset", "soft-one", "--all"]);
    assert!(
        !ok,
        "a pattern together with --all must be a usage error: {stdout} {stderr}"
    );
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("conflicts"),
        "expected a clap usage error naming the conflict: {stderr}"
    );

    let after = std::fs::read_to_string(&store_path).unwrap();
    assert_eq!(
        before, after,
        "a rejected `reset <pattern> --all` must not touch the store"
    );
}
