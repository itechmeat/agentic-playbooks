# Feature 1: `wf tunnel` + Discovery/Opt-in Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provide fast, secure remote access to the local web UI via `cloudflared`, plus a shared discovery/opt-in mechanism for both remote features (MCP offers it once; the user can dismiss it permanently), plus a mobile-responsive UI.

**Architecture:** `wf tunnel` starts the existing `wf server` on a background thread and spawns `cloudflared` as a child process (`dev_cmd` model: shared process group, Ctrl-C stops both). The opt-in state lives in a new `remote` field on `GlobalConfig` (`~/.config/wf/config.yaml`). The MCP server reads this field and appends a one-time note to the `initialize` response `instructions`; the `remote_dismiss_suggestion` tool and the `wf remote dismiss` CLI write the flag.

**Tech Stack:** Rust (edition 2024), clap, rmcp (MCP), tokio, serde_yaml_ng; web: Svelte 5 + Vite + TypeScript (mobile-responsive CSS).

## Global Constraints

- Do not use em dashes anywhere: code, comments, or documentation. Only a regular hyphen, a comma, or rephrasing.
- Do not use exclamation marks in technical documentation or messages.
- Comments in this project are written in Russian (follow the existing style of the files).
- `GlobalConfig` is annotated `#[serde(default, deny_unknown_fields)]`: any new field must be known and have `#[serde(default)]`; absence of the field in old configs means the default.
- Check for an external program only via `wf_core::config::program_in_path` (do not reinvent one).
- Spawn a long-lived child process and rely on the process group for shutdown, following the `dev_cmd` pattern in `crates/wf-cli/src/main.rs`.
- Each task ends with a green `cargo test -p <crate>` for the affected crate and `cargo clippy --workspace` with no new warnings.
- Default port is 7321; resolve the port via the existing `resolve_port`.

---

## File Structure

- `crates/wf-core/src/config.rs` (modify) - add `RemoteConfig`, `SuggestState`, the `remote` field on `GlobalConfig`, the `GlobalConfig::save()` method, and the `set_remote_suggest` helper.
- `crates/wf-core/tests/remote_config_test.rs` (create) - persistence and defaults of `remote`.
- `crates/wf-core/src/doctor.rs` (modify) - check for `cloudflared`.
- `crates/wf-core/tests/doctor_test.rs` (modify) - verify the `cloudflared` check is present.
- `crates/wf-mcp/src/server.rs` (modify) - dynamic `instructions` in `get_info`, `remote_dismiss_suggestion` tool.
- `crates/wf-mcp/src/tools.rs` (modify) - `remote_dismiss_suggestion` function (writes the config).
- `crates/wf-cli/src/main.rs` (modify) - `Tunnel` and `Remote { Dismiss | Suggest }` subcommands, plus the `tunnel_cmd` and `remote_cmd` functions.
- `crates/wf-cli/tests/tunnel_cli_test.rs` (create) - construction of the `cloudflared` command, and the missing-`cloudflared` case.
- `web/src/**` (modify) - mobile-responsive layout.
- `docs/INSTALL.md` (modify) - section on `cloudflared` and `wf tunnel`.

---

## Task 1: RemoteConfig in GlobalConfig + save()

**Files:**
- Modify: `crates/wf-core/src/config.rs`
- Test: `crates/wf-core/tests/remote_config_test.rs`

**Interfaces:**
- Produces:
  - `pub struct RemoteConfig { pub suggest: SuggestState, pub relay_url: Option<String> }` (`#[serde(default, deny_unknown_fields)]`)
  - `pub enum SuggestState { Unshown, Dismissed }` (`#[serde(rename_all = "snake_case")]`, default `Unshown`)
  - a `pub remote: RemoteConfig` field on `GlobalConfig`
  - `impl GlobalConfig { pub fn save(&self) -> Result<(), String>; pub fn set_remote_suggest(state: SuggestState) -> Result<(), String>; pub fn remote_suggest_dismissed(&self) -> bool; pub fn remote_configured(&self) -> bool }`

