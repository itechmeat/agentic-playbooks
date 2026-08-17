use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::registry::init_project;

/// Starts the single, global dashboard for the machine. There is no
/// project-scoped server: the dashboard aggregates every registered project,
/// so it does not bind to (or initialize) the current directory.
///
/// When `ingest.enabled` is true the inbound webhook listener starts in the
/// same process on its own socket with its own router (spec
/// 2026-08-16-webhook-ingest-design). Same process, two listeners: the
/// separation that matters is the router, not the process.
pub(crate) fn dashboard(bind: IpAddr, port: u16, no_open: bool) -> ExitCode {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if !no_open {
        let _ = open::that_detached(browse_url(bind, port));
    }
    let result = rt.block_on(async move {
        let ingest = spawn_ingest_if_enabled();
        let served = apb_server::run_server(bind, port).await;
        // The dashboard is the lifecycle owner: when it stops, so does the
        // listener it co-started.
        if let Some(handle) = ingest {
            handle.abort();
        }
        served
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if error_looks_like_addr_in_use(&e) {
                eprintln!("{}", port_in_use_message(DASHBOARD, port));
            } else {
                eprintln!("dashboard failed: {e}");
            }
            ExitCode::from(2)
        }
    }
}

/// Listener names for the bind diagnostics. They are read by an operator
/// staring at a port conflict, so each one has to name the listener that
/// actually failed: the co-start path used to say the dashboard failed while
/// the dashboard was serving fine on its own port.
const DASHBOARD: &str = "apb dashboard";
const INGEST: &str = "apb ingest";
const DEV_API: &str = "apb dev API server";

/// Resolves the ingest bind and port from an already-loaded ingest config
/// plus optional flags. Takes the config rather than loading one, so a caller
/// cannot gate on `enabled` from one read of `config.yaml` and bind from
/// another: an edit between the two reads (an operator disabling ingest, `apb
/// migrate`, an editor's atomic replace) would otherwise start a listener
/// whose address came from a file that no longer asks for one.
fn ingest_binding(
    ingest: &apb_core::config::IngestConfig,
    bind: Option<&str>,
    port: Option<u16>,
) -> Result<(IpAddr, u16), String> {
    Ok((ingest.resolve_bind(bind)?, ingest.resolve_port(port)))
}

/// Spawns the ingest listener when the config asks for it. Best effort by
/// design: a misconfigured ingest section must not stop the dashboard from
/// starting, so a failure is reported and the dashboard continues without an
/// inbound path. Reported, though, and never swallowed: a malformed
/// `config.yaml` used to leave the listener silently absent with no message
/// at all.
fn spawn_ingest_if_enabled() -> Option<tokio::task::JoinHandle<()>> {
    let cfg = match apb_core::config::GlobalConfig::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("apb ingest: not started: {e}");
            return None;
        }
    };
    if !cfg.ingest.enabled {
        return None;
    }
    let (bind, port) = match ingest_binding(&cfg.ingest, None, None) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("apb ingest: not started: {e}");
            return None;
        }
    };
    Some(tokio::spawn(async move {
        if let Err(e) = apb_server::ingest::run_ingest_server(bind, port).await {
            // Same diagnostic `ingest_cmd` and `dashboard` already use, named
            // for this listener: an address-in-use failure gets the
            // who-holds-this-port message rather than a raw `io::Error` the
            // operator has to decode themselves.
            if error_looks_like_addr_in_use(&e) {
                eprintln!("{}", port_in_use_message(INGEST, port));
            } else {
                eprintln!("apb ingest: listener stopped: {e}");
            }
        }
    }))
}

/// `apb ingest`: the inbound webhook listener on its own, for a headless
/// deployment that runs no dashboard. Same implementation the dashboard
/// co-starts, so the two paths cannot drift.
pub(crate) fn ingest_cmd(bind: Option<&str>, port: Option<u16>) -> ExitCode {
    let cfg = match apb_core::config::GlobalConfig::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("ingest failed: {e}");
            return ExitCode::from(2);
        }
    };
    let (bind, port) = match ingest_binding(&cfg.ingest, bind, port) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("ingest failed: {e}");
            return ExitCode::from(2);
        }
    };
    // Running the command is the intent, so a disabled config does not block
    // it; but `apb dashboard` will not co-start the listener until the flag
    // is set, and an operator who does not hear that will be surprised later.
    if !cfg.ingest.enabled {
        println!(
            "apb ingest: ingest.enabled is false in the global config, so `apb dashboard` will not start this listener on its own"
        );
    }
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    match rt.block_on(apb_server::ingest::run_ingest_server(bind, port)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if error_looks_like_addr_in_use(&e) {
                eprintln!("{}", port_in_use_message(INGEST, port));
            } else {
                eprintln!("ingest failed: {e}");
            }
            ExitCode::from(2)
        }
    }
}

