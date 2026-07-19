//! Secret resolution for connector accounts (spec 2026-07-18-connectors-design,
//! section 4.2): parsing `{{env.VAR}}` references, reading `KEY=value` dotenv
//! files, and the process-env -> project-dotenv -> global-dotenv resolution
//! chain. Also the `.gitignore` guard that keeps the project `secrets.env`
//! out of version control.
//!
//! Secret values themselves are never logged, cached, or embedded into a
//! prompt (see the crate-level rule in the project CLAUDE.md); this module
//! only reads them from disk/env on demand for the resolving apb process.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Parses a `{{env.VAR}}` secret reference: the whole value must be exactly
/// that placeholder (no surrounding text), with `VAR` matching
/// `[A-Z][A-Z0-9_]*`. Returns the env var name, or `None` if the value is
/// not a valid reference (a literal secret, extra text, lowercase, etc).
pub fn parse_env_ref(value: &str) -> Option<String> {
    let inner = value.strip_prefix("{{env.")?.strip_suffix("}}")?;
    let mut chars = inner.chars();
    let first = chars.next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    for c in chars {
        if !c.is_ascii_uppercase() && !c.is_ascii_digit() && c != '_' {
            return None;
        }
    }
    Some(inner.to_string())
}

/// Parses a `{{cmd:<command line>}}` secret reference: the whole value must
/// be exactly that placeholder (no surrounding text). Returns the command
/// line (the text between `{{cmd:` and the trailing `}}`), or `None` when
/// the value is not a valid reference (a literal, an env reference, empty or
/// whitespace-only inner text, or extra surrounding text). The command line
/// is returned verbatim for `shell_words` parsing at resolution time; this
/// function never executes anything.
pub fn parse_cmd_ref(value: &str) -> Option<String> {
    let inner = value.strip_prefix("{{cmd:")?.strip_suffix("}}")?;
    if inner.trim().is_empty() {
        return None;
    }
    Some(inner.to_string())
}

/// Parses a dotenv file's content into `KEY -> value` pairs. `#` comments
/// (leading whitespace tolerated) and blank lines are skipped; the value is
/// everything after the first `=` on a line, with no quote processing. CRLF
/// line endings are tolerated (`str::lines` already treats `\r\n` as a
/// single line terminator). A dotenv file is user-managed, so a line with no
/// `=` at all is malformed and skipped silently rather than rejected - this
/// module only reads, it never validates or rewrites the file's content.
pub fn parse_dotenv(content: &str) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(idx) = line.find('=') {
            let key = line[..idx].to_string();
            let value = line[idx + 1..].to_string();
            vars.insert(key, value);
        }
        // else: no '=' on the line - malformed, skipped silently by design.
    }
    vars
}

/// Path to the project secrets dotenv: `<root>/.apb/secrets.env`.
pub fn project_secrets_path(root: &Path) -> PathBuf {
    root.join(".apb/secrets.env")
}

/// Path to the global secrets dotenv: `<config_dir>/secrets.env`. `None` in
/// a no-config environment, mirroring `crate::config::config_dir`.
pub fn global_secrets_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|dir| dir.join("secrets.env"))
}

/// Reads and parses a dotenv file. A missing file resolves to an empty map
/// (an absent scope contributes nothing); this is not an error since neither
/// the project nor the global secrets file is required to exist.
fn load_dotenv(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .map(|content| parse_dotenv(&content))
        .unwrap_or_default()
}

/// Resolves one env var name through the chain (spec 4.2): the process
/// environment of the resolving apb process, then the project dotenv
/// `<root>/.apb/secrets.env`, then the global dotenv. The first scope that
/// defines the variable wins; a variable absent from all three is `None`.
pub fn resolve_var(root: &Path, var: &str) -> Option<String> {
    if let Ok(value) = std::env::var(var) {
        return Some(value);
    }
    let project = load_dotenv(&project_secrets_path(root));
    if let Some(value) = project.get(var) {
        return Some(value.clone());
    }
    if let Some(path) = global_secrets_path() {
        let global = load_dotenv(&path);
        if let Some(value) = global.get(var) {
            return Some(value.clone());
        }
    }
    None
}

