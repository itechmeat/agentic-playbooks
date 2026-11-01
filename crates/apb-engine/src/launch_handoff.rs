//! The handoff file: how a process that checked a launch hands the result
//! to the detached process that performs it (`apb run --supervise` to
//! `__drive-supervised`, `apb eval` to each case's `apb run`, a resume to
//! its `__drive-run --resume`), without putting it on an argv where any
//! process can read or forge it.
//!
//! `<root>/.apb/handoff/<128-bit random>.json`, created exclusively with
//! mode 0600; the receiver gets only its path. It is read once and deleted
//! at once, and accepted only when it is younger than [`MAX_AGE`], is a
//! regular file the user owns with mode 0600, and was written by the
//! receiver's own parent (the pid in the file is the parent pid the
//! receiver read when it started). The writer waits until the receiver took
//! it ([`await_consumed`]), so the parent check never races the writer's
//! exit, and deletes it itself when the spawn fails. A handoff is a
//! convenience between apb's own processes, not a proof: a process that
//! forges one is outside the launch guarantee.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::gate::launch::{HandoffGrant, LaunchGrant};

/// The directory, under the project's `.apb`.
pub const DIR: &str = "handoff";
/// How old a handoff may be when it is taken.
pub const MAX_AGE: Duration = Duration::from_secs(60);
/// How long a writer waits for its receiver to take the handoff.
pub const CONSUME_WAIT: Duration = Duration::from_secs(10);

/// What a handoff carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Handoff {
    /// A detached supervised start: the launch decision bound to the
    /// permit's nonce, and the irreversible consent with its own nonce.
    Supervise {
        grant: HandoffGrant,
        /// The version the parent's check ran and how it was chosen: the
        /// receiver fixes them, so it makes no draw of its own.
        version: String,
        choice: crate::gate::launch::Choice,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        consent: Option<ForwardedConsent>,
    },
    /// An eval case: the decision bound to the definition under test.
    Eval { grant: HandoffGrant },
    /// A detached resume: the grant the driver commits.
    Resume(LaunchGrant),
}

/// The irreversible consent a parent obtained: who gave it and the nonce of
/// the tree it was given for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardedConsent {
    pub by: String,
    pub nonce: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct File {
    writer_pid: u32,
    written_ms: u128,
    handoff: Handoff,
}

/// Writes `handoff` for a child of this process and returns its path.
pub fn write(root: &Path, handoff: &Handoff) -> std::io::Result<PathBuf> {
    apb_core::process_mode::guard()?;
    let dir = root.join(".apb").join(DIR);
    std::fs::create_dir_all(&dir)?;
    let name = format!("{}.json", uuid::Uuid::new_v4().simple());
    let path = dir.join(name);
    let body = serde_json::to_vec(&File {
        writer_pid: std::process::id(),
        written_ms: apb_core::clock::now_ms(),
        handoff: handoff.clone(),
    })
    .map_err(std::io::Error::other)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path)?;
    f.write_all(&body)?;
    f.sync_all()?;
    Ok(path)
}

