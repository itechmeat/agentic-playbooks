pub mod beneath;

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The path to re-exec this binary from (`__drive-run`, `__drive-supervised`,
/// the `__ask-server` sidecar). Normally `current_exe`. A long-running
/// process (`apb mcp` in an agent session, the dashboard) outlives a
/// reinstall of `apb`, which replaces the file: Linux then reports the old
/// executable as `<path> (deleted)`, which cannot be spawned. The binary now
/// at the original path is the one to run, exactly what a fresh `apb` would
/// be; it re-opens the run from disk like any driver.
pub fn reexec_exe() -> io::Result<PathBuf> {
    Ok(live_exe_path(std::env::current_exe()?))
}

/// Spawns `cmd`, retrying for up to about two seconds while the executable
/// is busy (`ETXTBSY`). A binary that was just written, by a reinstall or a
/// copy, stays "busy" for as long as any process still holds a write handle
/// to it, and on Linux a process forked while the writer had the file open
/// holds that handle until its own `exec`. The window is short but real on a
/// loaded host; any other error is returned at once.
pub fn spawn_when_not_busy(cmd: &mut std::process::Command) -> io::Result<std::process::Child> {
    crate::process_mode::guard_program_start()?;
    const ATTEMPTS: u32 = 40;
    const STEP: std::time::Duration = std::time::Duration::from_millis(50);
    let mut attempt = 1;
    loop {
        match cmd.spawn() {
            Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy && attempt < ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(STEP);
            }
            other => return other,
        }
    }
}

fn live_exe_path(exe: PathBuf) -> PathBuf {
    if exe.exists() {
        return exe;
    }
    exe.to_str()
        .and_then(|s| s.strip_suffix(" (deleted)"))
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .unwrap_or(exe)
}

/// Creates a symbolic link at `link` pointing at `target`.
///
/// Symlinks are a unix-only capability in apb (skill bridges, materialized
/// bundle copies). On every other platform this fails with
/// [`io::ErrorKind::Unsupported`], which callers map to their own domain
/// error rather than each re-deciding what an absent symlink API means.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    crate::process_mode::guard_write(link)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(not(unix))]
    {
        let _ = (target, link);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlinks are only supported on unix",
        ))
    }
}

/// Recursively copies the directory `src` into `dst` (created if missing). The
/// one tree copy for definition content: a new playbook version's `scripts/`,
/// the scripts a run starts with, a profile moved between scopes, a migrated
/// playbook, a skill snapshot.
///
/// A symlink is recreated as the same symlink, never followed: following it
/// would copy the content of whatever it points at, possibly a file outside
/// the repository, into a directory that is committed or executed. A
/// directory is copied recursively, a regular file byte for byte, and any
/// other entry (a FIFO, a socket, a device) is refused with
/// [`io::ErrorKind::InvalidInput`] rather than opened. Off unix a symlink
/// fails with [`io::ErrorKind::Unsupported`] (see [`symlink`]).
pub fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    crate::process_mode::guard_write(dst)?;
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        // `DirEntry::file_type` does not follow symlinks.
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            symlink(&fs::read_link(&from)?, &to)?;
        } else if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else if ft.is_file() {
            fs::copy(&from, &to)?;
            #[cfg(test)]
            copy_seam::after_file();
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "not a regular file, directory or symlink: {}",
                    from.display()
                ),
            ));
        }
    }
    Ok(())
}

/// A pause between two file reads of [`copy_tree`], for the unit tests
/// that race a writer against a copy in progress.
#[cfg(test)]
pub(crate) mod copy_seam {
    use std::cell::RefCell;

    thread_local! {
        static AFTER_FILE: RefCell<Option<Box<dyn FnMut()>>> = RefCell::new(None);
    }

    /// Runs `hook` after every file [`super::copy_tree`] copies on this
    /// thread; `None` removes it.
    pub(crate) fn set(hook: Option<Box<dyn FnMut()>>) {
        AFTER_FILE.with(|h| *h.borrow_mut() = hook);
    }

    pub(super) fn after_file() {
        AFTER_FILE.with(|h| {
            if let Some(hook) = h.borrow_mut().as_mut() {
                hook();
            }
        });
    }
}

