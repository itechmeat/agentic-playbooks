//! Optional desktop-history sync for zcode runs (`agents.zcode.ui_sync`).
//!
//! The ZCode desktop app lists sessions from its own task index,
//! `~/.zcode/v2/tasks-index.sqlite` (table `tasks`), not from the CLI's session
//! store. A session the headless CLI ran exists in the CLI store only, so the
//! desktop never shows it. With `ui_sync` on, apb inserts one `tasks` row per
//! finished zcode session, shaped like the rows the desktop writes itself, so
//! the session appears in the desktop history. Facts below were read from
//! ZCode desktop 3.14.3:
//!
//! - A row belongs to a workspace by `workspace_key`, which is the workspace
//!   identity when there is one and the plain workspace path otherwise
//!   (`getWorkspaceKey`). A project the desktop opened over its WSL remote
//!   connection has the identity `remote:wsl:<distro>:<user>:<path>` and its
//!   tasks live in the index of the WSL home; a local project has no identity,
//!   its key is its path.
//! - The desktop coordinates writers of this database through SQLite itself
//!   (WAL, `busy_timeout`, `BEGIN IMMEDIATE`); its lock-directory protocol
//!   guards JSON files, not this database. So apb takes the same SQLite write
//!   lock and retries while the database is busy.
//! - Rows the desktop does not recognize as its own provider (`glm`) are
//!   filtered out of its list.
//!
//! The sync is best effort by contract: the caller turns every error into a
//! warning, a run never fails because of it. apb never creates the database,
//! never changes its schema or journal mode, and never overwrites a row
//! (`INSERT OR IGNORE`): a row the desktop already manages stays untouched.

use std::path::Path;
use std::time::Duration;

/// The desktop task index, relative to `$HOME`.
pub const HOME_REL_TASKS_INDEX: &str = ".zcode/v2/tasks-index.sqlite";

/// The provider id every desktop task row carries; rows with another provider
/// are filtered out of the desktop's list.
pub const PROVIDER: &str = "glm";

/// The status a finished headless turn is recorded with.
pub const STATUS_COMPLETED: &str = "completed";

/// The desktop's cap on a row's full-text search column, in characters.
pub const SEARCHABLE_TEXT_MAX_CHARS: usize = 200_000;

/// The longest title apb writes, in characters.
pub const TITLE_MAX_CHARS: usize = 80;

/// Where the zcode run happened, which decides the workspace identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// Inside a WSL distro: the desktop (on Windows) reaches the workspace over
    /// its WSL remote connection as `<user>` in `<distro>`.
    Wsl { distro: String, user: String },
    /// Anywhere else: the desktop runs on the same machine and knows the
    /// workspace by its path.
    Native,
}

/// The desktop's path normalization for a workspace identity
/// (`normalizeWorkspacePathForIdentity`): backslashes become slashes, runs of
/// slashes collapse, and the result has exactly one leading slash and no
/// trailing one.
pub fn normalize_identity_path(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    let parts: Vec<&str> = slashed.split('/').filter(|p| !p.is_empty()).collect();
    format!("/{}", parts.join("/"))
}

/// The workspace identity of `path` on `host`: `remote:wsl:<distro>:<user>:<path>`
/// under WSL, `None` for a native workspace (the desktop stores none).
pub fn workspace_identity(host: &Host, path: &str) -> Option<String> {
    match host {
        Host::Wsl { distro, user } => Some(format!(
            "remote:wsl:{}:{}:{}",
            distro.trim(),
            user.trim(),
            normalize_identity_path(path)
        )),
        Host::Native => None,
    }
}

