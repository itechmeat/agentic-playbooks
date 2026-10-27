//! `apb runs <id> --html <path|->`: the run report as one self-contained
//! page (no script, no network, its stylesheet pinned by hash), the same
//! page the dashboard's "Save report" downloads and MCP `run_report` with
//! format html writes. `-` prints it to standard output.

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use apb_core::registry::is_safe_segment;

pub(crate) fn html_cmd(root: &Path, run_id: &str, out: &Path) -> ExitCode {
    if !is_safe_segment(run_id) {
        eprintln!("runs: invalid run id `{run_id}`");
        return ExitCode::from(2);
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        eprintln!("runs: run `{run_id}` not found");
        return ExitCode::from(2);
    }
    let html = match apb_engine::report_html::render(root, &run_dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("runs: cannot render the report: {e}");
            return ExitCode::from(2);
        }
    };
    if out == Path::new("-") {
        let mut stdout = std::io::stdout().lock();
        return match stdout.write_all(html.as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("runs: cannot write the report: {e}");
                ExitCode::from(2)
            }
        };
    }
    match apb_core::fsutil::atomic_write(out, html.as_bytes()) {
        Ok(()) => {
            eprintln!("report written: {} ({} bytes)", out.display(), html.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("runs: cannot write {}: {e}", out.display());
            ExitCode::from(2)
        }
    }
}
