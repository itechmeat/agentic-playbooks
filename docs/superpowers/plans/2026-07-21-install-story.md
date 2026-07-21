# Install Story (v0.9.0) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship dist-based install paths (shell one-liner, Homebrew tap, apb self-update), an interactive clack-style `apb init` questionnaire, and a full refresh of the install documentation, released as v0.9.0.

**Architecture:** dist (cargo-dist 0.32.0) generates the release workflow, shell installer, and Homebrew formula from `dist-workspace.toml`; our test gate rides `plan-jobs`, the bun web build rides `github-build-setup`, release notes ride `post-announce-jobs`. The CLI gains two features in `apb-cli`: a cliclack questionnaire inside `apb init` (feedback-loop consent plus the existing subscriptions survey re-skinned) and an axoupdater-backed `apb self-update`.

**Tech Stack:** Rust (edition 2024), dist 0.32.0, axoupdater 0.10.x (blocking feature, no tokio), cliclack 0.5.x, GitHub Actions, bun (web build).

## Global Constraints

- No em-dashes (U+2014), no exclamation marks, no CJK in docs or user-facing strings. Machine-facing fields are English.
- Every commit: `git commit --signoff` (DCO) plus the acting model's `Co-Authored-By` trailer.
- Formatting gates must stay clean: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`.
- Homebrew tap: `itechmeat/homebrew-agentic-playbooks`; Actions secret name: `HOMEBREW_TAP_TOKEN`; user command: `brew install itechmeat/agentic-playbooks/apb`.
- dist config file is `dist-workspace.toml` at the repo root with `cargo-dist-version = "0.32.0"`. Targets, exactly: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`.
- The feedback-loop block's canonical marker heading is `## apb feedback loop (standing instruction)`; idempotency checks look for the prefix `## apb feedback loop`.
- `apb self-update` exit codes: 0 = updated or already current; with `--check`: 0 = up to date, 10 = update available; 2 = any failure including no install receipt.
- Interactive prompts run only when BOTH stdin and stdout are terminals (`std::io::IsTerminal`); non-TTY `apb init` must behave byte-identically to today (same stdout line, same exit codes).
- Cancellation (Esc or Ctrl+C) surfaces from cliclack as `io::ErrorKind::Interrupted`; handle it with `cliclack::outro_cancel` and exit code 0, leaving everything already written valid.
- No live network calls in any test.
- Crate versions in this plan (dist 0.32.0, axoupdater 0.10, cliclack 0.5) were verified 2026-07-21; re-confirm each is still the latest stable when adding it and use the newer patch/minor if one exists.

## File Structure

- `crates/apb-cli/Cargo.toml` - package renamed to `apb` (directory stays `crates/apb-cli`), new deps cliclack + axoupdater.
- `crates/apb-cli/assets/feedback-loop.md` - canonical feedback-loop block (single source of truth, `include_str!`).
- `crates/apb-cli/src/onboarding.rs` - feedback-loop file logic (pure, tested) plus the cliclack questionnaire (thin shell).
- `crates/apb-cli/src/selfupdate.rs` - `apb self-update` implementation.
- `crates/apb-cli/src/manage.rs` - survey extraction: parsing/storage split from presentation; cliclack presentation replaces the hand-rolled prompt everywhere.
- `dist-workspace.toml`, `.github/build-setup.yml`, `.github/workflows/test-gate.yml`, `.github/workflows/release-notes.yml`, regenerated `.github/workflows/release.yml`.
- Docs: `docs/INSTALL.md`, `README.md`, `llms.txt`, `CLAUDE.md`, `AGENTS.md`, `docs/release-notes/v0.9.0.md`; `packaging/apb.rb` deleted.

---

### Task 1: Rename package apb-cli to apb

The dist app name, installer asset name (`apb-installer.sh`), Homebrew formula default, and axoupdater receipt dir (`~/.config/apb/`) all derive from the package name. Renaming the package (not the directory) makes every derived name clean without unverified override keys.