/// The `workspace_key` of a row: the identity when there is one, else the path.
pub fn workspace_key(path: &str, identity: Option<&str>) -> String {
    identity
        .map(str::trim)
        .filter(|i| !i.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// The distro name in `wslpath -w /` output (`\\wsl.localhost\<distro>\` or
/// the older `\\wsl$\<distro>\`).
pub fn distro_from_wslpath(out: &str) -> Option<String> {
    let rest = out.trim();
    let rest = rest
        .strip_prefix("\\\\wsl.localhost\\")
        .or_else(|| rest.strip_prefix("\\\\wsl$\\"))?;
    let distro = rest.split('\\').next()?.trim();
    (!distro.is_empty()).then(|| distro.to_string())
}

/// What [`detect_host`] reads about the machine, gathered up front so the
/// decision itself is a pure function.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostFacts {
    /// `/proc/sys/kernel/osrelease` (WSL kernels name `microsoft` there).
    pub osrelease: Option<String>,
    /// `$WSL_DISTRO_NAME` (set in WSL login shells, absent in services).
    pub distro_env: Option<String>,
    /// `wslpath -w /` output, the fallback for the distro name.
    pub wslpath_root: Option<String>,
    /// The user the process runs as.
    pub user: Option<String>,
}

impl HostFacts {
    /// The facts of the current process.
    pub fn current() -> Self {
        let non_empty = |v: String| (!v.trim().is_empty()).then(|| v.trim().to_string());
        let distro_env = std::env::var("WSL_DISTRO_NAME").ok().and_then(non_empty);
        let osrelease = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok();
        let under_wsl = distro_env.is_some() || osrelease.as_deref().is_some_and(is_wsl_kernel);
        let wslpath_root = (under_wsl && distro_env.is_none())
            .then(|| command_stdout("wslpath", &["-w", "/"]))
            .flatten();
        let user = std::env::var("USER")
            .ok()
            .and_then(non_empty)
            .or_else(|| std::env::var("LOGNAME").ok().and_then(non_empty))
            .or_else(|| {
                (under_wsl)
                    .then(|| command_stdout("id", &["-un"]))
                    .flatten()
            });
        HostFacts {
            osrelease,
            distro_env,
            wslpath_root,
            user,
        }
    }
}

/// Whether a kernel release string is a WSL kernel.
fn is_wsl_kernel(release: &str) -> bool {
    release.to_ascii_lowercase().contains("microsoft")
}

/// The trimmed stdout of a successful short command, `None` otherwise.
fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// Where a zcode run on this machine happens, derived at runtime (never
/// configured): under WSL the distro (`$WSL_DISTRO_NAME`, else the
/// `wslpath -w /` root) and the user; anywhere else [`Host::Native`]. An
/// error when the process is under WSL but the distro or user cannot be told,
/// since a guessed identity would put the row under the wrong workspace.
pub fn detect_host(facts: &HostFacts) -> Result<Host, String> {
    let under_wsl =
        facts.distro_env.is_some() || facts.osrelease.as_deref().is_some_and(is_wsl_kernel);
    if !under_wsl {
        return Ok(Host::Native);
    }
    let distro = facts
        .distro_env
        .clone()
        .or_else(|| facts.wslpath_root.as_deref().and_then(distro_from_wslpath))
        .ok_or(
            "running under WSL, but the distro name is unknown (no WSL_DISTRO_NAME, no wslpath)",
        )?;
    let user = facts
        .user
        .clone()
        .ok_or("running under WSL, but the user name is unknown")?;
    Ok(Host::Wsl { distro, user })
}

/// One task row, as apb records a finished zcode session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    /// The zcode session id (`sess_...`), the row's `task_id`.
    pub task_id: String,
    /// The turn's trace id from zcode's `--json` result, when it printed one.
    pub trace_id: Option<String>,
    pub title: String,
    /// The working directory the session ran in.
    pub workspace_path: String,
    /// See [`workspace_identity`].
    pub workspace_identity: Option<String>,
    /// The `--mode` the session ran with (`build`, `edit`, `yolo`).
    pub mode: String,
    /// `<providerId>/<modelId>`, the desktop's spelling; `None` when zcode ran
    /// the user's own default selection.
    pub model: Option<String>,
    /// The reasoning level (the desktop's `thoughtLevel`).
    pub thought_level: Option<String>,
    /// Text for the desktop's full-text search.
    pub searchable_text: String,
    /// Creation and update time, epoch milliseconds.
    pub now_ms: i64,
}

impl TaskRecord {
    /// The row's `meta_json`, shaped like the desktop's own.
    pub fn meta_json(&self) -> serde_json::Value {
        let mut meta = serde_json::json!({
            "taskId": self.task_id,
            "traceId": self.trace_id.as_deref().unwrap_or(&self.task_id),
            "title": capped(&self.title, TITLE_MAX_CHARS),
            "titleOverridden": false,
            "workspacePath": self.workspace_path,
            "createdAt": self.now_ms,
            "updatedAt": self.now_ms,
            "mode": self.mode,
            "provider": PROVIDER,
            "status": STATUS_COMPLETED,
            "target": null,
        });
        if let Some(id) = &self.workspace_identity {
            meta["workspaceIdentity"] = serde_json::json!(id);
        }
        if let Some(model) = &self.model {
            meta["model"] = serde_json::json!(model);
        }
        if let Some(level) = &self.thought_level {
            meta["thoughtLevel"] = serde_json::json!(level);
        }
        meta
    }
}

