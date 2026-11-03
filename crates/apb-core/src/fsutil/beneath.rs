//! Descriptor-relative file access beneath one anchor directory, for reads
//! and writes alike: the anchor is opened once, and every path is walked
//! component by component from the previous directory descriptor with
//! `O_NOFOLLOW | O_DIRECTORY`, so a component swapped for a link at any time
//! is refused. Result snapshots read their sources and write their store
//! through it.
//!
//! Reads look at the last component without following it and refuse
//! anything but a regular file with one link before opening it, then open
//! it with `O_NOFOLLOW | O_NONBLOCK`, check that the opened file is the one
//! looked at, and read at most a cap. Writes create directories 0700 with
//! `mkdirat`, create the temp file 0600 with `O_CREAT | O_EXCL | O_NOFOLLOW`
//! in the target directory's descriptor, fsync it, `renameat` it within that
//! descriptor and fsync the directory; every write first passes
//! `process_mode::guard_write`. Every descriptor is opened close-on-exec, so
//! a lock held here is never inherited by a program the process starts.
//!
//! Paths given to this module are relative, `/`-separated, with no empty,
//! `.` or `..` segment.
//!
//! Off unix there is no `openat`: each component is checked with
//! `symlink_metadata` before use, which refuses a link present at the time
//! of the check but not one swapped in between the check and the use.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// An open anchor directory.
#[derive(Debug)]
pub struct Beneath {
    anchor: PathBuf,
    #[cfg(unix)]
    dir: std::os::fd::OwnedFd,
}

/// A bounded read of one regular file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CappedRead {
    /// The whole file, at most the cap, with `sha256:<hex>` of its bytes.
    Read { bytes: Vec<u8>, digest: String },
    /// The file was not read.
    Refused(ReadRefusal),
}

/// Why a file was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadRefusal {
    /// No entry at the path.
    Missing,
    /// A link at some component.
    Link,
    /// A FIFO, device, socket or directory.
    NotRegular,
    /// A regular file with more than one hard link.
    HardLink,
    /// Larger than the cap; nothing past the size check was read.
    TooLarge { size: u64 },
}

/// What one directory entry is, as listed without following it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Link,
    Other,
}

/// One listed directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedEntry {
    pub name: OsString,
    pub kind: EntryKind,
}

/// The number of directory entries a capture may still examine, shared by
/// every listing of one settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListBudget {
    pub remaining: usize,
}

/// One directory level, and whether the budget ran out before its end.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<ListedEntry>,
    pub exhausted: bool,
}

/// Whether a lock file beneath the anchor is held by someone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    /// No lock file.
    Absent,
    /// The file is there and nobody holds it.
    Free,
    /// Another open handle holds it.
    Held,
}

/// The time between two attempts at a held lock.
const LOCK_STEP: std::time::Duration = std::time::Duration::from_millis(25);

/// The segments of a relative path; `""` is the anchor itself.
fn segments(rel: &str) -> io::Result<Vec<&str>> {
    if rel.is_empty() {
        return Ok(Vec::new());
    }
    rel.split('/')
        .map(|s| {
            if s.is_empty() || s == "." || s == ".." || s.contains('\0') {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("`{rel}` is not a plain relative path"),
                ))
            } else {
                Ok(s)
            }
        })
        .collect()
}

/// The parent segments and the last one of a path naming an entry.
fn split_leaf(rel: &str) -> io::Result<(Vec<&str>, &str)> {
    let mut segs = segments(rel)?;
    let leaf = segs.pop().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the anchor itself is no entry")
    })?;
    Ok((segs, leaf))
}

fn link_error(rel: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("`{rel}` goes through a link"),
    )
}

/// A temp name unique per write in this process and across processes.
fn temp_name() -> String {
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!(".tmp-{}-{n}-{}", std::process::id(), &random[..12])
}

#[cfg(unix)]
mod sys {
    //! The descriptor calls, each wrapped once.

    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;

    use super::EntryKind;

