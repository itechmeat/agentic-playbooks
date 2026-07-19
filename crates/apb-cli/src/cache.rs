//! `apb cache` subcommands (node-cache design, Task 8): inspect and manage
//! the project-local node result cache store (`apb_core::cache::CacheStore`)
//! directly - a thin dispatch layer for a human or an agent debugging cache
//! behavior outside a run. All output is plain text (or, for `inspect`,
//! pretty JSON of the record itself); every parse failure names the bad
//! value rather than silently falling back to an unbounded or disabled
//! setting.

use std::path::Path;
use std::process::ExitCode;

use apb_core::cache::CacheStore;
use apb_core::duration::parse_duration_str;
use clap::Subcommand;

use crate::util::open_registry;

#[derive(Subcommand)]
pub(crate) enum CacheCmd {
    /// Record and object counts and total size
    Status,
    /// Print one record as JSON
    Inspect { key: String },
    /// Remove old records and unreferenced objects
    Prune {
        /// Remove records older than this: `parse_duration_str` format - a
        /// plain integer of seconds, or an integer with a single `s`/`m`/
        /// `h`/`d` suffix (e.g. `30s`, `5m`, `2h`, `7d`)
        #[arg(long)]
        older_than: Option<String>,
        /// Remove the oldest records until referenced object bytes are at or
        /// under this budget: a plain integer of bytes, or an integer with a
        /// single `k`/`m`/`g` suffix (1024-based; e.g. `500k`, `100m`, `1g`)
        #[arg(long)]
        max_size: Option<String>,
    },
    /// Remove the entire cache
    Clear,
}

/// Parses a `--max-size` value. Mirrors `parse_duration_str`'s shape but
/// with 1024-based `k`/`m`/`g` suffixes instead of time units. Returns
/// `None` for anything else (empty, non-numeric, unknown suffix, or an
/// overflowing multiplication) so the caller can report the exact bad value
/// instead of quietly defaulting to no limit.
fn parse_size_str(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let (num, mult) = match s.as_bytes().last()? {
        b'k' => (&s[..s.len() - 1], 1024u64),
        b'm' => (&s[..s.len() - 1], 1024 * 1024),
        b'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => return None,
    };
    num.trim().parse::<u64>().ok()?.checked_mul(mult)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn cache_cmd(root: &Path, cmd: CacheCmd) -> ExitCode {
    if let Err(code) = open_registry(root) {
        return code;
    }
    let store = CacheStore::open(root);
    match cmd {
        CacheCmd::Status => cache_status(&store),
        CacheCmd::Inspect { key } => cache_inspect(&store, &key),
        CacheCmd::Prune {
            older_than,
            max_size,
        } => cache_prune(&store, older_than, max_size),
        CacheCmd::Clear => cache_clear(&store),
    }
}

fn cache_status(store: &CacheStore) -> ExitCode {
    let s = store.status();
    println!("records: {}", s.records);
    println!("objects: {}", s.objects);
    println!("total size: {} bytes", s.total_bytes);
    ExitCode::SUCCESS
}

fn cache_inspect(store: &CacheStore, key: &str) -> ExitCode {
    match store.inspect(key) {
        Some(record) => match serde_json::to_string_pretty(&record) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("cache inspect failed: {e}");
                ExitCode::from(2)
            }
        },
        None => {
            eprintln!("cache inspect failed: no record for key `{key}`");
            ExitCode::from(2)
        }
    }
}

fn cache_prune(
    store: &CacheStore,
    older_than: Option<String>,
    max_size: Option<String>,
) -> ExitCode {
    let older_than_secs = match older_than {
        Some(s) => match parse_duration_str(&s) {
            Some(v) => Some(v),
            None => {
                eprintln!("cache prune failed: invalid --older-than value `{s}`");
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let max_bytes = match max_size {
        Some(s) => match parse_size_str(&s) {
            Some(v) => Some(v),
            None => {
                eprintln!("cache prune failed: invalid --max-size value `{s}`");
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let report = store.prune(older_than_secs, max_bytes, unix_now());
    println!("removed records: {}", report.removed_records);
    println!("removed objects: {}", report.removed_objects);
    ExitCode::SUCCESS
}

fn cache_clear(store: &CacheStore) -> ExitCode {
    match store.clear() {
        Ok(()) => {
            println!("cache cleared");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cache clear failed: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_str_accepts_plain_bytes_and_suffixes() {
        assert_eq!(parse_size_str("500"), Some(500));
        assert_eq!(parse_size_str("500k"), Some(500 * 1024));
        assert_eq!(parse_size_str("100m"), Some(100 * 1024 * 1024));
        assert_eq!(parse_size_str("1g"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size_str("  2m  "), Some(2 * 1024 * 1024));
        assert_eq!(parse_size_str("2M"), Some(2 * 1024 * 1024));
    }

    #[test]
    fn parse_size_str_rejects_bad_values() {
        assert_eq!(parse_size_str(""), None);
        assert_eq!(parse_size_str("5x"), None);
        assert_eq!(parse_size_str("1.5m"), None);
        assert_eq!(parse_size_str("m"), None);
        assert_eq!(parse_size_str("k"), None);
    }

    #[test]
    fn parse_size_str_rejects_overflow() {
        assert_eq!(parse_size_str("99999999999999999999g"), None);
    }
}
