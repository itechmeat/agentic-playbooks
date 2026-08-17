//! `apb server` subcommands (spec 2026-08-16-server-mode-design): the API keys
//! that authenticate a networked dashboard. A thin dispatch over
//! `apb_core::server_auth`, which owns the file format and the crypto.
//!
//! The plaintext key crosses this module exactly once, on the stdout of
//! `issue`. It is never written to a log line, never echoed by `list`, and
//! never included in an error message.

use std::process::ExitCode;

use apb_core::server_auth;
use clap::Subcommand;
use serde_json::json;

use crate::util::{print_json, print_table};

#[derive(Subcommand)]
pub(crate) enum ServerAction {
    /// Manage the API keys that authenticate the dashboard and its API
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum KeyAction {
    /// Issue a key and print it once. At most two keys exist at a time, which
    /// is the rotation window: issue the second, move clients over, revoke the
    /// first.
    Issue,
    /// Show key ids and creation times. Never the keys themselves.
    List {
        /// Machine-readable output for scripts
        #[arg(long)]
        json: bool,
    },
    /// Revoke a key by id (see `apb server key list`)
    Revoke { id: String },
}

pub(crate) fn server_cmd(action: ServerAction) -> ExitCode {
    match action {
        ServerAction::Key { action } => match action {
            KeyAction::Issue => issue_cmd(),
            KeyAction::List { json } => list_cmd(json),
            KeyAction::Revoke { id } => revoke_cmd(&id),
        },
    }
}

fn issue_cmd() -> ExitCode {
    match server_auth::issue() {
        Ok((key, record)) => {
            println!("{key}");
            println!();
            println!("key id: {}", record.id);
            println!("This key is shown once and is stored only as a hash. Save it now.");
            println!(
                "A running dashboard picks the change up within a minute, and immediately after any failed request."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("apb server key issue: {e}");
            ExitCode::from(2)
        }
    }
}

fn list_cmd(as_json: bool) -> ExitCode {
    let keys = match server_auth::load() {
        Ok(file) => file.keys,
        Err(e) => {
            eprintln!("apb server key list: {e}");
            return ExitCode::from(2);
        }
    };
    if as_json {
        let rows: Vec<serde_json::Value> = keys
            .iter()
            .map(|k| json!({ "id": k.id, "created_at": k.created_at }))
            .collect();
        print_json(&json!({ "keys": rows }));
        return ExitCode::SUCCESS;
    }
    if keys.is_empty() {
        println!(
            "no server keys; the dashboard runs unauthenticated and may only bind the loopback interface"
        );
        println!("issue one with `apb server key issue`");
        return ExitCode::SUCCESS;
    }
    let mut rows = vec![vec!["ID".to_string(), "CREATED".to_string()]];
    for k in &keys {
        rows.push(vec![k.id.clone(), k.created_at.clone()]);
    }
    print_table(&rows);
    ExitCode::SUCCESS
}

fn revoke_cmd(id: &str) -> ExitCode {
    match server_auth::revoke(id) {
        Ok(record) => {
            println!("revoked key {} (created {})", record.id, record.created_at);
            println!(
                "A running dashboard stops accepting it within a minute, and immediately on its next use."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("apb server key revoke: {e}");
            ExitCode::from(2)
        }
    }
}