    pub(super) fn cstr(name: &str) -> io::Result<CString> {
        CString::new(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    pub(super) fn openat(
        dir: BorrowedFd<'_>,
        name: &CStr,
        flags: libc::c_int,
        mode: libc::c_uint,
    ) -> io::Result<OwnedFd> {
        // SAFETY: `dir` is an open descriptor for the whole call and `name`
        // a NUL-terminated string; the result is owned by nothing else.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC,
                mode,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// The directory `name` inside `dir`, never through a link.
    pub(super) fn open_dir(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<OwnedFd> {
        openat(
            dir,
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            0,
        )
    }

    /// `fstatat` without following a link at `name`.
    pub(super) fn lstat_at(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<libc::stat> {
        // SAFETY: an all-zero `stat` is a valid value to be overwritten.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid descriptor, NUL-terminated name, writable `st`.
        let rc = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(st)
    }

    pub(super) fn fstat(fd: BorrowedFd<'_>) -> io::Result<libc::stat> {
        // SAFETY: as in `lstat_at`.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid descriptor, writable `st`.
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(st)
    }

    pub(super) fn kind_of(st: &libc::stat) -> EntryKind {
        match st.st_mode as libc::mode_t & libc::S_IFMT {
            libc::S_IFREG => EntryKind::File,
            libc::S_IFDIR => EntryKind::Dir,
            libc::S_IFLNK => EntryKind::Link,
            _ => EntryKind::Other,
        }
    }

    // `st_nlink` is `u16` on macOS.
    #[allow(clippy::unnecessary_cast)]
    pub(super) fn nlink(st: &libc::stat) -> u64 {
        st.st_nlink as u64
    }

    pub(super) fn same_file(a: &libc::stat, b: &libc::stat) -> bool {
        a.st_dev == b.st_dev && a.st_ino == b.st_ino
    }

    pub(super) fn mkdirat(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<()> {
        // SAFETY: valid descriptor and NUL-terminated name.
        if unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn renameat(
        from_dir: BorrowedFd<'_>,
        from: &CStr,
        to_dir: BorrowedFd<'_>,
        to: &CStr,
    ) -> io::Result<()> {
        // SAFETY: valid descriptors and NUL-terminated names.
        let rc = unsafe {
            libc::renameat(
                from_dir.as_raw_fd(),
                from.as_ptr(),
                to_dir.as_raw_fd(),
                to.as_ptr(),
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn unlinkat(dir: BorrowedFd<'_>, name: &CStr, is_dir: bool) -> io::Result<()> {
        let flags = if is_dir { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: valid descriptor and NUL-terminated name.
        if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), flags) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn fsync(fd: BorrowedFd<'_>) -> io::Result<()> {
        // SAFETY: valid descriptor.
        if unsafe { libc::fsync(fd.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Listed names with the kind their entry reported, if it did.
    pub(super) type Names = Vec<(OsString, Option<EntryKind>)>;

    /// Every name of the directory `dir` (without `.` and `..`) with the
    /// kind its entry reports, read through a duplicate of the descriptor,
    /// stopping once `take` says no.
    pub(super) fn read_dir(
        dir: BorrowedFd<'_>,
        mut take: impl FnMut() -> bool,
    ) -> io::Result<(Names, bool)> {
        // SAFETY: valid descriptor; the duplicate is owned by the stream
        // below and closed by `closedir`.
        let dup: RawFd = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if dup < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `dup` is a directory descriptor nothing else owns.
        let stream = unsafe { libc::fdopendir(dup) };
        if stream.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: `fdopendir` failed, so `dup` is still ours to close.
            unsafe { libc::close(dup) };
            return Err(e);
        }
        // `rewinddir`: a duplicate shares its offset with the original.
        // SAFETY: `stream` is a live directory stream.
        unsafe { libc::rewinddir(stream) };
        let mut out = Vec::new();
        let mut stopped = false;
        loop {
            // SAFETY: `stream` is live; the entry is read before the next
            // call.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                break;
            }
            // SAFETY: `readdir` returned a valid entry with a NUL-terminated
            // name.
            let (name, d_type) = unsafe {
                (
                    CStr::from_ptr((*entry).d_name.as_ptr()).to_bytes().to_vec(),
                    (*entry).d_type,
                )
            };
            if name == b"." || name == b".." {
                continue;
            }
            if !take() {
                stopped = true;
                break;
            }
            let kind = match d_type {
                libc::DT_REG => Some(EntryKind::File),
                libc::DT_DIR => Some(EntryKind::Dir),
                libc::DT_LNK => Some(EntryKind::Link),
                libc::DT_UNKNOWN => None,
                _ => Some(EntryKind::Other),
            };
            out.push((OsStr::from_bytes(&name).to_os_string(), kind));
        }
        // SAFETY: `stream` is live and closed exactly once.
        unsafe { libc::closedir(stream) };
        Ok((out, stopped))
    }
}

#[cfg(unix)]
impl Beneath {
    /// Opens `anchor` once; it must be a directory and not a link.
    pub fn open(anchor: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(anchor)?;
        Ok(Beneath {
            anchor: anchor.to_path_buf(),
            dir: file.into(),
        })
    }

    /// The directory at `segs`, walked from the anchor.
    fn walk(&self, segs: &[&str]) -> Result<std::os::fd::OwnedFd, Walk> {
        use std::os::fd::AsFd;
        let mut cur = self.dir.try_clone().map_err(Walk::Io)?;
        for seg in segs {
            let name = sys::cstr(seg).map_err(Walk::Io)?;
            cur = match sys::open_dir(cur.as_fd(), &name) {
                Ok(fd) => fd,
                Err(e) => return Err(Walk::classify(cur.as_fd(), &name, e)),
            };
        }
        Ok(cur)
    }

    /// [`Beneath::walk`] as an I/O error for a write or a listing.
    fn walk_io(&self, segs: &[&str], rel: &str) -> io::Result<std::os::fd::OwnedFd> {
        self.walk(segs).map_err(|w| w.into_io(rel))
    }

    /// Reads the regular file at `rel`, at most `cap` bytes.
    pub fn read_regular_capped(&self, rel: &str, cap: u64) -> io::Result<CappedRead> {
        use std::io::Read as _;
        use std::os::fd::AsFd;
        let (parents, leaf) = split_leaf(rel)?;
        let dir = match self.walk(&parents) {
            Ok(fd) => fd,
            Err(Walk::Missing | Walk::NotDir) => {
                return Ok(CappedRead::Refused(ReadRefusal::Missing));
            }
            Err(Walk::Link) => return Ok(CappedRead::Refused(ReadRefusal::Link)),
            Err(Walk::Io(e)) => return Err(e),
        };
        let name = sys::cstr(leaf)?;
        // Looked at before it is opened: a FIFO or a device is never opened.
        let seen = match sys::lstat_at(dir.as_fd(), &name) {
            Ok(st) => st,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(CappedRead::Refused(ReadRefusal::Missing));
            }
            Err(e) => return Err(e),
        };
        if let Some(refusal) = refusal_of(&seen, cap) {
            return Ok(CappedRead::Refused(refusal));
        }
        #[cfg(test)]
        tests::between_look_and_open();
        let fd = match sys::openat(
            dir.as_fd(),
            &name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
            0,
        ) {
            Ok(fd) => fd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(CappedRead::Refused(ReadRefusal::Missing));
            }
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                return Ok(CappedRead::Refused(ReadRefusal::Link));
            }
            Err(e) => return Err(e),
        };
        // The entry may have been swapped between the look and the open.
        let opened = sys::fstat(fd.as_fd())?;
        if let Some(refusal) = refusal_of(&opened, cap) {
            return Ok(CappedRead::Refused(refusal));
        }
        if !sys::same_file(&seen, &opened) {
            return Ok(CappedRead::Refused(ReadRefusal::Missing));
        }
        let mut bytes = Vec::new();
        std::fs::File::from(fd)
            .take(cap.saturating_add(1))
            .read_to_end(&mut bytes)?;
        Ok(finish_read(bytes, cap))
    }

    /// Lists the directory at `rel` (`""` for the anchor), one level,
    /// counting every entry examined against `budget`.
    pub fn list_dir(&self, rel: &str, budget: &mut ListBudget) -> io::Result<Listing> {
        use std::os::fd::AsFd;
        let segs = segments(rel)?;
        let dir = self.walk_io(&segs, rel)?;
        let (raw, exhausted) = sys::read_dir(dir.as_fd(), || {
            if budget.remaining == 0 {
                return false;
            }
            budget.remaining -= 1;
            true
        })?;
        let mut entries = Vec::with_capacity(raw.len());
        for (name, kind) in raw {
            let kind = match kind {
                Some(k) => k,
                None => {
                    use std::os::unix::ffi::OsStrExt;
                    let c = std::ffi::CString::new(name.as_bytes())
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    match sys::lstat_at(dir.as_fd(), &c) {
                        Ok(st) => sys::kind_of(&st),
                        // Gone since the listing: nothing to report.
                        Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(e),
                    }
                }
            };
            entries.push(ListedEntry { name, kind });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Listing { entries, exhausted })
    }

    /// What the entry at `rel` is, without following it; `None` when there
    /// is none (or a component on the way is not a directory).
    pub fn kind(&self, rel: &str) -> io::Result<Option<EntryKind>> {
        use std::os::fd::AsFd;
        let (parents, leaf) = split_leaf(rel)?;
        let dir = match self.walk(&parents) {
            Ok(fd) => fd,
            Err(Walk::Missing | Walk::NotDir) => return Ok(None),
            Err(Walk::Link) => return Err(link_error(rel)),
            Err(Walk::Io(e)) => return Err(e),
        };
        match sys::lstat_at(dir.as_fd(), &sys::cstr(leaf)?) {
            Ok(st) => Ok(Some(sys::kind_of(&st))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The size of the regular file at `rel`, without following it;
    /// `None` when there is none or it is something else.
    pub fn file_size(&self, rel: &str) -> io::Result<Option<u64>> {
        use std::os::fd::AsFd;
        let (parents, leaf) = split_leaf(rel)?;
        let dir = match self.walk(&parents) {
            Ok(fd) => fd,
            Err(Walk::Missing | Walk::NotDir) => return Ok(None),
            Err(Walk::Link) => return Err(link_error(rel)),
            Err(Walk::Io(e)) => return Err(e),
        };
        match sys::lstat_at(dir.as_fd(), &sys::cstr(leaf)?) {
            Ok(st) if sys::kind_of(&st) == EntryKind::File => Ok(Some(st.st_size.max(0) as u64)),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Creates every missing directory of `rel`, mode 0700.
    pub fn mkdir_all(&self, rel: &str) -> io::Result<()> {
        use std::os::fd::AsFd;
        crate::process_mode::guard_write(&self.anchor.join(rel))?;
        let segs = segments(rel)?;
        let mut cur = self.dir.try_clone()?;
        for seg in segs {
            let name = sys::cstr(seg)?;
            let next = match sys::open_dir(cur.as_fd(), &name) {
                Ok(fd) => fd,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    match sys::mkdirat(cur.as_fd(), &name) {
                        Ok(()) => {}
                        // Created by someone else meanwhile.
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(e),
                    }
                    sys::open_dir(cur.as_fd(), &name)
                        .map_err(|e| Walk::classify(cur.as_fd(), &name, e).into_io(rel))?
                }
                Err(e) => return Err(Walk::classify(cur.as_fd(), &name, e).into_io(rel)),
            };
            cur = next;
        }
        Ok(())
    }

    /// Writes `bytes` as the new file `name` in the directory `dir_rel`:
    /// a 0600 temp file, fsync, rename within the directory, directory
    /// fsync. An existing `name` is replaced as a whole.
    pub fn write_new(&self, dir_rel: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write as _;
        use std::os::fd::AsFd;
        let segs = segments(dir_rel)?;
        let leaf = one_segment(name)?;
        crate::process_mode::guard_write(&self.anchor.join(dir_rel).join(leaf))?;
        let dir = self.walk_io(&segs, dir_rel)?;
        #[cfg(test)]
        tests::between_steps();
        let tmp = sys::cstr(&temp_name())?;
        let fd = sys::openat(
            dir.as_fd(),
            &tmp,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o600,
        )?;
        let mut file = std::fs::File::from(fd);
        let written = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| sys::renameat(dir.as_fd(), &tmp, dir.as_fd(), &sys::cstr(leaf)?));
        if let Err(e) = written {
            let _ = sys::unlinkat(dir.as_fd(), &tmp, false);
            return Err(e);
        }
        sys::fsync(dir.as_fd())
    }

    /// Moves `from_rel` to `to_rel`, both beneath the anchor.
    pub fn rename_within(&self, from_rel: &str, to_rel: &str) -> io::Result<()> {
        use std::os::fd::AsFd;
        crate::process_mode::guard_write(&self.anchor.join(from_rel))?;
        crate::process_mode::guard_write(&self.anchor.join(to_rel))?;
        let (from_parents, from_leaf) = split_leaf(from_rel)?;
        let (to_parents, to_leaf) = split_leaf(to_rel)?;
        let from_dir = self.walk_io(&from_parents, from_rel)?;
        let to_dir = self.walk_io(&to_parents, to_rel)?;
        sys::renameat(
            from_dir.as_fd(),
            &sys::cstr(from_leaf)?,
            to_dir.as_fd(),
            &sys::cstr(to_leaf)?,
        )?;
        sys::fsync(to_dir.as_fd())?;
        if from_parents != to_parents {
            sys::fsync(from_dir.as_fd())?;
        }
        Ok(())
    }

    /// Removes the file, link or empty directory at `rel` (a link itself,
    /// never what it points at). A missing entry is no error.
    pub fn remove(&self, rel: &str) -> io::Result<()> {
        use std::os::fd::AsFd;
        crate::process_mode::guard_write(&self.anchor.join(rel))?;
        let (parents, leaf) = split_leaf(rel)?;
        let dir = match self.walk(&parents) {
            Ok(fd) => fd,
            Err(Walk::Missing | Walk::NotDir) => return Ok(()),
            Err(w) => return Err(w.into_io(rel)),
        };
        let name = sys::cstr(leaf)?;
        let is_dir = match sys::lstat_at(dir.as_fd(), &name) {
            Ok(st) => sys::kind_of(&st) == EntryKind::Dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        match sys::unlinkat(dir.as_fd(), &name, is_dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Holds the lock file `name` in the directory `dir_rel` (created 0600
    /// when missing), waiting up to `wait` while another handle holds it;
    /// then a `WouldBlock` error. The kernel releases it when the holder's
    /// process ends, however it ends.
    pub fn hold_lock(
        &self,
        dir_rel: &str,
        name: &str,
        wait: std::time::Duration,
    ) -> io::Result<super::HeldLock> {
        use std::os::fd::AsFd;
        let segs = segments(dir_rel)?;
        let leaf = one_segment(name)?;
        crate::process_mode::guard_write(&self.anchor.join(dir_rel).join(leaf))?;
        let dir = self.walk_io(&segs, dir_rel)?;
        let fd = sys::openat(
            dir.as_fd(),
            &sys::cstr(leaf)?,
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )?;
        let file = std::fs::File::from(fd);
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("`{dir_rel}/{leaf}` is not a regular file"),
            ));
        }
        #[cfg(test)]
        tests::after_lock_file(&self.anchor.join(dir_rel).join(leaf));
        let deadline = std::time::Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(super::HeldLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(LOCK_STEP);
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!("`{dir_rel}/{leaf}` is held by a live owner; try again"),
                    ));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
    }

    /// Whether the lock file `rel` is held, without creating or holding
    /// it: writes nothing.
    pub fn probe_lock(&self, rel: &str) -> io::Result<LockState> {
        use std::os::fd::AsFd;
        let (parents, leaf) = split_leaf(rel)?;
        let dir = match self.walk(&parents) {
            Ok(fd) => fd,
            Err(Walk::Missing | Walk::NotDir) => return Ok(LockState::Absent),
            Err(w) => return Err(w.into_io(rel)),
        };
        let fd = match sys::openat(
            dir.as_fd(),
            &sys::cstr(leaf)?,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        ) {
            Ok(fd) => fd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(LockState::Absent),
            Err(e) => return Err(e),
        };
        let file = std::fs::File::from(fd);
        match file.try_lock_shared() {
            Ok(()) => Ok(LockState::Free),
            Err(std::fs::TryLockError::WouldBlock) => Ok(LockState::Held),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

/// How walking to a directory failed.
#[cfg(unix)]
enum Walk {
    Missing,
    Link,
    NotDir,
    Io(io::Error),
}

#[cfg(unix)]
impl Walk {
    /// The failed open of `name` in `dir`, told apart by looking at the
    /// entry without following it (`ELOOP`, `ENOTDIR` and `EMLINK` mean a
    /// link on some systems and a file on others).
    fn classify(dir: std::os::fd::BorrowedFd<'_>, name: &std::ffi::CStr, e: io::Error) -> Walk {
        if e.kind() == io::ErrorKind::NotFound {
            return Walk::Missing;
        }
        match sys::lstat_at(dir, name) {
            Ok(st) if sys::kind_of(&st) == EntryKind::Link => Walk::Link,
            Ok(st) if sys::kind_of(&st) != EntryKind::Dir => Walk::NotDir,
            Err(e2) if e2.kind() == io::ErrorKind::NotFound => Walk::Missing,
            _ => Walk::Io(e),
        }
    }

    fn into_io(self, rel: &str) -> io::Error {
        match self {
            Walk::Missing => {
                io::Error::new(io::ErrorKind::NotFound, format!("`{rel}` does not exist"))
            }
            Walk::Link => link_error(rel),
            Walk::NotDir => io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("`{rel}` goes through something that is not a directory"),
            ),
            Walk::Io(e) => e,
        }
    }
}

/// Why the file `st` describes is not read under `cap`, if it is not.
#[cfg(unix)]
fn refusal_of(st: &libc::stat, cap: u64) -> Option<ReadRefusal> {
    match sys::kind_of(st) {
        EntryKind::File => {}
        EntryKind::Link => return Some(ReadRefusal::Link),
        EntryKind::Dir | EntryKind::Other => return Some(ReadRefusal::NotRegular),
    }
    if sys::nlink(st) > 1 {
        return Some(ReadRefusal::HardLink);
    }
    let size = st.st_size.max(0) as u64;
    (size > cap).then_some(ReadRefusal::TooLarge { size })
}

/// The bytes read with one byte of room past the cap: a file that grew
/// past the cap after its size was checked is refused, never cut.
fn finish_read(bytes: Vec<u8>, cap: u64) -> CappedRead {
    if bytes.len() as u64 > cap {
        return CappedRead::Refused(ReadRefusal::TooLarge {
            size: bytes.len() as u64,
        });
    }
    let digest = crate::content::sha256_hex(&bytes);
    CappedRead::Read { bytes, digest }
}

fn one_segment(name: &str) -> io::Result<&str> {
    match segments(name)?.as_slice() {
        [one] => Ok(one),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{name}` is not one path segment"),
        )),
    }
}

#[cfg(not(unix))]
impl Beneath {
    /// Opens `anchor` once; it must be a directory and not a link.
    pub fn open(anchor: &Path) -> io::Result<Self> {
        let meta = std::fs::symlink_metadata(anchor)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", anchor.display()),
            ));
        }
        Ok(Beneath {
            anchor: anchor.to_path_buf(),
        })
    }

    /// The path of `segs`, every component checked not to be a link.
    fn checked(&self, segs: &[&str], rel: &str) -> io::Result<PathBuf> {
        let mut cur = self.anchor.clone();
        for seg in segs {
            cur.push(seg);
            match std::fs::symlink_metadata(&cur) {
                Ok(m) if m.file_type().is_symlink() => return Err(link_error(rel)),
                _ => {}
            }
        }
        Ok(cur)
    }

    /// Reads the regular file at `rel`, at most `cap` bytes.
    pub fn read_regular_capped(&self, rel: &str, cap: u64) -> io::Result<CappedRead> {
        use std::io::Read as _;
        let segs = segments(rel)?;
        let path = match self.checked(&segs, rel) {
            Ok(p) => p,
            Err(_) => return Ok(CappedRead::Refused(ReadRefusal::Link)),
        };
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(CappedRead::Refused(ReadRefusal::Missing));
            }
            Err(e) => return Err(e),
        };
        if !meta.is_file() {
            return Ok(CappedRead::Refused(ReadRefusal::NotRegular));
        }
        if meta.len() > cap {
            return Ok(CappedRead::Refused(ReadRefusal::TooLarge {
                size: meta.len(),
            }));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&path)?
            .take(cap.saturating_add(1))
            .read_to_end(&mut bytes)?;
        Ok(finish_read(bytes, cap))
    }

    /// Lists the directory at `rel` (`""` for the anchor), one level,
    /// counting every entry examined against `budget`.
    pub fn list_dir(&self, rel: &str, budget: &mut ListBudget) -> io::Result<Listing> {
        let path = self.checked(&segments(rel)?, rel)?;
        let mut listing = Listing::default();
        for entry in std::fs::read_dir(&path)? {
            if budget.remaining == 0 {
                listing.exhausted = true;
                break;
            }
            budget.remaining -= 1;
            let entry = entry?;
            let ft = entry.file_type()?;
            let kind = if ft.is_symlink() {
                EntryKind::Link
            } else if ft.is_dir() {
                EntryKind::Dir
            } else if ft.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            listing.entries.push(ListedEntry {
                name: entry.file_name(),
                kind,
            });
        }
        listing.entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(listing)
    }

    /// What the entry at `rel` is, without following it.
    pub fn kind(&self, rel: &str) -> io::Result<Option<EntryKind>> {
        let path = self.checked(&segments(rel)?, rel)?;
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => Ok(Some(EntryKind::Link)),
            Ok(m) if m.is_dir() => Ok(Some(EntryKind::Dir)),
            Ok(m) if m.is_file() => Ok(Some(EntryKind::File)),
            Ok(_) => Ok(Some(EntryKind::Other)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The size of the regular file at `rel`, without following it.
    pub fn file_size(&self, rel: &str) -> io::Result<Option<u64>> {
        let path = self.checked(&segments(rel)?, rel)?;
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() => Ok(Some(m.len())),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Creates every missing directory of `rel`.
    pub fn mkdir_all(&self, rel: &str) -> io::Result<()> {
        crate::process_mode::guard_write(&self.anchor.join(rel))?;
        let path = self.checked(&segments(rel)?, rel)?;
        std::fs::create_dir_all(path)
    }

    /// Writes `bytes` as the new file `name` in the directory `dir_rel`.
    pub fn write_new(&self, dir_rel: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write as _;
        let leaf = one_segment(name)?;
        crate::process_mode::guard_write(&self.anchor.join(dir_rel).join(leaf))?;
        let dir = self.checked(&segments(dir_rel)?, dir_rel)?;
        let tmp = dir.join(temp_name());
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        let written = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| std::fs::rename(&tmp, dir.join(leaf)));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }

    /// Moves `from_rel` to `to_rel`, both beneath the anchor.
    pub fn rename_within(&self, from_rel: &str, to_rel: &str) -> io::Result<()> {
        crate::process_mode::guard_write(&self.anchor.join(from_rel))?;
        crate::process_mode::guard_write(&self.anchor.join(to_rel))?;
        let from = self.checked(&segments(from_rel)?, from_rel)?;
        let to = self.checked(&segments(to_rel)?, to_rel)?;
        std::fs::rename(from, to)
    }

    /// Removes the file, link or empty directory at `rel`.
    pub fn remove(&self, rel: &str) -> io::Result<()> {
        crate::process_mode::guard_write(&self.anchor.join(rel))?;
        let path = self.checked(&segments(rel)?, rel)?;
        let removed = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => std::fs::remove_dir(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(e) => Err(e),
        };
        match removed {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Holds the lock file `name` in the directory `dir_rel`.
    pub fn hold_lock(
        &self,
        dir_rel: &str,
        name: &str,
        wait: std::time::Duration,
    ) -> io::Result<super::HeldLock> {
        let leaf = one_segment(name)?;
        crate::process_mode::guard_write(&self.anchor.join(dir_rel).join(leaf))?;
        let dir = self.checked(&segments(dir_rel)?, dir_rel)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(leaf))?;
        let deadline = std::time::Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(super::HeldLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(LOCK_STEP);
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!("`{dir_rel}/{leaf}` is held by a live owner; try again"),
                    ));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
    }

    /// Whether the lock file `rel` is held, without creating or holding it.
    pub fn probe_lock(&self, rel: &str) -> io::Result<LockState> {
        let path = self.checked(&segments(rel)?, rel)?;
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(LockState::Absent),
            Err(e) => return Err(e),
        };
        match file.try_lock_shared() {
            Ok(()) => Ok(LockState::Free),
            Err(std::fs::TryLockError::WouldBlock) => Ok(LockState::Held),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

impl Beneath {
    /// The anchor this was opened on.
    pub fn anchor(&self) -> &Path {
        &self.anchor
    }
}

#[cfg(unix)]
impl Beneath {
    /// Removes the tree at `rel` (a file, a link or a directory with
    /// everything in it), never following a link. A missing entry is no
    /// error. Every entry is removed by its raw name through its parent's
    /// descriptor, so a name that is not UTF-8 goes too, and the walk keeps
    /// its own stack, so a deep tree needs no deep recursion.
    pub fn remove_tree(&self, rel: &str) -> io::Result<()> {
        use std::os::fd::{AsFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        crate::process_mode::guard_write(&self.anchor.join(rel))?;
        let (parents, leaf) = split_leaf(rel)?;
        let parent = match self.walk(&parents) {
            Ok(fd) => fd,
            Err(Walk::Missing | Walk::NotDir) => return Ok(()),
            Err(w) => return Err(w.into_io(rel)),
        };
        let name = sys::cstr(leaf)?;
        let gone = |r: io::Result<()>| match r {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
        match sys::lstat_at(parent.as_fd(), &name) {
            Ok(st) if sys::kind_of(&st) == EntryKind::Dir => {}
            Ok(_) => return gone(sys::unlinkat(parent.as_fd(), &name, false)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
        // Each level: its parent's descriptor, its name there, its own.
        let top = sys::open_dir(parent.as_fd(), &name)?;
        let mut stack: Vec<(OwnedFd, std::ffi::CString, OwnedFd)> = vec![(parent, name, top)];
        while let Some(depth) = stack.len().checked_sub(1) {
            let (names, _) = sys::read_dir(stack[depth].2.as_fd(), || true)?;
            let mut child = None;
            for (raw, kind) in names {
                let c = std::ffi::CString::new(raw.as_bytes())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                let dir = stack[depth].2.as_fd();
                let kind = match kind {
                    Some(k) => k,
                    None => match sys::lstat_at(dir, &c) {
                        Ok(st) => sys::kind_of(&st),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(e),
                    },
                };
                if kind == EntryKind::Dir {
                    match sys::open_dir(dir, &c) {
                        Ok(fd) => {
                            child = Some((c, fd));
                            break;
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                        // Swapped for a link or a file since the listing:
                        // the entry itself goes.
                        Err(_) => gone(sys::unlinkat(dir, &c, false))?,
                    }
                } else {
                    gone(sys::unlinkat(dir, &c, false))?;
                }
            }
            match child {
                Some((c, fd)) => {
                    let parent = stack[depth].2.try_clone()?;
                    stack.push((parent, c, fd));
                }
                None => {
                    let (parent, name, dir) = stack.pop().expect("a level");
                    drop(dir);
                    gone(sys::unlinkat(parent.as_fd(), &name, true))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(not(unix))]
impl Beneath {
    /// Removes the tree at `rel` (a file, a link or a directory with
    /// everything in it), never following a link. A missing entry is no
    /// error.
    pub fn remove_tree(&self, rel: &str) -> io::Result<()> {
        if self.kind(rel)? == Some(EntryKind::Dir) {
            let mut budget = ListBudget {
                remaining: usize::MAX,
            };
            for entry in self.list_dir(rel, &mut budget)?.entries {
                let name = entry.name.to_str().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "a name that is not UTF-8")
                })?;
                self.remove_tree(&format!("{rel}/{name}"))?;
            }
        }
        self.remove(rel)
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;

    thread_local! {
        /// What runs inside [`Beneath::write_new`] after the directory
        /// descriptor is open and before the temp file is created, on this
        /// thread (a test's swap).
        static BETWEEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    thread_local! {
        /// What runs inside [`Beneath::read_regular_capped`] after the
        /// entry was looked at and before it is opened, on this thread.
        static AFTER_LOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// A step planted after a lock file exists; `true` once it ran.
    type LockStep = Box<dyn FnMut(&Path) -> bool>;

    thread_local! {
        /// What runs inside [`Beneath::hold_lock`] after the lock file
        /// exists and before it is locked, on this thread, with its path;
        /// it stays planted until it answers that it ran.
        static AFTER_LOCK_FILE: std::cell::RefCell<Option<LockStep>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn after_lock_file(path: &Path) {
        if let Some(mut step) = AFTER_LOCK_FILE.with(|b| b.borrow_mut().take())
            && !step(path)
        {
            AFTER_LOCK_FILE.with(|b| *b.borrow_mut() = Some(step));
        }
    }

    /// Plants a step for the next [`Beneath::hold_lock`] calls on this
    /// thread, until one answers `true`.
    pub(crate) fn plant_after_lock_file(step: impl FnMut(&Path) -> bool + 'static) {
        AFTER_LOCK_FILE.with(|b| *b.borrow_mut() = Some(Box::new(step)));
    }

    /// Runs the swap a test planted, once.
    pub(super) fn between_steps() {
        if let Some(swap) = BETWEEN.with(|b| b.borrow_mut().take()) {
            swap();
        }
    }

    /// Plants a swap for the next [`Beneath::write_new`] on this thread.
    pub(crate) fn plant_between(swap: impl FnOnce() + 'static) {
        BETWEEN.with(|b| *b.borrow_mut() = Some(Box::new(swap)));
    }

    pub(super) fn between_look_and_open() {
        if let Some(swap) = AFTER_LOOK.with(|b| b.borrow_mut().take()) {
            swap();
        }
    }

    fn plant_after_look(swap: impl FnOnce() + 'static) {
        AFTER_LOOK.with(|b| *b.borrow_mut() = Some(Box::new(swap)));
    }

    /// A regular file with one link is looked at, then swapped for a hard
    /// link of another file before it is opened: the open file is checked
    /// again and refused, its bytes never read.
    #[test]
    fn a_swap_to_a_hard_link_after_the_look_is_refused() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("report.md"), b"plain").unwrap();
        std::fs::write(t.path().join("secret"), b"other file").unwrap();
        let (dir, report) = (t.path().to_path_buf(), t.path().join("report.md"));
        plant_after_look(move || {
            std::fs::remove_file(&report).unwrap();
            std::fs::hard_link(dir.join("secret"), &report).unwrap();
        });
        let b = Beneath::open(t.path()).unwrap();
        assert!(matches!(
            b.read_regular_capped("report.md", 1024).unwrap(),
            CappedRead::Refused(ReadRefusal::HardLink)
        ));
    }

    /// A FIFO swapped in after the look is opened without blocking and
    /// refused as not regular.
    #[test]
    fn a_swap_to_a_fifo_after_the_look_is_refused_without_blocking() {
        let t = tempfile::tempdir().unwrap();
        let report = t.path().join("report.md");
        std::fs::write(&report, b"plain").unwrap();
        let path = t.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        // On its own thread, so a blocking open shows as a timeout here.
        std::thread::spawn(move || {
            plant_after_look(move || {
                std::fs::remove_file(&report).unwrap();
                let c = std::ffi::CString::new(report.as_os_str().as_encoded_bytes()).unwrap();
                // SAFETY: a NUL-terminated path.
                assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
            });
            let b = Beneath::open(&path).unwrap();
            let _ = tx.send(b.read_regular_capped("report.md", 1024).unwrap());
        });
        let read = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the read never blocks");
        assert!(matches!(read, CappedRead::Refused(ReadRefusal::NotRegular)));
    }

    /// Another regular file renamed over the entry after the look is
    /// refused: what was looked at is not what was opened.
    #[test]
    fn a_swap_to_another_file_after_the_look_is_refused() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("report.md"), b"plain").unwrap();
        std::fs::write(t.path().join("other"), b"other bytes").unwrap();
        let dir = t.path().to_path_buf();
        plant_after_look(move || {
            std::fs::rename(dir.join("other"), dir.join("report.md")).unwrap();
        });
        let b = Beneath::open(t.path()).unwrap();
        assert!(matches!(
            b.read_regular_capped("report.md", 1024).unwrap(),
            CappedRead::Refused(_)
        ));
    }

    /// A name that is not UTF-8 and a deep chain of directories are
    /// removed whole.
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "APFS refuses a file name that is not UTF-8"
    )]
    fn remove_tree_removes_names_that_are_not_utf8_and_deep_trees() {
        use std::os::unix::ffi::OsStrExt;
        let t = tempfile::tempdir().unwrap();
        let top = t.path().join("results/staging/tok");
        std::fs::create_dir_all(&top).unwrap();
        std::fs::write(top.join(std::ffi::OsStr::from_bytes(b"bad\xff")), b"x").unwrap();
        let mut deep = top.clone();
        for _ in 0..200 {
            deep.push("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("leaf"), b"y").unwrap();
        let b = Beneath::open(t.path()).unwrap();
        b.remove_tree("results/staging/tok").unwrap();
        assert!(!top.exists());
        assert!(t.path().join("results/staging").exists());
    }

    /// The target directory is swapped for a link after its descriptor was
    /// opened and before the temp file is created: the file lands in the
    /// directory that was opened (now moved aside), never where the link
    /// points.
    #[test]
    fn a_directory_swapped_inside_a_write_is_never_gone_through() {
        let t = tempfile::tempdir().unwrap();
        let run = t.path().join("run");
        let outside = t.path().join("outside");
        std::fs::create_dir_all(run.join("results/staging")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let b = Beneath::open(&run).unwrap();
        let (staging, aside, target) = (
            run.join("results/staging"),
            run.join("results/aside"),
            outside.clone(),
        );
        BETWEEN.with(|s| {
            *s.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(&staging, &aside).unwrap();
                std::os::unix::fs::symlink(&target, &staging).unwrap();
            }))
        });
        b.write_new("results/staging", "txn.json", b"{}").unwrap();
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        assert_eq!(
            std::fs::read(run.join("results/aside/txn.json")).unwrap(),
            b"{}"
        );
    }

    crate::read_only_test! {
        /// Every write is refused in a read-only process; reads are not.
        fn guard_refuses_in_read_only_child() before t = {
            let t = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(t.path().join("results")).unwrap();
            std::fs::write(t.path().join("results/a"), b"x").unwrap();
            t
        } then {
            let b = Beneath::open(t.path()).unwrap();
            assert!(b.mkdir_all("results/objects").is_err());
            assert!(b.write_new("results", "b", b"y").is_err());
            assert!(b.rename_within("results/a", "results/c").is_err());
            assert!(b.remove("results/a").is_err());
            assert!(b.hold_lock("results", ".lock", std::time::Duration::ZERO).is_err());
            assert!(matches!(
                b.read_regular_capped("results/a", 10).unwrap(),
                CappedRead::Read { .. }
            ));
            assert!(!t.path().join("results/objects").exists());
            assert!(!t.path().join("results/b").exists());
            assert!(!t.path().join("results/.lock").exists());
        }
    }
}
