use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::registry::init_project;

/// Starts the single, global dashboard for the machine. There is no
/// project-scoped server: the dashboard aggregates every registered project,
/// so it does not bind to (or initialize) the current directory.
pub(crate) fn dashboard(port: u16, no_open: bool) -> ExitCode {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if !no_open {
        let url = format!("http://127.0.0.1:{port}");
        let _ = open::that_detached(&url);
    }
    match rt.block_on(apb_server::run_server(port)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if error_looks_like_addr_in_use(&e) {
                let holders = lookup_port_holders(port);
                eprintln!("{}", format_port_in_use_error(port, holders.as_deref()));
            } else {
                eprintln!("dashboard failed: {e}");
            }
            ExitCode::from(2)
        }
    }
}

/// True when `err`'s Display text names an address-already-in-use failure.
/// Best-effort: the server returns `Box<dyn Error>`, so we match the usual
/// OS phrasings rather than requiring a concrete IO error type.
fn error_looks_like_addr_in_use(err: &dyn std::fmt::Display) -> bool {
    let text = err.to_string().to_ascii_lowercase();
    text.contains("address already in use")
        || text.contains("addrinuse")
        || text.contains("only one usage of each socket address")
}

/// Best-effort PIDs listening on `port` (TCP). On unix, probes with `lsof`
/// when it is on PATH; returns `None` when the holder cannot be determined.
/// Never fails the caller - a missing or broken `lsof` is treated as unknown.
fn lookup_port_holders(port: u16) -> Option<String> {
    #[cfg(unix)]
    {
        let port_arg = format!("-iTCP:{port}");
        let output = std::process::Command::new("lsof")
            .args(["-nP", &port_arg, "-sTCP:LISTEN", "-t"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut pids: Vec<&str> = stdout
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        pids.sort_unstable();
        pids.dedup();
        if pids.is_empty() {
            None
        } else {
            Some(pids.join(", "))
        }
    }
    #[cfg(not(unix))]
    {
        let _ = port;
        None
    }
}

/// User-facing dashboard bind failure when the port is already taken.
/// `holders` is a comma-separated pid list from a best-effort lookup, or
/// `None` when the holder could not be determined. No automatic kill or
/// takeover - only name the holder and hint how to stop a stale instance.
fn format_port_in_use_error(port: u16, holders: Option<&str>) -> String {
    let holder_line = match holders {
        Some(pids) if !pids.is_empty() => {
            format!("dashboard failed: port {port} is already in use (held by pid {pids})")
        }
        _ => format!(
            "dashboard failed: port {port} is already in use (holder pid could not be determined)"
        ),
    };
    let hint = match holders {
        Some(pids) if !pids.is_empty() => {
            format!(
                "hint: another apb dashboard may already be running; stop it (for example: kill {pids}) and retry"
            )
        }
        _ => {
            "hint: another apb dashboard may already be running; stop the process listening on that port and retry"
                .to_string()
        }
    };
    format!("{holder_line}\n{hint}")
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
            if error_looks_like_addr_in_use(&e) {
                let holders = lookup_port_holders(7321);
                eprintln!("{}", format_port_in_use_error(7321, holders.as_deref()));
            } else {
                eprintln!("apb dev: API server on 7321 stopped: {e}");
            }
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

#[cfg(test)]
mod tests {
    use super::{error_looks_like_addr_in_use, format_port_in_use_error};

    #[test]
    fn format_port_in_use_error_names_holder_pids() {
        let msg = format_port_in_use_error(7321, Some("1234, 5678"));
        assert!(
            msg.contains("port 7321 is already in use"),
            "must name the port: {msg}"
        );
        assert!(
            msg.contains("held by pid 1234, 5678"),
            "must name holder pids: {msg}"
        );
        assert!(
            msg.contains("kill 1234, 5678"),
            "hint must suggest stopping the holder: {msg}"
        );
        assert!(
            msg.contains("apb dashboard"),
            "hint must mention apb dashboard: {msg}"
        );
        assert!(
            !msg.contains('!'),
            "user-facing strings must not use exclamation marks: {msg}"
        );
        assert!(
            !msg.contains('\u{2014}'),
            "user-facing strings must not use em-dashes: {msg}"
        );
    }

    #[test]
    fn format_port_in_use_error_when_holder_unknown() {
        let msg = format_port_in_use_error(7321, None);
        assert!(
            msg.contains("holder pid could not be determined"),
            "must say the holder is unknown: {msg}"
        );
        assert!(
            msg.contains("stop the process listening on that port"),
            "hint must still guide the operator: {msg}"
        );
        assert!(!msg.contains('!'), "no exclamation marks: {msg}");
    }

    #[test]
    fn error_looks_like_addr_in_use_detects_display_text() {
        assert!(error_looks_like_addr_in_use(
            &"Address already in use (os error 48)"
        ));
        assert!(error_looks_like_addr_in_use(&std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "Address already in use"
        )));
        assert!(!error_looks_like_addr_in_use(&"connection refused"));
    }
}