/// The first `max` characters of `s`.
fn capped(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// How hard [`insert_task`] tries against a busy database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    /// SQLite's own wait for a lock, per attempt.
    pub busy_timeout: Duration,
    /// Pauses between attempts after a busy/locked error; their count is the
    /// number of retries.
    pub backoff: &'static [Duration],
}

/// The default pauses between attempts on a busy database.
const DEFAULT_BACKOFF: &[Duration] = &[Duration::from_millis(200), Duration::from_millis(400)];

impl Default for Retry {
    fn default() -> Self {
        Retry {
            busy_timeout: Duration::from_secs(2),
            backoff: DEFAULT_BACKOFF,
        }
    }
}

/// Inserts `rec` into the task index at `db`. `Ok(true)` when the row was
/// written, `Ok(false)` when a row with the same workspace key and task id
/// already existed (it is left untouched). A missing database is an error:
/// apb never creates one.
pub fn insert_task(db: &Path, rec: &TaskRecord, retry: Retry) -> Result<bool, String> {
    let mut pauses = retry.backoff.iter();
    loop {
        match try_insert(db, rec, retry.busy_timeout) {
            Err(e) if is_busy(&e) => match pauses.next() {
                Some(pause) => std::thread::sleep(*pause),
                None => return Err(format!("{}: {e}", db.display())),
            },
            Err(e) => return Err(format!("{}: {e}", db.display())),
            Ok(written) => return Ok(written),
        }
    }
}

