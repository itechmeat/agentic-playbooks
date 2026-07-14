//! Shared test helpers for the engine (schema 2). Not a separate test binary -
//! included via `mod common;`.
#![allow(dead_code)]

use std::path::Path;

/// Writes `content` to `path` and fsyncs. On Linux, `fs::write` without
/// fsync can cause the following `execve` to fail with `ETXTBSY` when the
/// file is a shebang script executed directly (not via `sh script.sh`),
/// because the page cache may still hold dirty pages after close. Use this
/// for any file that will be exec'd in the same test.
pub fn write_sync(path: &Path, content: &str) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.sync_all().unwrap();
}

/// Seeds a profile to disk: `<root>/.apb/profiles/<name>/{profile.yaml,SOUL.md}`.
/// The agent/model under the stub agent (APB_AGENT_CMD) do not matter - what matters
/// is only that the profile resolves and builds an invocation chain.
pub fn seed_profile(root: &Path, name: &str, agent: &str, model: &str, fallbacks: &[(&str, &str)]) {
    let dir = root.join(".apb/profiles").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut y =
        format!("name: {name}\ndescription: test\nexecutor:\n  agent: {agent}\n  model: {model}\n");
    if !fallbacks.is_empty() {
        y.push_str("  fallbacks:\n");
        for (a, m) in fallbacks {
            y.push_str(&format!("    - {{ agent: {a}, model: {m} }}\n"));
        }
    }
    std::fs::write(dir.join("profile.yaml"), y).unwrap();
    std::fs::write(dir.join("SOUL.md"), "").unwrap();
}

/// Profile `main` under the stub agent (a single executor, no fallbacks).
pub fn seed_main(root: &Path) {
    seed_profile(root, "main", "claude-code", "haiku", &[]);
}
