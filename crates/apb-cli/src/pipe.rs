//! A closed stdout ends the CLI quietly, like other command line tools:
//! `apb runs | head` stops reading after its lines, the next `println!`
//! fails with a broken pipe, and that write panics in std. One panic hook
//! for the whole binary turns exactly that panic into a silent exit with
//! the status a process killed by `SIGPIPE` reports (141), and leaves
//! every other panic to the default hook. Only the main thread exits so: a
//! background printer (the host task announcer of a foreground run) that
//! loses its stream ends alone, as it did before, and the run goes on.
//! `SIGPIPE` itself stays ignored,
//! so the dashboard and the MCP server still see a closed socket as an
//! error instead of being killed by it.

/// The exit status of a process `SIGPIPE` ended (128 + 13).
pub(crate) const BROKEN_PIPE_EXIT: i32 = 141;

/// Installs the hook; call once, first thing in `main`.
pub(crate) fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info.payload_as_str().is_some_and(is_closed_stdout)
            && std::thread::current().name() == Some("main")
        {
            std::process::exit(BROKEN_PIPE_EXIT);
        }
        default(info);
    }));
}

/// Whether a panic message is std's failed print to a closed stdout or
/// stderr.
fn is_closed_stdout(message: &str) -> bool {
    (message.starts_with("failed printing to stdout")
        || message.starts_with("failed printing to stderr"))
        && message.contains("Broken pipe")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_print_to_a_closed_pipe_is_quiet() {
        assert!(is_closed_stdout(
            "failed printing to stdout: Broken pipe (os error 32)"
        ));
        assert!(is_closed_stdout(
            "failed printing to stderr: Broken pipe (os error 32)"
        ));
        assert!(!is_closed_stdout(
            "failed printing to stdout: No space left on device (os error 28)"
        ));
        assert!(!is_closed_stdout("index out of bounds"));
    }
}