/// Fails when `path`, or any existing component of it strictly below
/// `anchor`, is a symbolic link. `anchor` itself is trusted (the caller
/// created it). A write or a removal through such a path would land at the
/// link's target, outside the directory the caller meant.
pub fn ensure_no_symlink_below(anchor: &Path, path: &Path) -> io::Result<()> {
    let rel = path.strip_prefix(anchor).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not under {}", path.display(), anchor.display()),
        )
    })?;
    let mut cur = anchor.to_path_buf();
    for c in rel.components() {
        match c {
            std::path::Component::Normal(name) => cur.push(name),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a plain path under the anchor", path.display()),
                ));
            }
        }
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is a symlink", cur.display()),
                ));
            }
            Ok(_) => {}
            // Nothing below a missing component exists yet either.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Opens the regular file at `path` for reading without following a
/// symbolic link at its last component (`O_NOFOLLOW` on unix; elsewhere the
/// entry is checked first). A link, a directory, a fifo or a device is
/// refused: reading one could block or leave the tree the caller meant.
pub fn open_regular_no_follow(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = {
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a symlink", path.display()),
            ));
        }
        File::open(path)?
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(file)
}

/// The largest settings file apb reads (`policy.yaml`, a project's
/// `.apb/config.yaml`).
pub const SETTINGS_FILE_MAX_BYTES: u64 = 1 << 20;

/// The text of the small settings file at `path`, `None` when it is absent.
/// A link is followed, but what it reaches must be a regular file (never a
/// fifo or a device, which could block or never end) of at most `cap`
/// bytes, in UTF-8; anything else is an error that names the file.
pub fn read_settings_file(path: &Path, cap: u64) -> io::Result<Option<String>> {
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
    };
    #[cfg(not(unix))]
    let opened = File::open(path);
    let file = match opened {
        Ok(f) => f,
        // No `.apb` directory (a file in its place) is no settings file.
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let too_large = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is larger than {cap} bytes", path.display()),
        )
    };
    if meta.len() > cap {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut io::Read::take(file, cap.saturating_add(1)), &mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(too_large());
    }
    String::from_utf8(bytes).map(Some).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not UTF-8 text", path.display()),
        )
    })
}

/// Reads at most `cap` bytes of `reader`. Returns them and whether more was
/// left unread. The reader is left right after the bytes returned (the one
/// byte read to tell whether more is left is given back), so a second call
/// continues where this one stopped.
pub fn read_prefix_capped<R: io::Read + io::Seek>(
    reader: &mut R,
    cap: u64,
) -> io::Result<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    let mut limited = io::Read::take(&mut *reader, cap.saturating_add(1));
    io::Read::read_to_end(&mut limited, &mut out)?;
    let cut = out.len() as u64 > cap;
    if cut {
        out.truncate(cap as usize);
        reader.seek(io::SeekFrom::Current(-1))?;
    }
    Ok((out, cut))
}

/// [`copy_tree`] into `dst` under `anchor` that never writes through a
/// symlink already at the destination: `dst` and every existing path
/// component below `anchor` are checked first, and every entry is checked
/// again before it is written. A symlink in the source is copied as a
/// symlink (never followed), and never over an existing entry.
pub fn copy_tree_no_follow(src: &Path, anchor: &Path, dst: &Path) -> io::Result<()> {
    crate::process_mode::guard_write(dst)?;
    ensure_no_symlink_below(anchor, dst)?;
    copy_no_follow_inner(src, dst)
}

fn copy_no_follow_inner(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dst) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a symlink", dst.display()),
            ));
        }
        Ok(m) if !m.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", dst.display()),
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(dst)?,
        Err(e) => return Err(e),
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        let existing = match fs::symlink_metadata(&to) {
            Ok(m) => Some(m),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if existing
            .as_ref()
            .is_some_and(|m| m.file_type().is_symlink())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a symlink", to.display()),
            ));
        }
        if ft.is_symlink() {
            if existing.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} already exists", to.display()),
                ));
            }
            symlink(&fs::read_link(&from)?, &to)?;
        } else if ft.is_dir() {
            copy_no_follow_inner(&from, &to)?;
        } else if ft.is_file() {
            if existing.as_ref().is_some_and(|m| !m.is_file()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a regular file", to.display()),
                ));
            }
            fs::copy(&from, &to)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "not a regular file, directory or symlink: {}",
                    from.display()
                ),
            ));
        }
    }
    Ok(())
}

/// Control files are always written this way: temp + fsync + atomic rename (spec 4.3).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::process_mode::guard_write(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    write_into_existing_dir(dir, path, bytes)
}