**Files:**
- Modify: `crates/apb-cli/Cargo.toml` (package name)
- Modify: every file referencing the package name `apb-cli` in cargo commands or prose (found via grep below)

**Interfaces:**
- Produces: package `apb` (bin `apb`, unchanged), directory still `crates/apb-cli`. Later tasks use `-p apb` in cargo commands.

- [ ] **Step 1: Rename the package**

In `crates/apb-cli/Cargo.toml` change:

```toml
[package]
name = "apb"
```

(keep everything else; the `[[bin]] name = "apb"` section stays as is).

- [ ] **Step 2: Find and update all references**

Run: `grep -rn "apb-cli" --include="*.toml" --include="*.yml" --include="*.yaml" --include="*.md" --include="*.rs" --include="*.txt" . | grep -v target | grep -v "crates/apb-cli/"` and also `grep -rn "\-p apb-cli\|cargo uninstall apb-cli" .`

Update every cargo-command reference: `-p apb-cli` becomes `-p apb`, `cargo uninstall apb-cli` becomes `cargo uninstall apb`. Path references (`crates/apb-cli`, `cargo install --path crates/apb-cli`) stay unchanged. In prose (CLAUDE.md, AGENTS.md crate list), keep describing the crate by its directory `apb-cli` but note the package name is `apb`; apply the mirror rule (CLAUDE.md and AGENTS.md updated identically). The old `.github/workflows/release.yml` also contains `-p apb-cli`; update it too so the tree stays consistent until Task 5 replaces the file.

- [ ] **Step 3: Verify the workspace builds and tests pass**

Run: `cargo build --workspace && cargo nextest run -p apb`
Expected: builds clean; apb-cli test suite passes (binary env var `CARGO_BIN_EXE_apb` is unaffected because the bin name never changed).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit --signoff -m "refactor: rename package apb-cli to apb for clean dist-derived names"
```

---

### Task 2: Feedback-loop asset and file logic

Pure, tested logic for writing the feedback-loop block into `CLAUDE.md` / `AGENTS.md`. No prompts in this task.

**Files:**
- Create: `crates/apb-cli/assets/feedback-loop.md`
- Create: `crates/apb-cli/src/onboarding.rs` (module registered in `main.rs`)
- Test: unit tests inside `onboarding.rs` (tempdir based)

**Interfaces:**
- Produces: `pub(crate) const FEEDBACK_BLOCK: &str` (include_str of the asset); `pub(crate) enum FeedbackAction { Created, Appended, AlreadyConfigured }`; `pub(crate) fn apply_feedback_loop(dir: &Path) -> std::io::Result<Vec<(String, FeedbackAction)>>` returning one entry per file name (`CLAUDE.md`, `AGENTS.md`); `pub(crate) fn feedback_loop_fully_configured(dir: &Path) -> bool` (true when both files exist and contain the marker).

- [ ] **Step 1: Create the asset from the README (single source of truth)**

Copy the inner markdown block from `README.md` section `### Help apb improve: the feedback loop` verbatim into `crates/apb-cli/assets/feedback-loop.md`. It starts with the line `## apb feedback loop (standing instruction)` and ends with `content in an issue.` (the content of the fenced ```markdown block, without the fence lines). Ensure the file ends with a single trailing newline.

- [ ] **Step 2: Write failing tests**

