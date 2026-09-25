//! `apb trust` subcommands: the approvals in the global trust store. A thin
//! dispatch over `apb_core::trust::TrustStore::{entries, revoke}`, the same
//! path the dashboard's Trust view and the MCP trust tools use.

use std::process::ExitCode;

use apb_core::trust::{Kind, TrustEntry, TrustSelector, TrustStore};
use clap::Subcommand;

use crate::util::{print_json, print_table};

#[derive(Subcommand)]
pub(crate) enum TrustAction {
    /// List every approval: kind, id, origin, approval time and digest
    List {
        /// Only this kind: playbook, profile_bundle, connector or
        /// connector_account
        #[arg(long)]
        kind: Option<String>,
        /// Machine-readable output for scripts
        #[arg(long)]
        json: bool,
    },
    /// Revoke approvals. TARGET is a digest (`sha256:...`, exactly that
    /// approval) or an id (every approval recorded under it: all versions of
    /// a playbook, all bundles of a profile). The content then needs a new
    /// approval, or an acknowledge, to run through the MCP gate.
    /// Exit codes: 0 revoked, 2 nothing matched or error
    Revoke {
        target: String,
        /// With an id: only approvals of this kind
        #[arg(long)]
        kind: Option<String>,
        /// Machine-readable output for scripts
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn trust_cmd(action: TrustAction) -> ExitCode {
    match action {
        TrustAction::List { kind, json } => match parse_kind(kind.as_deref()) {
            Ok(kind) => list_cmd(kind, json),
            Err(code) => code,
        },
        TrustAction::Revoke { target, kind, json } => match parse_kind(kind.as_deref()) {
            Ok(kind) => revoke_cmd(&target, kind, json),
            Err(code) => code,
        },
    }
}

fn parse_kind(kind: Option<&str>) -> Result<Option<Kind>, ExitCode> {
    match kind {
        None => Ok(None),
        Some(k) => Kind::parse(k).map(Some).ok_or_else(|| {
            eprintln!(
                "unknown kind `{k}`; use playbook, profile_bundle, connector or connector_account"
            );
            ExitCode::from(2)
        }),
    }
}

fn list_cmd(kind: Option<Kind>, as_json: bool) -> ExitCode {
    let entries: Vec<TrustEntry> = TrustStore::load()
        .entries()
        .into_iter()
        .filter(|e| kind.is_none_or(|k| k == e.kind))
        .collect();
    if as_json {
        print_json(&serde_json::to_value(&entries).unwrap_or_default());
        return ExitCode::SUCCESS;
    }
    if entries.is_empty() {
        println!("no approvals");
        return ExitCode::SUCCESS;
    }
    print_entries(&entries);
    ExitCode::SUCCESS
}

fn revoke_cmd(target: &str, kind: Option<Kind>, as_json: bool) -> ExitCode {
    let selector = TrustSelector::parse(target, kind);
    let removed = match TrustStore::load().revoke(&selector) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("trust revoke failed: {e}");
            return ExitCode::from(2);
        }
    };
    if as_json {
        print_json(&serde_json::to_value(&removed).unwrap_or_default());
    } else if !removed.is_empty() {
        println!("revoked {} approval(s):", removed.len());
        print_entries(&removed);
    }
    if removed.is_empty() {
        eprintln!("no approval matches `{target}`");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

fn print_entries(entries: &[TrustEntry]) {
    let mut rows: Vec<Vec<String>> = vec![vec![
        "KIND".to_string(),
        "ID".to_string(),
        "ORIGIN".to_string(),
        "APPROVED".to_string(),
        "DIGEST".to_string(),
    ]];
    for e in entries {
        rows.push(vec![
            e.kind.as_str().to_string(),
            e.id.clone(),
            e.origin_kind.as_str().to_string(),
            apb_core::dismiss::iso_utc(u64::try_from(e.approved_at_ms).unwrap_or(u64::MAX)),
            e.digest.clone(),
        ]);
    }
    print_table(&rows);
}
