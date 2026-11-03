//! `apb demo`: installs the keyless demo in `<config dir>/demo/` (a
//! project of its own, registered for the dashboard), and with `--run`
//! starts it as `apb run demo-report --detach` there would. `--reset`
//! reinstalls a demo whose files differ from the shipped ones; `--remove`
//! deletes the directory and unregisters the project, refused while a run
//! of the demo holds its busy lock. A thin dispatch over
//! `apb_core::demo`; the run is an ordinary start, with the same
//! acknowledgement of a playbook nobody approved as any `apb run`.

use std::process::ExitCode;

use apb_core::demo::{DEMO_ID, DemoProject};
use clap::Args;

use crate::util::print_json;

#[derive(Args)]
pub(crate) struct DemoArgs {
    /// Start the demo run once it is installed
    #[arg(long, conflicts_with = "remove")]
    run: bool,
    /// Reinstall a demo whose files differ from the shipped ones (its
    /// definition goes to the project's trash)
    #[arg(long, conflicts_with = "remove")]
    reset: bool,
    /// Unregister the demo project and delete its directory
    #[arg(long)]
    remove: bool,
    /// Machine-readable output
    #[arg(long)]
    json: bool,
}

pub(crate) fn demo_cmd(args: DemoArgs) -> ExitCode {
    let Some(config) = apb_core::config::config_dir() else {
        eprintln!("demo failed: no config directory on this machine");
        return ExitCode::from(2);
    };
    let dir = apb_core::demo::demo_dir(&config);
    if args.remove {
        // A run that drives the demo (waiting at its review, or paused
        // with its lock kept) holds its busy lock: removing is refused,
        // and holding the lock keeps a new run from starting meanwhile.
        let _busy = match dir.join(".apb").is_dir() {
            true => match apb_engine::workdir::acquire(&dir, false) {
                Ok(guard) => guard,
                Err(e) => {
                    eprintln!("demo failed: a run of the demo is active ({e}); stop it first");
                    return ExitCode::from(2);
                }
            },
            false => None,
        };
        return match apb_core::demo::remove(&dir) {
            Ok(removed) => {
                if args.json {
                    print_json(&serde_json::json!({ "removed": removed }));
                } else if removed {
                    println!("demo removed: {}", dir.display());
                } else {
                    println!("no demo installed");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("demo failed: {e}");
                ExitCode::from(2)
            }
        };
    }
    let project = match apb_core::demo::install(&dir, args.reset) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("demo failed: {e}");
            return ExitCode::from(2);
        }
    };
    if !args.run {
        if args.json {
            print_json(&serde_json::json!(project));
        } else {
            print_installed(&project);
        }
        return ExitCode::SUCCESS;
    }
    start(&project)
}

fn print_installed(p: &DemoProject) {
    println!("demo installed: {}", p.dir);
    println!("  playbook {} {}", p.playbook, p.version);
    println!("  start it: apb demo --run (or Run on its page in the dashboard)");
}

/// Starts the demo detached, as `apb run demo-report --detach` in its
/// directory, and says how to follow it to the review.
fn start(p: &DemoProject) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("demo failed: {e}");
            return ExitCode::from(2);
        }
    };
    let out = std::process::Command::new(exe)
        .args(["run", DEMO_ID, "--detach"])
        .current_dir(&p.dir)
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            eprintln!("demo failed: {e}");
            return ExitCode::from(2);
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    print!("{stdout}");
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        return ExitCode::from(2);
    }
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .map(str::trim);
    if let Some(id) = run_id {
        println!("in {}:", p.dir);
        println!("  apb wait {id}   (it stops at the review)");
        println!("  apb review {id} review --decision approve   (or reject)");
        println!("  then read report.md");
    }
    ExitCode::SUCCESS
}
