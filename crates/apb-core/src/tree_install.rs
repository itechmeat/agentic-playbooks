//! Materializing a tree of files compiled into the binary (official
//! connectors, bundled skills) into a folder on disk: staged beside the
//! target, digested there, and only then moved into place, so a failure part
//! way through never leaves a half-written tree where a reader could load
//! it. Shared by [`crate::connector::install`] and [`crate::bundled`], so the
//! staging, digest and swap semantics live here once.

use std::collections::BTreeMap;
use std::path::Path;

/// The files of one embedded tree: path relative to the tree root -> bytes.
pub type FileMap = BTreeMap<String, Vec<u8>>;

/// Groups embedded paths of the form `<folder>/<rest>` by their first
/// segment. A path without a folder (a stray top-level file) is ignored, and
/// so is a folder whose name is not a valid slug.
pub(crate) fn group_by_folder<I>(files: I) -> BTreeMap<String, FileMap>
where
    I: IntoIterator<Item = (String, Vec<u8>)>,
{
    let mut grouped: BTreeMap<String, FileMap> = BTreeMap::new();
    for (path, bytes) in files {
        let Some((name, rel)) = path.split_once('/') else {
            continue;
        };
        if crate::profile::validate_profile_name(name).is_err() {
            continue;
        }
        grouped
            .entry(name.to_string())
            .or_default()
            .insert(rel.to_string(), bytes);
    }
    grouped
}

/// Writes every file of `files` into `dir` (creating parents), atomically
/// per file.
pub(crate) fn write_files(dir: &Path, files: &FileMap) -> std::io::Result<()> {
    for (rel, bytes) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::fsutil::atomic_write(&path, bytes)?;
    }
    Ok(())
}

/// Whether the files under `dir` are exactly `files`: the same relative
/// paths with the same bytes, nothing more, nothing less. A tree that cannot
/// be read never matches.
pub(crate) fn dir_matches(dir: &Path, files: &FileMap) -> bool {
    fn collect(base: &Path, dir: &Path, out: &mut FileMap) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                collect(base, &path, out)?;
            } else {
                let rel = path
                    .strip_prefix(base)
                    .map_err(std::io::Error::other)?
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&path)?);
            }
        }
        Ok(())
    }
    let mut on_disk = FileMap::new();
    collect(dir, dir, &mut on_disk).is_ok() && &on_disk == files
}

/// What [`install_tree`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeInstalled {
    /// The tree digest of the installed folder.
    pub digest: String,
    /// The target already held this exact digest: nothing changed on disk.
    pub no_op: bool,
}

/// Why [`install_tree`] refused.
#[derive(Debug)]
pub(crate) enum TreeInstallError {
    /// The target exists, differs, and the caller did not allow replacing
    /// it.
    NeedsForce { path: String },
    /// A filesystem step failed; the message is caller-ready.
    Io(String),
}

/// The name parts of the per-call staging and backup folders beside a
/// target (`.<name><infix><created_ms>-<unique>`).
const STAGING_INFIX: &str = ".install-tmp-";
const BACKUP_INFIX: &str = ".install-backup-";
/// The lock one install of a target holds beside it
/// (`.<name><LOCK_SUFFIX>`).
const LOCK_SUFFIX: &str = ".install.lock";

/// The name of the lock one install of `name` holds beside it.
pub(crate) fn lock_name(name: &str) -> String {
    format!(".{name}{LOCK_SUFFIX}")
}
/// How old a staging or backup folder must be before another call clears
/// it: far longer than any install takes, so only a crashed call's
/// leftovers go.
const LEFTOVER_AGE_MS: u128 = 600_000;

/// When the staging or backup folder `file_name` (after its `prefix`) was
/// created, from the time its name carries.
fn created_ms(file_name: &str, prefix: &str) -> Option<u128> {
    file_name
        .strip_prefix(prefix)?
        .split_once('-')?
        .0
        .parse()
        .ok()
}

