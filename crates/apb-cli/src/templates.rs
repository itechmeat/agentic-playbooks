//! `apb templates list | show | install`: the example playbooks apb ships,
//! as drafts. A template is a starting point a person adapts: the install
//! approves nothing, and the answer names what the new draft still needs
//! (the fit report) and the commands that run it.

use std::path::Path;
use std::process::ExitCode;

use apb_core::registry::Registry;
use clap::Args;

use crate::util::{print_json, sanitize_command, sanitize_for_terminal};

#[derive(Args)]
pub(crate) struct TemplatesArgs {
    #[command(subcommand)]
    cmd: TemplatesCmd,
}

#[derive(clap::Subcommand)]
enum TemplatesCmd {
    /// List the templates
    List {
        #[arg(long)]
        json: bool,
    },
    /// Print a template's definition
    Show { id: String },
    /// Install a template as a draft
    Install {
        id: String,
        /// Where the draft goes: project (default) or global
        #[arg(long, default_value = "project")]
        scope: String,
        /// The playbook id to install as
        #[arg(long = "as", value_name = "ID")]
        as_id: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn templates_cmd(root: &Path, args: TemplatesArgs) -> ExitCode {
    match args.cmd {
        TemplatesCmd::List { json } => list_cmd(json),
        TemplatesCmd::Show { id } => show_cmd(&id),
        TemplatesCmd::Install {
            id,
            scope,
            as_id,
            json,
        } => install_cmd(root, &id, &scope, as_id.as_deref(), json),
    }
}

fn list_cmd(as_json: bool) -> ExitCode {
    let v = apb_mcp::tools::templates::list();
    if as_json {
        print_json(&v);
        return ExitCode::SUCCESS;
    }
    for t in v["templates"].as_array().unwrap_or(&Vec::new()) {
        println!(
            "{}\t{}\t{}\t{}",
            t["id"].as_str().unwrap_or(""),
            t["version"].as_str().unwrap_or(""),
            t["effects"]
                .as_array()
                .map(|a| a
                    .iter()
                    .filter_map(|e| e.as_str())
                    .collect::<Vec<_>>()
                    .join(","))
                .unwrap_or_default(),
            sanitize_for_terminal(t["name"].as_str().unwrap_or(""), 120),
        );
    }
    ExitCode::SUCCESS
}

fn show_cmd(id: &str) -> ExitCode {
    match apb_mcp::tools::templates::show(id) {
        Ok(v) => {
            print!("{}", v["yaml"].as_str().unwrap_or(""));
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("template `{}` not found", sanitize_for_terminal(id, 120));
            ExitCode::from(2)
        }
    }
}

fn install_cmd(root: &Path, id: &str, scope: &str, as_id: Option<&str>, as_json: bool) -> ExitCode {
    // The fit report is made only when the person named a project: `list`
    // and `show` need no project, a global install proceeds without one.
    let with_fit = Registry::open(root).is_ok();
    if scope != "global" && !with_fit {
        eprintln!("no project here (run `apb init`)");
        return ExitCode::from(2);
    }
    let req = apb_mcp::tools::templates::InstallRequest {
        template: id,
        scope,
        as_id,
        with_fit,
    };
    match apb_mcp::tools::templates::install(root, req) {
        Ok(v) => {
            if let Some(rejected) = v.get("rejected") {
                if rejected == "duplicate_id" {
                    eprintln!(
                        "playbook `{}` already exists in the {scope} scope; pass --as <other id>",
                        sanitize_for_terminal(v["id"].as_str().unwrap_or(id), 120)
                    );
                    return ExitCode::from(1);
                }
                eprintln!("install rejected: {rejected}");
                return ExitCode::from(2);
            }
            if as_json {
                print_json(&v);
                return ExitCode::SUCCESS;
            }
            let installed = sanitize_for_terminal(v["id"].as_str().unwrap_or(id), 120);
            let template = sanitize_for_terminal(v["template"].as_str().unwrap_or(id), 120);
            println!(
                "installed template {template} as draft {installed} {} ({scope})",
                v["version"].as_str().unwrap_or("1.0.0")
            );
            for n in v["needs"].as_array().unwrap_or(&Vec::new()) {
                if let Some(t) = n.as_str() {
                    println!("  needs: {}", sanitize_for_terminal(t, 200));
                }
            }
            for w in v["warnings"].as_array().unwrap_or(&Vec::new()) {
                if let Some(t) = w.as_str() {
                    println!("  warning: {}", sanitize_for_terminal(t, 200));
                }
            }
            // The next commands are copied into a shell: the exact id,
            // quoted, through the command sanitizer, which keeps spaces.
            let word =
                apb_engine::next_action::command::shell_quote(v["id"].as_str().unwrap_or(id));
            let adopt = sanitize_command(&format!("apb adopt {word} --scope {scope}"));
            let run = sanitize_command(&format!("apb run {word} --scope {scope}"));
            println!("next: {adopt}");
            println!(
                "next: {run} (a normal start needs the draft approved; a trial runs it without activating it)"
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!(
                "templates install failed: {}",
                sanitize_for_terminal(&e.to_string(), 300)
            );
            ExitCode::from(2)
        }
    }
}