/// The wall-clock budget for a command-sourced secret (spec 4.1): a helper
/// that hangs must not stall a call indefinitely.
pub const CMD_SECRET_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a command-sourced secret could not be resolved. Carries a trimmed
/// single-line stderr excerpt where the process produced one: credential
/// helpers put diagnostics (not secrets) on stderr, which is what makes an
/// error actionable. The engine maps this to a `config` call error naming
/// the account and field.
#[derive(Debug)]
pub enum CmdSecretError {
    /// The command line did not parse into argv, or was empty.
    Parse(String),
    /// The binary could not be started (not found on PATH, not executable).
    Spawn(String),
    /// The command did not finish within the timeout.
    Timeout,
    /// The command exited non-zero. `code` is `None` when killed by a signal.
    NonZero { code: Option<i32>, stderr: String },
    /// The command succeeded but produced no non-whitespace output.
    Empty { stderr: String },
}

/// Collapses stderr to a single trimmed line and caps its length, so an
/// error message stays one line and cannot dump an unbounded helper log.
fn stderr_excerpt(stderr: &[u8]) -> String {
    const MAX: usize = 200;
    let text = String::from_utf8_lossy(stderr);
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > MAX {
        let mut s: String = one_line.chars().take(MAX).collect();
        s.push_str("...");
        s
    } else {
        one_line
    }
}

/// Spawns `cmd`, retrying briefly on a transient ETXTBSY (errno 26). On
/// Linux, spawning is fork + exec: between the fork and the child's execve,
/// the child holds inherited copies of any write file descriptors another
/// thread has open on some other freshly written executable. If that other
/// thread's own spawn races the execve for the same window, its exec sees the
/// file still "open for writing" and fails with ETXTBSY, even though the
/// executable was already fsynced. The engine's `spawn_in_group`
/// (`crates/apb-engine/src/adapter.rs`) hit the same race and retries the
/// same way; mirror it here rather than inventing a second policy.
fn spawn_retrying_etxtbsy(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    for _ in 0..20 {
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.raw_os_error() == Some(26) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
    cmd.spawn()
}

/// Resolves a command-sourced secret (spec 4.1). The command line is parsed
/// into argv with shell-words rules (quoted arguments supported); NO shell is
/// involved, so pipes, redirection, and substitution are not interpreted. The
/// binary is resolved via `PATH`. Stdout with trailing whitespace trimmed is
/// the secret value. A parse failure, a spawn failure, a non-zero exit, a
/// timeout, or empty stdout is an error. The resolved value lives only in the
/// returned `String`; it is never logged here.
pub fn resolve_cmd(cmdline: &str, timeout: Duration) -> Result<String, CmdSecretError> {
    let argv = shell_words::split(cmdline).map_err(|e| CmdSecretError::Parse(e.to_string()))?;
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| CmdSecretError::Parse("empty command".to_string()))?;

    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_retrying_etxtbsy(&mut cmd)
        .map_err(|e| CmdSecretError::Spawn(format!("{program}: {e}")))?;

    // Drain both pipes on their own threads so a large output cannot deadlock
    // the child against a full pipe while we wait; keep the child handle so
    // the deadline can kill it.
    let mut out_pipe = child.stdout.take().expect("stdout piped");
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let out_join = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_join = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CmdSecretError::Timeout);
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(CmdSecretError::Spawn(e.to_string())),
        }
    };

    let stdout = out_join.join().unwrap_or_default();
    let stderr = err_join.join().unwrap_or_default();
    let excerpt = stderr_excerpt(&stderr);

    if !status.success() {
        return Err(CmdSecretError::NonZero {
            code: status.code(),
            stderr: excerpt,
        });
    }
    let value = String::from_utf8_lossy(&stdout);
    let trimmed = value.trim_end();
    if trimmed.is_empty() {
        return Err(CmdSecretError::Empty { stderr: excerpt });
    }
    Ok(trimmed.to_string())
}

