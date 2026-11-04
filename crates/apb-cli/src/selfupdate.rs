//! `apb self-update`: updates an install made by the apb installer, using the
//! install receipt that installer wrote under the XDG config dir. Builds
//! that came from Homebrew or from source have no receipt, so self-update
//! deliberately refuses to guess and instead points the user at the right
//! upgrade path for their install method.
//!
//! After an update the new binary brings the default playbooks and skills
//! setup installed in line with its own embedded content: this process
//! still runs the old build, so it re-executes the updated file as `apb
//! __reconcile-defaults` instead of reconciling with its own old copy.
//! When that cannot start, the next `apb mcp` or dashboard start does it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use axoupdater::{AxoUpdater, AxoupdateError, UpdateRequest, Version};

const NO_RECEIPT_GUIDANCE: &str = "self-update only works for installs made by the apb installer. \
If you installed with Homebrew, run: brew upgrade apb. \
If you built from source, rebuild and reinstall from the repo.";

/// Outcome of a self-update attempt, decoupled from process exit so it can be
/// asserted on directly in tests.
pub(crate) struct Outcome {
    pub(crate) code: u8,
    pub(crate) message: String,
    /// A new build was installed.
    pub(crate) updated: bool,
    /// Printed to stderr before `message`: a part of the check that failed
    /// without ending the command.
    pub(crate) warning: Option<String>,
}

/// Runs `apb self-update`. `check` selects the dry-run path (report only,
/// never installs). Reads the install receipt from the real XDG config dir.
pub(crate) fn run_self_update(check: bool) -> ExitCode {
    let out = self_update_with_config_root(check, None);
    if let Some(warning) = &out.warning {
        eprintln!("warning: {warning}");
    }
    if out.code == 0 {
        println!("{}", out.message);
    } else {
        eprintln!("{}", out.message);
    }
    if out.updated {
        reconcile_in_new_binary();
    }
    ExitCode::from(out.code)
}

/// Runs `apb __reconcile-defaults` in the updated binary. A failure to
/// start it never fails the update.
fn reconcile_in_new_binary() {
    let started = std::env::current_exe().map(updated_binary).and_then(|exe| {
        std::process::Command::new(exe)
            .arg(RECONCILE_COMMAND)
            .status()
    });
    if let Err(e) = started {
        eprintln!(
            "could not check the default playbooks and skills with the new build ({e}); the next apb mcp or dashboard start does it"
        );
    }
}

/// The hidden command the updated binary runs.
const RECONCILE_COMMAND: &str = "__reconcile-defaults";

/// The path of the updated binary, from this process's own path. On Linux
/// the path of a running file that was replaced reads `<path> (deleted)`;
/// the new file sits at `<path>`.
fn updated_binary(exe: PathBuf) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(path) => PathBuf::from(path),
        None => exe,
    }
}

/// `apb __reconcile-defaults`: the reconcile of the defaults setup
/// installed, with its notes on stdout and failures on stderr. Exit 1 when
/// an item could not be updated.
pub(crate) fn reconcile_defaults_cmd() -> ExitCode {
    let notes = apb_mcp::setup::reconcile_if_set_up();
    apb_core::bundled::print_notes("apb: ", &notes, false);
    if notes.iter().any(|n| n.failed) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Testable core of `self-update`. `config_root`, when set, overrides where
/// axoupdater looks for the install receipt (its own config-path override,
/// not a general apb setting) so the no-receipt path can be exercised
/// deterministically against an empty directory instead of the real XDG
/// config dir. axoupdater only exposes this override via an environment
/// variable (`AXOUPDATER_CONFIG_PATH`; see its `receipt::get_config_paths`),
/// so this function sets it for the duration of the call and restores the
/// previous value afterward.
///
/// Safety note: mutating process env is normally unsound under parallel
/// tests, but cargo-nextest (the runner this workspace uses, see
/// `crates/apb-cli/tests/main.rs`) runs each `#[test]` in its own process,
/// so this mutation never races another test. It is still scoped and
/// restored so a plain `cargo test` (single-process, multi-threaded) run
/// stays safe as long as no other test reads `AXOUPDATER_CONFIG_PATH`
/// concurrently; the tests that touch it hold one lock.
pub(crate) fn self_update_with_config_root(check: bool, config_root: Option<&Path>) -> Outcome {
    self_update_as(check, config_root, &running_version())
}

/// The version of this build: the install the receipt describes (axoupdater
/// refuses a receipt written for another executable), and so the release
/// line a pre-release install follows.
fn running_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("cargo checks the package version is semver")
}