/// Removes a handoff nobody took.
pub fn discard(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Waits until the receiver took the handoff at `path` (it is gone);
/// `false` when `wait` passed first.
pub fn await_consumed(path: &Path, wait: Duration) -> bool {
    let deadline = std::time::Instant::now() + wait;
    while path.exists() {
        if std::time::Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// The parent pid of this process, read once as early as possible by a
/// receiver ([`take`] compares it with the writer's).
pub fn parent_pid() -> u32 {
    #[cfg(unix)]
    {
        std::os::unix::process::parent_id()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Takes the handoff at `path`: reads it once, deletes it, and returns it
/// only when every check passes; the error says which one did not.
/// `parent` is the parent pid the receiver read when it started.
///
/// Only a file apb could have written is touched: its directory must be
/// `<root>/.apb/handoff` (compared canonically) and its name a handoff's,
/// and it is opened without following a link and without blocking (a FIFO
/// never stalls the receiver). The checks of type, mode and owner run on
/// the open file; only then is it deleted. A path anywhere else is refused
/// and left alone.
pub fn take(root: &Path, path: &Path, parent: u32) -> Result<Handoff, String> {
    confined(root, path)?;
    let opened = open_no_follow(path);
    let (meta, raw) = match opened {
        Ok(mut f) => {
            let meta = f.metadata().map_err(|e| format!("no handoff: {e}"));
            let raw = meta.as_ref().ok().filter(|m| m.is_file()).map(|_| {
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut f, &mut buf).map(|_| buf)
            });
            (meta, raw)
        }
        Err(e) => {
            // A link or a missing file: a link inside the handoff directory
            // is not ours either, and goes.
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                discard(path);
                return Err("the handoff is not a regular file".into());
            }
            return Err(format!("no handoff: {e}"));
        }
    };
    discard(path);
    let meta = meta?;
    if !meta.is_file() {
        return Err("the handoff is not a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.mode() & 0o777 != 0o600 {
            return Err("the handoff is not private (mode 0600)".into());
        }
        // SAFETY: `getuid` has no preconditions and cannot fail.
        if meta.uid() != unsafe { libc::getuid() } {
            return Err("the handoff belongs to another user".into());
        }
    }
    let raw = raw
        .ok_or_else(|| "cannot read the handoff".to_string())?
        .map_err(|e| format!("cannot read the handoff: {e}"))?;
    let file: File =
        serde_json::from_slice(&raw).map_err(|e| format!("the handoff does not read: {e}"))?;
    let age = apb_core::clock::now_ms().saturating_sub(file.written_ms);
    if age > MAX_AGE.as_millis() {
        return Err("the handoff is stale".into());
    }
    if file.writer_pid != parent || parent == 0 {
        return Err("the handoff was not written by this process's parent".into());
    }
    Ok(file.handoff)
}

/// Whether `path` names a handoff in `<root>/.apb/handoff`: a name
/// [`write`] makes, in that very directory.
fn confined(root: &Path, path: &Path) -> Result<(), String> {
    let outside = || {
        format!(
            "`{}` is not a launch handoff of this project; it is left as it is",
            path.display()
        )
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(outside)?;
    let well_named = name
        .strip_suffix(".json")
        .is_some_and(|stem| stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_hexdigit()));
    let dir = std::fs::canonicalize(root.join(".apb").join(DIR)).map_err(|_| outside())?;
    let parent = path
        .parent()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .ok_or_else(outside)?;
    if !well_named || parent != dir {
        return Err(outside());
    }
    Ok(())
}

fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    opts.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::launch::{LaunchPolicy, LaunchRecord};

    fn grant() -> Handoff {
        Handoff::Resume(LaunchGrant {
            record: LaunchRecord {
                kind: "resume".into(),
                initiator: "agent_mcp".into(),
                confirmation: Some("host_relayed".into()),
                policy: LaunchPolicy::Ask,
                sources: vec![],
            },
            permit_digest: Some("p".into()),
            policy_state: Some("s".into()),
            tolerant: false,
            run_id: Some("r1".into()),
            pins: None,
            nonce: None,
            version: None,
            choice: None,
        })
    }

    #[test]
    fn a_handoff_is_taken_once_by_the_writers_child() {
        let root = tempfile::tempdir().unwrap();
        let path = write(root.path(), &grant()).unwrap();
        // This test process plays the child of itself.
        assert_eq!(
            take(root.path(), &path, std::process::id()).unwrap(),
            grant()
        );
        assert!(!path.exists());
        assert!(take(root.path(), &path, std::process::id()).is_err());
    }

    #[test]
    fn a_handoff_of_another_parent_is_refused_and_still_deleted() {
        let root = tempfile::tempdir().unwrap();
        let path = write(root.path(), &grant()).unwrap();
        let err = take(root.path(), &path, std::process::id() + 1).unwrap_err();
        assert!(err.contains("parent"), "{err}");
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_wide_handoff_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let path = write(root.path(), &grant()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            take(root.path(), &path, std::process::id())
                .unwrap_err()
                .contains("private")
        );
    }

    #[test]
    fn a_stale_handoff_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let path = write(root.path(), &grant()).unwrap();
        let mut file: File = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        file.written_ms -= MAX_AGE.as_millis() + 1;
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(
            take(root.path(), &path, std::process::id())
                .unwrap_err()
                .contains("stale")
        );
    }

    /// A handoff-shaped name in the project's handoff directory.
    fn slot(root: &Path) -> PathBuf {
        let dir = root.join(".apb").join(DIR);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{}.json", uuid::Uuid::new_v4().simple()))
    }

    #[test]
    fn a_path_outside_the_handoff_directory_is_refused_and_left_alone() {
        let root = tempfile::tempdir().unwrap();
        // A real handoff's bytes, copied anywhere else, are still not one.
        let real = write(root.path(), &grant()).unwrap();
        let outside = root.path().join("key");
        std::fs::copy(&real, &outside).unwrap();
        let err = take(root.path(), &outside, std::process::id()).unwrap_err();
        assert!(
            err.contains("not a launch handoff of this project"),
            "{err}"
        );
        assert!(outside.exists(), "left as it is");
        // The same file under another project's handoff directory.
        let other = tempfile::tempdir().unwrap();
        assert!(take(other.path(), &real, std::process::id()).is_err());
        assert!(real.exists(), "left as it is");
    }

    #[test]
    fn a_misnamed_file_in_the_handoff_directory_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let real = write(root.path(), &grant()).unwrap();
        let misnamed = real.with_file_name("notes.json");
        std::fs::rename(&real, &misnamed).unwrap();
        let err = take(root.path(), &misnamed, std::process::id()).unwrap_err();
        assert!(err.contains("not a launch handoff"), "{err}");
        assert!(misnamed.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_never_followed_and_its_target_survives() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("precious");
        std::fs::write(&target, "keep").unwrap();
        let link = slot(root.path());
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = take(root.path(), &link, std::process::id()).unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        assert!(std::fs::symlink_metadata(&link).is_err(), "the link goes");
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_or_a_directory_is_refused_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        let fifo = slot(root.path());
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: mkfifo(3) on a NUL-terminated path owned here.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let done = std::sync::mpsc::channel();
        let path = fifo.clone();
        let root_path = root.path().to_path_buf();
        std::thread::spawn(move || {
            let _ = done.0.send(take(&root_path, &path, std::process::id()));
        });
        let out = done
            .1
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a FIFO never blocks the receiver");
        assert!(out.unwrap_err().contains("not a regular file"));
        let dir = slot(root.path());
        std::fs::create_dir(&dir).unwrap();
        assert!(take(root.path(), &dir, std::process::id()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_handoff_of_another_user_is_refused() {
        // SAFETY: getuid has no preconditions.
        if unsafe { libc::getuid() } != 0 {
            eprintln!("skipped: changing a file's owner needs root");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let path = write(root.path(), &grant()).unwrap();
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: chown(2) on a NUL-terminated path owned here; 65534 is nobody.
        assert_eq!(unsafe { libc::chown(c.as_ptr(), 65534, 65534) }, 0);
        let err = take(root.path(), &path, std::process::id()).unwrap_err();
        assert!(err.contains("another user"), "{err}");
    }
}
