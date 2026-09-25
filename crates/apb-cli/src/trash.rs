//! `apb trash` subcommands: the playbook trash of the current project. A thin
//! dispatch over `apb_core::versioning::{list_trash, restore_from_trash}`, the
//! same path the dashboard's Trash view and the MCP trash tools use.

use std::path::Path;
use std::process::ExitCode;

use apb_core::versioning::{VersioningError, list_trash, restore_from_trash};
use clap::Subcommand;

use crate::util::{open_registry, print_json, print_table};

#[derive(Subcommand)]
pub(crate) enum TrashAction {
    /// List deleted playbooks, newest deletion first
    List {
        /// Machine-readable output for scripts
        #[arg(long)]
        json: bool,
    },
    /// Restore a deleted playbook with all its versions. NAME is a trash
    /// entry from `apb trash list` or a playbook id (its latest deletion).
    /// Exit codes: 0 restored, 1 a playbook with that id exists again,
    /// 2 not found or error
    Restore { name: String },
}

pub(crate) fn trash_cmd(root: &Path, action: TrashAction) -> ExitCode {
    // Outside a project there is no trash to show: say so, like `apb list`.
    if let Err(code) = open_registry(root) {
        return code;
    }
    match action {
        TrashAction::List { json } => list_cmd(root, json),
        TrashAction::Restore { name } => restore_cmd(root, &name),
    }
}

fn list_cmd(root: &Path, as_json: bool) -> ExitCode {
    let entries = match list_trash(root) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("trash list failed: {e}");
            return ExitCode::from(2);
        }
    };
    if as_json {
        print_json(&serde_json::to_value(&entries).unwrap_or_default());
        return ExitCode::SUCCESS;
    }
    if entries.is_empty() {
        println!("the trash is empty");
        return ExitCode::SUCCESS;
    }
    let mut rows: Vec<Vec<String>> = vec![vec![
        "NAME".to_string(),
        "ID".to_string(),
        "DELETED".to_string(),
        "VERSIONS".to_string(),
        "NOTE".to_string(),
    ]];
    for e in &entries {
        rows.push(vec![
            e.name.clone(),
            e.id.clone(),
            apb_core::dismiss::iso_utc(u64::try_from(e.deleted_at_ms).unwrap_or(u64::MAX)),
            e.versions.join(", "),
            if e.conflict {
                "id in use again".to_string()
            } else {
                String::new()
            },
        ]);
    }
    print_table(&rows);
    ExitCode::SUCCESS
}

fn restore_cmd(root: &Path, name: &str) -> ExitCode {
    match restore_from_trash(root, name) {
        Ok(r) => {
            println!(
                "restored {} from {} (current: {}, versions: {})",
                r.id,
                r.name,
                r.current.as_deref().unwrap_or("-"),
                r.versions.join(", ")
            );
            ExitCode::SUCCESS
        }
        Err(VersioningError::Conflict(m)) => {
            eprintln!("restore refused: {m}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("restore failed: {e}");
            ExitCode::from(2)
        }
    }
}