In `crates/apb-cli/src/onboarding.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn creates_both_files_when_missing() {
        let dir = tmp();
        let out = apply_feedback_loop(dir.path()).unwrap();
        assert_eq!(out.len(), 2);
        for (name, action) in &out {
            assert!(matches!(action, FeedbackAction::Created), "{name}");
            let text = fs::read_to_string(dir.path().join(name)).unwrap();
            assert_eq!(text, FEEDBACK_BLOCK);
        }
    }

    #[test]
    fn appends_to_existing_file_with_separator() {
        let dir = tmp();
        fs::write(dir.path().join("CLAUDE.md"), "# My project\n").unwrap();
        let out = apply_feedback_loop(dir.path()).unwrap();
        let claude = out.iter().find(|(n, _)| n == "CLAUDE.md").unwrap();
        assert!(matches!(claude.1, FeedbackAction::Appended));
        let text = fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(text.starts_with("# My project\n"));
        assert!(text.contains("\n\n## apb feedback loop (standing instruction)"));
    }

    #[test]
    fn rerun_is_idempotent() {
        let dir = tmp();
        apply_feedback_loop(dir.path()).unwrap();
        let before = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        let out = apply_feedback_loop(dir.path()).unwrap();
        for (_, action) in &out {
            assert!(matches!(action, FeedbackAction::AlreadyConfigured));
        }
        let after = fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn fully_configured_detection() {
        let dir = tmp();
        assert!(!feedback_loop_fully_configured(dir.path()));
        apply_feedback_loop(dir.path()).unwrap();
        assert!(feedback_loop_fully_configured(dir.path()));
    }

    #[test]
    fn readme_and_asset_do_not_drift() {
        let readme = include_str!("../../../README.md");
        for line in FEEDBACK_BLOCK.lines() {
            assert!(
                readme.contains(line),
                "README lost a feedback-loop line: {line}"
            );
        }
    }
}
```

Add `tempfile` to `[dev-dependencies]` in `crates/apb-cli/Cargo.toml` if it is not already there (check first; other crates in the workspace already use it).

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo nextest run -p apb onboarding`
Expected: FAIL (module/functions do not exist yet).

- [ ] **Step 4: Implement**

```rust
use std::io;
use std::path::Path;

pub(crate) const FEEDBACK_BLOCK: &str = include_str!("../assets/feedback-loop.md");
const MARKER: &str = "## apb feedback loop";
const TARGET_FILES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

pub(crate) enum FeedbackAction {
    Created,
    Appended,
    AlreadyConfigured,
}

pub(crate) fn apply_feedback_loop(dir: &Path) -> io::Result<Vec<(String, FeedbackAction)>> {
    let mut out = Vec::new();
    for name in TARGET_FILES {
        let path = dir.join(name);
        let action = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            if text.contains(MARKER) {
                FeedbackAction::AlreadyConfigured
            } else {
                let sep = if text.ends_with("\n\n") {
                    ""
                } else if text.ends_with('\n') {
                    "\n"
                } else {
                    "\n\n"
                };
                std::fs::write(&path, format!("{text}{sep}{FEEDBACK_BLOCK}"))?;
                FeedbackAction::Appended
            }
        } else {
            std::fs::write(&path, FEEDBACK_BLOCK)?;
            FeedbackAction::Created
        };
        out.push((name.to_string(), action));
    }
    Ok(out)
}

pub(crate) fn feedback_loop_fully_configured(dir: &Path) -> bool {
    TARGET_FILES.iter().all(|name| {
        std::fs::read_to_string(dir.join(name))
            .map(|t| t.contains(MARKER))
            .unwrap_or(false)
    })
}
```

Register `mod onboarding;` in `crates/apb-cli/src/main.rs`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p apb onboarding`
Expected: PASS (all 5).

- [ ] **Step 6: Commit**

```bash
git add crates/apb-cli
git commit --signoff -m "feat: canonical feedback-loop block with idempotent CLAUDE.md/AGENTS.md writer"
```

---

### Task 3: Interactive init questionnaire

cliclack questionnaire wired into `apb init`, plus the existing subscriptions survey split into logic (kept) and presentation (re-skinned with cliclack, used from init and from the old call sites).

**Files:**
- Modify: `crates/apb-cli/Cargo.toml` (add `cliclack = "0.5"`; re-confirm latest 0.5.x)
- Modify: `crates/apb-cli/src/onboarding.rs` (questionnaire shell)
- Modify: `crates/apb-cli/src/manage.rs` (survey split: extract the storage step of `interactive_survey`, manage.rs:197-264, into a reusable `record_subscription`-style function; replace the hand-rolled prompt loop with the cliclack survey)
- Modify: `crates/apb-cli/src/main.rs` (init dispatch calls the questionnaire after `run_init`)
- Test: `crates/apb-cli/tests/init_interactive_test.rs` (non-TTY behavior)