/// Whether `e` is SQLite's "database is busy/locked", the only error worth
/// another attempt.
fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// One attempt: open the existing database (never create it), take the write
/// lock the way the desktop does (`BEGIN IMMEDIATE`), insert, commit.
fn try_insert(db: &Path, rec: &TaskRecord, busy_timeout: Duration) -> rusqlite::Result<bool> {
    use rusqlite::OpenFlags;
    let mut con = rusqlite::Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    con.busy_timeout(busy_timeout)?;
    let tx = con.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let key = workspace_key(&rec.workspace_path, rec.workspace_identity.as_deref());
    let meta = rec.meta_json().to_string();
    let written = tx.execute(
        "INSERT OR IGNORE INTO tasks (workspace_key, workspace_path, workspace_identity, \
         task_id, title, task_status, provider, mode, model, created_at, updated_at, \
         meta_json, searchable_text) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11, ?12)",
        rusqlite::params![
            key,
            rec.workspace_path,
            rec.workspace_identity,
            rec.task_id,
            capped(&rec.title, TITLE_MAX_CHARS),
            STATUS_COMPLETED,
            PROVIDER,
            rec.mode,
            rec.model,
            rec.now_ms,
            meta,
            capped(&rec.searchable_text, SEARCHABLE_TEXT_MAX_CHARS),
        ],
    )?;
    tx.commit()?;
    Ok(written > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = include_str!("../tests/fixtures/zcode/tasks-index-schema.sql");

    fn fixture_db(dir: &Path) -> std::path::PathBuf {
        let db = dir.join("tasks-index.sqlite");
        let con = rusqlite::Connection::open(&db).unwrap();
        con.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
        con.execute_batch(SCHEMA).unwrap();
        db
    }

    fn wsl() -> Host {
        Host::Wsl {
            distro: "Ubuntu".into(),
            user: "user".into(),
        }
    }

    fn record(identity: Option<String>) -> TaskRecord {
        TaskRecord {
            task_id: "sess_00000000-0000-4000-8000-000000000001".into(),
            trace_id: Some("00000000-0000-4000-8000-0000000000a1".into()),
            title: "apb: review".into(),
            workspace_path: "/home/user/project".into(),
            workspace_identity: identity,
            mode: "build".into(),
            model: Some("account:zai-individual-coding-plan/GLM-5.3-Flash".into()),
            thought_level: Some("high".into()),
            searchable_text: "review the diff".into(),
            now_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn identity_paths_are_normalized_like_the_desktop() {
        assert_eq!(normalize_identity_path("/home/user/p/"), "/home/user/p");
        assert_eq!(normalize_identity_path("//home//user\\p"), "/home/user/p");
        assert_eq!(normalize_identity_path("home/user"), "/home/user");
        assert_eq!(normalize_identity_path("/"), "/");
    }

    #[test]
    fn wsl_workspaces_get_the_remote_identity_and_native_ones_their_path() {
        let id = workspace_identity(&wsl(), "/home/user/project/").unwrap();
        assert_eq!(id, "remote:wsl:Ubuntu:user:/home/user/project");
        assert_eq!(workspace_key("/home/user/project", Some(&id)), id);
        assert_eq!(workspace_identity(&Host::Native, "/Users/dev/p"), None);
        assert_eq!(workspace_key("/Users/dev/p", None), "/Users/dev/p");
        assert_eq!(workspace_key("/Users/dev/p", Some("  ")), "/Users/dev/p");
    }

    #[test]
    fn distro_is_read_from_wslpath_output() {
        assert_eq!(
            distro_from_wslpath("\\\\wsl.localhost\\Ubuntu-24.04\\\n").as_deref(),
            Some("Ubuntu-24.04")
        );
        assert_eq!(
            distro_from_wslpath("\\\\wsl$\\Debian\\").as_deref(),
            Some("Debian")
        );
        assert_eq!(distro_from_wslpath("C:\\Windows"), None);
        assert_eq!(distro_from_wslpath(""), None);
    }

    #[test]
    fn host_is_derived_from_runtime_facts() {
        let wsl_kernel = Some("6.6.87.2-microsoft-standard-WSL2\n".to_string());
        // A login shell: the distro comes from the env.
        let shell = HostFacts {
            osrelease: wsl_kernel.clone(),
            distro_env: Some("Ubuntu".into()),
            wslpath_root: None,
            user: Some("user".into()),
        };
        assert_eq!(detect_host(&shell), Ok(wsl()));
        // A service without WSL_DISTRO_NAME: the distro comes from wslpath.
        let service = HostFacts {
            osrelease: wsl_kernel.clone(),
            distro_env: None,
            wslpath_root: Some("\\\\wsl.localhost\\Ubuntu\\".into()),
            user: Some("user".into()),
        };
        assert_eq!(detect_host(&service), Ok(wsl()));
        // WSL without a way to name the distro or the user: an error, not a
        // guess.
        let blind = HostFacts {
            osrelease: wsl_kernel,
            ..HostFacts::default()
        };
        assert!(detect_host(&blind).unwrap_err().contains("distro"));
        let nameless = HostFacts {
            user: None,
            ..service
        };
        assert!(detect_host(&nameless).unwrap_err().contains("user"));
        // Any other kernel is native.
        let linux = HostFacts {
            osrelease: Some("6.8.0-45-generic".into()),
            user: Some("user".into()),
            ..HostFacts::default()
        };
        assert_eq!(detect_host(&linux), Ok(Host::Native));
        assert_eq!(detect_host(&HostFacts::default()), Ok(Host::Native));
    }

    /// The meta document carries the same keys, in the same spelling, as a
    /// row the desktop wrote for a WSL remote workspace.
    #[test]
    fn meta_json_matches_the_desktop_shape() {
        let id = workspace_identity(&wsl(), "/home/user/project");
        let meta = record(id.clone()).meta_json();
        assert_eq!(
            meta,
            serde_json::json!({
                "taskId": "sess_00000000-0000-4000-8000-000000000001",
                "traceId": "00000000-0000-4000-8000-0000000000a1",
                "title": "apb: review",
                "titleOverridden": false,
                "workspacePath": "/home/user/project",
                "workspaceIdentity": id.unwrap(),
                "createdAt": 1_700_000_000_000_i64,
                "updatedAt": 1_700_000_000_000_i64,
                "mode": "build",
                "model": "account:zai-individual-coding-plan/GLM-5.3-Flash",
                "thoughtLevel": "high",
                "provider": "glm",
                "status": "completed",
                "target": null
            })
        );
        // A native workspace has no identity key at all; a missing trace id
        // falls back to the task id, a missing model/effort is omitted.
        let mut native = record(None);
        native.trace_id = None;
        native.model = None;
        native.thought_level = None;
        let meta = native.meta_json();
        assert!(meta.get("workspaceIdentity").is_none());
        assert!(meta.get("model").is_none());
        assert!(meta.get("thoughtLevel").is_none());
        assert_eq!(meta["traceId"], meta["taskId"]);
    }

    #[test]
    fn insert_writes_one_row_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let id = workspace_identity(&wsl(), "/home/user/project");
        let rec = record(id.clone());
        assert_eq!(insert_task(&db, &rec, Retry::default()), Ok(true));

        let con = rusqlite::Connection::open(&db).unwrap();
        // The one row, as column name -> JSON value.
        let row: serde_json::Map<String, serde_json::Value> = con
            .query_row("SELECT * FROM tasks", [], |r| {
                let names: Vec<String> = r
                    .as_ref()
                    .column_names()
                    .iter()
                    .map(|n| n.to_string())
                    .collect();
                let mut out = serde_json::Map::new();
                for (i, name) in names.into_iter().enumerate() {
                    let v = match r.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                        rusqlite::types::ValueRef::Integer(n) => serde_json::json!(n),
                        rusqlite::types::ValueRef::Text(t) => {
                            serde_json::json!(String::from_utf8_lossy(t))
                        }
                        other => panic!("unexpected column type {other:?}"),
                    };
                    out.insert(name, v);
                }
                Ok(out)
            })
            .unwrap();
        let id = id.unwrap();
        let want = serde_json::json!({
            "workspace_key": id,
            "workspace_path": "/home/user/project",
            "workspace_identity": id,
            "task_id": rec.task_id,
            "title": "apb: review",
            "task_status": "completed",
            "provider": "glm",
            "mode": "build",
            "model": rec.model,
            "migration_source": null,
            "forked_from_task_id": null,
            "created_at": rec.now_ms,
            "updated_at": rec.now_ms,
            "unread_at": null,
            "last_unread_at": 0,
            "pinned": 0,
            "archived": 0,
            "deleted": 0,
            "title_overridden": 0,
            "meta_json": rec.meta_json().to_string(),
            "searchable_text": "review the diff",
            "cron_automation_id": null,
            "off_peak_task_id": null,
        });
        assert_eq!(serde_json::Value::Object(row), want);

        // A second insert of the same session (a resumed turn) keeps the row
        // the desktop may already have renamed or updated.
        con.execute("UPDATE tasks SET title = 'renamed'", [])
            .unwrap();
        let mut again = rec.clone();
        again.now_ms += 1;
        assert_eq!(insert_task(&db, &again, Retry::default()), Ok(false));
        let (n, title): (i64, String) = con
            .query_row("SELECT COUNT(*), MAX(title) FROM tasks", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((n, title.as_str()), (1, "renamed"));
    }

    /// Long titles and search text are capped at the desktop's limits,
    /// counted in characters.
    #[test]
    fn insert_caps_title_and_search_text() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let mut rec = record(None);
        rec.title = "é".repeat(TITLE_MAX_CHARS + 10);
        rec.searchable_text = "x".repeat(SEARCHABLE_TEXT_MAX_CHARS + 10);
        insert_task(&db, &rec, Retry::default()).unwrap();
        let con = rusqlite::Connection::open(&db).unwrap();
        let (title, text, key): (String, String, String) = con
            .query_row(
                "SELECT title, searchable_text, workspace_key FROM tasks",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
        assert_eq!(text.chars().count(), SEARCHABLE_TEXT_MAX_CHARS);
        assert_eq!(key, "/home/user/project");
    }

    #[test]
    fn a_missing_database_is_an_error_and_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tasks-index.sqlite");
        let err = insert_task(&db, &record(None), Retry::default()).unwrap_err();
        assert!(err.contains("tasks-index.sqlite"), "{err}");
        assert!(!db.exists(), "apb must never create the desktop's index");
    }

    #[test]
    fn schema_drift_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tasks-index.sqlite");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE other (x INTEGER);")
            .unwrap();
        let err = insert_task(&db, &record(None), Retry::default()).unwrap_err();
        assert!(err.contains("tasks"), "{err}");
    }

    const QUICK: Retry = Retry {
        busy_timeout: Duration::from_millis(20),
        backoff: &[Duration::from_millis(20), Duration::from_millis(20)],
    };

    /// A writer holding the database gives up after the retries with an
    /// error, not a hang, and leaves nothing behind.
    #[test]
    fn a_database_that_stays_busy_fails_after_bounded_retries() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let holder = rusqlite::Connection::open(&db).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let started = std::time::Instant::now();
        let err = insert_task(&db, &record(None), QUICK).unwrap_err();
        assert!(err.contains("locked") || err.contains("busy"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
        holder.execute_batch("ROLLBACK;").unwrap();
        let n: i64 = holder
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    /// A writer that finishes while apb waits: the retry gets the row in.
    #[test]
    fn a_briefly_busy_database_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        let holder = rusqlite::Connection::open(&db).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let release = std::thread::spawn(move || {
            rx.recv().unwrap();
            std::thread::sleep(Duration::from_millis(150));
            holder.execute_batch("COMMIT;").unwrap();
        });
        tx.send(()).unwrap();
        const SLOW: Retry = Retry {
            busy_timeout: Duration::from_millis(50),
            backoff: &[
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(400),
            ],
        };
        assert_eq!(insert_task(&db, &record(None), SLOW), Ok(true));
        release.join().unwrap();
    }
}