/// [`self_update_with_config_root`] for an install of `running`.
fn self_update_as(check: bool, config_root: Option<&Path>, running: &Version) -> Outcome {
    let _env_guard = config_root.map(ConfigRootGuard::set);

    let (mut updater, warning) = match updater_for(running) {
        Ok(found) => found,
        Err(outcome) => return outcome,
    };
    let mut out = run_updater(&mut updater, check);
    out.warning = warning;
    out
}

/// The check or the install through `updater`.
fn run_updater(updater: &mut AxoUpdater, check: bool) -> Outcome {
    if check {
        match updater.is_update_needed_sync() {
            Ok(true) => Outcome {
                code: 10,
                message: "update available".to_string(),
                updated: false,
                warning: None,
            },
            Ok(false) => Outcome {
                code: 0,
                message: "apb is up to date".to_string(),
                updated: false,
                warning: None,
            },
            Err(e) => Outcome {
                code: 2,
                message: format!("self-update check failed: {e}"),
                updated: false,
                warning: None,
            },
        }
    } else {
        match updater.run_sync() {
            Ok(Some(result)) => Outcome {
                code: 0,
                message: format!("updated to {}", result.new_version),
                updated: true,
                warning: None,
            },
            Ok(None) => Outcome {
                code: 0,
                message: "apb is up to date".to_string(),
                updated: false,
                warning: None,
            },
            Err(e) => Outcome {
                code: 2,
                message: format!("self-update failed: {e}"),
                updated: false,
                warning: None,
            },
        }
    }
}

/// An updater loaded from the install receipt, or the outcome that ends
/// the command when there is none.
fn load_updater() -> Result<AxoUpdater, Outcome> {
    let mut updater = AxoUpdater::new_for("apb");
    match updater.load_receipt() {
        Ok(_) => Ok(updater),
        Err(AxoupdateError::NoReceipt { .. }) => Err(Outcome {
            code: 2,
            message: NO_RECEIPT_GUIDANCE.to_string(),
            updated: false,
            warning: None,
        }),
        Err(other) => Err(Outcome {
            code: 2,
            message: format!("self-update failed: {other}"),
            updated: false,
            warning: None,
        }),
    }
}

/// The updater for an install of `running`. A stable install takes the
/// newest stable release only. A pre-release install also takes the newest
/// release of all when it is a pre-release of its own line
/// ([`takes_prerelease`]); when the newest release of all is a pre-release
/// of another line, it takes the newest stable release. axoupdater offers
/// the newest release of all or the newest stable one, not the list, so a
/// newer pre-release of this line published next to a pre-release of a
/// later line is not seen; apb ships one line at a time. A failed look at
/// the pre-releases falls back to the stable path, which reports its own
/// failure, and comes back as the warning that the pre-releases were not
/// checked.
fn updater_for(running: &Version) -> Result<(AxoUpdater, Option<String>), Outcome> {
    let mut stable = load_updater()?;
    stable.configure_version_specifier(UpdateRequest::Latest);
    if running.pre.is_empty() {
        return Ok((stable, None));
    }
    let mut any = load_updater()?;
    any.configure_version_specifier(UpdateRequest::LatestMaybePrerelease);
    let not_checked = |why: String| {
        format!("pre-releases of {running} were not checked ({why}); stable releases only")
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return Ok((stable, Some(not_checked(e.to_string())))),
    };
    // The release found here is kept by the updater, so the check and the
    // install below act on this very release.
    let newest = match runtime.block_on(any.query_new_version()) {
        Ok(newest) => newest.cloned(),
        Err(e) => return Ok((stable, Some(not_checked(e.to_string())))),
    };
    match newest {
        Some(v) if takes_prerelease(running, &v) => Ok((any, None)),
        _ => Ok((stable, None)),
    }
}