- [ ] **Step 1: Write a failing test**

Create `crates/wf-core/tests/remote_config_test.rs`:

```rust
use wf_core::config::{GlobalConfig, SuggestState};

/// Isolate the config in a temp dir via WF_CONFIG_DIR.
/// A single test in the file (separate process) so the env variable is not
/// shared with other tests of the same binary.
#[test]
fn remote_suggest_roundtrips_through_save() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this test binary, no env race.
    unsafe { std::env::set_var("WF_CONFIG_DIR", dir.path()) };

    // Default: nothing shown, remote not configured.
    let cfg = GlobalConfig::load().unwrap();
    assert_eq!(cfg.remote.suggest, SuggestState::Unshown);
    assert!(!cfg.remote_suggest_dismissed());
    assert!(!cfg.remote_configured());

    // Write a dismissal and reload.
    GlobalConfig::set_remote_suggest(SuggestState::Dismissed).unwrap();
    let cfg2 = GlobalConfig::load().unwrap();
    assert_eq!(cfg2.remote.suggest, SuggestState::Dismissed);
    assert!(cfg2.remote_suggest_dismissed());
}
```

Add to `crates/wf-core/Cargo.toml` (if not present) under `[dev-dependencies]`: `tempfile = "3"` (check: already used by other tests of the crate, likely present).

- [ ] **Step 2: Run the test - confirm it does not compile / fails**

Run: `cargo test -p wf-core --test remote_config_test`
Expected: compile error (`SuggestState`, `remote`, `set_remote_suggest` do not exist).

- [ ] **Step 3: Implement**

In `crates/wf-core/src/config.rs`:

```rust
/// State of the remote-access suggestion (spec 5.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestState {
    /// Not yet shown, or the user has not decided yet.
    #[default]
    Unshown,
    /// The user dismissed it; do not offer again.
    Dismissed,
}

/// Remote-access settings (spec 5.1). No secrets are stored here,
/// only the suggestion state and the relay URL.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteConfig {
    pub suggest: SuggestState,
    pub relay_url: Option<String>,
}
```

Add the field to `GlobalConfig`:

```rust
    /// Remote access: suggestion state, relay URL.
    pub remote: RemoteConfig,
```

Add methods to `impl GlobalConfig`:

```rust
    /// Whether a remote control-plane is configured (relay URL present).
    pub fn remote_configured(&self) -> bool {
        self.remote.relay_url.is_some()
    }

    /// Whether the user dismissed the remote-access suggestion.
    pub fn remote_suggest_dismissed(&self) -> bool {
        self.remote.suggest == SuggestState::Dismissed
    }

    /// Atomically saves the config to `<config_dir>/config.yaml`, creating the
    /// directory if needed. Returns Err if the config dir cannot be resolved
    /// (none of WF_CONFIG_DIR, XDG_CONFIG_HOME, or HOME is set).
    pub fn save(&self) -> Result<(), String> {
        let dir = config_dir().ok_or("no config dir (set HOME or XDG_CONFIG_HOME)")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("config.yaml");
        let yaml = serde_yaml_ng::to_string(self).map_err(|e| e.to_string())?;
        crate::fsutil::atomic_write(&path, yaml.as_bytes()).map_err(|e| e.to_string())
    }

    /// Reads the config, sets the suggestion state, and saves. A targeted
    /// operation for `wf remote dismiss/suggest` and the dismiss MCP tool.
    pub fn set_remote_suggest(state: SuggestState) -> Result<(), String> {
        let mut cfg = Self::load()?;
        cfg.remote.suggest = state;
        cfg.save()
    }
```

Verify that `crate::fsutil::atomic_write` exists with the signature `(&Path, &[u8]) -> io::Result<()>` (used in the CLI as `atomic_write`). If it returns a different error type, adapt with `.map_err(|e| e.to_string())`.

