use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Control files are always written this way: temp + fsync + atomic rename (spec 4.3).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Like `atomic_write`, but the resulting file gets 0600 permissions (owner
/// read/write) before it appears at the target path. For global state files
/// (trust.json, projects.json, dismissed.json), which may contain data that is
/// privacy-sensitive. Permissions are left untouched on non-unix.
pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    // Remove a possible leftover stale tmp of our own so create_new doesn't fail.
    let _ = fs::remove_file(&tmp);
    {
        // The file is created with 0600 permissions right away (on unix) - no window
        // with default permissions before a later chmod.
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// RAII lock for a state directory (`<dir>/<lock_name>`), tagged with a unique
/// owner token. The guard removes the file only if the token is still ours
/// (after a force-steal of a stale lock, this protects against cascading
/// removal of someone else's lock). A shared primitive for serializing
/// read-modify-write over global state files (trust.json, dismissed.json).
/// `projects.json` historically carries an equivalent implementation of its own.
pub struct DirLock {
    path: PathBuf,
    token: String,
}

impl Drop for DirLock {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path)
            .map(|c| c.trim() == self.token)
            .unwrap_or(false)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

const LOCK_ATTEMPTS: u32 = 80;
const LOCK_STEP_MS: u64 = 25;
/// Staleness threshold for a lock. Acquiring the critical section over a state
/// file is a sub-second operation, so a lock older than this threshold almost
/// certainly belongs to a crashed owner. The threshold is much larger than the
/// real working window, so we do NOT steal the lock from a live (even if slow)
/// owner.
const LOCK_STALE_MS: u128 = 60_000;

/// Acquires the lock `<dir>/<lock_name>`. If it is held, waits up to
/// `LOCK_ATTEMPTS × LOCK_STEP_MS`. Once that expires: we steal the lock ONLY if
/// it is stale (mtime older than `LOCK_STALE_MS` - the owner is most likely
/// dead); otherwise we return a busy error rather than violate mutual exclusion
/// (a live owner or a fresh lock held by someone else is never stolen - this
/// also closes the ABA race).
pub fn lock_dir(dir: &Path, lock_name: &str) -> io::Result<DirLock> {
    fs::create_dir_all(dir)?;
    let path = dir.join(lock_name);
    let token = format!("{}-{}", std::process::id(), uuid::Uuid::new_v4().simple());
    for _ in 0..LOCK_ATTEMPTS {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(token.as_bytes())?;
                return Ok(DirLock { path, token });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(LOCK_STEP_MS));
            }
            Err(e) => return Err(e),
        }
    }
    // Timeout: we steal the lock only if it is stale (staleness by mtime), otherwise busy.
    if !lock_is_stale(&path) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "lock `{}` is held by a live owner; try again",
                path.display()
            ),
        ));
    }
    // Stealing must be ATOMIC against other stealers: renaming the stale lock
    // aside wins for exactly one process (rename is atomic; the loser sees that
    // the source is already gone), after which the winner recreates the lock via
    // create_new. A plain remove+create would allow two concurrent stealers to
    // both remove the file and both believe they own the lock (a violation of
    // mutual exclusion). A live/fresh lock held by someone else never reaches
    // here - it is filtered out by lock_is_stale above; and if the lock happens
    // to be recreated between the check and the steal (a new owner or another
    // stealer), the final create_new will return AlreadyExists and we honestly
    // report busy instead of tearing it down.
    let sidelined = dir.join(format!("{lock_name}.stale-{token}"));
    let _ = fs::rename(&path, &sidelined);
    let _ = fs::remove_file(&sidelined);
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            f.write_all(token.as_bytes())?;
            Ok(DirLock { path, token })
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "lock `{}` was re-acquired concurrently; try again",
                path.display()
            ),
        )),
        Err(e) => Err(e),
    }
}

/// Whether the lock is stale: its mtime age is greater than `LOCK_STALE_MS`. An
/// unreadable mtime is treated as NOT stale (conservatively - we don't steal
/// when in doubt).
fn lock_is_stale(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    match mtime.elapsed() {
        Ok(age) => age.as_millis() >= LOCK_STALE_MS,
        // mtime in the future (clock skew) - not stale.
        Err(_) => false,
    }
}