/// The subset of `vars` that `resolve_var` cannot resolve in any scope,
/// in the same order they were given (duplicates preserved - callers that
/// want a deduplicated report can dedupe the input first).
pub fn missing_vars(root: &Path, vars: &[String]) -> Vec<String> {
    vars.iter()
        .filter(|v| resolve_var(root, v).is_none())
        .cloned()
        .collect()
}

/// The exact line `ensure_gitignored` writes and `gitignore_gap` looks for.
const GITIGNORE_LINE: &str = ".apb/secrets.env";

/// Whether a `.gitignore` line (with surrounding whitespace trimmed) already
/// covers the secrets path: an exact match, or the same path with a leading
/// `/` (an equally valid, root-anchored gitignore pattern for the same file).
fn line_covers_secrets(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == GITIGNORE_LINE || trimmed == format!("/{GITIGNORE_LINE}")
}

/// Ensures the project `.gitignore` lists `.apb/secrets.env`: creates the
/// file with that single line if it does not exist, appends the line (with
/// a preceding newline if the file does not already end with one) if it
/// exists but does not cover the path yet, and does nothing if it is
/// already covered (idempotent - never writes a duplicate line).
pub fn ensure_gitignored(root: &Path) -> std::io::Result<()> {
    let path = root.join(".gitignore");
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return std::fs::write(&path, format!("{GITIGNORE_LINE}\n"));
        }
        Err(e) => return Err(e),
    };

    if content.lines().any(line_covers_secrets) {
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
    if content.is_empty() || content.ends_with('\n') {
        writeln!(file, "{GITIGNORE_LINE}")
    } else {
        writeln!(file, "\n{GITIGNORE_LINE}")
    }
}

