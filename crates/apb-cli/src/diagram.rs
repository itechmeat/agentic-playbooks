//! `apb diagram`: the Mermaid flowchart of a playbook version
//! (people-in-the-loop D6, "Surfaces"). The text is derived from the YAML
//! on every call and never read back from `playbook.mmd`, so a version
//! whose file a person edited still prints what its YAML renders.
//!
//! - `apb diagram <id> [--version V] [--scope project|global] [--out
//!   PATH]`: the text of the version's YAML, on stdout or written to PATH;
//! - `apb diagram --rebuild <id>`: rewrite `playbook.mmd` of every version
//!   of `id` in its scope;
//! - `apb diagram --rebuild --all`: the same for every playbook of the
//!   project and the global store. A version that does not read is
//!   skipped, named, and makes the command exit 1.
//!
//! Every read and write goes through `apb_core::fsutil::beneath`, anchored
//! at the store's `playbooks` directory: a playbook or version directory
//! that is a link, or a link anywhere below the store, is never followed,
//! so nothing is read or written outside the store.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::diagram;
use apb_core::fsutil::beneath::{Beneath, CappedRead, EntryKind, ListBudget, ReadRefusal};
use apb_core::registry::{is_safe_segment, parse_version};
use clap::{Args, ValueEnum};

use crate::util::sanitize_for_terminal;

/// How long a file-derived reason or name may be on the terminal.
const TEXT_MAX: usize = 300;
/// The most entries one listing of a store examines.
const LIST_MAX: usize = 100_000;

/// The definition store a command reads or rebuilds.
#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    Project,
    Global,
}

#[derive(Args)]
pub(crate) struct DiagramArgs {
    /// The playbook id (with --rebuild --all, none)
    playbook: Option<String>,
    /// Print this version instead of the current one
    #[arg(long, conflicts_with = "rebuild")]
    version: Option<String>,
    /// Which store to read or rebuild
    #[arg(long, value_enum, default_value_t = Scope::Project)]
    scope: Scope,
    /// Write the text to this file instead of stdout
    #[arg(long, value_name = "PATH", conflicts_with = "rebuild")]
    out: Option<PathBuf>,
    /// Rewrite playbook.mmd of every version instead of printing
    #[arg(long)]
    rebuild: bool,
    /// With --rebuild: every playbook of the project and the global store
    #[arg(long, requires = "rebuild", conflicts_with = "playbook")]
    all: bool,
}

pub(crate) fn diagram_cmd(root: &Path, args: DiagramArgs) -> ExitCode {
    if args.rebuild {
        return if args.all {
            rebuild_all(root)
        } else {
            match args.playbook.as_deref() {
                Some(id) => rebuild_one(root, id, args.scope),
                None => refused("--rebuild needs a playbook id, or --all"),
            }
        };
    }
    let Some(id) = args.playbook.as_deref() else {
        return refused("a playbook id is required");
    };
    print_one(
        root,
        id,
        args.version.as_deref(),
        args.scope,
        args.out.as_deref(),
    )
}

fn refused(why: &str) -> ExitCode {
    eprintln!("apb: {why}");
    ExitCode::from(2)
}

/// The `playbooks` directory of `scope`.
fn store_dir(root: &Path, scope: Scope) -> Option<PathBuf> {
    match scope {
        Scope::Project => Some(root.join(".apb").join("playbooks")),
        Scope::Global => apb_core::store::global_playbooks_parent().map(|p| p.join("playbooks")),
    }
}

/// The store of `scope` opened beneath its `playbooks` directory, when it
/// holds playbook `id`, or why there is none (the way `apb version`
/// resolves one). A playbook directory that is a link is no playbook here.
fn playbook_dir(root: &Path, id: &str, scope: Scope) -> Result<Beneath, String> {
    let not_found = || format!("playbook `{id}` not found");
    if !is_safe_segment(id) {
        return Err(not_found());
    }
    let store = store_dir(root, scope).ok_or_else(not_found)?;
    let beneath = Beneath::open(&store).map_err(|_| not_found())?;
    match beneath.kind(id) {
        Ok(Some(EntryKind::Dir)) => Ok(beneath),
        Ok(Some(EntryKind::Link)) => {
            Err(format!("playbook `{id}` is a link; apb does not follow it"))
        }
        _ => Err(not_found()),
    }
}

/// The most bytes of a `current` pointer read.
const POINTER_MAX_BYTES: u64 = 256;