/// Removes the staging and backup folders of `name` an interrupted earlier
/// call left in `base`, once they are older than [`LEFTOVER_AGE_MS`]. The
/// age is the creation time in the folder's name, never its mtime: a rename
/// keeps a folder's old mtime, so a backup made a moment ago from an old
/// tree would look old. A folder whose name carries no time (an older apb's)
/// is left alone. The caller holds the install lock of `name`, so no live
/// call of this apb owns a folder it clears. Best effort.
fn clear_leftovers(base: &Path, name: &str) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let prefixes = [
        format!(".{name}{STAGING_INFIX}"),
        format!(".{name}{BACKUP_INFIX}"),
    ];
    let now = crate::clock::now_ms();
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        let old = prefixes.iter().any(|p| {
            created_ms(&file_name, p).is_some_and(|at| now.saturating_sub(at) >= LEFTOVER_AGE_MS)
        });
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Installs `files` as `<base>/<name>`.
///
/// Stages them into `<base>/.<name>.install-tmp-<unique>`, digests the
/// staged tree, and only then moves it onto the target. An existing target
/// with the same digest is a no-op; one that differs is replaced only when
/// `may_replace` says so for it, through a backup swap that puts the
/// previous tree back when the second rename fails (and, when even that
/// fails, names where the previous tree is kept in the error). The staging and backup
/// names are unique per call, so two processes installing the same tree
/// never write into each other's staging, and one install of a target holds
/// its lock (`.<name>.install.lock` beside it) throughout; the staging
/// folder is removed on every path, and leftovers of an interrupted call are
/// cleared once the time in their name is old enough that no live call can
/// own them.
pub(crate) fn install_tree(
    base: &Path,
    name: &str,
    files: &FileMap,
    may_replace: impl FnOnce(&Path) -> bool,
) -> Result<TreeInstalled, TreeInstallError> {
    let target = base.join(name);
    std::fs::create_dir_all(base)
        .map_err(|e| TreeInstallError::Io(format!("cannot create {}: {e}", base.display())))?;
    let _lock = crate::fsutil::lock_dir(base, &lock_name(name))
        .map_err(|e| TreeInstallError::Io(format!("cannot lock the install of `{name}`: {e}")))?;
    clear_leftovers(base, name);
    let unique = format!(
        "{}-{}",
        crate::clock::now_ms(),
        uuid::Uuid::new_v4().simple()
    );
    let staging = base.join(format!(".{name}{STAGING_INFIX}{unique}"));
    if let Err(e) = write_files(&staging, files) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(TreeInstallError::Io(format!("cannot stage `{name}`: {e}")));
    }
    let limits = crate::content::TreeLimits::default();
    let digest = match crate::content::tree_digest(&staging, &limits) {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(TreeInstallError::Io(format!("cannot digest `{name}`: {e}")));
        }
    };

    if target.exists() {
        let current = crate::content::tree_digest(&target, &limits).ok();
        if current.as_deref() == Some(digest.as_str()) {
            let _ = std::fs::remove_dir_all(&staging);
            return Ok(TreeInstalled {
                digest,
                no_op: true,
            });
        }
        if !may_replace(&target) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(TreeInstallError::NeedsForce {
                path: target.display().to_string(),
            });
        }
        // Backup swap: move the existing target aside before moving staging
        // into place, so a rename failure part way through never leaves
        // nothing installed. On a failed second rename the backup is moved
        // straight back, and it is removed only once the swap succeeded.
        let backup = base.join(format!(".{name}{BACKUP_INFIX}{unique}"));
        if let Err(e) = std::fs::rename(&target, &backup) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(TreeInstallError::Io(format!(
                "cannot replace {}: {e}",
                target.display()
            )));
        }
        #[cfg(test)]
        tests::BEFORE_SWAP.with(|h| {
            if let Some(h) = h.borrow().as_ref() {
                h(&target);
            }
        });
        if let Err(e) = std::fs::rename(&staging, &target) {
            let _ = std::fs::remove_dir_all(&staging);
            let kept = match std::fs::rename(&backup, &target) {
                Ok(()) => String::new(),
                Err(r) => format!(
                    "; the previous copy could not be put back and is kept at {}: {r}",
                    backup.display()
                ),
            };
            return Err(TreeInstallError::Io(format!(
                "cannot install into {}: {e}{kept}",
                target.display()
            )));
        }
        let _ = std::fs::remove_dir_all(&backup);
        return Ok(TreeInstalled {
            digest,
            no_op: false,
        });
    }

    if let Err(e) = std::fs::rename(&staging, &target) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(TreeInstallError::Io(format!(
            "cannot install into {}: {e}",
            target.display()
        )));
    }
    Ok(TreeInstalled {
        digest,
        no_op: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type SwapHook = Box<dyn Fn(&Path)>;

    thread_local! {
        /// Runs right before the staged tree is renamed onto the target in
        /// a backup swap (this thread only).
        pub(super) static BEFORE_SWAP: std::cell::RefCell<Option<SwapHook>> =
            const { std::cell::RefCell::new(None) };
    }

    /// When the swap's second rename fails and the previous tree cannot be
    /// put back (another program created the target in between), the
    /// staging is removed and the error names where the previous tree is
    /// kept, so a changed copy is never lost without a word.
    #[test]
    fn a_failed_swap_removes_the_staging_and_names_a_backup_it_could_not_restore() {
        let base = tempfile::tempdir().unwrap();
        install_tree(base.path(), "skill", &tree(3, "old"), |_| true).unwrap();
        BEFORE_SWAP.with(|h| {
            *h.borrow_mut() = Some(Box::new(|target: &Path| {
                std::fs::create_dir_all(target).unwrap();
                std::fs::write(target.join("other"), "x").unwrap();
            }));
        });
        let err = install_tree(base.path(), "skill", &tree(3, "new"), |_| true).unwrap_err();
        BEFORE_SWAP.with(|h| *h.borrow_mut() = None);
        let names: Vec<String> = std::fs::read_dir(base.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            !names.iter().any(|n| n.contains(STAGING_INFIX)),
            "the staging is removed: {names:?}"
        );
        let backup = names
            .iter()
            .find(|n| n.contains(BACKUP_INFIX))
            .expect("the previous tree is kept");
        assert!(dir_matches(&base.path().join(backup), &tree(3, "old")));
        let TreeInstallError::Io(msg) = err else {
            panic!("an io error: {err:?}")
        };
        assert!(msg.contains(backup.as_str()), "{msg}");
    }

    /// Every apb on the machine, older and newer side by side, takes the
    /// same lock for the same target: its name never changes.
    #[test]
    fn the_install_lock_name_is_shared_across_versions() {
        assert_eq!(lock_name("apb-review"), ".apb-review.install.lock");
    }

    fn tree(n: usize, tag: &str) -> FileMap {
        (0..n)
            .map(|i| {
                (
                    format!("d{}/f{i}.md", i % 7),
                    format!("{tag} {i}\n").into_bytes(),
                )
            })
            .collect()
    }

    /// Two processes installing at once never write into each other's
    /// staging: whichever wins, the target is one whole tree and nothing is
    /// left beside it.
    #[test]
    fn concurrent_installs_of_one_tree_leave_a_whole_tree_and_no_leftovers() {
        let base = tempfile::tempdir().unwrap();
        for round in 0..5 {
            let files = [
                tree(60, &format!("a{round}")),
                tree(60, &format!("b{round}")),
            ];
            std::thread::scope(|s| {
                for f in &files {
                    let base = base.path();
                    s.spawn(move || install_tree(base, "skill", f, |_| true));
                }
            });
            let target = base.path().join("skill");
            assert!(
                files.iter().any(|f| dir_matches(&target, f)),
                "round {round}: a whole tree"
            );
            let leftovers: Vec<_> = std::fs::read_dir(base.path())
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with(".skill."))
                .collect();
            assert!(leftovers.is_empty(), "{leftovers:?}");
        }
    }

    /// A backup renamed from an old tree keeps that tree's old mtime; its
    /// age is read from its name, so a fresh one is never cleared, an old
    /// one is, and a name without a time is never touched.
    #[test]
    fn leftovers_are_judged_by_the_time_in_their_name() {
        let base = tempfile::tempdir().unwrap();
        let now = crate::clock::now_ms();
        let fresh = format!(".skill{BACKUP_INFIX}{now}-a");
        let stale = format!(".skill{STAGING_INFIX}{}-b", now - LEFTOVER_AGE_MS - 1);
        let legacy = format!(".skill{BACKUP_INFIX}0123abcd");
        for n in [&fresh, &stale, &legacy] {
            std::fs::create_dir_all(base.path().join(n)).unwrap();
        }
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
        std::fs::File::open(base.path().join(&fresh))
            .unwrap()
            .set_modified(old)
            .unwrap();
        clear_leftovers(base.path(), "skill");
        assert!(base.path().join(&fresh).exists(), "a live call's backup");
        assert!(
            !base.path().join(&stale).exists(),
            "a crashed call's staging"
        );
        assert!(base.path().join(&legacy).exists(), "no time in the name");
    }
}