**Interfaces:**
- Consumes: `apply_feedback_loop`, `feedback_loop_fully_configured`, `FeedbackAction` from Task 2; existing `run_init` (manage.rs:287-298), `interactive_survey` internals (manage.rs:197-264), `agents_detect`.
- Produces: `pub(crate) fn run_init_questionnaire(root: &Path)` called from init when TTY; cliclack-based `subscriptions_survey()` used by init and by the existing offer sites in `manage.rs`/`profile.rs`.

- [ ] **Step 1: Write the failing non-TTY integration test**

`crates/apb-cli/tests/init_interactive_test.rs`:

```rust
use std::process::Command;

#[test]
fn init_with_piped_stdio_stays_noninteractive() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("init")
        .current_dir(dir.path())
        .env("APB_NO_REGISTRY", "1")
        .stdin(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("initialized"));
    // questionnaire must not have run: no consent files written
    assert!(!dir.path().join("CLAUDE.md").exists());
    assert!(!dir.path().join("AGENTS.md").exists());
    assert!(dir.path().join(".apb/config.yaml").exists());
}

#[test]
fn init_rerun_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let out = Command::new(env!("CARGO_BIN_EXE_apb"))
            .arg("init")
            .current_dir(dir.path())
            .env("APB_NO_REGISTRY", "1")
            .stdin(std::process::Stdio::piped())
            .output()
            .unwrap();
        assert!(out.status.success());
    }
}
```

- [ ] **Step 2: Run the test; it should already pass for the first assertions (init is non-interactive today) but treat it as the safety net for the change**

Run: `cargo nextest run -p apb init_interactive`
Expected: PASS now; it must STILL pass after wiring the questionnaire.

- [ ] **Step 3: Extract survey storage from presentation in manage.rs**

In `manage.rs`, split `interactive_survey` (lines 197-264): keep the parsing/storage of one entry (`agent[:plan[:coverage]]` handling and whatever state write it performs, lines 232-249) as a private function, e.g. `fn record_survey_entry(...)` with the same arguments the loop body uses today. The old stdin loop is deleted; the new cliclack presentation (Step 4) is the only caller. Do not change what gets stored or its format.

- [ ] **Step 4: Implement the cliclack survey and questionnaire**

In `onboarding.rs` (presentation; exact copy below, adjust only if the cliclack 0.5 API differs on compile):

```rust
use std::io::IsTerminal;

pub(crate) fn run_init_questionnaire(root: &std::path::Path) {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return;
    }
    if let Err(e) = questionnaire(root) {
        if e.kind() == std::io::ErrorKind::Interrupted {
            let _ = cliclack::outro_cancel("Setup cancelled. Run apb init again anytime.");
        } else {
            eprintln!("init questionnaire failed: {e}");
        }
    }
}

fn questionnaire(root: &std::path::Path) -> std::io::Result<()> {
    cliclack::intro(" apb init ")?;
    if feedback_loop_fully_configured(root) {
        cliclack::log::success("Feedback loop already configured in CLAUDE.md and AGENTS.md")?;
    } else {
        let consent = cliclack::confirm(
            "Allow coding agents to report apb errors after playbook runs? \
             Anonymized, consolidated issues are filed transparently at \
             https://github.com/itechmeat/agentic-playbooks (no secrets, no private prompts)",
        )
        .initial_value(true)
        .interact()?;
        if consent {
            for (name, action) in apply_feedback_loop(root)? {
                match action {
                    FeedbackAction::Created => {
                        cliclack::log::success(format!("{name} created with the apb feedback loop section"))?
                    }
                    FeedbackAction::Appended => {
                        cliclack::log::success(format!("{name} updated with the apb feedback loop section"))?
                    }
                    FeedbackAction::AlreadyConfigured => {
                        cliclack::log::info(format!("{name} already configured"))?
                    }
                }
            }
        } else {
            cliclack::log::info("Skipped. You can add the section later from the README")?;
        }
    }
    crate::manage::subscriptions_survey_step()?;
    cliclack::outro("Project ready. Try: apb --help")?;
    Ok(())
}
```

