//! `apb discover`: the step sequences a person's past agent sessions
//! repeat, found in the agents' own history files and offered as playbook
//! drafts; the stored results (`list`, `show`), and a person's save or
//! decline of one suggestion. A thin dispatch over
//! `apb_mcp::tools::discover`, the layer MCP and the dashboard use, which
//! refuses every subcommand (reads too) inside a run step.
//!
//! Only here can a person name a raw history directory (`--sessions`) or
//! see the absolute paths of a result's sources (`show --paths`): both are
//! typed and read at the person's own terminal. A `--sessions` directory
//! is named [`SESSIONS_ROOT`], a name no configured root can take, and its
//! path is not kept: `show --paths` says so for its sources.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::discover::{HistoryAgent, HistoryRoot, Suggestion, SuggestionStatus, project_names};
use apb_mcp::tools::ToolError;
use apb_mcp::tools::discover::{
    DiscoverArgs, ProjectPick, ResultSummary, ResultView, SaveRequest, configured_roots,
    discover_dismiss, discover_save, result_get, results_list, run_discover,
};
use clap::{Args, Subcommand};

use crate::util::{StepCaller, print_json, print_table, sanitize_for_terminal};

/// The longest text shown on one line.
const LINE_MAX: usize = 300;

/// The root name a `--sessions` directory is read under: `@` cannot start
/// a configured root's name, so it never stands for one.
const SESSIONS_ROOT: &str = "@sessions";

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct DiscoverCmdArgs {
    #[command(subcommand)]
    action: Option<DiscoverAction>,
    #[command(flatten)]
    run: RunFlags,
    /// Machine-readable output (the same objects as MCP)
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Args)]
struct RunFlags {
    /// A project to read the sessions of: a registered workspace id, else
    /// a directory (repeatable); default: the current project
    #[arg(long = "project")]
    projects: Vec<String>,
    /// Every registered project that is there now
    #[arg(long, conflicts_with = "projects")]
    all_projects: bool,
    /// A history root by its name (repeatable; see `discover.roots` in the
    /// global config); default: every configured root that is there
    #[arg(long = "root")]
    roots: Vec<String>,
    /// A history directory to read instead of the configured roots (needs
    /// --agent)
    #[arg(long, requires = "agent", conflicts_with = "roots")]
    sessions: Option<PathBuf>,
    /// Whose history --sessions holds: claude-code or codex
    #[arg(long, requires = "sessions")]
    agent: Option<String>,
    /// How far back: days, as `30d` or `30` (1 to 365); default: the
    /// configured value, else 90
    #[arg(long)]
    since: Option<String>,
}

#[derive(Subcommand)]
enum DiscoverAction {
    /// The stored results, newest first
    List,
    /// One stored result in full
    Show {
        id: String,
        /// Print each source's absolute path (this terminal only)
        #[arg(long)]
        paths: bool,
    },
    /// Save a suggestion as a draft playbook
    Save {
        id: String,
        slug: String,
        /// `project` (the current one) or `global`
        #[arg(long, default_value = "project")]
        scope: String,
    },
    /// Decline a suggestion: not now (soft), or never with --hard
    Dismiss {
        id: String,
        slug: String,
        /// Never offer it again
        #[arg(long)]
        hard: bool,
        /// Decline it in every project, not only the current one
        #[arg(long)]
        global: bool,
    },
}

pub(crate) fn discover_cmd(root: &Path, args: DiscoverCmdArgs) -> ExitCode {
    let json = args.json;
    let step = StepCaller::from_env();
    match args.action {
        None => run(root, args.run, json, &step),
        Some(DiscoverAction::List) => match results_list(&step) {
            Ok(list) if json => {
                print_json(&serde_json::json!(list));
                ExitCode::SUCCESS
            }
            Ok(list) => {
                print_list(&list);
                ExitCode::SUCCESS
            }
            Err(e) => failed("discover list", e),
        },
        Some(DiscoverAction::Show { id, paths }) => match result_get(&id, &step) {
            Ok(v) if json => {
                print_json(&serde_json::json!(v));
                ExitCode::SUCCESS
            }
            Ok(v) => {
                print_result(&v, paths);
                ExitCode::SUCCESS
            }
            Err(e) => failed("discover show", e),
        },
        Some(DiscoverAction::Save { id, slug, scope }) => {
            let req = SaveRequest {
                id: &id,
                slug: &slug,
                scope: &scope,
                yaml: None,
            };
            answered("discover save", discover_save(root, req, &step), json)
        }
        Some(DiscoverAction::Dismiss {
            id,
            slug,
            hard,
            global,
        }) => {
            let kind = if hard { "hard" } else { "soft" };
            let scope = if global { "global" } else { "project" };
            let out = discover_dismiss(root, &id, &slug, kind, Some(scope), &step);
            answered("discover dismiss", out, json)
        }
    }
}

