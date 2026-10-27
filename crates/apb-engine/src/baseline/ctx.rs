//! What a baseline source read needs from the run, the snapshot of a
//! source that could not be read at all, and the two rules every process a
//! read spawns (a script, a project command) follows: it never sees a
//! connector secret, and it must leave the working tree as it found it.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::{Snapshot, status};
use crate::event::Event;

/// Everything a source read needs from the run.
pub(crate) struct ReadCtx<'a> {
    pub run_dir: &'a Path,
    /// The project root (connector secrets resolve against it).
    pub root: &'a Path,
    /// The run's working tree: scripts and project checks run in it.
    pub workdir: &'a Path,
    pub params: &'a BTreeMap<String, String>,
    pub spawn_env: &'a [(String, String)],
    /// Every env var name an installed connector references: removed from
    /// each process a read spawns, as from every agent and script.
    pub scrub: &'a [String],
    pub cancel: &'a AtomicBool,
    /// `before` or `after`, handed to a script as `APB_BASELINE_PHASE`.
    pub phase: &'a str,
    /// The journal so far (the scripts digest pin is in it).
    pub events: &'a [Event],
}

impl ReadCtx<'_> {
    pub(crate) fn workdir_text(&self) -> String {
        self.workdir.to_string_lossy().into_owned()
    }

    /// The scrubbed names, as the spawn helpers take them.
    pub(crate) fn scrub_names(&self) -> Vec<&str> {
        self.scrub.iter().map(String::as_str).collect()
    }

    /// Runs `f`, which spawns a process in the working tree, and checks
    /// that the tree is as it was: `Err` with the reason the read is
    /// unknown when the tree changed, or when it cannot be checked at all
    /// (not a git work tree, no commit yet, git failed).
    pub(crate) fn tree_guarded<T>(&self, f: impl FnOnce() -> T) -> Result<T, &'static str> {
        const UNCHECKED: &str = "the working tree could not be checked";
        let before = apb_core::fingerprint::git_fingerprint(self.workdir, &[]).ok_or(UNCHECKED)?;
        let out = f();
        match apb_core::fingerprint::git_fingerprint(self.workdir, &[]) {
            None => Err(UNCHECKED),
            Some(after) if after != before => Err("the check changed the working tree"),
            Some(_) => Ok(out),
        }
    }
}

/// A snapshot whose source could not be read at all.
pub(crate) fn unknown(
    reason: impl Into<String>,
    input_digest: String,
    inputs: Vec<String>,
    ctx: &ReadCtx<'_>,
) -> Snapshot {
    Snapshot {
        status: status::UNKNOWN.into(),
        reason: Some(reason.into()),
        input_digest,
        inputs,
        workdir: Some(ctx.workdir_text()),
        ..Snapshot::default()
    }
}