In `manage.rs`, implement `pub(crate) fn subscriptions_survey_step() -> std::io::Result<()>` with cliclack primitives: gate exactly like today (only when onboarding state is `Uninitialized`, mirroring manage.rs:182-188; otherwise return Ok immediately); show detected agents via `agents_detect` as a `cliclack::multiselect` (item per detected agent, hint = detected version/path if available); for each selected agent, `cliclack::input` for optional plan and optional coverage with placeholders matching the `agent[:plan[:coverage]]` semantics; store each entry via the Step 3 extracted function. Replace the old survey invocation at the existing offer sites (`subscriptions_cmd`, `profile.rs:91`, `profile.rs:346`) with this function so there is one survey implementation. Keep the TTY gates those sites already have.

Wire init in `main.rs`: after a successful `run_init(&root)`, call `onboarding::run_init_questionnaire(&root)`. Init's exit code logic does not change; questionnaire failures never turn a successful init into a failure.

- [ ] **Step 5: Run the full crate suite plus manual smoke**

Run: `cargo nextest run -p apb && cargo build -p apb`
Expected: PASS including Task 2 and Step 1 tests.
Manual smoke (implementer, in a scratch dir): run the debug binary `apb init` in a real terminal; verify the intro frame, default-Yes confirm, created files, re-run showing "already configured", and Esc producing the cancel outro with exit code 0 (`echo $?`).

- [ ] **Step 6: Commit**

```bash
git add crates/apb-cli
git commit --signoff -m "feat: interactive clack-style apb init questionnaire"
```

---

### Task 4: apb self-update

**Files:**
- Modify: `crates/apb-cli/Cargo.toml` (add `axoupdater` with the `blocking` feature, no tokio; re-confirm 0.10.x is latest on crates.io)
- Create: `crates/apb-cli/src/selfupdate.rs`
- Modify: `crates/apb-cli/src/main.rs` (clap subcommand + dispatch)
- Test: unit tests in `selfupdate.rs` plus CLI test in `crates/apb-cli/tests/init_interactive_test.rs` sibling file or existing CLI test file

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: `SelfUpdate { check: bool }` clap variant; `pub(crate) fn run_self_update(check: bool) -> std::process::ExitCode`.

- [ ] **Step 1: Write failing tests**

In `selfupdate.rs` tests: point axoupdater at an empty config dir so `load_receipt` fails deterministically, and assert exit codes and messages:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_receipt_maps_to_guidance_and_code_2() {
        let dir = tempfile::tempdir().unwrap();
        // axoupdater resolves the receipt under XDG config; point it at an empty dir
        let out = self_update_with_config_root(false, Some(dir.path()));
        assert_eq!(out.code, 2);
        assert!(out.message.contains("brew upgrade") || out.message.contains("source"));
    }

    #[test]
    fn check_flag_is_parsed() {
        // covered by clap derive; assert the CLI accepts it
        use clap::Parser;
        let cli = crate::Cli::try_parse_from(["apb", "self-update", "--check"]).unwrap();
        // match the variant produced
        let _ = cli;
    }
}
```

Shape the implementation so the receipt location is injectable: `fn self_update_with_config_root(check: bool, config_root: Option<&Path>) -> Outcome` where `Outcome { code: u8, message: String }`; the public `run_self_update` calls it with `None` and prints/exits. Use axoupdater's API to point at a custom receipt path if it exposes one (`load_receipt_as` / config helpers); if it only honors environment overrides, set the relevant env var inside the test process instead and adjust the helper accordingly.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p apb selfupdate`
Expected: FAIL (module missing).