/// [`atomic_write`] for state inside a directory that someone else owns and
/// may delete at any time (a run directory, a workspace's `.apb`). The file is
/// written only while `anchor` exists, and only the directories between
/// `anchor` and the file are created; once `anchor` is gone this fails with
/// `NotFound`. A late writer (a detached driver finishing a node after its
/// workspace was removed) therefore never brings the deleted tree back, which
/// the `create_dir_all` in [`atomic_write`] did.
pub fn atomic_write_under(anchor: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::process_mode::guard_write(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    create_dir_under(anchor, dir)?;
    write_into_existing_dir(dir, path, bytes)
}

/// Creates `dir` and the directories between `anchor` and it, never `anchor`
/// itself or anything above it: `NotFound` when `anchor` is missing, an error
/// when `dir` is not inside `anchor`. See [`atomic_write_under`].
pub fn create_dir_under(anchor: &Path, dir: &Path) -> io::Result<()> {
    crate::process_mode::guard_write(dir)?;
    if !anchor.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("`{}` no longer exists", anchor.display()),
        ));
    }
    let rel = dir.strip_prefix(anchor).map_err(|_| {
        io::Error::other(format!(
            "`{}` is not inside `{}`",
            dir.display(),
            anchor.display()
        ))
    })?;
    let mut cur = anchor.to_path_buf();
    for part in rel.components() {
        cur.push(part);
        match fs::create_dir(&cur) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// [`atomic_write_under`] whose file is created 0600 (owner read and
/// write) before it appears at the target path: run content an agent may
/// have echoed a secret into (a transcript, a node output).
pub fn atomic_write_private_under(anchor: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::process_mode::guard_write(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    create_dir_under(anchor, dir)?;
    write_via_temp(dir, path, bytes, create_private)
}

/// `path` opened for appending, created 0600 when missing (an existing
/// file keeps its mode): a run's journal and its question, answer and
/// review logs.
pub fn open_append_private(path: &Path) -> io::Result<File> {
    crate::process_mode::guard_write(path)?;
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    narrow_open(&file);
    Ok(file)
}

/// Opens (creating, mode 0600) the regular file `name` inside the directory
/// `dir` for appending, where a less trusted writer may own `dir`'s
/// content. On unix `dir` is opened first without following a link at its
/// last component, and the file is opened relative to that descriptor
/// (`openat`), without following a link and without blocking: a `dir`
/// swapped for a link after the check is never gone through, and a fifo or
/// a device at `name` fails at once instead of hanging the caller. Anything
/// but a regular file is refused, and so is a file with more than one hard
/// link (its mode is another file's too); a file an older apb left readable
/// beyond its owner is narrowed. `name` must be one path segment.
pub fn open_append_private_in(dir: &Path, name: &str) -> io::Result<File> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{name}` is not one path segment"),
        ));
    }
    crate::process_mode::guard_write(&dir.join(name))?;
    #[cfg(unix)]
    let file = {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::fs::OpenOptionsExt as _;
        let parent = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)?;
        #[cfg(test)]
        unix_tests::between_opens();
        let leaf = std::ffi::CString::new(name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let flags = libc::O_WRONLY
            | libc::O_APPEND
            | libc::O_CREAT
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC;
        // SAFETY: `parent` is an open directory descriptor for the whole
        // call and `leaf` a NUL-terminated string; the returned descriptor
        // is owned by the `File` built from it and by nothing else.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                leaf.as_ptr(),
                flags,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor nothing else owns.
        unsafe { File::from_raw_fd(fd) }
    };
    #[cfg(not(unix))]
    let file = {
        if fs::symlink_metadata(dir)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a symlink", dir.display()),
            ));
        }
        let path = dir.join(name);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a symlink", path.display()),
            ));
        }
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", dir.join(name).display()),
        ));
    }
    // A hard link shares its mode with a file elsewhere: narrowing it would
    // change that file.
    #[cfg(unix)]
    if std::os::unix::fs::MetadataExt::nlink(&meta) > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has more than one link", dir.join(name).display()),
        ));
    }
    narrow_open(&file);
    Ok(file)
}

/// Clears the group and other bits of an open file, through its handle (so
/// a path swapped for a link after the open is not followed). A mode given
/// at open applies only to a file the open creates: one an older apb left
/// readable beyond its owner keeps its mode otherwise. Best effort; nothing
/// off unix.
pub fn narrow_open(file: &File) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = file.metadata() {
            let mode = meta.permissions().mode();
            if mode & 0o077 != 0 {
                let _ = file.set_permissions(fs::Permissions::from_mode(mode & !0o077));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = file;
}

/// Creates the directory `dir` (its parent must exist) with mode 0700 on
/// unix: a run directory, whose content only its owner may read.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    crate::process_mode::guard_write(dir)?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Temp + fsync + rename inside `dir`, which must exist.
fn write_into_existing_dir(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_via_temp(dir, path, bytes, |tmp| File::create(tmp))
}

/// A new file at `tmp`, created 0600 on unix: no window with default
/// permissions before a later chmod.
fn create_private(tmp: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(tmp)
    }
    #[cfg(not(unix))]
    {
        File::create(tmp)
    }
}

/// A temp name in `dir` for writing `path`, unique per write: the pid and
/// a per-process counter keep two threads of one process apart (the items
/// of a list node writing the same file never rename each other's temp
/// away), and a random part keeps processes apart, so a reused pid (a
/// container's fixed pid) never meets the temp a killed earlier process
/// left behind.
fn unique_temp(dir: &Path, path: &Path) -> PathBuf {
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let random = uuid::Uuid::new_v4().simple().to_string();
    dir.join(format!(
        ".tmp-{}-{n}-{}-{}",
        std::process::id(),
        &random[..12],
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

/// The one atomic write: `bytes` into a [`unique_temp`] that `create`
/// opens, fsync, then rename onto `path`. A write that fails removes its
/// temp. A process killed between the create and the rename leaves its
/// `.tmp-<pid>-<n>-<random>-<name>` behind: nothing reads such a file and
/// no later write reuses its name, but in a long-lived directory (the
/// trust and project state, an agent home) it stays until removed by hand.
fn write_via_temp(
    dir: &Path,
    path: &Path,
    bytes: &[u8],
    create: impl FnOnce(&Path) -> io::Result<File>,
) -> io::Result<()> {
    write_via_temp_checked(dir, path, bytes, create, || Ok(()))
}

/// [`write_via_temp`] that runs `before_rename` once the temp is written and
/// synced, right before the rename: an error from it leaves `path` as it
/// is and removes the temp.
fn write_via_temp_checked(
    dir: &Path,
    path: &Path,
    bytes: &[u8],
    create: impl FnOnce(&Path) -> io::Result<File>,
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    crate::process_mode::guard_write(path)?;
    let tmp = unique_temp(dir, path);
    let res = (|| {
        let mut f = create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        before_rename()?;
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// The file a write of `path` lands in: `path` itself, or, when `path` is a
/// symbolic link, the file it finally resolves to (which must exist). A
/// rewrite of a user's own file (a host config kept in a dotfiles folder)
/// edits that file and leaves the link in place.
pub fn link_target(path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(path),
        _ => Ok(path.to_path_buf()),
    }
}

/// Rewrites the user's own file `path` atomically, in place: a symbolic
/// link is followed ([`link_target`]) and stays a link, and the new file
/// carries the permissions the old one had before it appears, so a private
/// file is never readable by others, not even for a moment. A file that
/// does not exist yet is created like [`atomic_write`]. Returns the file
/// written.
pub fn atomic_rewrite_in_place(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    atomic_rewrite_in_place_checked(path, bytes, |_| Ok(()))
}

/// [`atomic_rewrite_in_place`] that hands `check` the file it resolved
/// `path` to, once the new content is written beside it and right before
/// the rename. An error from `check` writes nothing and is returned: a
/// caller that compares the file it read with what is there now (another
/// program's save, a link moved to another file) makes that comparison and
/// the publication one step on the same resolved file.
pub fn atomic_rewrite_in_place_checked(
    path: &Path,
    bytes: &[u8],
    check: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    crate::process_mode::guard_write(path)?;
    let target = link_target(path)?;
    let dir = target
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?
        .to_path_buf();
    let perm = fs::metadata(&target).ok().map(|m| m.permissions());
    let Some(perm) = perm else {
        fs::create_dir_all(&dir)?;
        write_via_temp_checked(
            &dir,
            &target,
            bytes,
            |tmp| File::create(tmp),
            || check(&target),
        )?;
        return Ok(target);
    };
    write_via_temp_checked(
        &dir,
        &target,
        bytes,
        |tmp| {
            #[cfg(unix)]
            let f = {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                // Created no wider than the old file (the umask only narrows),
                // then set to exactly its mode before the rename.
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(perm.mode() & 0o7777)
                    .open(tmp)?
            };
            #[cfg(not(unix))]
            let f = File::create(tmp)?;
            f.set_permissions(perm.clone())?;
            Ok(f)
        },
        || check(&target),
    )?;
    Ok(target)
}

/// Like `atomic_write`, but the resulting file gets 0600 permissions (owner
/// read/write) before it appears at the target path. For global state files
/// (trust.json, projects.json, suggestions.json), which may contain data that is
/// privacy-sensitive. Permissions are left untouched on non-unix.
pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::process_mode::guard_write(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    write_via_temp(dir, path, bytes, create_private)
}

/// Writes `bytes` to `path` only if `path` does not exist: a synced
/// temporary file in the same directory, then `std::fs::hard_link` to
/// `path` (fails with `AlreadyExists` when the target exists), then the
/// temporary removed and the directory synced. The file mode is the
/// default (not private). Never replaces a file.
pub fn write_new_no_clobber(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::process_mode::guard_write(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = unique_temp(dir, path);
    let res = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        // A hard link fails when `path` exists, where a rename would
        // replace it: this is the one refusal a save of one name gives a
        // second writer.
        std::fs::hard_link(&tmp, path)
    })();
    let _ = fs::remove_file(&tmp);
    if res.is_ok() {
        sync_dir(dir)?;
    }
    res
}

/// Flushes the directory entry list of `dir` to disk, so a rename into it
/// survives a power cut. A no-op off unix.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// An exclusive OS lock (`flock` on unix) on `<dir>/<lock_name>`, held for
/// as long as the guard lives. The kernel releases it when the owner's
/// process ends, however it ends, so a live owner is never displaced and a
/// dead one never blocks: no age rule decides who holds it. The file stays
/// in place after release (removing it would let a second process lock an
/// unlinked file while a third creates a new one).
pub struct HeldLock {
    _file: File,
}

/// Takes [`HeldLock`] on `<dir>/<lock_name>`, waiting up to
/// `LOCK_ATTEMPTS x LOCK_STEP_MS` while another handle holds it; then a
/// `WouldBlock` error. The file is created private and is never followed
/// through a symbolic link.
pub fn hold_lock(dir: &Path, lock_name: &str) -> io::Result<HeldLock> {
    hold_lock_within(dir, lock_name, LOCK_ATTEMPTS)
}

/// [`hold_lock`] with one attempt and no wait: `WouldBlock` at once while
/// another handle (in this process or another) holds it. A probe of
/// whether an owner is alive.
pub fn try_hold_lock(dir: &Path, lock_name: &str) -> io::Result<HeldLock> {
    hold_lock_within(dir, lock_name, 1)
}

fn hold_lock_within(dir: &Path, lock_name: &str, attempts: u32) -> io::Result<HeldLock> {
    crate::process_mode::guard_write(&dir.join(lock_name))?;
    fs::create_dir_all(dir)?;
    let path = dir.join(lock_name);
    let mut opts = fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(&path)?;
    for attempt in 0..attempts {
        match file.try_lock() {
            Ok(()) => return Ok(HeldLock { _file: file }),
            Err(fs::TryLockError::WouldBlock) => {
                if attempt + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(LOCK_STEP_MS));
                }
            }
            Err(fs::TryLockError::Error(e)) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!(
            "lock `{}` is held by a live owner; try again",
            path.display()
        ),
    ))
}

/// RAII lock for a state directory (`<dir>/<lock_name>`), tagged with a unique
/// owner token. The guard removes the file only if the token is still ours
/// (after a force-steal of a stale lock, this protects against cascading
/// removal of someone else's lock). A shared primitive for serializing
/// read-modify-write over global state files (trust.json, suggestions.json,
/// projects.json).
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
    lock_dir_stale_after(dir, lock_name, LOCK_STALE_MS).map(|(lock, _)| lock)
}

/// [`lock_dir`] for a critical section that can legitimately run longer than
/// a state-file update (a network request under the lock): a held lock counts
/// as stale only once it is older than `stale_after`, which the caller derives
/// from its own bounded working window. Also says whether a stale lock was
/// broken to get it, so the caller can record that.
pub fn lock_dir_with_lifetime(
    dir: &Path,
    lock_name: &str,
    stale_after: std::time::Duration,
) -> io::Result<(DirLock, bool)> {
    lock_dir_stale_after(dir, lock_name, stale_after.as_millis())
}

/// [`lock_dir`] without waiting: one attempt, then a stale lock (by the
/// same staleness rule) is stolen and a live one is `WouldBlock` at once.
/// For a writer on a hot path that would rather drop its write than wait.
pub fn try_lock_dir(dir: &Path, lock_name: &str) -> io::Result<DirLock> {
    lock_dir_attempts(dir, lock_name, LOCK_STALE_MS, 1).map(|(lock, _)| lock)
}

fn lock_dir_stale_after(
    dir: &Path,
    lock_name: &str,
    stale_ms: u128,
) -> io::Result<(DirLock, bool)> {
    lock_dir_attempts(dir, lock_name, stale_ms, LOCK_ATTEMPTS)
}

fn lock_dir_attempts(
    dir: &Path,
    lock_name: &str,
    stale_ms: u128,
    attempts: u32,
) -> io::Result<(DirLock, bool)> {
    crate::process_mode::guard_write(&dir.join(lock_name))?;
    fs::create_dir_all(dir)?;
    let path = dir.join(lock_name);
    let token = format!("{}-{}", std::process::id(), uuid::Uuid::new_v4().simple());
    for attempt in 0..attempts {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(token.as_bytes())?;
                return Ok((DirLock { path, token }, false));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if attempt + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(LOCK_STEP_MS));
                }
            }
            Err(e) => return Err(e),
        }
    }
    // Timeout: we steal the lock only if it is stale (staleness by mtime), otherwise busy.
    if !lock_is_stale(&path, stale_ms) {
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
            Ok((DirLock { path, token }, true))
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

/// Whether the lock is stale: its mtime age is at least `stale_ms`
/// (`LOCK_STALE_MS` for [`lock_dir`]). An unreadable mtime is treated as NOT
/// stale (conservatively - we don't steal when in doubt).
fn lock_is_stale(path: &Path, stale_ms: u128) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    match mtime.elapsed() {
        Ok(age) => age.as_millis() >= stale_ms,
        // mtime in the future (clock skew) - not stale.
        Err(_) => false,
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;

    /// A rewrite in place follows a link and keeps it, and the new file has
    /// the old mode before it appears.
    #[test]
    fn a_rewrite_in_place_keeps_the_link_and_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("dotfiles/config.toml");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "a = 1\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let link = t.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let written = atomic_rewrite_in_place(&link, b"a = 2\n").unwrap();
        assert_eq!(written, fs::canonicalize(&real).unwrap());
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "a = 2\n");
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // A plain file keeps its mode too; a new one is simply created.
        let plain = t.path().join("plain");
        fs::write(&plain, "x").unwrap();
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o640)).unwrap();
        atomic_rewrite_in_place(&plain, b"y").unwrap();
        assert_eq!(
            fs::metadata(&plain).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let new = t.path().join("sub/new");
        assert_eq!(atomic_rewrite_in_place(&new, b"z").unwrap(), new);
        assert_eq!(fs::read_to_string(&new).unwrap(), "z");
    }

    /// An overlay whose directory name matches a symlink already in the
    /// destination is refused, and nothing lands at the link's target;
    /// neither can a destination reached through a symlinked component.
    #[test]
    fn the_no_follow_copy_never_writes_through_a_destination_symlink() {
        let t = tempfile::tempdir().unwrap();
        let (anchor, outside, src) = (t.path().join("a"), t.path().join("out"), t.path().join("s"));
        fs::create_dir_all(anchor.join("tree")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(src.join("link")).unwrap();
        fs::write(src.join("link/f"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, anchor.join("tree/link")).unwrap();
        let tree = anchor.join("tree");
        assert!(copy_tree_no_follow(&src, &anchor, &tree).is_err());
        assert!(!outside.join("f").exists());
        let through = anchor.join("tree/link/sub");
        assert!(copy_tree_no_follow(&src, &anchor, &through).is_err());
        assert!(!outside.join("sub").exists());
        // A plain destination copies, links in the source stay links.
        fs::remove_file(anchor.join("tree/link")).unwrap();
        std::os::unix::fs::symlink("f", src.join("l")).unwrap();
        copy_tree_no_follow(&src, &anchor, &tree).unwrap();
        assert_eq!(fs::read_to_string(tree.join("link/f")).unwrap(), "x");
        assert!(
            fs::symlink_metadata(tree.join("l"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    /// Threads writing the same file at once each land a whole file, none
    /// fails on another's temp, and no temp is left behind; the private
    /// write keeps 0600.
    #[test]
    fn concurrent_atomic_writes_of_one_file_all_land_whole() {
        let t = tempfile::tempdir().unwrap();
        let (plain, private) = (t.path().join("f.json"), t.path().join("p.json"));
        std::thread::scope(|s| {
            for i in 0..8u8 {
                let (plain, private) = (&plain, &private);
                s.spawn(move || {
                    let body = vec![b'a' + i; 4096];
                    for _ in 0..20 {
                        atomic_write(plain, &body).unwrap();
                        atomic_write_private(private, &body).unwrap();
                    }
                });
            }
        });
        for f in [&plain, &private] {
            let got = fs::read(f).unwrap();
            assert_eq!(got.len(), 4096);
            assert!(got.iter().all(|b| *b == got[0]), "one writer's whole body");
        }
        let left: Vec<_> = fs::read_dir(t.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(left.is_empty(), "no temp is left behind");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    thread_local! {
        /// What runs between the directory open and the leaf open of
        /// [`open_append_private_in`] on this thread (a test's swap).
        static BETWEEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Runs the swap a test planted between the two opens, once.
    pub(super) fn between_opens() {
        if let Some(swap) = BETWEEN.with(|b| b.borrow_mut().take()) {
            swap();
        }
    }

    /// The directory is swapped for a link after its descriptor was opened
    /// and before the leaf is: the leaf is still created in the directory
    /// that was opened, never where the link points. Deterministic: the
    /// swap runs at that exact point.
    #[test]
    fn a_directory_swapped_between_the_two_opens_is_never_gone_through() {
        let t = tempfile::tempdir().unwrap();
        let (outside, dir, moved) = (
            t.path().join("outside"),
            t.path().join("wakers"),
            t.path().join("moved"),
        );
        fs::create_dir(&outside).unwrap();
        fs::create_dir(&dir).unwrap();
        let (d, m, o) = (dir.clone(), moved.clone(), outside.clone());
        BETWEEN.with(|b| {
            *b.borrow_mut() = Some(Box::new(move || {
                fs::rename(&d, &m).unwrap();
                std::os::unix::fs::symlink(&o, &d).unwrap();
            }))
        });
        let mut f = open_append_private_in(&dir, "notify").unwrap();
        use std::io::Write as _;
        f.write_all(b"x").unwrap();
        assert!(
            fs::symlink_metadata(&dir).unwrap().file_type().is_symlink(),
            "swapped"
        );
        assert!(!outside.join("notify").exists(), "went through the link");
        assert_eq!(fs::read(moved.join("notify")).unwrap(), b"x");
    }

    /// The directory `open_append_private_in` is handed is opened without
    /// following a link: a `dir` swapped for a link after the caller checked
    /// it (the state a racing writer leaves) is refused, and nothing is
    /// created where the link points. Deterministic: the swap has already
    /// happened when the helper runs.
    #[test]
    fn a_directory_swapped_for_a_link_is_never_gone_through() {
        let t = tempfile::tempdir().unwrap();
        let (outside, dir) = (t.path().join("outside"), t.path().join("wakers"));
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, &dir).unwrap();
        assert!(open_append_private_in(&dir, "notify").is_err());
        assert!(!outside.join("notify").exists());
    }

    /// The leaf is one plain regular file: a path, a fifo (refused at once,
    /// never waited on), a link and a hard link are refused, a plain file is
    /// created 0600 or narrowed to it.
    #[test]
    fn the_leaf_is_one_private_regular_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("wakers");
        fs::create_dir(&dir).unwrap();
        for bad in ["", ".", "..", "a/b"] {
            let e = open_append_private_in(&dir, bad).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        let fifo = std::ffi::CString::new(dir.join("fifo").to_str().unwrap()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(open_append_private_in(&dir, "fifo").is_err());
        let victim = t.path().join("victim");
        fs::write(&victim, "x").unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("link")).unwrap();
        assert!(open_append_private_in(&dir, "link").is_err());
        fs::hard_link(&victim, dir.join("hard")).unwrap();
        assert!(open_append_private_in(&dir, "hard").is_err());
        assert_eq!(
            fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let old = dir.join("old");
        fs::write(&old, "").unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o644)).unwrap();
        open_append_private_in(&dir, "old").unwrap();
        open_append_private_in(&dir, "new").unwrap();
        for name in ["old", "new"] {
            let mode = fs::metadata(dir.join(name)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }
}

/// Every file writer, called directly in a quick scenario process: each
/// writes below the write root and refuses anywhere else, before it acts;
/// and no program starts.
#[cfg(test)]
mod scenario_tests {
    use super::*;

    fn refused(r: io::Result<()>) {
        let e = r.unwrap_err();
        assert!(
            e.to_string()
                .starts_with(crate::process_mode::SCENARIO_REFUSAL),
            "{e}"
        );
    }

    crate::scenario_test! {
        fn every_file_writer_writes_only_below_the_scenario_root() at Quick, root => {
            // A project next to the root, as a scenario run sees it.
            let outside = tempfile::tempdir().unwrap();
            let d = outside.path();
            let f = d.join("sub/f");
            refused(atomic_write(&f, b"x"));
            refused(atomic_write_private(&f, b"x"));
            refused(atomic_write_under(d, &f, b"x"));
            refused(atomic_write_private_under(d, &f, b"x"));
            refused(create_dir_under(d, &d.join("sub")));
            refused(create_private_dir(&d.join("sub")));
            refused(open_append_private(&d.join("log")).map(drop));
            refused(open_append_private_in(d, "log").map(drop));
            refused(lock_dir(d, "x.lock").map(drop));
            refused(hold_lock(d, "x.lock").map(drop));
            refused(atomic_rewrite_in_place(&f, b"x").map(drop));
            refused(write_new_no_clobber(&f, b"x"));
            refused(copy_tree(root, &d.join("copy")));
            refused(copy_tree_no_follow(root, d, &d.join("copy")));
            refused(symlink(root, &d.join("link")));
            assert_eq!(fs::read_dir(d).unwrap().count(), 0, "nothing was created");
            // Below the root every writer acts as in any other process.
            let run = root.join("run-1");
            atomic_write(&run.join("a"), b"x").unwrap();
            atomic_write_private(&run.join("b"), b"x").unwrap();
            atomic_write_under(root, &run.join("sub/c"), b"x").unwrap();
            open_append_private(&run.join("events.jsonl")).unwrap();
            drop(lock_dir(&run, "x.lock").unwrap());
            assert_eq!(fs::read(run.join("sub/c")).unwrap(), b"x");
            refused(
                spawn_when_not_busy(&mut std::process::Command::new("true")).map(drop),
            );
        }
    }
}

/// `write_new_no_clobber`: never replaces, and one winner under a race.
#[cfg(test)]
mod write_new_tests {
    use super::*;

    /// A new file is written; an existing file keeps its bytes and the
    /// second write is `AlreadyExists`.
    #[test]
    fn a_new_file_is_written_and_an_existing_one_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/f");
        write_new_no_clobber(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");
        let e = write_new_no_clobber(&path, b"second").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"first");
    }

    /// Two threads writing one path: exactly one succeeds and the file
    /// holds the winner's bytes whole.
    #[test]
    fn two_threads_on_one_path_exactly_one_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let path_a = path.clone();
        let path_b = path.clone();
        let t1 = std::thread::spawn(move || write_new_no_clobber(&path_a, b"first"));
        let t2 = std::thread::spawn(move || write_new_no_clobber(&path_b, b"second"));
        let (r1, r2) = (t1.join().unwrap(), t2.join().unwrap());
        let oks = [r1.is_ok(), r2.is_ok()].into_iter().filter(|b| *b).count();
        assert_eq!(oks, 1, "{r1:?} {r2:?}");
        let stored = fs::read(&path).unwrap();
        assert!(
            stored == b"first" || stored == b"second",
            "mixed bytes: {stored:?}"
        );
        // No temp left behind.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// A second open of a held lock in the same process is refused at once,
    /// and taken after the first is dropped.
    #[test]
    fn try_hold_lock_refuses_while_held() {
        let tmp = tempfile::tempdir().unwrap();
        let first = try_hold_lock(tmp.path(), "r.lock").unwrap();
        let started = std::time::Instant::now();
        let e = try_hold_lock(tmp.path(), "r.lock").err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        drop(first);
        try_hold_lock(tmp.path(), "r.lock").unwrap();
    }
}
