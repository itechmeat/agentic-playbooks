//! The one git call apb runs with a transport and the operator's
//! credentials: the publish step's push, and the read-only remote listings
//! its plan and reconcile take
//! ([decision record 0009](../../../../docs/adr/0009-apb-git-never-runs-a-program-the-tree-names.md)
//! names this builder as its one exception).
//!
//! Every such call runs from a shadow git directory, never the tree's
//! repository: a bare repository made with an empty template in a new
//! directory `<run_dir>/publish/shadow-<random>` (mode 0700), created for
//! each plan, push and reconcile, never reused and removed afterwards
//! ([`Shadow`]). It reaches the tree's objects through its
//! `objects/info/alternates`. The tree's local config is never read, so its
//! `core.sshCommand`, `credential.helper`, `url.*.insteadOf`,
//! `url.*.pushInsteadOf`, `remote.*.receivepack` and hooks cannot act, and
//! nothing planted ahead of time can either; the operator's global and
//! system config (their credential helper and ssh setup) still apply, and a
//! `url.*` rewrite there that changes the URL refuses the plan
//! ([`rewrites`]).
//!
//! On top of the hardened settings ([`super::HARDENED_SETTINGS`], minus the
//! credential helper reset) exactly one transport is allowed, the URL's
//! own; redirects are not followed, nothing prompts, nothing is signed, and
//! no submodule is pushed. Every other apb git call keeps
//! `protocol.allow=never` and an empty credential helper.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::publish::refname;
use crate::publish::url::PushUrl;

/// The settings the transport builder adds after the hardened ones.
pub const TRANSPORT_SETTINGS: [&str; 6] = [
    "http.followRedirects=false",
    "push.gpgSign=false",
    "push.useForceIfIncludes=false",
    "push.recurseSubmodules=no",
    "submodule.recurse=false",
    "credential.interactive=false",
];

/// The environment that keeps every helper from prompting.
const NO_PROMPT_ENV: [(&str, &str); 4] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_NO_REPLACE_OBJECTS", "1"),
    ("GCM_INTERACTIVE", "never"),
    ("SSH_ASKPASS_REQUIRE", "never"),
];

/// Variables removed besides every `GIT_*` one: git falls back to
/// `SSH_ASKPASS` for a password when `GIT_ASKPASS` and `core.askPass` are
/// empty, which would open a dialog.
const NO_PROMPT_REMOVED: [&str; 1] = ["SSH_ASKPASS"];

/// The directory under a run's directory the shadows are made in.
pub fn shadows_dir(run_dir: &Path) -> PathBuf {
    run_dir.join("publish")
}

/// A shadow git directory: a fresh bare repository with an empty template
/// (no hooks, no config beyond what `init` writes) in a new 0700 directory,
/// reaching the objects of one repository through `objects/info/alternates`.
/// Removed when dropped.
#[derive(Debug)]
pub struct Shadow {
    dir: PathBuf,
}

impl Shadow {
    /// Makes a new shadow under [`shadows_dir`] for the repository whose
    /// common directory is `common_dir`. The directory gets a random name
    /// and is created exclusively: one that already exists is never used.
    /// Runs no transport.
    pub fn create(run_dir: &Path, common_dir: &Path) -> io::Result<Shadow> {
        let parent = shadows_dir(run_dir);
        std::fs::create_dir_all(&parent)?;
        crate::fsutil::ensure_no_symlink_below(run_dir, &parent)?;
        let dir = parent.join(format!("shadow-{}", uuid::Uuid::new_v4().simple()));
        create_private_dir(&dir)?;
        // Owned from here: a failure below removes it.
        let shadow = Shadow { dir };
        let out = super::base()
            .args(["init", "--quiet", "--bare", "--template="])
            .arg(&shadow.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "git init of the shadow directory failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let info = shadow.dir.join("objects").join("info");
        std::fs::create_dir_all(&info)?;
        std::fs::write(info.join("alternates"), alternates_line(common_dir)?)?;
        Ok(shadow)
    }

    /// The shadow's git directory.
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Shadow {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Creates `dir` (not its parents) with mode 0700; an existing one is an
/// error.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// The `objects/info/alternates` line naming `common_dir`'s objects. The
/// file takes one absolute path per line, so a relative path or one with a
/// newline is refused.
fn alternates_line(common_dir: &Path) -> io::Result<String> {
    let objects = common_dir.join("objects");
    let text = objects.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the object directory is not UTF-8",
        )
    })?;
    // A relative entry would be read against the shadow's own objects, and
    // a line led by `#` or `"` as a comment or a quoted path.
    if !objects.is_absolute() || text.contains(['\n', '\r']) || text.starts_with(['"', '#']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the object directory's path cannot be written to an alternates file",
        ));
    }
    Ok(format!("{text}\n"))
}