- [ ] **Step 3: Implement**

```rust
use axoupdater::{AxoUpdater, AxoupdateError};
use std::process::ExitCode;

pub(crate) fn run_self_update(check: bool) -> ExitCode {
    let mut updater = AxoUpdater::new_for("apb");
    match updater.load_receipt() {
        Err(AxoupdateError::NoReceipt) => {
            eprintln!(
                "self-update only works for installs made by the apb installer. \
                 If you installed with Homebrew, run: brew upgrade apb. \
                 If you built from source, rebuild and reinstall from the repo."
            );
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("self-update failed: {e}");
            return ExitCode::from(2);
        }
        Ok(_) => {}
    }
    if check {
        match updater.is_update_needed_sync() {
            Ok(true) => {
                println!("update available");
                ExitCode::from(10)
            }
            Ok(false) => {
                println!("apb is up to date");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("self-update check failed: {e}");
                ExitCode::from(2)
            }
        }
    } else {
        match updater.run_sync() {
            Ok(Some(result)) => {
                println!("updated to {}", result.new_version);
                ExitCode::SUCCESS
            }
            Ok(None) => {
                println!("apb is up to date");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("self-update failed: {e}");
                ExitCode::from(2)
            }
        }
    }
}
```

Adjust to the crate's actual 0.10 signatures on compile (the `run_sync` return type is `AxoupdateResult<Option<UpdateResult>>`; field name for the new version comes from `UpdateResult` - check docs.rs). Add the clap variant `SelfUpdate { #[arg(long)] check: bool }` with doc comment "Update apb to the latest released version" and dispatch it in `main.rs`. Refactor into the testable `self_update_with_config_root` split described in Step 1.

- [ ] **Step 4: Run tests and gates**

Run: `cargo nextest run -p apb && cargo clippy -p apb --all-targets -- -D warnings`
Expected: PASS, no warnings. Verify `cargo tree -p apb | grep -c tokio` prints 0 (the blocking feature must not pull tokio).

- [ ] **Step 5: Commit**

```bash
git add crates/apb-cli
git commit --signoff -m "feat: apb self-update via axoupdater install receipts"
```

---

### Task 5: dist adoption (config, generated workflow, gate, notes)

**Files:**
- Create: `dist-workspace.toml`
- Create: `.github/build-setup.yml`
- Create: `.github/workflows/test-gate.yml`
- Create: `.github/workflows/release-notes.yml`
- Modify (regenerate): `.github/workflows/release.yml`
- Modify: `crates/apb-cli/Cargo.toml` (`[package.metadata.dist] dist = true`)

**Interfaces:**
- Consumes: package name `apb` from Task 1.
- Produces: the release pipeline all later docs describe; installer asset `apb-installer.sh`; formula `apb`.

- [ ] **Step 1: Install the pinned dist CLI**

Run: `cargo install cargo-dist@0.32.0 --locked` (first re-check the latest stable on the releases page; if newer than 0.32.0, use it and pin the same number in Step 2). Verify: `dist --version`.

- [ ] **Step 2: Write dist-workspace.toml**

```toml
[workspace]
members = ["cargo:."]

[dist]
cargo-dist-version = "0.32.0"
ci = "github"
installers = ["shell", "homebrew"]
tap = "itechmeat/homebrew-agentic-playbooks"
publish-jobs = ["homebrew"]
targets = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "x86_64-unknown-linux-gnu",
  "x86_64-unknown-linux-musl",
]
install-updater = true
github-build-setup = "../build-setup.yml"
plan-jobs = ["./test-gate"]
post-announce-jobs = ["./release-notes"]
pr-run-mode = "plan"
```

Add to `crates/apb-cli/Cargo.toml`:

```toml
[package.metadata.dist]
dist = true
```

- [ ] **Step 3: Write the build-setup snippet**