/// A `--project` value: a registered workspace id first (a directory of
/// the same name in the current directory does not shadow it), else a
/// directory, else a workspace id the tool layer reports as unknown.
fn project_pick(value: String, registered: &[String]) -> ProjectPick {
    if !registered.contains(&value) && Path::new(&value).is_dir() {
        ProjectPick::Root(PathBuf::from(value))
    } else {
        ProjectPick::Workspace(value)
    }
}

/// `30d` or `30`: days.
fn since_days(s: &str) -> Option<u32> {
    s.trim().strip_suffix('d').unwrap_or(s.trim()).parse().ok()
}

fn run(root: &Path, flags: RunFlags, json: bool, step: &StepCaller) -> ExitCode {
    let since = match flags.since.as_deref().map(since_days) {
        Some(None) => {
            eprintln!("discover: --since takes days, as 30d or 30");
            return ExitCode::from(2);
        }
        Some(d) => d,
        None => None,
    };
    let extra_root = match (flags.sessions, flags.agent.as_deref()) {
        (Some(path), Some(agent)) => match HistoryAgent::parse(agent) {
            Some(agent) => Some(HistoryRoot {
                name: SESSIONS_ROOT.to_string(),
                agent,
                path,
            }),
            None => {
                eprintln!("discover: --agent takes claude-code or codex");
                return ExitCode::from(2);
            }
        },
        _ => None,
    };
    let projects = if flags.all_projects {
        vec![ProjectPick::AllReachable]
    } else if flags.projects.is_empty() {
        vec![ProjectPick::Root(root.to_path_buf())]
    } else {
        let registered: Vec<String> = apb_core::projects::list_active()
            .into_iter()
            .map(|e| e.workspace_id)
            .collect();
        flags
            .projects
            .into_iter()
            .map(|p| project_pick(p, &registered))
            .collect()
    };
    let args = DiscoverArgs {
        projects,
        roots: flags.roots,
        extra_root,
        since_days: since,
    };
    match run_discover(&args, step) {
        Ok(v) if json => {
            print_json(&serde_json::json!(v));
            ExitCode::SUCCESS
        }
        Ok(v) => {
            print_result(&v, false);
            ExitCode::SUCCESS
        }
        Err(e) => failed("discover", e),
    }
}

