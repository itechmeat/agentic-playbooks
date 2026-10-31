//! `apb make-global`: makes a project playbook available in every project
//! on this machine. A thin dispatch over `apb_mcp::promote`, the same code
//! MCP `playbook_make_global` runs.

use std::path::Path;
use std::process::ExitCode;

use apb_mcp::promote::{MakeGlobalOpts, Variant, make_global};
use serde_json::Value;

use crate::util::{print_json, sanitize_for_terminal};

pub(crate) struct MakeGlobalArgs {
    pub id: String,
    pub variant: Option<String>,
    pub move_profiles: bool,
    pub dry_run: bool,
    pub json: bool,
}

pub(crate) fn make_global_cmd(root: &Path, args: MakeGlobalArgs) -> ExitCode {
    let variant = match args.variant.as_deref() {
        None => None,
        Some(v) => match Variant::parse(v) {
            Some(v) => Some(v),
            None => {
                eprintln!("unknown --variant `{v}`; use trash, retire or keep");
                return ExitCode::from(2);
            }
        },
    };
    let opts = MakeGlobalOpts {
        variant,
        move_profiles: args.move_profiles,
        dry_run: args.dry_run,
    };
    let out = match make_global(root, &args.id, opts) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("make-global failed: {e}");
            return ExitCode::from(2);
        }
    };
    let blocked = out["plan"]["blockers"]
        .as_array()
        .is_some_and(|b| !b.is_empty());
    let refused = out.get("error").is_some();
    if args.json {
        print_json(&out);
    } else {
        print_outcome(&out);
    }
    if blocked || refused {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn print_outcome(out: &Value) {
    let plan = &out["plan"];
    for (title, key) in [
        ("blockers", "blockers"),
        ("actions", "actions"),
        ("warnings", "warnings"),
    ] {
        print_items(title, &plan[key]);
    }
    if let Some(e) = out["error"].as_str() {
        eprintln!(
            "refused ({}): {}",
            sanitize_for_terminal(e, 64),
            sanitize_for_terminal(out["detail"].as_str().unwrap_or_default(), 300)
        );
        return;
    }
    if out["dry_run"] == true {
        println!("dry run: nothing changed");
        return;
    }
    if out["applied"] != true {
        println!("nothing changed: resolve the blockers first");
        return;
    }
    for w in out["warnings"].as_array().into_iter().flatten() {
        println!(
            "warning: {}",
            sanitize_for_terminal(w.as_str().unwrap_or(""), 300)
        );
    }
    let id = sanitize_for_terminal(out["id"].as_str().unwrap_or(""), 128);
    let variant = out["variant"].as_str().unwrap_or("");
    let fit = &out["fit"];
    if fit.is_null() {
        println!("{id} is global now (project copy: {variant})");
        return;
    }
    println!(
        "{id} is global now (project copy: {variant}); here it is {} and {}",
        if fit["ready"] == true {
            "ready"
        } else {
            "not ready"
        },
        if fit["trusted"] == true {
            "trusted"
        } else {
            "not trusted"
        },
    );
}

fn print_items(title: &str, items: &Value) {
    let Some(items) = items.as_array().filter(|a| !a.is_empty()) else {
        return;
    };
    println!("{title}:");
    for i in items {
        let subject = i["subject"]
            .as_str()
            .map(|s| format!(" [{}]", sanitize_for_terminal(s, 128)))
            .unwrap_or_default();
        println!(
            "  {}{subject}: {}",
            i["code"].as_str().unwrap_or(""),
            sanitize_for_terminal(i["detail"].as_str().unwrap_or(""), 300)
        );
    }
}