`.github/build-setup.yml` (a YAML list of steps, injected after checkout and before cargo build on every runner):

```yaml
- name: Install Bun
  uses: oven-sh/setup-bun@v2
  with:
    bun-version: latest
- name: Build web frontend (embedded by apb-server at compile time)
  shell: bash
  run: |
    cd web
    bun install --frozen-lockfile
    bun run build
```

- [ ] **Step 4: Write the test gate as a reusable workflow**

`.github/workflows/test-gate.yml`: port the ENTIRE test job from the current (pre-dist) `.github/workflows/release.yml` verbatim (checkout, toolchain, bun web build, fmt check, clippy, nextest, doc tests - read the old file in git before regenerating: `git show HEAD:.github/workflows/release.yml`), wrapped as:

```yaml
name: test-gate
on:
  workflow_call:
    inputs:
      plan:
        required: true
        type: string
jobs:
  gate:
    runs-on: ubuntu-latest
    steps:
      # ported steps from the old release.yml test job go here, verbatim,
      # including the bun web build (tests compile apb-server which embeds web/dist)
      - name: Release notes must exist for this tag
        if: github.ref_type == 'tag'
        run: test -f "docs/release-notes/${GITHUB_REF_NAME}.md"
```

The notes-existence check is the LAST step and only runs on tag refs (PR plan runs skip it).

- [ ] **Step 5: Write the release-notes post-announce workflow**

`.github/workflows/release-notes.yml`:

```yaml
name: release-notes
on:
  workflow_call:
    inputs:
      plan:
        required: true
        type: string
permissions:
  contents: write
jobs:
  apply-notes:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Apply release notes as the release body
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: gh release edit "${GITHUB_REF_NAME}" --notes-file "docs/release-notes/${GITHUB_REF_NAME}.md"
```

If the generated caller does not grant `contents: write` to this job, add the documented `[dist.github-custom-job-permissions]` entry for `./release-notes` in `dist-workspace.toml` (check the generated release.yml to see whether the permission arrived; the config key exists per dist docs, exact table shape to be confirmed against `dist generate` output).

- [ ] **Step 6: Generate and inspect the workflow**

Run: `dist init --yes` (consumes the existing dist-workspace.toml; do not let it rewrite config choices) then `dist plan`.
Expected: plan lists exactly one app `apb` version 0.9.0-to-be (current 0.8.0 is fine at this point), 4 targets, artifacts including `apb-installer.sh` and a Homebrew formula named `apb`. If the app or formula shows as `apb-cli` anything, stop and fix (Task 1 rename incomplete).
Inspect the regenerated `.github/workflows/release.yml`: confirm (a) build-setup steps injected after checkout in build-local-artifacts, (b) plan job depends on `./test-gate`, (c) post-announce wiring to `./release-notes`, (d) triggers include pull_request (pr-run-mode plan). Do NOT hand-edit release.yml; every change goes through config plus `dist generate`.

- [ ] **Step 7: Commit**

```bash
git add dist-workspace.toml .github crates/apb-cli/Cargo.toml
git commit --signoff -m "feat: adopt dist for release packaging, installers, and Homebrew tap publishing"
```

---

### Task 6: Documentation refresh

**Files:**
- Modify: `docs/INSTALL.md` (rewrite), `README.md` (Install section + init mention), `llms.txt`, `CLAUDE.md`, `AGENTS.md`
- Delete: `packaging/apb.rb` (and `packaging/` if now empty)

**Interfaces:**
- Consumes: final install commands from Task 5 (`apb-installer.sh` asset name, tap name, formula `apb`), `apb self-update` from Task 4, questionnaire behavior from Task 3.

- [ ] **Step 1: Rewrite docs/INSTALL.md**

