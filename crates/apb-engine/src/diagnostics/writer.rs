//! The one writer of the diagnostics sidecar. It never fails execution:
//! every error is counted and dropped.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::run_redact::{RunRedactor, WorkspaceRedaction};

use super::index::Index;
use super::{FILE, LOCK, MAX_BYTES, VERSION};

static WRITE_ERRORS: AtomicU64 = AtomicU64::new(0);

/// How many diagnostic writes failed in this process (and were dropped).
pub fn write_errors() -> u64 {
    WRITE_ERRORS.load(Ordering::Relaxed)
}

#[cfg(any(test, feature = "testing"))]
static DISABLED: Mutex<std::collections::BTreeSet<PathBuf>> =
    Mutex::new(std::collections::BTreeSet::new());

/// Turns the writer off, for this process, for every run directory under
/// `dir` (a run directory or a project): the equal-journals rule test runs
/// every playbook once without diagnostics. Test builds only.
#[cfg(any(test, feature = "testing"))]
pub fn disable_under(dir: &Path) {
    if let Ok(mut set) = DISABLED.lock() {
        set.insert(dir.to_path_buf());
    }
}

#[cfg(any(test, feature = "testing"))]
fn disabled(run_dir: &Path) -> bool {
    DISABLED
        .lock()
        .is_ok_and(|set| set.iter().any(|d| run_dir.starts_with(d)))
}

#[cfg(not(any(test, feature = "testing")))]
fn disabled(_run_dir: &Path) -> bool {
    false
}

/// How long a run's resolved workspace secrets are reused before they are
/// resolved again, so a secret changed while a run drives is picked up.
const WORKSPACE_TTL: Duration = Duration::from_secs(30);

/// The most runs a process keeps a writer state for; past it the states
/// are dropped and rebuilt on the next write.
const MAX_CACHED_RUNS: usize = 64;

/// What the writer keeps between the writes of one run in this process, so
/// a decision on a long loop neither resolves the workspace's connector
/// secrets nor rereads the whole sidecar each time.
struct RunState {
    workspace: WorkspaceRedaction,
    resolved: Instant,
    /// The index of the sidecar and its length when it was read or last
    /// appended to; reread when the file's length differs.
    index: Option<(u64, Index)>,
}

static STATES: Mutex<Option<HashMap<PathBuf, RunState>>> = Mutex::new(None);

/// The run's redactor, from the cached workspace redaction when it is
/// fresh. The run's own values and declarations are read every time: a
/// patch may add a declaration.
fn redactor_for(run_dir: &Path) -> RunRedactor {
    let lock = || STATES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = lock().as_ref().and_then(|m| m.get(run_dir))
        && state.resolved.elapsed() < WORKSPACE_TTL
    {
        return state.workspace.run_dir(run_dir);
    }
    // Resolved outside the lock: it may run a connector's secret command,
    // and other runs of this process write meanwhile.
    let root = crate::run_config::project_root_of(run_dir).unwrap_or_else(|| run_dir.to_path_buf());
    let workspace = WorkspaceRedaction::new(&root);
    let redactor = workspace.run_dir(run_dir);
    let mut states = lock();
    let states = states.get_or_insert_with(HashMap::new);
    if states.len() >= MAX_CACHED_RUNS && !states.contains_key(run_dir) {
        states.clear();
    }
    let index = states.remove(run_dir).and_then(|s| s.index);
    states.insert(
        run_dir.to_path_buf(),
        RunState {
            workspace,
            resolved: Instant::now(),
            index,
        },
    );
    redactor
}

/// Whether the sidecar of `len` bytes already holds `(kind, key)`, from the
/// cached index while the file has the length it had.
fn already_written(run_dir: &Path, len: u64, kind: &str, key: &str) -> bool {
    let mut states = STATES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = states.as_mut().and_then(|m| m.get_mut(run_dir)) else {
        return Index::load(run_dir).contains(kind, key);
    };
    match &state.index {
        Some((at, index)) if *at == len => index.contains(kind, key),
        _ => {
            let index = Index::load(run_dir);
            let found = index.contains(kind, key);
            state.index = Some((len, index));
            found
        }
    }
}

/// Records an append of `(kind, key)` that brought the sidecar to `len`.
fn appended(run_dir: &Path, len: u64, kind: &str, key: &str) {
    let mut states = STATES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = states.as_mut().and_then(|m| m.get_mut(run_dir))
        && let Some((at, index)) = state.index.as_mut()
    {
        index.insert(kind, key);
        *at = len;
    }
}

/// The text scrubber a record builder gets: the run's redactor (its own and
/// its ancestors' secret values, the declarations it keeps, the workspace's
/// connector secrets), applied to a complete value before it is clipped.
pub struct Scrub<'a>(&'a RunRedactor);