/// Whether a pre-release install of `running` takes the release `newest`:
/// a stable release, or a pre-release of the same line (the same
/// major.minor.patch), so a tester of 1.0.0 betas gets the next beta and
/// 1.0.0 itself, never a 1.1.0 alpha.
fn takes_prerelease(running: &Version, newest: &Version) -> bool {
    newest.pre.is_empty()
        || (newest.major, newest.minor, newest.patch)
            == (running.major, running.minor, running.patch)
}

/// Scopes `AXOUPDATER_CONFIG_PATH` to a single call, restoring whatever was
/// there before (including "unset") on drop. Exists only to make the
/// no-receipt path testable; production calls (`config_root: None`) never
/// touch the environment.
struct ConfigRootGuard {
    previous: Option<std::ffi::OsString>,
}

impl ConfigRootGuard {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os("AXOUPDATER_CONFIG_PATH");
        // SAFETY: scoped to this process's self-update invocation; see the
        // doc comment on `self_update_with_config_root` for why this is safe
        // under this workspace's test runner (nextest, one process per test).
        unsafe {
            std::env::set_var("AXOUPDATER_CONFIG_PATH", path);
        }
        Self { previous }
    }
}

impl Drop for ConfigRootGuard {
    fn drop(&mut self) {
        // SAFETY: see `ConfigRootGuard::set`.
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var("AXOUPDATER_CONFIG_PATH", v),
                None => std::env::remove_var("AXOUPDATER_CONFIG_PATH"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_receipt_maps_to_guidance_and_code_2() {
        let _env = env_lock();
        let dir = tempfile::tempdir().unwrap();
        // axoupdater resolves the receipt under XDG config; point it at an
        // empty dir via its own env override so `load_receipt` fails
        // deterministically without touching the real XDG config dir.
        let out = self_update_with_config_root(false, Some(dir.path()));
        assert_eq!(out.code, 2);
        assert!(out.message.contains("brew upgrade") || out.message.contains("source"));
        assert!(!out.updated, "a failed update reconciles nothing");
    }

    /// A loopback stand-in for the GitHub releases API of the receipt's
    /// repository: `/releases/latest` answers the newest stable release, as
    /// GitHub does, and `/releases` lists every release. Serves until the
    /// test process ends.
    fn serve_releases(releases: Vec<(&'static str, bool)>) -> String {
        serve_api(releases, true)
    }

    /// [`serve_releases`] whose `/releases` listing answers 500 unless
    /// `list_ok`.
    fn serve_api(releases: Vec<(&'static str, bool)>, list_ok: bool) -> String {
        use std::io::{Read, Write};
        let release = |tag: &str, prerelease: bool| {
            serde_json::json!({
                "tag_name": tag,
                "name": tag,
                "url": "",
                "assets": [{
                    "url": "",
                    "browser_download_url": "",
                    "name": "apb-installer.sh"
                }],
                "prerelease": prerelease
            })
        };
        let list: Vec<_> = releases.iter().map(|(t, p)| release(t, *p)).collect();
        let latest = releases
            .iter()
            .filter(|(_, p)| !p)
            .map(|(t, _)| release(t, false))
            .next_back()
            .unwrap_or_default();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let line = String::from_utf8_lossy(&head);
                let path = line.split_whitespace().nth(1).unwrap_or_default();
                let (status, body) = if path.contains("/releases/latest") {
                    ("200 OK", latest.to_string())
                } else if list_ok {
                    ("200 OK", serde_json::Value::from(list.clone()).to_string())
                } else {
                    ("500 Internal Server Error", "{}".to_string())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        format!("http://{addr}")
    }

    /// An install receipt for this very executable at `version`, in `dir`.
    fn write_receipt(dir: &Path, version: &str) {
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        let receipt = serde_json::json!({
            "binaries": ["apb"],
            "install_prefix": exe.parent().unwrap(),
            "provider": { "source": "cargo-dist", "version": "0.32.0" },
            "source": {
                "app_name": "apb",
                "name": "agentic-playbooks",
                "owner": "itechmeat",
                "release_type": "github"
            },
            "version": version,
            "modify_path": false
        });
        std::fs::write(dir.join("apb-receipt.json"), receipt.to_string()).unwrap();
    }

    /// The releases the owner saw: a stable line below, and the next line
    /// only as pre-releases.
    fn owner_releases() -> String {
        serve_releases(vec![
            ("v0.30.0", false),
            ("v1.0.0-beta.2", true),
            ("v1.0.0-beta.3", true),
        ])
    }

    /// Serializes the tests that set process environment, for a plain
    /// `cargo test` that runs them as threads of one process.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    const API_VARS: [&str; 3] = [
        "APB_INSTALLER_GITHUB_BASE_URL",
        "APB_INSTALLER_GHE_BASE_URL",
        "NO_PROXY",
    ];

    /// The variables [`use_api`] changed, as they were; put back on drop.
    struct ApiGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl Drop for ApiGuard {
        fn drop(&mut self) {
            // SAFETY: the holder of the guard holds `env_lock`.
            unsafe {
                for (name, value) in &self.0 {
                    match value {
                        Some(v) => std::env::set_var(name, v),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    /// Points axoupdater's GitHub API at `base` until the guard drops.
    #[must_use]
    fn use_api(base: &str) -> ApiGuard {
        let saved = API_VARS
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        // SAFETY: the callers hold `env_lock`.
        unsafe {
            std::env::remove_var("APB_INSTALLER_GITHUB_BASE_URL");
            std::env::set_var("APB_INSTALLER_GHE_BASE_URL", base);
            std::env::set_var("NO_PROXY", "127.0.0.1");
        }
        ApiGuard(saved)
    }

    #[test]
    fn the_api_override_is_put_back() {
        let _env = env_lock();
        let before: Vec<_> = API_VARS.iter().map(std::env::var_os).collect();
        drop(use_api("http://127.0.0.1:9"));
        let after: Vec<_> = API_VARS.iter().map(std::env::var_os).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn a_prerelease_install_sees_a_newer_prerelease_of_its_line() {
        let _env = env_lock();
        let api = owner_releases();
        let _api = use_api(&api);
        let dir = tempfile::tempdir().unwrap();
        write_receipt(dir.path(), "1.0.0-beta.2");
        let out = self_update_as(true, Some(dir.path()), &v("1.0.0-beta.2"));
        assert_eq!(out.message, "update available");
        assert_eq!(out.code, 10);
    }

    #[test]
    fn a_prerelease_install_at_the_newest_of_its_line_is_up_to_date() {
        let _env = env_lock();
        let api = owner_releases();
        let _api = use_api(&api);
        let dir = tempfile::tempdir().unwrap();
        write_receipt(dir.path(), "1.0.0-beta.3");
        let out = self_update_as(true, Some(dir.path()), &v("1.0.0-beta.3"));
        assert_eq!((out.code, out.message.as_str()), (0, "apb is up to date"));
    }

    #[test]
    fn a_prerelease_install_never_takes_a_prerelease_of_a_later_line() {
        let _env = env_lock();
        let api = serve_releases(vec![
            ("v0.30.0", false),
            ("v1.0.0-beta.3", true),
            ("v1.1.0-alpha.1", true),
        ]);
        let _api = use_api(&api);
        let dir = tempfile::tempdir().unwrap();
        write_receipt(dir.path(), "1.0.0-beta.3");
        let out = self_update_as(true, Some(dir.path()), &v("1.0.0-beta.3"));
        assert_eq!((out.code, out.message.as_str()), (0, "apb is up to date"));
    }

    #[test]
    fn a_prerelease_install_takes_a_newer_stable_release() {
        let _env = env_lock();
        let api = serve_releases(vec![
            ("v1.0.0-beta.3", true),
            ("v1.0.0", false),
            ("v1.1.0-alpha.1", true),
        ]);
        let _api = use_api(&api);
        let dir = tempfile::tempdir().unwrap();
        write_receipt(dir.path(), "1.0.0-beta.3");
        let out = self_update_as(true, Some(dir.path()), &v("1.0.0-beta.3"));
        assert_eq!((out.code, out.message.as_str()), (10, "update available"));
    }

    /// The look at the pre-releases fails; the stable path still answers,
    /// and the outcome says the pre-releases were not checked.
    #[test]
    fn a_failed_prerelease_check_is_reported_next_to_the_stable_answer() {
        let _env = env_lock();
        let api = serve_api(vec![("v0.30.0", false), ("v1.0.0-beta.3", true)], false);
        let _api = use_api(&api);
        let dir = tempfile::tempdir().unwrap();
        write_receipt(dir.path(), "1.0.0-beta.2");
        let out = self_update_as(true, Some(dir.path()), &v("1.0.0-beta.2"));
        assert_eq!((out.code, out.message.as_str()), (0, "apb is up to date"));
        let warning = out
            .warning
            .expect("the failed pre-release check is reported");
        assert!(
            warning.contains("pre-releases of 1.0.0-beta.2 were not checked"),
            "{warning}"
        );
    }

    /// A stable install never looks at pre-releases, so nothing is reported.
    #[test]
    fn a_stable_install_keeps_ignoring_prereleases() {
        let _env = env_lock();
        let api = owner_releases();
        let _api = use_api(&api);
        let dir = tempfile::tempdir().unwrap();
        write_receipt(dir.path(), "0.30.0");
        let out = self_update_as(true, Some(dir.path()), &v("0.30.0"));
        assert_eq!((out.code, out.message.as_str()), (0, "apb is up to date"));
    }

    /// The stand-in API is reached at all: a stable install below the
    /// newest stable release sees it.
    #[test]
    fn a_stable_install_sees_a_newer_stable_release() {
        let _env = env_lock();
        let api = owner_releases();
        let _api = use_api(&api);
        let dir = tempfile::tempdir().unwrap();
        write_receipt(dir.path(), "0.29.0");
        let out = self_update_as(true, Some(dir.path()), &v("0.29.0"));
        assert_eq!((out.code, out.message.as_str()), (10, "update available"));
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn a_prerelease_install_takes_its_line_and_stable_releases_only() {
        let running = v("1.0.0-beta.2");
        assert!(takes_prerelease(&running, &v("1.0.0-beta.3")));
        assert!(takes_prerelease(&running, &v("1.0.0-rc.1")));
        assert!(takes_prerelease(&running, &v("1.0.0")));
        assert!(takes_prerelease(&running, &v("1.2.0")));
        assert!(!takes_prerelease(&running, &v("1.1.0-alpha.1")));
        assert!(!takes_prerelease(&running, &v("2.0.0-beta.1")));
    }

    #[test]
    fn the_updated_binary_is_the_replaced_path() {
        assert_eq!(
            updated_binary(PathBuf::from("/opt/bin/apb (deleted)")),
            PathBuf::from("/opt/bin/apb")
        );
        assert_eq!(
            updated_binary(PathBuf::from("/opt/bin/apb")),
            PathBuf::from("/opt/bin/apb")
        );
    }

    #[test]
    fn the_reconcile_command_is_hidden() {
        use clap::CommandFactory;
        let cmd = crate::Cli::command();
        let sub = cmd.find_subcommand(RECONCILE_COMMAND).unwrap();
        assert!(sub.is_hide_set());
    }

    #[test]
    fn check_flag_is_parsed() {
        // covered by clap derive; assert the CLI accepts it and produces the
        // expected variant (Command has no Debug derive, so match instead of
        // asserting equality)
        use clap::Parser;
        let cli = crate::Cli::try_parse_from(["apb", "self-update", "--check"]).unwrap();
        let matched = matches!(
            cli.command,
            Some(crate::Command::SelfUpdate { check: true })
        );
        assert!(matched, "expected SelfUpdate {{ check: true }}");
    }
}