/// The transport-enabled `git` for `url`, run from `shadow`. The caller
/// adds the subcommand.
pub fn command(shadow: &Shadow, url: &PushUrl) -> Command {
    let mut cmd = super::bare_git();
    for s in super::HARDENED_SETTINGS
        .iter()
        .filter(|s| **s != super::NO_CREDENTIAL_HELPER)
    {
        cmd.arg("-c").arg(s);
    }
    cmd.arg("-c")
        .arg(format!("protocol.{}.allow=always", url.scheme().as_str()));
    for s in TRANSPORT_SETTINGS {
        cmd.arg("-c").arg(s);
    }
    cmd.env("GIT_DIR", shadow.path());
    for (k, v) in NO_PROMPT_ENV {
        cmd.env(k, v);
    }
    for k in NO_PROMPT_REMOVED {
        cmd.env_remove(k);
    }
    cmd.stdin(Stdio::null());
    cmd
}

/// The push of `after` to `refs/heads/<branch>` under an exact lease: the
/// remote ref must still be `remote_before` (absent when `None`).
pub fn push(
    shadow: &Shadow,
    url: &PushUrl,
    branch: &str,
    after: &str,
    remote_before: Option<&str>,
) -> Command {
    let full = refname::full(branch);
    let mut cmd = command(shadow, url);
    cmd.args([
        "push",
        "--porcelain",
        "--no-verify",
        "--no-follow-tags",
        "--no-signed",
    ])
    .arg(format!(
        "--force-with-lease={full}:{}",
        remote_before.unwrap_or("")
    ))
    .arg(url.as_str())
    .arg(format!("{after}:{full}"));
    cmd
}

/// The listing of the remote's branch heads, or only `branch` when given:
/// `ls-remote --heads --refs <url> [refs/heads/<branch>]`.
pub fn list_heads(shadow: &Shadow, url: &PushUrl, branch: Option<&str>) -> Command {
    let mut cmd = command(shadow, url);
    cmd.args(["ls-remote", "--heads", "--refs"])
        .arg(url.as_str());
    if let Some(b) = branch {
        cmd.arg(refname::full(b));
    }
    cmd
}

/// The `url.*.insteadOf` and `url.*.pushInsteadOf` rules visible from the
/// shadow (the operator's global and system config, includes counted), as
/// `config -z --get-regexp`. Runs no transport. Exits 1 when there are none.
pub fn rewrite_rules(shadow: &Shadow, url: &PushUrl) -> Command {
    let mut cmd = command(shadow, url);
    cmd.args([
        "config",
        "-z",
        "--get-regexp",
        r"^url\..*\.(insteadof|pushinsteadof)$",
    ]);
    cmd
}

/// Whether the rules of the `config -z --get-regexp` listing `rules` send
/// `url` to another address, exactly as git picks them: among the rules of
/// one kind the longest matching prefix wins; a listing uses the
/// `insteadOf` winner, and a push the `pushInsteadOf` winner when one
/// matches, else the `insteadOf` one. Either effective URL differing from
/// `url` counts: the push or the listing would leave for an address nobody
/// consented to. An identity rule (one that rewrites the URL to itself)
/// changes nothing, also when it outranks a broader redirect.
pub fn rewrites(rules: &str, url: &str) -> bool {
    let mut fetch: Option<(usize, String)> = None;
    let mut push: Option<(usize, String)> = None;
    for (key, prefix) in rules.split('\0').filter_map(|entry| entry.split_once('\n')) {
        let Some(rest) = key.strip_prefix("url.") else {
            continue;
        };
        let (base, slot) = match rest.strip_suffix(".pushinsteadof") {
            Some(b) => (b, &mut push),
            None => match rest.strip_suffix(".insteadof") {
                Some(b) => (b, &mut fetch),
                None => continue,
            },
        };
        let Some(tail) = url.strip_prefix(prefix) else {
            continue;
        };
        if slot.as_ref().is_none_or(|(len, _)| prefix.len() > *len) {
            *slot = Some((prefix.len(), format!("{base}{tail}")));
        }
    }
    let fetch_url = fetch.map_or_else(|| url.to_string(), |(_, u)| u);
    let push_url = push.map_or_else(|| fetch_url.clone(), |(_, u)| u);
    fetch_url != url || push_url != url
}