/// True only when the project secrets file exists but the project
/// `.gitignore` does not cover it: a warning-worthy gap, not a hard error,
/// since a fresh checkout with no secrets file yet has nothing to warn
/// about, and a `.gitignore`-covered file is fine regardless of exact
/// pattern spelling (leading `/` tolerated).
pub fn gitignore_gap(root: &Path) -> bool {
    if !project_secrets_path(root).is_file() {
        return false;
    }
    let covered = std::fs::read_to_string(root.join(".gitignore"))
        .map(|content| content.lines().any(line_covers_secrets))
        .unwrap_or(false);
    !covered
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_env_ref -----------------------------------------------------

    #[test]
    fn parse_env_ref_accepts_valid_reference() {
        assert_eq!(
            parse_env_ref("{{env.JIRA_TOKEN}}"),
            Some("JIRA_TOKEN".to_string())
        );
        assert_eq!(parse_env_ref("{{env.A}}"), Some("A".to_string()));
    }

    #[test]
    fn parse_env_ref_rejects_malformed_values() {
        assert_eq!(parse_env_ref("literal"), None);
        assert_eq!(parse_env_ref("{{env.lower}}"), None);
        assert_eq!(parse_env_ref("prefix {{env.A}}"), None);
        assert_eq!(parse_env_ref("{{env.A}} suffix"), None);
        assert_eq!(parse_env_ref("{{env.}}"), None);
        assert_eq!(parse_env_ref("{{secret.a}}"), None);
    }

    // --- parse_cmd_ref -----------------------------------------------------

    #[test]
    fn parse_cmd_ref_accepts_valid_reference() {
        assert_eq!(
            parse_cmd_ref("{{cmd:gh auth token}}"),
            Some("gh auth token".to_string())
        );
        assert_eq!(
            parse_cmd_ref("{{cmd:op read \"op://vault/item/field\"}}"),
            Some("op read \"op://vault/item/field\"".to_string())
        );
    }

    #[test]
    fn parse_cmd_ref_rejects_malformed_values() {
        assert_eq!(parse_cmd_ref("literal"), None);
        assert_eq!(parse_cmd_ref("{{env.TOKEN}}"), None);
        assert_eq!(parse_cmd_ref("prefix {{cmd:gh}}"), None);
        assert_eq!(parse_cmd_ref("{{cmd:gh}} suffix"), None);
        assert_eq!(parse_cmd_ref("{{cmd:}}"), None);
        assert_eq!(parse_cmd_ref("{{cmd:   }}"), None);
    }

    #[test]
    fn parse_env_ref_and_cmd_ref_are_mutually_exclusive() {
        assert!(parse_env_ref("{{cmd:gh}}").is_none());
        assert!(parse_cmd_ref("{{env.T}}").is_none());
    }

    // --- parse_dotenv --------------------------------------------------

    #[test]
    fn parse_dotenv_skips_comments_and_blank_lines() {
        let content = "\n# a comment\nFOO=bar\n   # indented comment\n\nBAZ=qux\n";
        let vars = parse_dotenv(content);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars["FOO"], "bar");
        assert_eq!(vars["BAZ"], "qux");
    }

    #[test]
    fn parse_dotenv_splits_on_first_equals_only() {
        let vars = parse_dotenv("URL=https://a.example?x=1&y=2\n");
        assert_eq!(vars["URL"], "https://a.example?x=1&y=2");
    }

    #[test]
    fn parse_dotenv_tolerates_crlf() {
        let vars = parse_dotenv("FOO=bar\r\nBAZ=qux\r\n");
        assert_eq!(vars.len(), 2);
        assert_eq!(vars["FOO"], "bar");
        assert_eq!(vars["BAZ"], "qux");
    }

    #[test]
    fn parse_dotenv_skips_malformed_lines_silently() {
        let vars = parse_dotenv("not-a-line\nFOO=bar\n");
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["FOO"], "bar");
    }

    // --- resolve_var --------------------------------------------------

    struct EnvGuard {
        var: &'static str,
        prior: Option<std::ffi::OsString>,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var(self.var, v),
                    None => std::env::remove_var(self.var),
                }
            }
        }
    }

    struct ConfigDirGuard {
        prior: Option<std::ffi::OsString>,
    }
    impl Drop for ConfigDirGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                    None => std::env::remove_var("APB_CONFIG_DIR"),
                }
            }
        }
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn resolve_var_process_env_beats_project_and_global_files() {
        let _lock = crate::env_test_lock();
        const VAR: &str = "APB_TEST_SECRET_R7Q3K";

        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _cfg_guard = ConfigDirGuard {
            prior: std::env::var_os("APB_CONFIG_DIR"),
        };
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }

        write(
            &project_secrets_path(root.path()),
            &format!("{VAR}=project-value\n"),
        );
        write(
            &global_secrets_path().unwrap(),
            &format!("{VAR}=global-value\n"),
        );

        // Global-only: neither process env nor project file defines it yet.
        let _env_guard = EnvGuard {
            var: VAR,
            prior: std::env::var_os(VAR),
        };
        unsafe {
            std::env::remove_var(VAR);
        }
        std::fs::remove_file(project_secrets_path(root.path())).unwrap();
        assert_eq!(
            resolve_var(root.path(), VAR),
            Some("global-value".to_string())
        );

        // Project beats global once the project file defines it too.
        write(
            &project_secrets_path(root.path()),
            &format!("{VAR}=project-value\n"),
        );
        assert_eq!(
            resolve_var(root.path(), VAR),
            Some("project-value".to_string())
        );

        // Process env beats both files.
        unsafe {
            std::env::set_var(VAR, "process-value");
        }
        assert_eq!(
            resolve_var(root.path(), VAR),
            Some("process-value".to_string())
        );
    }

    #[test]
    fn resolve_var_none_when_unresolved_anywhere() {
        let _lock = crate::env_test_lock();
        const VAR: &str = "APB_TEST_SECRET_UNSET_M9X";

        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _cfg_guard = ConfigDirGuard {
            prior: std::env::var_os("APB_CONFIG_DIR"),
        };
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _env_guard = EnvGuard {
            var: VAR,
            prior: std::env::var_os(VAR),
        };
        unsafe {
            std::env::remove_var(VAR);
        }

        assert_eq!(resolve_var(root.path(), VAR), None);
    }

    #[test]
    fn global_secrets_path_none_without_config_dir() {
        let _lock = crate::env_test_lock();

        struct FullEnvGuard {
            config_dir: Option<std::ffi::OsString>,
            xdg_config_home: Option<std::ffi::OsString>,
            home: Option<std::ffi::OsString>,
        }
        impl Drop for FullEnvGuard {
            fn drop(&mut self) {
                unsafe {
                    match &self.config_dir {
                        Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                        None => std::env::remove_var("APB_CONFIG_DIR"),
                    }
                    match &self.xdg_config_home {
                        Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                        None => std::env::remove_var("XDG_CONFIG_HOME"),
                    }
                    match &self.home {
                        Some(v) => std::env::set_var("HOME", v),
                        None => std::env::remove_var("HOME"),
                    }
                }
            }
        }
        let _g = FullEnvGuard {
            config_dir: std::env::var_os("APB_CONFIG_DIR"),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            home: std::env::var_os("HOME"),
        };

        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("HOME");
        }
        assert!(global_secrets_path().is_none());
    }

    // --- missing_vars --------------------------------------------------

    #[test]
    fn missing_vars_reports_only_unresolved_names() {
        let _lock = crate::env_test_lock();
        const VAR_PRESENT: &str = "APB_TEST_SECRET_PRESENT_K2";
        const VAR_MISSING: &str = "APB_TEST_SECRET_MISSING_K2";

        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _cfg_guard = ConfigDirGuard {
            prior: std::env::var_os("APB_CONFIG_DIR"),
        };
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _env_guard = EnvGuard {
            var: VAR_PRESENT,
            prior: std::env::var_os(VAR_PRESENT),
        };
        let _env_guard2 = EnvGuard {
            var: VAR_MISSING,
            prior: std::env::var_os(VAR_MISSING),
        };
        unsafe {
            std::env::set_var(VAR_PRESENT, "value");
            std::env::remove_var(VAR_MISSING);
        }

        let vars = vec![VAR_PRESENT.to_string(), VAR_MISSING.to_string()];
        assert_eq!(missing_vars(root.path(), &vars), vec![VAR_MISSING]);
    }

    #[test]
    fn missing_vars_empty_when_all_resolve() {
        let _lock = crate::env_test_lock();
        const VAR: &str = "APB_TEST_SECRET_ALL_OK_P4";

        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _cfg_guard = ConfigDirGuard {
            prior: std::env::var_os("APB_CONFIG_DIR"),
        };
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        let _env_guard = EnvGuard {
            var: VAR,
            prior: std::env::var_os(VAR),
        };
        unsafe {
            std::env::set_var(VAR, "value");
        }

        assert!(missing_vars(root.path(), &[VAR.to_string()]).is_empty());
    }

    // --- ensure_gitignored ----------------------------------------------

    #[test]
    fn ensure_gitignored_creates_missing_file() {
        let root = tempfile::tempdir().unwrap();
        ensure_gitignored(root.path()).unwrap();
        let content = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert_eq!(content, ".apb/secrets.env\n");
    }

    #[test]
    fn ensure_gitignored_appends_with_newline_when_missing() {
        let root = tempfile::tempdir().unwrap();
        write(&root.path().join(".gitignore"), "node_modules");
        ensure_gitignored(root.path()).unwrap();
        let content = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert_eq!(content, "node_modules\n.apb/secrets.env\n");
    }

    #[test]
    fn ensure_gitignored_appends_without_extra_blank_line_when_file_ends_in_newline() {
        let root = tempfile::tempdir().unwrap();
        write(&root.path().join(".gitignore"), "node_modules\n");
        ensure_gitignored(root.path()).unwrap();
        let content = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert_eq!(content, "node_modules\n.apb/secrets.env\n");
    }

    #[test]
    fn ensure_gitignored_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        ensure_gitignored(root.path()).unwrap();
        ensure_gitignored(root.path()).unwrap();
        let content = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert_eq!(content, ".apb/secrets.env\n");
    }

    #[test]
    fn ensure_gitignored_noop_when_leading_slash_variant_present() {
        let root = tempfile::tempdir().unwrap();
        write(&root.path().join(".gitignore"), "/.apb/secrets.env\n");
        ensure_gitignored(root.path()).unwrap();
        let content = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert_eq!(content, "/.apb/secrets.env\n");
    }

    // --- gitignore_gap ----------------------------------------------------

    #[test]
    fn gitignore_gap_false_when_secrets_file_absent() {
        let root = tempfile::tempdir().unwrap();
        assert!(!gitignore_gap(root.path()));
    }

    #[test]
    fn gitignore_gap_true_when_secrets_exists_and_not_covered() {
        let root = tempfile::tempdir().unwrap();
        write(&project_secrets_path(root.path()), "FOO=bar\n");
        assert!(gitignore_gap(root.path()));
    }

    #[test]
    fn gitignore_gap_false_when_covered_exactly() {
        let root = tempfile::tempdir().unwrap();
        write(&project_secrets_path(root.path()), "FOO=bar\n");
        write(&root.path().join(".gitignore"), ".apb/secrets.env\n");
        assert!(!gitignore_gap(root.path()));
    }

    #[test]
    fn gitignore_gap_false_when_covered_by_leading_slash_variant() {
        let root = tempfile::tempdir().unwrap();
        write(&project_secrets_path(root.path()), "FOO=bar\n");
        write(&root.path().join(".gitignore"), "/.apb/secrets.env\n");
        assert!(!gitignore_gap(root.path()));
    }

    // --- resolve_cmd (unix stub executable) --------------------------------

    #[cfg(unix)]
    fn write_stub(dir: &Path, name: &str, script: &str) -> PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        // create + write_all + sync_all before exec: without the sync, an
        // immediate execve of the freshly written file can fail with ETXTBSY
        // on Linux (see engine tests/suite/common write_sync, PR #10).
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        f.sync_all().unwrap();
        drop(f);
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn resolve_cmd_returns_trimmed_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub(
            dir.path(),
            "ok-secret",
            "#!/bin/sh\nprintf 'resolved-token-value\\n\\n'\n",
        );
        let out = resolve_cmd(
            &format!("{} arg1", stub.to_string_lossy()),
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(out, "resolved-token-value");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_cmd_nonzero_exit_is_error_with_stderr_excerpt() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub(
            dir.path(),
            "fail-secret",
            "#!/bin/sh\necho 'gh: not logged in' >&2\nexit 3\n",
        );
        match resolve_cmd(&stub.to_string_lossy(), std::time::Duration::from_secs(10)) {
            Err(CmdSecretError::NonZero { code, stderr }) => {
                assert_eq!(code, Some(3));
                assert!(stderr.contains("not logged in"), "stderr excerpt: {stderr}");
            }
            other => panic!("expected NonZero, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_cmd_empty_output_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub(dir.path(), "empty-secret", "#!/bin/sh\nexit 0\n");
        assert!(matches!(
            resolve_cmd(&stub.to_string_lossy(), std::time::Duration::from_secs(10)),
            Err(CmdSecretError::Empty { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_cmd_times_out_and_kills_child() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub(dir.path(), "slow-secret", "#!/bin/sh\nsleep 30\n");
        let started = std::time::Instant::now();
        assert!(matches!(
            resolve_cmd(
                &stub.to_string_lossy(),
                std::time::Duration::from_millis(300)
            ),
            Err(CmdSecretError::Timeout)
        ));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "timeout did not preempt sleep"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_cmd_missing_binary_is_spawn_error() {
        assert!(matches!(
            resolve_cmd("apb-no-such-binary-zzz", std::time::Duration::from_secs(10)),
            Err(CmdSecretError::Spawn(_))
        ));
    }

    #[test]
    fn resolve_cmd_empty_command_is_parse_error() {
        assert!(matches!(
            resolve_cmd("   ", std::time::Duration::from_secs(10)),
            Err(CmdSecretError::Parse(_))
        ));
    }

    #[test]
    fn resolve_cmd_unbalanced_quotes_is_parse_error() {
        assert!(matches!(
            resolve_cmd("gh \"unterminated", std::time::Duration::from_secs(10)),
            Err(CmdSecretError::Parse(_))
        ));
    }
}