- [ ] **Step 4: Run the test - green**

Run: `cargo test -p wf-core --test remote_config_test`
Expected: PASS.

- [ ] **Step 5: Clippy and commit**

Run: `cargo clippy -p wf-core --all-targets` (no new warnings)

```bash
git add crates/wf-core/src/config.rs crates/wf-core/tests/remote_config_test.rs crates/wf-core/Cargo.toml
git commit -m "feat(core): remote config state (suggest/relay) + GlobalConfig::save"
```

---

## Task 2: Discovery note in MCP + the dismiss tool

**Files:**
- Modify: `crates/wf-mcp/src/server.rs`, `crates/wf-mcp/src/tools.rs`
- Test: add a test to the existing test module in `server.rs` (`#[cfg(test)]`) or a new `crates/wf-mcp/tests/remote_suggest_test.rs`

**Interfaces:**
- Consumes: `wf_core::config::{GlobalConfig, SuggestState}` (Task 1).
- Produces:
  - `get_info` returns `instructions` with a one-time note when `!remote_configured() && !remote_suggest_dismissed()`.
  - the MCP tool `remote_dismiss_suggestion` -> calls `tools::remote_dismiss_suggestion()`.
  - `tools::remote_dismiss_suggestion() -> Result<serde_json::Value, ToolError>`.

- [ ] **Step 1: Write a failing test**

Create `crates/wf-mcp/tests/remote_suggest_test.rs`:

```rust
use wf_core::config::{GlobalConfig, SuggestState};
use wf_mcp::server::WfMcp;
use rmcp::ServerHandler;

/// A single test in the file: isolate WF_CONFIG_DIR in its own process.
#[test]
fn instructions_offer_remote_until_dismissed() {
    let cfgdir = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("WF_CONFIG_DIR", cfgdir.path()) };

    // By default the note is present.
    let server = WfMcp::new(proj.path().to_path_buf());
    let info = server.get_info();
    let text = info.instructions.unwrap_or_default();
    assert!(text.contains("wf tunnel"), "offer should mention wf tunnel: {text}");

    // After dismissal, there is no note.
    GlobalConfig::set_remote_suggest(SuggestState::Dismissed).unwrap();
    let server2 = WfMcp::new(proj.path().to_path_buf());
    let info2 = server2.get_info();
    let text2 = info2.instructions.unwrap_or_default();
    assert!(!text2.contains("wf tunnel"), "no offer after dismiss: {text2}");
}
```