/// A save's or a decline's answer: refused (exit 2, in `--json` too, so a
/// script sees it), or done, with the store's diagnostics and a warning
/// on stderr.
fn answered(what: &str, out: Result<serde_json::Value, ToolError>, json: bool) -> ExitCode {
    let v = match out {
        Ok(v) => v,
        Err(e) => return failed(what, e),
    };
    let refused = v.get("rejected").is_some();
    if json {
        print_json(&v);
    } else if refused {
        eprintln!(
            "{what} refused: {}",
            text(v["rejected"].as_str().unwrap_or_default())
        );
    } else {
        match v.get("id").and_then(|i| i.as_str()) {
            Some(id) => println!(
                "saved as draft playbook {} {} ({}): it runs only after a trial or an approval",
                text(id),
                text(v["version"].as_str().unwrap_or_default()),
                text(v["scope"].as_str().unwrap_or_default())
            ),
            None => println!(
                "declined {} ({}) until {}",
                text(v["pattern"].as_str().unwrap_or_default()),
                text(v["kind"].as_str().unwrap_or_default()),
                text(v["snoozed_until"].as_str().unwrap_or_default())
            ),
        }
        let notes = v["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(v.get("warning"));
        for note in notes {
            eprintln!("warning: {}", text(note.as_str().unwrap_or_default()));
        }
    }
    if refused {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn failed(what: &str, e: ToolError) -> ExitCode {
    eprintln!(
        "{what} failed: {}",
        sanitize_for_terminal(&e.to_string(), 600)
    );
    ExitCode::from(2)
}

fn text(s: &str) -> String {
    sanitize_for_terminal(s, LINE_MAX)
}

fn print_list(list: &[ResultSummary]) {
    if list.is_empty() {
        println!("no stored discoveries (apb discover runs one)");
        return;
    }
    let mut rows = vec![vec![
        "ID".to_string(),
        "WHEN".to_string(),
        "PROJECTS".to_string(),
        "SESSIONS".to_string(),
        "SUGGESTIONS".to_string(),
        "SAVED".to_string(),
        "DISMISSED".to_string(),
        "SIGNED".to_string(),
    ]];
    for s in list {
        rows.push(vec![
            s.id.clone(),
            apb_core::clock::iso_utc(s.created_at),
            text(&project_names(&s.request.projects)),
            s.sessions.to_string(),
            s.suggestions.to_string(),
            s.saved.to_string(),
            s.dismissed.to_string(),
            if s.signed { "yes" } else { "no" }.to_string(),
        ]);
    }
    print_table(&rows);
}

fn status_text(s: &SuggestionStatus) -> String {
    match s {
        SuggestionStatus::New => "new".to_string(),
        SuggestionStatus::Saved {
            playbook, scope, ..
        } => format!("saved as {} ({})", text(playbook), text(scope)),
        SuggestionStatus::Dismissed => "dismissed".to_string(),
    }
}

/// Where a result's sources lie, for `show --paths`: the configured roots,
/// read once; a config that does not load is said once.
fn source_roots(paths: bool) -> SourceRoots {
    if !paths {
        return SourceRoots::Off;
    }
    match configured_roots() {
        Ok(roots) => SourceRoots::Loaded(roots),
        Err(e) => {
            eprintln!(
                "discover show: the history roots do not load: {}",
                sanitize_for_terminal(&e.to_string(), 600)
            );
            SourceRoots::Unloaded
        }
    }
}

/// What `show --paths` locates sources under.
enum SourceRoots {
    /// No `--paths`: sources are not located.
    Off,
    Loaded(Vec<HistoryRoot>),
    /// The config did not load: every source says so, not that its file is
    /// gone.
    Unloaded,
}

fn print_result(v: &ResultView, paths: bool) {
    let r = &v.result;
    let roots = source_roots(paths);
    println!(
        "discovery {} over {} ({} days): {} sessions",
        r.id,
        text(&project_names(&r.request.projects)),
        r.request.since_days,
        r.read.sessions
    );
    for root in &r.read.roots {
        let skipped: Vec<String> = root
            .skipped
            .iter()
            .map(|(why, n)| format!("{} {n}", text(why)))
            .collect();
        println!(
            "  root {} ({}): {} files read of {}, {} bytes, {} cut{}",
            text(&root.name),
            text(&root.agent),
            root.files_read,
            root.files_seen,
            root.bytes_read,
            root.cut_files,
            if skipped.is_empty() {
                String::new()
            } else {
                format!(", skipped: {}", skipped.join(", "))
            }
        );
    }
    if !v.signed {
        println!("  not signed by this installation: nothing is saved from it");
    }
    if r.suggestions.is_empty() {
        println!("no repeated step sequences found");
    }
    for s in &r.suggestions {
        print_suggestion(s, &roots);
    }
    for s in &r.suppressed {
        println!(
            "left out {} ({} {}, {})",
            text(&s.slug),
            if s.kind == "saved" {
                "earlier:"
            } else {
                "declined:"
            },
            text(&s.kind),
            text(&s.scope)
        );
    }
    if r.dropped_drafts > 0 {
        println!(
            "{} drafts dropped: their text still looked like it held a secret",
            r.dropped_drafts
        );
    }
}

/// Prints one suggestion; with `roots` (`show --paths`), each source's
/// absolute path under them.
fn print_suggestion(s: &Suggestion, roots: &SourceRoots) {
    println!();
    println!(
        "{} [{}] {}",
        text(&s.slug),
        status_text(&s.status),
        text(&s.title)
    );
    println!(
        "  seen in {} sessions on {} days ({}), score {}",
        s.support.sessions,
        s.support.days,
        text(&project_names(&s.support.projects)),
        s.score
    );
    println!("  steps: {}", text(&s.steps.join(" > ")));
    if !s.effects.is_empty() {
        println!("  effects: {}", text(&s.effects.join(", ")));
    }
    for n in &s.notes {
        println!("  note: {}", text(n));
    }
    for e in &s.examples {
        println!("  asked: {}", text(&e.text));
    }
    for src in &s.sources {
        let at = format!(
            "{}:{}:{}-{}",
            text(&src.root),
            text(&src.file),
            src.lines[0],
            src.lines[1]
        );
        let roots = match roots {
            SourceRoots::Off => {
                println!("  source: {at}");
                continue;
            }
            SourceRoots::Unloaded => {
                println!("  source: {at} (the history roots did not load)");
                continue;
            }
            SourceRoots::Loaded(roots) => roots,
        };
        if src.root == SESSIONS_ROOT {
            println!("  source: {at} (read from a --sessions directory: its path is not kept)");
            continue;
        }
        let path = roots
            .iter()
            .find(|r| r.name == src.root)
            .and_then(|r| apb_core::discover::walk::locate(r, &src.file));
        match path {
            Some(p) => println!("  source: {at} {}", text(&p.display().to_string())),
            None => println!("  source: {at} (not found under a configured root)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProjectPick, project_pick, since_days};

    #[test]
    fn a_registered_workspace_id_wins_over_a_directory_of_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("ws-1");
        std::fs::create_dir(&named).unwrap();
        let value = named.to_string_lossy().into_owned();
        assert_eq!(
            project_pick(value.clone(), &[]),
            ProjectPick::Root(named.clone())
        );
        assert_eq!(
            project_pick(value.clone(), std::slice::from_ref(&value)),
            ProjectPick::Workspace(value)
        );
        assert_eq!(
            project_pick("ws-unknown".into(), &[]),
            ProjectPick::Workspace("ws-unknown".into())
        );
    }

    #[test]
    fn since_takes_days_with_or_without_the_unit() {
        assert_eq!(since_days("30d"), Some(30));
        assert_eq!(since_days("7"), Some(7));
        assert_eq!(since_days("2w"), None);
    }
}
