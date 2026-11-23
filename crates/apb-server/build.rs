//! Stops a build of the server crate without the built dashboard, so a
//! checkout that never ran the dashboard build cannot compile to a binary
//! with an empty dashboard. The dashboard build writes
//! `crates/apb-server/dashboard/`, which `src/assets.rs` embeds.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    println!("cargo:rerun-if-changed=dashboard");
    if Path::new("dashboard/index.html").is_file() {
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "the dashboard is not built: run (cd web && bun install --frozen-lockfile && bun run build) \
         from the repository root; it writes crates/apb-server/dashboard"
    );
    ExitCode::FAILURE
}