impl Scrub<'_> {
    /// `text` scrubbed whole, then clipped to `max` characters. Clipping
    /// first could cut a secret so the scrubber no longer recognizes the
    /// fragment.
    pub fn clip(&self, text: &str, max: usize) -> String {
        let scrubbed = self.0.redact_text(text);
        match scrubbed.char_indices().nth(max) {
            Some((at, _)) => format!("{}...", &scrubbed[..at]),
            None => scrubbed,
        }
    }

    /// Whether the scrubber changes `text` at all (a secret value replaced,
    /// or the run's text withheld): a value derived from such a text, such
    /// as the number it reads as, must not be recorded beside it.
    pub fn alters(&self, text: &str) -> bool {
        self.0.redact_text(text) != text
    }
}

/// One record: its kind, the journal seq it follows, the dedup key the
/// [`super::index::Index`] knows it by, and its own fields.
pub struct Record {
    pub kind: &'static str,
    pub after_seq: u64,
    pub key: String,
    pub fields: Map<String, Value>,
}

/// Builds a record with the run's scrubber and appends it, unless the
/// sidecar already holds one with the same key. `build` returning `None`
/// writes nothing. Never fails: an error is counted in [`write_errors`].
pub fn write_with(run_dir: &Path, build: impl FnOnce(&Scrub<'_>) -> Option<Record>) {
    if disabled(run_dir) {
        return;
    }
    if try_write(run_dir, build).is_err() {
        WRITE_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
}

fn try_write(
    run_dir: &Path,
    build: impl FnOnce(&Scrub<'_>) -> Option<Record>,
) -> std::io::Result<()> {
    apb_core::process_mode::guard()?;
    let redactor = redactor_for(run_dir);
    let Some(record) = build(&Scrub(&redactor)) else {
        return Ok(());
    };
    let path = run_dir.join(FILE);
    // One attempt: a held lock drops this record rather than holding up
    // the drive (a stale one is taken over by the usual rule).
    let _lock = apb_core::fsutil::try_lock_dir(run_dir, LOCK)?;
    let len = match std::fs::symlink_metadata(&path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(std::io::Error::other(
                "diagnostics file is not a regular file",
            ));
        }
        Ok(meta) if meta.len() >= MAX_BYTES => {
            return Err(std::io::Error::other("diagnostics file is full"));
        }
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    if already_written(run_dir, len, record.kind, &record.key) {
        return Ok(());
    }
    let mut value = Map::new();
    value.insert("v".into(), Value::from(VERSION));
    value.insert("kind".into(), Value::from(record.kind));
    value.insert("after_seq".into(), Value::from(record.after_seq));
    value.insert("key".into(), Value::from(record.key.clone()));
    for (k, v) in record.fields {
        value.entry(k).or_insert(v);
    }
    let mut value = Value::Object(value);
    // Every string once more, whole: a builder that forgot to scrub a text
    // still writes no secret value.
    redactor.known_values().scrub_values_json(&mut value);
    let mut line = serde_json::to_vec(&value).map_err(std::io::Error::other)?;
    line.push(b'\n');
    let mut file = apb_core::fsutil::open_append_private(&path)?;
    file.write_all(&line)?;
    appended(run_dir, len + line.len() as u64, record.kind, &record.key);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(key: &str) -> Record {
        let mut fields = Map::new();
        fields.insert("note".into(), Value::from("x"));
        Record {
            kind: "test_kind",
            after_seq: 3,
            key: key.into(),
            fields,
        }
    }

    fn lines(run_dir: &Path) -> Vec<Value> {
        super::super::index::read_raw(run_dir)
    }

    /// Forgets this process's state of a run, as a resume in a new process
    /// starts without it.
    fn forget(run_dir: &Path) {
        if let Some(m) = STATES.lock().unwrap().as_mut() {
            m.remove(run_dir);
        }
    }

    #[test]
    fn a_record_is_written_once_per_key_in_a_private_file_across_a_resume() {
        let dir = tempfile::tempdir().unwrap();
        write_with(dir.path(), |_| Some(record("a|1")));
        write_with(dir.path(), |_| Some(record("a|1")));
        forget(dir.path());
        write_with(dir.path(), |_| Some(record("a|1")));
        write_with(dir.path(), |_| Some(record("a|2")));
        let keys: Vec<String> = lines(dir.path())
            .iter()
            .map(|v| v["key"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(keys, vec!["a|1", "a|2"]);
        let first = &lines(dir.path())[0];
        assert_eq!(
            (
                first["v"].as_u64(),
                first["kind"].as_str(),
                first["after_seq"].as_u64()
            ),
            (Some(1), Some("test_kind"), Some(3))
        );
        // A record written by another process meanwhile is seen too.
        let mut text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        text.push_str("{\"v\":1,\"kind\":\"test_kind\",\"after_seq\":9,\"key\":\"a|3\"}\n");
        std::fs::write(dir.path().join(FILE), text).unwrap();
        write_with(dir.path(), |_| Some(record("a|3")));
        assert_eq!(lines(dir.path()).len(), 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::remove_file(dir.path().join(FILE)).unwrap();
            forget(dir.path());
            write_with(dir.path(), |_| Some(record("b|1")));
            let mode = std::fs::metadata(dir.path().join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_full_sidecar_takes_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let full = vec![b' '; MAX_BYTES as usize];
        std::fs::write(dir.path().join(FILE), &full).unwrap();
        let errors = write_errors();
        write_with(dir.path(), |_| Some(record("a|1")));
        assert_eq!(
            std::fs::metadata(dir.path().join(FILE)).unwrap().len(),
            MAX_BYTES
        );
        assert!(write_errors() > errors);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_or_directory_sidecar_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere.jsonl");
        std::fs::write(&target, "").unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir(&run).unwrap();
        std::os::unix::fs::symlink(&target, run.join(FILE)).unwrap();
        write_with(&run, |_| Some(record("a|1")));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
        let other = dir.path().join("other");
        std::fs::create_dir_all(other.join(FILE)).unwrap();
        write_with(&other, |_| Some(record("a|1")));
        assert!(other.join(FILE).is_dir());
        assert!(
            std::fs::read_dir(other.join(FILE))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn a_held_lock_drops_the_record_without_waiting() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LOCK), "someone").unwrap();
        let started = Instant::now();
        write_with(dir.path(), |_| Some(record("a|1")));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(!dir.path().join(FILE).exists());
        // The lock is someone else's: it stays.
        assert_eq!(
            std::fs::read_to_string(dir.path().join(LOCK)).unwrap(),
            "someone"
        );
    }

    #[test]
    fn a_disabled_run_and_an_empty_builder_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write_with(dir.path(), |_| None);
        assert!(!dir.path().join(FILE).exists());
        let run = dir.path().join("runs/r1");
        std::fs::create_dir_all(&run).unwrap();
        disable_under(&dir.path().join("runs"));
        let mut built = false;
        write_with(&run, |_| {
            built = true;
            Some(record("a|1"))
        });
        assert!(!built, "a disabled writer does not even build");
        assert!(!run.join(FILE).exists());
    }

    /// A declaration a patch adds while this process already holds the
    /// run's writer state is honoured on the next write, and one the patch
    /// drops from the snapshot stays (it is kept in the manifest).
    #[test]
    fn the_run_values_and_declarations_are_read_on_every_write() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run).unwrap();
        let value = "quietharbor".repeat(6);
        let cfg = crate::run_config::RunConfig {
            params: [("note".to_string(), value.clone())].into(),
            ..Default::default()
        };
        crate::run_config::write_run_config(&run, &cfg).unwrap();
        let plain = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - { name: note, type: text }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
        let declared = plain.replace("type: text }", "type: text, secret: true }");
        std::fs::write(run.join("playbook.yaml"), plain).unwrap();
        let text_of = |key: &str| {
            let v = lines(&run).into_iter().find(|v| v["key"] == key).unwrap();
            v["note"].as_str().unwrap().to_string()
        };
        let write = |key: &str| {
            let key = key.to_string();
            write_with(&run, |scrub| {
                let mut r = record(&key);
                r.fields
                    .insert("note".into(), Value::from(scrub.clip(&value, 200)));
                Some(r)
            })
        };
        write("k|1");
        assert_eq!(text_of("k|1"), value, "an undeclared value is kept");
        // A patch declares it: the next write in the same process scrubs it.
        let pb = apb_core::schema::Playbook::from_yaml(&declared).unwrap();
        crate::run_redact::keep_secret_declarations(&run, &[&pb]).unwrap();
        write("k|2");
        assert!(
            !text_of("k|2").contains("quietharbor"),
            "{}",
            text_of("k|2")
        );
        // A later snapshot without it: the kept declaration still counts.
        std::fs::write(run.join("playbook.yaml"), plain).unwrap();
        write("k|3");
        assert!(
            !text_of("k|3").contains("quietharbor"),
            "{}",
            text_of("k|3")
        );
    }

    apb_core::read_only_test! {
        fn a_read_only_process_writes_nothing() before dir = { tempfile::tempdir().unwrap() } then {
            let errors = write_errors();
            write_with(dir.path(), |_| Some(record("a|1")));
            assert!(!dir.path().join(FILE).exists());
            assert!(!dir.path().join(LOCK).exists());
            assert!(write_errors() > errors);
        }
    }
}