/// Reads `ls-remote --heads --refs` output into `(sha, refs/heads/x)` pairs.
pub fn parse_heads(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter_map(|l| {
            let (sha, name) = l.split_once('\t')?;
            name.starts_with(refname::HEADS)
                .then(|| (sha.trim().to_string(), name.trim().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn env(cmd: &Command, key: &str) -> Option<Option<String>> {
        cmd.get_envs()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    }

    /// A shadow over a scratch run directory; the common directory need not
    /// exist for the command's shape.
    fn shadow(tmp: &tempfile::TempDir) -> Shadow {
        Shadow::create(tmp.path(), &tmp.path().join("tree/.git")).unwrap()
    }

    #[test]
    fn the_push_allows_exactly_one_transport_and_keeps_the_operators_helper() {
        let tmp = tempfile::tempdir().unwrap();
        let sh = shadow(&tmp);
        let url = PushUrl::parse("https://example.com/o/r.git").unwrap();
        let cmd = push(&sh, &url, "apb/x", "abc", None);
        let a = args(&cmd);
        assert_eq!(a[0], "--no-pager");
        assert!(a.contains(&"protocol.allow=never".into()));
        let allows: Vec<&String> = a
            .iter()
            .filter(|s| s.starts_with("protocol.") && s.ends_with(".allow=always"))
            .collect();
        assert_eq!(allows, [&"protocol.https.allow=always".to_string()]);
        assert!(!a.contains(&"credential.helper=".into()));
        assert!(a.contains(&"core.hooksPath=/dev/null".into()));
        assert!(a.contains(&"core.askPass=".into()));
        for s in TRANSPORT_SETTINGS {
            assert!(a.contains(&s.to_string()), "{s}");
        }
        let tail: Vec<&str> = a[a.iter().position(|s| s == "push").unwrap()..]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            [
                "push",
                "--porcelain",
                "--no-verify",
                "--no-follow-tags",
                "--no-signed",
                "--force-with-lease=refs/heads/apb/x:",
                "https://example.com/o/r.git",
                "abc:refs/heads/apb/x",
            ]
        );
        assert_eq!(
            env(&cmd, "GIT_DIR"),
            Some(Some(sh.path().display().to_string()))
        );
        // The objects come from the shadow's alternates file, never from
        // a variable git splits on `:`.
        assert_eq!(env(&cmd, "GIT_ALTERNATE_OBJECT_DIRECTORIES"), None);
        for (k, v) in NO_PROMPT_ENV {
            assert_eq!(env(&cmd, k), Some(Some(v.to_string())), "{k}");
        }
        assert_eq!(env(&cmd, "SSH_ASKPASS"), Some(None));
    }

    #[test]
    fn the_lease_names_the_remote_value_the_person_saw() {
        let tmp = tempfile::tempdir().unwrap();
        let url = PushUrl::parse("ssh://git@h/r.git").unwrap();
        let a = args(&push(
            &shadow(&tmp),
            &url,
            "refs/heads/main",
            "abc",
            Some("def"),
        ));
        assert!(a.contains(&"--force-with-lease=refs/heads/main:def".into()));
        assert!(a.contains(&"protocol.ssh.allow=always".into()));
    }

    #[test]
    fn the_hardened_base_still_resets_the_helper() {
        let a = args(&super::super::base());
        assert!(a.contains(&"credential.helper=".into()));
        assert!(a.contains(&"protocol.allow=never".into()));
        assert!(!a.iter().any(|s| s.ends_with(".allow=always")));
    }

    #[test]
    fn every_shadow_is_new_private_and_removed_after_use() {
        let tmp = tempfile::tempdir().unwrap();
        let common = tmp.path().join("work:tree/.git");
        let a = Shadow::create(tmp.path(), &common).unwrap();
        let b = Shadow::create(tmp.path(), &common).unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a.path().starts_with(shadows_dir(tmp.path())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(a.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        // No hooks, no rewrite, nothing but what `init` writes.
        assert!(!a.path().join("hooks").exists());
        let config = std::fs::read_to_string(a.path().join("config")).unwrap();
        assert!(!config.contains("url"), "{config}");
        // A `:` in the path survives: the alternates file is one path a line.
        assert_eq!(
            std::fs::read_to_string(a.path().join("objects/info/alternates")).unwrap(),
            format!("{}\n", common.join("objects").display())
        );
        let path = a.path().to_path_buf();
        drop(a);
        assert!(!path.exists());
    }

    #[test]
    fn an_existing_directory_is_never_made_a_shadow() {
        let tmp = tempfile::tempdir().unwrap();
        let planted = tmp.path().join("planted");
        std::fs::create_dir(&planted).unwrap();
        assert_eq!(
            create_private_dir(&planted).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn a_path_the_alternates_file_cannot_hold_is_refused() {
        assert!(alternates_line(Path::new("/a\nb/.git")).is_err());
        assert!(alternates_line(Path::new("\"q/.git")).is_err());
        assert!(alternates_line(Path::new("rel/.git")).is_err());
        assert!(alternates_line(Path::new("#c/.git")).is_err());
        assert!(alternates_line(Path::new("/a:b/.git")).is_ok());
    }

    #[test]
    fn a_rule_that_changes_the_url_is_a_rewrite() {
        let url = "file:///srv/good.git";
        let rules = |entries: &[(&str, &str)]| {
            entries
                .iter()
                .map(|(k, v)| format!("{k}\n{v}\0"))
                .collect::<String>()
        };
        assert!(!rewrites("", url));
        for (key, prefix) in [
            (
                "url.file:///srv/evil.git.pushinsteadof",
                "file:///srv/good.git",
            ),
            ("url.file:///srv/evil.insteadof", "file:///srv/good"),
            ("url.ext::sh -c x% y.insteadof", "file://"),
        ] {
            assert!(rewrites(&rules(&[(key, prefix)]), url), "{key}");
        }
        for (key, prefix) in [
            // Another prefix, or a rule that rewrites to the same address.
            ("url.file:///srv/evil.git.pushinsteadof", "https://h/"),
            ("url.file:///srv/good.git.insteadof", "file:///srv/good.git"),
            ("core.pushinsteadof", "file://"),
        ] {
            assert!(!rewrites(&rules(&[(key, prefix)]), url), "{key}");
        }
        // git takes the longest matching prefix: an identity rule for the
        // pinned URL outranks a broader redirect of its scheme.
        let identity_wins = rules(&[
            ("url.file:///srv/evil/.pushinsteadof", "file:///srv/"),
            (
                "url.file:///srv/good.git.pushinsteadof",
                "file:///srv/good.git",
            ),
        ]);
        assert!(!rewrites(&identity_wins, url));
        // A longer redirect outranks a shorter identity rule.
        let redirect_wins = rules(&[
            ("url.file:///srv/.pushinsteadof", "file:///srv/"),
            (
                "url.file:///srv/evil.git.pushinsteadof",
                "file:///srv/good.git",
            ),
        ]);
        assert!(rewrites(&redirect_wins, url));
        // A push rule outranks every fetch rule, even an identity one.
        let push_redirects = rules(&[
            ("url.file:///srv/good.git.insteadof", "file:///srv/good.git"),
            ("url.file:///srv/evil.git.pushinsteadof", "file:///srv/"),
        ]);
        assert!(rewrites(&push_redirects, url));
    }

    #[test]
    fn heads_listing_is_parsed() {
        let out = "abc\trefs/heads/main\ndef\trefs/heads/apb/x\nzzz\trefs/tags/v1\n";
        assert_eq!(
            parse_heads(out),
            [
                ("abc".to_string(), "refs/heads/main".to_string()),
                ("def".to_string(), "refs/heads/apb/x".to_string()),
            ]
        );
    }
}