Structure: (1) one-liner install for macOS/Linux: `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/itechmeat/agentic-playbooks/releases/latest/download/apb-installer.sh | sh` with a note on pinning to a version via `releases/download/vX.Y.Z/...`; (2) Homebrew: `brew install itechmeat/agentic-playbooks/apb`; (3) updating: `apb self-update` (installer installs), `brew upgrade apb` (brew), and what `apb self-update --check` returns; (4) manual download and verify: pick the tarball for your platform from the Releases page, `shasum -a 256 -c <file>.sha256`, unpack, move to PATH; (5) build from source (contributor path, unchanged content from today incl. the web build prerequisite and why `cargo install --git` does not work); (6) uninstall per method, including removing the installer receipt dir `~/.config/apb/` and that `.apb/` project data and `~/.config/apb/` config are never touched by uninstalling the binary; (7) after install: run `apb init` and describe the interactive questionnaire (feedback-loop consent writing CLAUDE.md/AGENTS.md, agents survey) and that non-TTY runs skip it. Remove every "planned for v0.1.0" claim. No exclamation marks, no em-dashes.

- [ ] **Step 2: Update README.md**

Install section: condensed version of INSTALL.md's paths 1-3 with the one-liner first, link to docs/INSTALL.md for the rest; remove stale claims (README lines 91-131 region); mention `apb init` questionnaire in the getting-started flow. Keep the feedback-loop section (Task 2's drift test depends on it) untouched.

- [ ] **Step 3: Update llms.txt, delete packaging/apb.rb, mirror CLAUDE.md/AGENTS.md**

llms.txt: agent-install path prefers the one-liner, falls back to source build. Delete `packaging/apb.rb`; remove `packaging/` if empty. CLAUDE.md + AGENTS.md (mirror rule, identical edits): update the release/build commands section to describe the dist pipeline (tag push triggers dist workflow; test gate; `dist plan` runs on PRs; release notes file still required per tag) and the interactive init in the crate description.

- [ ] **Step 4: Verify docs consistency**

Run: `grep -rn "planned for v0.1.0\|apb.rb\|cargo install apb-cli" README.md docs/ llms.txt` — expected: no hits. Run `cargo nextest run -p apb onboarding` — the README drift test still passes.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit --signoff -m "docs: install story rewrite for one-liner, Homebrew, and self-update paths"
```

---

### Task 7: Version 0.9.0, release notes, full gates

**Files:**
- Modify: `Cargo.toml` (workspace version 0.9.0), `Cargo.lock`
- Create: `docs/release-notes/v0.9.0.md`

- [ ] **Step 1: Bump version**

Set `[workspace.package] version = "0.9.0"` in the root `Cargo.toml`; run `cargo build` to refresh `Cargo.lock`.

- [ ] **Step 2: Write docs/release-notes/v0.9.0.md**

Follow the existing convention (see `docs/release-notes/v0.8.0.md` for shape; one paragraph per line, no hard wraps). Cover: one-line installer, Homebrew tap, `apb self-update`, interactive `apb init` questionnaire with feedback-loop consent, refreshed install docs, dist-based release pipeline. State honestly that the first dist-published release is this one.

- [ ] **Step 3: Full gates**

Run, all must pass clean:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --release --workspace --all-targets
cargo nextest run --workspace
cargo test --doc --workspace
(cd web && bun run test && bun run check)
cargo metadata --format-version 1 >/dev/null && code-ranker check .
dist plan
```

- [ ] **Step 4: Commit**

```bash
git add -A
git commit --signoff -m "chore: release 0.9.0"
```

---

## Out of plan (controller/owner actions, per-action approval)

- Create the public tap repo `itechmeat/homebrew-agentic-playbooks` (can start empty; dist populates it on first release).
- Create a fine-grained PAT with Contents read/write on the tap repo only; add it to the main repo as Actions secret `HOMEBREW_TAP_TOKEN`.
- After the v0.9.0 release: manual verification checklist from the spec (one-liner install, brew install, self-update from a previous installer-installed version), then update docs if the formula path or asset naming differs from the plan's assumptions.
- Propose a tracking GitHub issue for the deferred crates.io publishing when the PR is opened.