/// The version `id`'s `current` names, read beneath the store as a small
/// regular file without following a link; `None` when it is absent, not
/// such a file, or not a version name.
fn read_pointer(beneath: &Beneath, id: &str) -> Option<String> {
    match beneath.read_regular_capped(&format!("{id}/current"), POINTER_MAX_BYTES) {
        Ok(CappedRead::Read { bytes, .. }) => String::from_utf8(bytes)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && is_safe_segment(s)),
        _ => None,
    }
}

/// A version's `playbook.yaml`, read beneath the store without following
/// a link.
fn read_yaml(beneath: &Beneath, id: &str, version: &str) -> Result<String, String> {
    let rel = format!("{id}/{version}/playbook.yaml");
    match beneath.read_regular_capped(&rel, diagram::YAML_READ_MAX_BYTES) {
        Ok(CappedRead::Read { bytes, .. }) => {
            String::from_utf8(bytes).map_err(|_| "playbook.yaml: not UTF-8".to_string())
        }
        Ok(CappedRead::Refused(why)) => Err(format!("playbook.yaml: {}", refusal_words(why))),
        Err(e) => Err(format!("playbook.yaml: {e}")),
    }
}

/// `apb diagram <id>`: the text of one version's YAML, on stdout or in
/// `--out`. Without `--version`, the version `current` names.
fn print_one(
    root: &Path,
    id: &str,
    version: Option<&str>,
    scope: Scope,
    out: Option<&Path>,
) -> ExitCode {
    let beneath = match playbook_dir(root, id, scope) {
        Ok(d) => d,
        Err(m) => return refused(&m),
    };
    let version = match version.map_or_else(|| read_pointer(&beneath, id), |v| Some(v.to_string()))
    {
        Some(v) => v,
        None => {
            return refused(&format!(
                "playbook `{id}` has no current version; pass --version"
            ));
        }
    };
    // A version is one path segment: `--version ../../x` (or a pointer
    // file naming one) never reads outside the playbook's directory.
    if !is_safe_segment(&version) {
        return refused(&format!(
            "version `{}` is not a version name",
            safe(&version)
        ));
    }
    let yaml = match read_yaml(&beneath, id, &version) {
        Ok(y) => y,
        Err(e) => return refused(&format!("cannot read `{id}` {version}: {}", safe(&e))),
    };
    let playbook = match apb_core::schema::Playbook::from_yaml(&yaml) {
        Ok(p) => p,
        Err(e) => {
            return refused(&format!(
                "`{id}` {version} does not read: {}",
                safe(&e.to_string())
            ));
        }
    };
    let text = diagram::mermaid(&playbook, &apb_core::scope::digest_str(&yaml));
    match out {
        Some(path) => match apb_core::fsutil::atomic_write(path, text.as_bytes()) {
            Ok(()) => {
                println!("wrote {}", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => refused(&format!("cannot write {}: {e}", path.display())),
        },
        None => {
            // Raw bytes, so the text pipes into a Mermaid renderer
            // unchanged.
            let stdout = std::io::stdout();
            let _ = stdout.lock().write_all(text.as_bytes());
            ExitCode::SUCCESS
        }
    }
}

/// `apb diagram --rebuild <id>`: rewrite `playbook.mmd` of every version of
/// `id` in `scope`.
fn rebuild_one(root: &Path, id: &str, scope: Scope) -> ExitCode {
    let beneath = match playbook_dir(root, id, scope) {
        Ok(d) => d,
        Err(m) => return refused(&m),
    };
    let mut n = 0usize;
    let mut skipped = false;
    skipped |= rebuild_versions(&beneath, id, &mut n);
    println!("rebuilt {n} diagrams");
    if skipped {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `apb diagram --rebuild --all`: every playbook of the project and the
/// global store. A store that does not exist counts nothing.
fn rebuild_all(root: &Path) -> ExitCode {
    let mut n = 0usize;
    let mut skipped = false;
    for scope in [Scope::Project, Scope::Global] {
        if let Some(store) = store_dir(root, scope) {
            rebuild_store(&store, &mut n, &mut skipped);
        }
    }
    println!("rebuilt {n} diagrams");
    if skipped {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Rewrites every version diagram under `playbooks_dir`
/// (`<dir>/<id>/<version>/playbook.yaml`), beneath it: an entry that is a
/// link is passed by. A store that does not exist counts nothing; one
/// that does not open (a link, a permission error) or does not list is
/// named and skipped, and the other stores are still rebuilt.
fn rebuild_store(playbooks_dir: &Path, n: &mut usize, skipped: &mut bool) {
    let store = || safe(&playbooks_dir.display().to_string());
    let beneath = match Beneath::open(playbooks_dir) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            println!("skipped store {}: {}", store(), safe(&e.to_string()));
            *skipped = true;
            return;
        }
    };
    let mut budget = ListBudget {
        remaining: LIST_MAX,
    };
    let listing = match beneath.list_dir("", &mut budget) {
        Ok(l) => l,
        Err(e) => {
            println!(
                "skipped store {}: it does not list: {}",
                store(),
                safe(&e.to_string())
            );
            *skipped = true;
            return;
        }
    };
    if listing.exhausted {
        println!(
            "skipped part of store {}: more than {LIST_MAX} entries",
            store()
        );
        *skipped = true;
    }
    for entry in listing.entries {
        if entry.kind != EntryKind::Dir {
            continue;
        }
        let id = entry.name.to_string_lossy().into_owned();
        if !is_safe_segment(&id) {
            continue;
        }
        *skipped |= rebuild_versions(&beneath, &id, n);
    }
}

/// Rewrites every stored version of playbook `id`, oldest first; whether
/// one was skipped. The versions are listed beneath the store, so a
/// playbook directory swapped for a link is refused, never listed through.
/// A directory whose versions do not list is skipped and named, never
/// counted as a playbook without versions. A version directory that is a
/// link is not listed (directories only), and the write walks from the
/// store without following a link, so one swapped in later is refused too.
fn rebuild_versions(beneath: &Beneath, id: &str, n: &mut usize) -> bool {
    let versions = match versions_beneath(beneath, id) {
        Ok(v) => v,
        Err(e) => {
            println!(
                "skipped {}: its versions do not list: {}",
                safe(id),
                safe(&e.to_string())
            );
            return true;
        }
    };
    let mut skipped = false;
    for version in versions {
        skipped |= rebuild_version(beneath, id, &version, n);
    }
    skipped
}

/// The version directories of `id` listed beneath the store, by the
/// registry's version-name rule, oldest first.
fn versions_beneath(beneath: &Beneath, id: &str) -> std::io::Result<Vec<String>> {
    let mut budget = ListBudget {
        remaining: LIST_MAX,
    };
    let listing = beneath.list_dir(id, &mut budget)?;
    if listing.exhausted {
        return Err(std::io::Error::other(format!(
            "more than {LIST_MAX} entries"
        )));
    }
    let mut versions: Vec<((u32, u32, u32), String)> = listing
        .entries
        .into_iter()
        .filter(|e| e.kind == EntryKind::Dir)
        .filter_map(|e| {
            let name = e.name.to_str()?.to_string();
            parse_version(&name).map(|key| (key, name))
        })
        .collect();
    versions.sort();
    Ok(versions.into_iter().map(|(_, name)| name).collect())
}

/// Writes one version's `playbook.mmd` beneath the store; whether the
/// version was skipped.
fn rebuild_version(beneath: &Beneath, id: &str, version: &str, n: &mut usize) -> bool {
    let wrote = read_yaml(beneath, id, version).and_then(|yaml| {
        let text = diagram::text_of(&yaml).map_err(|e| e.to_string())?;
        // Whatever sits at playbook.mmd is replaced, never followed: the
        // file is derived, so an edit is overwritten.
        diagram::replace_beside(beneath, &format!("{id}/{version}"), &text)
            .map_err(|e| e.to_string())
    });
    match wrote {
        Ok(()) => {
            *n += 1;
            false
        }
        Err(why) => {
            println!("skipped {} {}: {}", safe(id), safe(version), safe(&why));
            true
        }
    }
}

/// Why a `playbook.yaml` was not read, in words.
fn refusal_words(why: ReadRefusal) -> &'static str {
    match why {
        ReadRefusal::Missing => "not found",
        ReadRefusal::Link => "a link on its path; apb does not follow it",
        ReadRefusal::NotRegular => "not a regular file",
        ReadRefusal::HardLink => "a file with more than one link",
        ReadRefusal::TooLarge { .. } => "too large",
    }
}

/// File-derived text, safe for the terminal.
fn safe(s: &str) -> String {
    sanitize_for_terminal(s, TEXT_MAX)
}