/// The URL to open in a local browser for a given bind address. An
/// all-interfaces bind has no address of its own to visit, so the loopback
/// alias is used; any other bind is visited at its own address, IPv6
/// bracketed.
fn browse_url(bind: IpAddr, port: u16) -> String {
    let host = if bind.is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        bind
    };
    match host {
        IpAddr::V4(v4) => format!("http://{v4}:{port}"),
        IpAddr::V6(v6) => format!("http://[{v6}]:{port}"),
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

/// The full bind-failure message for `listener` on `port`, holder lookup
/// included. The one entry point every call site uses, so the diagnostic
/// cannot be wired up on one path and forgotten on another.
fn port_in_use_message(listener: &str, port: u16) -> String {
    let holders = lookup_port_holders(port);
    format_port_in_use_error(listener, port, holders.as_deref())
}

/// User-facing bind failure when the port is already taken. `listener` names
/// which listener failed, because `apb ingest` and the dashboard's co-started
/// listener share this message and a message that always said "dashboard"
/// pointed the operator at the wrong process. `holders` is a comma-separated
/// pid list from a best-effort lookup, or `None` when the holder could not be
/// determined. No automatic kill or takeover - only name the holder and hint
/// how to stop a stale instance.
fn format_port_in_use_error(listener: &str, port: u16, holders: Option<&str>) -> String {
    let holder_line = match holders {
        Some(pids) if !pids.is_empty() => {
            format!("{listener} failed: port {port} is already in use (held by pid {pids})")
        }
        _ => format!(
            "{listener} failed: port {port} is already in use (holder pid could not be determined)"
        ),
    };
    let hint = match holders {
        Some(pids) if !pids.is_empty() => {
            format!(
                "hint: another {listener} may already be running; stop it (for example: kill {pids}) and retry"
            )
        }
        _ => format!(
            "hint: another {listener} may already be running; stop the process listening on that port and retry"
        ),
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
        if let Err(e) = rt.block_on(apb_server::run_server(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            7321,
        )) {
            if error_looks_like_addr_in_use(&e) {
                eprintln!("{}", port_in_use_message(DEV_API, 7321));
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
    use super::{
        DASHBOARD, DEV_API, INGEST, error_looks_like_addr_in_use, format_port_in_use_error,
    };

    #[test]
    fn format_port_in_use_error_names_holder_pids() {
        let msg = format_port_in_use_error(DASHBOARD, 7321, Some("1234, 5678"));
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

    /// The message names the listener that actually failed.
    ///
    /// `apb ingest --port 7399` against a port another ingest holds used to
    /// print "dashboard failed", and on the dashboard co-start path it said
    /// the dashboard had failed while the dashboard was serving fine on 7321.
    #[test]
    fn format_port_in_use_error_names_the_listener_that_failed() {
        let ingest = format_port_in_use_error(INGEST, 7399, Some("87839"));
        assert!(
            ingest.contains("apb ingest failed: port 7399 is already in use"),
            "the ingest listener names itself: {ingest}"
        );
        assert!(
            !ingest.contains("dashboard"),
            "and never blames the dashboard: {ingest}"
        );
        assert!(
            ingest.contains("another apb ingest may already be running"),
            "the hint points at the same process: {ingest}"
        );

        let dev = format_port_in_use_error(DEV_API, 7321, None);
        assert!(dev.contains("apb dev API server failed"), "was: {dev}");
        assert!(!dev.contains('!'), "no exclamation marks: {dev}");
    }

    #[test]
    fn format_port_in_use_error_when_holder_unknown() {
        let msg = format_port_in_use_error(DASHBOARD, 7321, None);
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
    fn browse_url_maps_bind_to_a_visitable_address() {
        use super::browse_url;
        use std::net::{IpAddr, Ipv4Addr};
        assert_eq!(
            browse_url(IpAddr::V4(Ipv4Addr::LOCALHOST), 7321),
            "http://127.0.0.1:7321"
        );
        assert_eq!(
            browse_url(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 7321),
            "http://127.0.0.1:7321",
            "an all-interfaces bind is visited on loopback"
        );
        assert_eq!(
            browse_url("10.0.0.5".parse().unwrap(), 8080),
            "http://10.0.0.5:8080"
        );
        assert_eq!(
            browse_url("::1".parse().unwrap(), 7321),
            "http://[::1]:7321",
            "IPv6 hosts are bracketed"
        );
    }

    /// `ingest_binding` takes the config it resolves against, so this can
    /// assert the real function rather than the type it delegates to. It used
    /// to load the global config itself, which a unit test must not depend
    /// on, and the test that carried this name asserted `IngestConfig`
    /// directly and never reached `ingest_binding` at all.
    #[test]
    fn ingest_binding_falls_back_to_loopback_and_the_default_port() {
        use super::ingest_binding;
        use apb_core::config::{DEFAULT_INGEST_PORT, IngestConfig};
        use std::net::{IpAddr, Ipv4Addr};

        let cfg = IngestConfig::default();
        assert_eq!(
            ingest_binding(&cfg, None, None).unwrap(),
            (IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_INGEST_PORT)
        );
        // Flags win over the config, which wins over the defaults.
        assert_eq!(
            ingest_binding(&cfg, Some("10.0.0.5"), Some(7400)).unwrap(),
            ("10.0.0.5".parse::<IpAddr>().unwrap(), 7400)
        );
        let configured = IngestConfig {
            bind: Some("0.0.0.0".to_string()),
            port: Some(9000),
            ..IngestConfig::default()
        };
        assert_eq!(
            ingest_binding(&configured, None, None).unwrap(),
            (IpAddr::V4(Ipv4Addr::UNSPECIFIED), 9000)
        );
        assert_eq!(
            ingest_binding(&configured, Some("127.0.0.1"), Some(7400)).unwrap(),
            (IpAddr::V4(Ipv4Addr::LOCALHOST), 7400)
        );
        // An unparseable address is an error, never a silent fallback.
        let err = ingest_binding(&cfg, Some("not-an-ip"), None).unwrap_err();
        assert!(
            err.contains("not-an-ip"),
            "the error names the value: {err}"
        );
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
