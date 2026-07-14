use std::path::Path;
use std::process::ExitCode;

use apb_core::registry::Registry;

pub(crate) fn print_json(v: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    );
}

pub(crate) fn open_registry(root: &Path) -> Result<Registry, ExitCode> {
    Registry::open(root).map_err(|e| {
        eprintln!("no project here: {e} (run `apb init`)");
        ExitCode::from(2)
    })
}

pub(crate) fn resolve_port(flag: Option<u16>) -> u16 {
    flag.or_else(|| {
        apb_core::config::GlobalConfig::load()
            .ok()
            .and_then(|c| c.port)
    })
    .unwrap_or(7321)
}
