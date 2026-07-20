use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::registry::init_project;

/// Starts the single, global dashboard for the machine. There is no
/// project-scoped server: the dashboard aggregates every registered project,
/// so it does not bind to (or initialize) the current directory.
pub(crate) fn serve(port: u16, no_open: bool) -> ExitCode {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if !no_open {
        let url = format!("http://127.0.0.1:{port}");
        let _ = open::that_detached(&url);
    }
    match rt.block_on(apb_server::run_server(port)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("serve failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// Dev mode: brings up the API server on 7321 (the Vite proxy target, see
/// web/vite.config.ts) in a background thread and starts the Vite dev server
/// (HMR) as a child process in web/. Only works in the source tree (needs
/// web/ and bun). Exits together with Vite (Ctrl-C kills both - shared
/// terminal process group).
pub(crate) fn dev_cmd(root: PathBuf, no_open: bool) -> ExitCode {
    let web = root.join("web");
    if !web.join("package.json").is_file() {
        eprintln!(
            "apb dev: frontend not found at {} (run from the source tree)",
            web.display()
        );
        return ExitCode::from(2);
    }
    if !apb_core::config::program_in_path("bun") {
        eprintln!("apb dev: `bun` not found in PATH (needed for the Vite dev server)");
        return ExitCode::from(2);
    }
    if !root.join(".apb").is_dir()
        && let Err(e) = init_project(&root)
    {
        eprintln!("init failed: {e}");
        return ExitCode::from(2);
    }

    // API server in the background on 7321 (fixed to match the Vite proxy).
    // Daemon thread: dies with the process when Vite exits.
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = rt.block_on(apb_server::run_server(7321)) {
            eprintln!("apb dev: API server on 7321 stopped: {e}");
        }
    });

    if !no_open {
        // Vite listens on 5173 by default; the browser will reconnect on its
        // own if the server is still starting up.
        let _ = open::that_detached("http://localhost:5173");
    }

    let mut child = match std::process::Command::new("bun")
        .arg("run")
        .arg("dev")
        .current_dir(&web)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("apb dev: failed to start Vite (`bun run dev`): {e}");
            return ExitCode::from(2);
        }
    };
    match child.wait() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(e) => {
            eprintln!("apb dev: Vite process error: {e}");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn mcp_cmd(root: &Path) -> ExitCode {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    match rt.block_on(apb_mcp::server::serve_stdio(root.to_path_buf())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mcp failed: {e}");
            ExitCode::from(2)
        }
    }
}

/// `apb __ask-server`: the hidden live-question sidecar (spec 2026-07-20-
/// interactive-nodes, Task 10). Blocking; serves stdio MCP until the injecting
/// agent closes stdin. Errors (unset/mismatched `APB_RUN_DIR`, unsafe segment)
/// exit non-zero with a message that names the offending input.
pub(crate) fn ask_server_cmd(run: &str, node: &str, attempt: u32) -> ExitCode {
    match apb_mcp::ask_server::serve(run, node, attempt) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ask-server failed: {e}");
            ExitCode::from(2)
        }
    }
}