Make sure `WfMcp` and the `server` module are public (`pub use`/`pub mod`). In `crates/wf-mcp/src/lib.rs`, add `pub mod server;` if needed (check the current contents, it's two lines).

- [ ] **Step 2: Run the test - it fails**

Run: `cargo test -p wf-mcp --test remote_suggest_test`
Expected: FAIL - the current `get_info` returns a static string without "wf tunnel".

- [ ] **Step 3: Implement**

In `crates/wf-mcp/src/server.rs`, replace the body of `get_info`:

```rust
impl ServerHandler for WfMcp {
    fn get_info(&self) -> ServerInfo {
        let base = "wf workflows MCP server: read and run tools over a project root";
        let info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        // One-time remote-access recommendation (spec 5.2): shown while
        // remote is not configured and the user has not dismissed it. The
        // dismissal is written by the remote_dismiss_suggestion tool or by
        // `wf remote dismiss`.
        let offer = match wf_core::config::GlobalConfig::load() {
            Ok(cfg) if !cfg.remote_configured() && !cfg.remote_suggest_dismissed() => Some(
                " Remote access: wf can be reached from outside via Cloudflare. \
                 Quick access to the local UI - `wf tunnel`; a full remote \
                 control-plane (triggering runs and confirming review gates from a \
                 phone) - `wf connect --help`. Tell the user about this in the \
                 conversation language. If the user declines, call the \
                 `remote_dismiss_suggestion` tool, and the offer will not appear again.",
            ),
            _ => None,
        };
        match offer {
            Some(extra) => info.with_instructions(format!("{base}.{extra}")),
            None => info.with_instructions(base),
        }
    }
}
```

Add an MCP tool in `#[tool_router] impl WfMcp` (alongside the other `#[tool]`s):

```rust
    #[tool(description = "Dismiss the remote-access suggestion so it is not shown again")]
    async fn remote_dismiss_suggestion(&self) -> CallToolResult {
        to_call_tool_result(tools::remote_dismiss_suggestion())
    }
```

Add to `crates/wf-mcp/src/tools.rs`:

```rust
/// Writes `remote.suggest = dismissed` to the global config (spec 5.3).
pub fn remote_dismiss_suggestion() -> Result<serde_json::Value, ToolError> {
    wf_core::config::GlobalConfig::set_remote_suggest(wf_core::config::SuggestState::Dismissed)
        .map_err(ToolError::Engine)?;
    Ok(serde_json::json!({ "dismissed": true }))
}
```

(Check the exact name/variant of `ToolError` in `tools.rs` - use the same constructor as the other functions, e.g. `ToolError::Engine(String)`.)

- [ ] **Step 4: Run the test - green**

Run: `cargo test -p wf-mcp --test remote_suggest_test`
Expected: PASS.

- [ ] **Step 5: Clippy and commit**

Run: `cargo clippy -p wf-mcp --all-targets`

```bash
git add crates/wf-mcp/src/server.rs crates/wf-mcp/src/tools.rs crates/wf-mcp/src/lib.rs crates/wf-mcp/tests/remote_suggest_test.rs
git commit -m "feat(mcp): one-time remote-access suggestion + dismiss tool"
```

---

## Task 3: `cloudflared` check in `wf doctor`

**Files:**
- Modify: `crates/wf-core/src/doctor.rs`
- Test: `crates/wf-core/tests/doctor_test.rs`

**Interfaces:**
- Consumes: `program_in_path` (already imported in doctor.rs).
- Produces: a check named `"cloudflared"` is added to `DoctorReport` (Ok if found in PATH, otherwise Warn explaining it's only needed for `wf tunnel`).

- [ ] **Step 1: Write a failing test**

Add to `crates/wf-core/tests/doctor_test.rs`:

```rust
#[test]
fn doctor_reports_cloudflared_check() {
    let dir = tempfile::tempdir().unwrap();
    wf_core::registry::init_project(dir.path()).unwrap();
    let report = wf_core::doctor::diagnose(dir.path());
    assert!(
        report.checks.iter().any(|c| c.name == "cloudflared"),
        "doctor must include a cloudflared check"
    );
}
```

- [ ] **Step 2: Run it - it fails**

Run: `cargo test -p wf-core --test doctor_test doctor_reports_cloudflared_check`
Expected: FAIL (the check does not exist).

- [ ] **Step 3: Implement**

In `crates/wf-core/src/doctor.rs`, at the end of `diagnose`, before `r`:

```rust
    // 6. cloudflared - only needed for `wf tunnel`; absence does not block anything.
    if program_in_path("cloudflared") {
        r.push(CheckStatus::Ok, "cloudflared", "found (used by `wf tunnel`)");
    } else {
        r.push(
            CheckStatus::Warn,
            "cloudflared",
            "not in PATH; needed only for `wf tunnel` (see docs/INSTALL.md)",
        );
    }
```

- [ ] **Step 4: Run it - green**

Run: `cargo test -p wf-core --test doctor_test`
Expected: PASS.

- [ ] **Step 5: Clippy and commit**

Run: `cargo clippy -p wf-core --all-targets`

```bash
git add crates/wf-core/src/doctor.rs crates/wf-core/tests/doctor_test.rs
git commit -m "feat(doctor): cloudflared availability check"
```

---

## Task 4: `wf tunnel`

**Files:**
- Modify: `crates/wf-cli/src/main.rs`
- Test: `crates/wf-cli/tests/tunnel_cli_test.rs`

**Interfaces:**
- Consumes: `resolve_port`, `wf_server::run_server`, `wf_core::config::program_in_path`.
- Produces:
  - The subcommand `Tunnel { port: Option<u16>, name: Option<String>, no_open: bool }`.
  - `fn cloudflared_args(port: u16, name: Option<&str>) -> Vec<String>` (pure, testable).
  - `fn tunnel_cmd(root: PathBuf, port: u16, name: Option<String>, no_open: bool) -> ExitCode`.

Note: the mode is `--name <NAME>` (a value), not a boolean `--named`, because a named Cloudflare tunnel requires a name (`cloudflared tunnel run <NAME>`). Omitting `--name` means quick mode. The spec (sections 6.1/6.2) is consistent with this.

- [ ] **Step 1: Write a failing test**

Create `crates/wf-cli/tests/tunnel_cli_test.rs`. Since `cloudflared_args` is a private function of the binary crate, either pull it into a small module reachable by the test, OR test through the public surface. The simplest path without refactoring the binary: check the construction via an integration run of `wf tunnel` in a directory without `cloudflared`. Test:

```rust
use std::process::Command;

/// `wf tunnel` without cloudflared in PATH must fail with a clear message and
/// a non-zero exit code, rather than trying to spawn a nonexistent binary.
#[test]
fn tunnel_without_cloudflared_fails_cleanly() {
    let proj = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_wf");
    let out = Command::new(bin)
        .arg("tunnel")
        .current_dir(proj.path())
        .env("PATH", "") // empty PATH: cloudflared is guaranteed not to be found
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cloudflared"), "stderr should mention cloudflared: {err}");
}
```

- [ ] **Step 2: Run it - it fails**

Run: `cargo test -p wf-cli --test tunnel_cli_test`
Expected: FAIL - the `tunnel` subcommand does not exist yet (clap will exit with an error, but not about cloudflared).

- [ ] **Step 3: Implement**

Add to `enum Command`:

```rust
    /// Expose the local web UI over a Cloudflare tunnel (needs `cloudflared`)
    Tunnel {
        #[arg(long)]
        port: Option<u16>,
        /// Named tunnel to run (`cloudflared tunnel run <NAME>`); omit for a
        /// quick ephemeral *.trycloudflare.com URL
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        no_open: bool,
    },
```

Add to `match cli.command`:

```rust
        Some(Command::Tunnel { port, name, no_open }) =>
            tunnel_cmd(root, resolve_port(port), name, no_open),
```

Add the functions:

```rust
/// Arguments for launching cloudflared. Quick mode (name = None) brings up an
/// ephemeral tunnel to the local port; named mode runs a tunnel the user has
/// already configured (they set up ingress themselves).
fn cloudflared_args(port: u16, name: Option<&str>) -> Vec<String> {
    match name {
        None => vec![
            "tunnel".into(),
            "--url".into(),
            format!("http://localhost:{port}"),
        ],
        Some(n) => vec!["tunnel".into(), "run".into(), n.into()],
    }
}

/// Starts the local web server on a background thread and runs cloudflared as
/// a child process (the dev_cmd model: a shared terminal process group,
/// Ctrl-C stops both). Prints the cloudflared URL from its own stdout/stderr.
fn tunnel_cmd(root: PathBuf, port: u16, name: Option<String>, no_open: bool) -> ExitCode {
    if !wf_core::config::program_in_path("cloudflared") {
        eprintln!(
            "wf tunnel: `cloudflared` not found in PATH.\n\
             Install it: https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/\n\
             (see docs/INSTALL.md)"
        );
        return ExitCode::from(2);
    }
    if name.is_none() {
        eprintln!(
            "wf tunnel: quick mode gives a PUBLIC *.trycloudflare.com URL with NO auth - \
             anyone with the link can reach your UI. Use `--name <tunnel>` with Cloudflare \
             Access for protected access."
        );
    }
    if !root.join(".wf").is_dir()
        && let Err(e) = init_project(&root)
    {
        eprintln!("init failed: {e}");
        return ExitCode::from(2);
    }

    // Local API/UI server in the background (a daemon thread that dies with the process).
    let api_root = root.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = rt.block_on(wf_server::run_server(api_root, port)) {
            eprintln!("wf tunnel: local server on {port} stopped: {e}");
        }
    });

    if !no_open {
        let _ = open::that_detached(format!("http://127.0.0.1:{port}"));
    }

    let args = cloudflared_args(port, name.as_deref());
    let mut child = match std::process::Command::new("cloudflared").args(&args).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wf tunnel: failed to start cloudflared: {e}");
            return ExitCode::from(2);
        }
    };
    match child.wait() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(e) => {
            eprintln!("wf tunnel: cloudflared process error: {e}");
            ExitCode::from(2)
        }
    }
}
```

- [ ] **Step 4: Run it - green**

Run: `cargo test -p wf-cli --test tunnel_cli_test`
Expected: PASS (fails with a cloudflared message, exit code != 0).

- [ ] **Step 5: Clippy and commit**

Run: `cargo clippy -p wf-cli --all-targets`

```bash
git add crates/wf-cli/src/main.rs crates/wf-cli/tests/tunnel_cli_test.rs
git commit -m "feat(cli): wf tunnel over cloudflared (quick + named)"
```

---

## Task 5: `wf remote dismiss` / `wf remote suggest`

**Files:**
- Modify: `crates/wf-cli/src/main.rs`
- Test: `crates/wf-cli/tests/tunnel_cli_test.rs` (extend) - via WF_CONFIG_DIR.

**Interfaces:**
- Consumes: `wf_core::config::{GlobalConfig, SuggestState}`.
- Produces: the subcommand `Remote { #[command(subcommand)] action: RemoteAction }`, `enum RemoteAction { Dismiss, Suggest }`, `fn remote_cmd(action: RemoteAction) -> ExitCode`.

- [ ] **Step 1: Write a failing test**

Add to `crates/wf-cli/tests/tunnel_cli_test.rs`:

```rust
#[test]
fn remote_dismiss_persists_flag() {
    let cfgdir = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_wf");
    let out = Command::new(bin)
        .args(["remote", "dismiss"])
        .current_dir(proj.path())
        .env("WF_CONFIG_DIR", cfgdir.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let cfg_path = cfgdir.path().join("config.yaml");
    let raw = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(raw.contains("dismissed"), "config should record dismissal: {raw}");
}
```

- [ ] **Step 2: Run it - it fails**

Run: `cargo test -p wf-cli --test tunnel_cli_test remote_dismiss_persists_flag`
Expected: FAIL (the `remote` subcommand does not exist).

- [ ] **Step 3: Implement**

In `enum Command`:

```rust
    /// Manage the remote-access suggestion
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },
```

A new enum alongside `Command`:

```rust
#[derive(Subcommand)]
enum RemoteAction {
    /// Stop suggesting remote access
    Dismiss,
    /// Re-enable the remote-access suggestion
    Suggest,
}
```

In `match cli.command`:

```rust
        Some(Command::Remote { action }) => remote_cmd(action),
```

The function:

```rust
fn remote_cmd(action: RemoteAction) -> ExitCode {
    use wf_core::config::{GlobalConfig, SuggestState};
    // A single match computes both the state and the label - action is not
    // reused after the move (otherwise a second match would be a
    // use-after-move).
    let (state, label) = match action {
        RemoteAction::Dismiss => (SuggestState::Dismissed, "dismissed"),
        RemoteAction::Suggest => (SuggestState::Unshown, "enabled"),
    };
    match GlobalConfig::set_remote_suggest(state) {
        Ok(()) => {
            println!("remote suggestion: {label}");
            ExitCode::SUCCESS
        }
        Err(e) => { eprintln!("remote: {e}"); ExitCode::from(2) }
    }
}
```

- [ ] **Step 4: Run it - green**

Run: `cargo test -p wf-cli --test tunnel_cli_test`
Expected: PASS.

- [ ] **Step 5: Clippy and commit**

Run: `cargo clippy -p wf-cli --all-targets`

```bash
git add crates/wf-cli/src/main.rs crates/wf-cli/tests/tunnel_cli_test.rs
git commit -m "feat(cli): wf remote dismiss/suggest"
```

---

## Task 6: Mobile-responsive UI

**Files:**
- Modify: `web/src/**` (global styles and panel/graph layout).
- Verification: `bun run build` in `web/` and `bun test` (vitest), if layout coverage exists.

**Interfaces:**
- Consumes: the existing UI (the `@xyflow/svelte` graph, panels).
- Produces: a layout that works on a narrow screen (a phone): panels collapse into a vertical stack / drawer, the graph stays scrollable/zoomable, tap targets are >= 44px.

- [ ] **Step 1: Find the root layout and the hard-coded widths**

Run: `grep -rn "width:\|min-width\|flex-direction\|grid-template" web/src | head -40`
Goal: find the fixed panel widths and horizontal layouts that break on a narrow screen.

- [ ] **Step 2: Add a breakpoint and adaptive rules**

In the root layout component (e.g. `web/src/App.svelte` or the main layout - determined in step 1), add a media query:

```css
@media (max-width: 720px) {
  /* Panels collapse into a vertical stack, the graph sits above the properties panel. */
  .app-layout { flex-direction: column; }
  .side-panel { width: 100%; max-height: 45vh; overflow-y: auto; }
  .node-panel { width: 100%; }
  button, .tap-target { min-height: 44px; }
}
```

(Take the exact selectors from step 1; map them to the real component classes.)

- [ ] **Step 3: Verify the build**

Run (in `web/`): `bun run build`
Expected: the build succeeds without errors.

- [ ] **Step 4: Manual check in devtools (mobile viewport)**

Open `wf dev`, enable a mobile viewport in the browser (e.g. 390x844), verify: panels are readable, the graph does not run off-screen horizontally, buttons are tappable. Record the outcome in the task report (a screenshot or a description).

- [ ] **Step 5: Commit**

```bash
git add web/src
git commit -m "feat(web): mobile-responsive layout for remote access"
```

---

## Task 7: `cloudflared` + `wf tunnel` documentation

**Files:**
- Modify: `docs/INSTALL.md`

- [ ] **Step 1: Add a section**

Add a "Remote access (wf tunnel)" section to `docs/INSTALL.md`:
- how to install `cloudflared` (link to Cloudflare's official downloads page);
- quick mode: `wf tunnel` -> a public `*.trycloudflare.com` URL, no auth, a warning about being public;
- named mode: `cloudflared login`, creating a tunnel, configuring ingress to `localhost:<port>`, Cloudflare Access, then `wf tunnel --name <tunnel>`;
- the server case: `wf tunnel --no-open`, no inbound ports.

- [ ] **Step 2: Check for em dashes and exclamation marks**

Run: `grep -nP "\x{2014}|!" docs/INSTALL.md` (there must be no new em dashes or exclamation marks in the added text).

- [ ] **Step 3: Commit**

```bash
git add docs/INSTALL.md
git commit -m "docs: cloudflared install + wf tunnel usage"
```

---

## Final check (after all tasks)

- [ ] `cargo test --workspace` - green.
- [ ] `cargo clippy --workspace --all-targets` - no new warnings.
- [ ] `cd web && bun run build` - passes.
- [ ] `grep -rnP "\x{2014}" crates docs` on the changed files - no em dashes.
- [ ] Manual run: `wf doctor` shows the `cloudflared` check; `wf tunnel` without cloudflared gives a clear error; `wf remote dismiss` writes the flag, after which the MCP note disappears.
