//! The stdio server a session starts: what it is started with, and the
//! program file it runs. The start itself is the session's.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// A value of the server's environment: a plain account field, or a
/// resolved secret, exposed only when the process is spawned.
pub enum EnvValue {
    Plain(String),
    Secret(apb_core::secret::Secret),
}

/// How a session starts a [`ProgramFile`]: from the descriptor that was
/// hashed, or by its path after hashing it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartFrom {
    /// Linux: the process is executed through `/proc/self/fd/<n>`, the
    /// descriptor the digest was read from, inherited for exactly that
    /// purpose. The file that runs is the file that was hashed, whatever
    /// happens to its path afterwards.
    Descriptor,
    /// Every other platform (macOS): the path is opened again without
    /// following a link and hashed again immediately before the spawn; a
    /// different digest refuses the start. The window between that second
    /// hash and the exec remains.
    PathCheckedAgain,
}

impl StartFrom {
    /// The way this platform starts a program: `Descriptor` on Linux,
    /// `PathCheckedAgain` everywhere else.
    pub fn platform() -> StartFrom {
        if cfg!(target_os = "linux") {
            StartFrom::Descriptor
        } else {
            StartFrom::PathCheckedAgain
        }
    }
}

/// Why a program file cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramRefusal {
    /// Missing, a link (never followed), or not a regular file.
    NotARegularFile,
    /// It could not be opened or read.
    Unreadable,
    /// `PathCheckedAgain`: the path no longer holds the bytes that were
    /// hashed.
    Changed,
}

/// The one open of a program file: opened once without following a link,
/// its digest read from that descriptor, the descriptor kept open until
/// the session starts the program from it ([`StartFrom::Descriptor`]) or
/// checks the path against the digest again ([`StartFrom::PathCheckedAgain`]).
/// Every MCP start (a call, the pin-check healthcheck, an approval's
/// listing) goes through it, and so does every digest of a program file.
pub struct ProgramFile {
    path: PathBuf,
    file: File,
    digest: String,
    start: StartFrom,
}

impl std::fmt::Debug for ProgramFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgramFile")
            .field("path", &self.path)
            .field("digest", &self.digest)
            .field("start", &self.start)
            .finish()
    }
}

/// Opens `path` without following a link at its last component and
/// without blocking on a FIFO, close-on-exec.
fn open_no_follow(path: &Path) -> Result<File, ProgramRefusal> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) | Some(libc::ENOENT) | Some(libc::ENOTDIR) => {
                ProgramRefusal::NotARegularFile
            }
            _ => ProgramRefusal::Unreadable,
        })?;
    let meta = file.metadata().map_err(|_| ProgramRefusal::Unreadable)?;
    if !meta.is_file() {
        return Err(ProgramRefusal::NotARegularFile);
    }
    Ok(file)
}

/// `sha256:<hex>` of everything `file` holds from its start, streamed.
fn digest_of(file: &mut File) -> Result<String, ProgramRefusal> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ProgramRefusal::Unreadable)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| ProgramRefusal::Unreadable)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!(
        "sha256:{}",
        apb_core::content::hex_lower(&hasher.finalize())
    ))
}

impl ProgramFile {
    /// Opens `path` once (no link followed at its last component, a
    /// regular file only) and reads its digest from that descriptor.
    pub fn open(path: &Path, start: StartFrom) -> Result<ProgramFile, ProgramRefusal> {
        let mut file = open_no_follow(path)?;
        let digest = digest_of(&mut file)?;
        Ok(ProgramFile {
            path: path.to_path_buf(),
            file,
            digest,
            start,
        })
    }

    /// The path the file was opened at.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `sha256:<hex>` of the file's bytes as this descriptor read them.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn start_from(&self) -> StartFrom {
        self.start
    }

    /// The descriptor the digest was read from.
    pub(crate) fn raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.file.as_raw_fd()
    }

    /// For `PathCheckedAgain`: the path opened again without following a
    /// link and hashed again; `Err` when it no longer holds the digest.
    pub(crate) fn check_path_again(&self) -> Result<(), ProgramRefusal> {
        let mut again = open_no_follow(&self.path)?;
        if digest_of(&mut again)? == self.digest {
            Ok(())
        } else {
            Err(ProgramRefusal::Changed)
        }
    }
}

/// A stdio server to start. `env` is the whole environment: the client
/// clears the inherited one first. The working directory is a fresh empty
/// private directory the session makes and removes; it is never the
/// caller's.
pub struct StdioServer {
    /// The program file the person's approval pinned, opened and hashed
    /// once. The client never looks a name up.
    pub program: ProgramFile,
    pub args: Vec<String>,
    pub env: Vec<(String, EnvValue)>,
}
