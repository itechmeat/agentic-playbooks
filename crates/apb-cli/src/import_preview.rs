//! What `apb import` prints before and after an import: the preview of a
//! bundle (every bundle-derived string sanitized for the terminal), the
//! y/N question on a terminal, and how the imported version gets approved.

use std::io::{BufRead, Write};
use std::path::Path;

use apb_core::bundle::ImportPreview;

use crate::util::sanitize_for_terminal;

fn names(list: &[String]) -> String {
    list.iter()
        .map(|s| sanitize_for_terminal(s, 128))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Prints the preview of an import.
pub(crate) fn print_preview(p: &ImportPreview) {
    let id = sanitize_for_terminal(&p.id, 128);
    let version = if p.version.is_empty() {
        "(assigned on import)".to_string()
    } else {
        sanitize_for_terminal(&p.version, 64)
    };
    let state = if p.unchanged {
        "unchanged: the current version already holds this bundle"
    } else if p.new_playbook {
        "new playbook"
    } else {
        "new version of an existing playbook"
    };
    println!("bundle {id} @ {version}: {state}");
    println!("bundle digest: {}", p.bundle_digest);
    if let Some(o) = &p.origin {
        println!(
            "exported from: {} scope, apb {}, digest {}",
            sanitize_for_terminal(&o.scope, 32),
            sanitize_for_terminal(&o.apb_version, 32),
            sanitize_for_terminal(&o.digest, 80)
        );
    }
    if p.version_taken {
        println!("the version {version} already exists: the import would be refused");
    }
    if let Some(d) = &p.definition {
        for (label, list) in [
            ("nodes added", &d.nodes_added),
            ("nodes removed", &d.nodes_removed),
            ("nodes changed", &d.nodes_changed),
            ("edges added", &d.edges_added),
            ("edges removed", &d.edges_removed),
        ] {
            if !list.is_empty() {
                println!("{label}: {}", names(list));
            }
        }
        // The line diff keeps the common lines as context: the definition is
        // the same when no line was added or removed.
        if d.yaml_diff.lines().all(|l| l.starts_with(' ')) {
            println!("definition: same as the current version");
        }
    }
    let s = &p.scripts;
    for (label, list) in [
        ("scripts added", &s.added),
        ("scripts removed", &s.removed),
        ("scripts changed", &s.changed),
    ] {
        if !list.is_empty() {
            println!("{label}: {}", names(list));
        }
    }
    if s.unchanged > 0 {
        println!("scripts unchanged: {}", s.unchanged);
    }
    for e in &p.errors {
        println!(
            "error {}: {}",
            e.code,
            sanitize_for_terminal(&e.message, 300)
        );
    }
    if p.needs_script_approval {
        println!(
            "the imported version carries scripts: it runs only after an approval that names its digest"
        );
    }
}

/// Asks `question` on the terminal; true only for an explicit yes.
pub(crate) fn confirm(question: &str) -> bool {
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Says how the version an import wrote gets approved: by its exact trust
/// digest when it carries scripts, else like any untrusted version.
pub(crate) fn print_approval_hint(root: &Path, id: &str, version: &str) {
    let playbook_dir = apb_core::versioning::playbooks_dir(root).join(id);
    let imported =
        apb_core::versioning::read_provenance_in(&playbook_dir, version).and_then(|p| p.imported);
    let (safe_id, safe_version) = (
        sanitize_for_terminal(id, 128),
        sanitize_for_terminal(version, 64),
    );
    match imported {
        Some(i) if !i.scripts.is_empty() => println!(
            "{safe_id} @ {safe_version} carries scripts and does not run until approved by digest: \
             review them, then run `apb trust approve {safe_id} --version {safe_version} --digest {}`",
            i.digest
        ),
        _ => println!(
            "{safe_id} @ {safe_version} is untrusted until approved: review it, then run \
             `apb trust approve {safe_id} --version {safe_version}`, \
             or confirm it for one run with `apb run {safe_id} --version {safe_version}`"
        ),
    }
}
