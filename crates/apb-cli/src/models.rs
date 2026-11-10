//! `apb models [--refresh] [--json]`: the cloud model catalogs apb knows
//! (`apb_core::cloud_models`), each with its source, age and rows.
//! `--refresh` fetches each provider's public listing first; no credential
//! is ever sent. A person's command: refused inside a run step.

use std::process::ExitCode;

use apb_core::cloud_models::{self, catalog};

pub(crate) fn models_cmd(refresh: bool, json: bool) -> ExitCode {
    if let Some(why) = apb_core::step_env::StepCaller::from_env().refusal("`apb models`") {
        eprintln!("{why}");
        return ExitCode::from(3);
    }
    let mut failed = false;
    if refresh {
        for err in catalog::refresh_all().into_iter().filter_map(Result::err) {
            eprintln!("{err}");
            failed = true;
        }
    }
    let snaps = catalog::load_all();
    if json {
        let rows: Vec<serde_json::Value> = snaps
            .iter()
            .map(|s| {
                let p = cloud_models::lookup(&s.provider);
                serde_json::json!({
                    "provider": s.provider,
                    "display": p.map(|p| p.display),
                    "key_env": p.map(|p| p.key_env),
                    "key_present": p.is_some_and(|p| std::env::var_os(p.key_env).is_some_and(|v| !v.is_empty())),
                    "source": s.source,
                    "fetched_at": s.fetched_at,
                    "age_days": catalog::age_days(s),
                    "digest": s.digest,
                    "dropped": s.dropped,
                    "cache_problem": s.cache_problem,
                    "models": s.models,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "cloud": rows })).unwrap_or_default()
        );
    } else {
        for s in &snaps {
            print_text(s);
        }
    }
    if failed {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn price(p: Option<f64>) -> String {
    p.map_or_else(|| "-".to_string(), |v| format!("{v}"))
}

fn print_text(s: &catalog::CatalogSnapshot) {
    let display = cloud_models::lookup(&s.provider).map_or(s.provider.as_str(), |p| p.display);
    let age = catalog::age_days(s).map_or_else(String::new, |d| format!(", {d} days old"));
    println!(
        "{display} ({}): {} catalog fetched {}{age}, {} models{}",
        s.provider,
        match s.source {
            catalog::CatalogSource::Embedded => "embedded",
            catalog::CatalogSource::Refreshed => "refreshed",
        },
        s.fetched_at,
        s.models.len(),
        if s.dropped > 0 {
            format!(", {} rows refused", s.dropped)
        } else {
            String::new()
        }
    );
    if let Some(problem) = &s.cache_problem {
        println!("  note: {problem}; the embedded catalog is used");
    }
    for m in &s.models {
        println!(
            "  {}  {}  tools={}  in={} out={} USD/Mtok",
            m.id,
            m.privacy_class.as_str(),
            if m.features.tools { "yes" } else { "no" },
            price(m.price_in_usd_mtok),
            price(m.price_out_usd_mtok),
        );
    }
}
