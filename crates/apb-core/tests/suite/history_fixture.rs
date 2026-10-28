//! Synthetic agent history for the discovery tests: claude session files
//! and codex rollouts with chosen ages, under a temporary directory. Every
//! line is made up here; nothing is copied from a real history.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use apb_core::discover::{
    DiscoverRequest, HistoryAgent, HistoryRoot, MineParams, ProjectRef, ReadLimits,
};
use serde_json::{Value, json};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// A temporary history: a claude root, a codex root and project roots.
pub struct History {
    pub dir: tempfile::TempDir,
    /// The one instant every age is counted from: the start of the current
    /// UTC day (and a second), so a midnight that passes while a test
    /// writes its files never splits one day's files into two days.
    now: SystemTime,
}

impl History {
    pub fn new() -> Self {
        let since_epoch = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let day_start = since_epoch - since_epoch % DAY.as_secs();
        let h = History {
            dir: tempfile::tempdir().unwrap(),
            now: SystemTime::UNIX_EPOCH + Duration::from_secs(day_start + 1),
        };
        std::fs::create_dir_all(h.claude_root()).unwrap();
        std::fs::create_dir_all(h.codex_root()).unwrap();
        h
    }

    pub fn claude_root(&self) -> PathBuf {
        self.base().join("claude/projects")
    }

    pub fn codex_root(&self) -> PathBuf {
        self.base().join("codex/sessions")
    }

    fn base(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    /// A project root named `name`.
    pub fn project(&self, name: &str) -> ProjectRef {
        let root = self.base().join("work").join(name);
        std::fs::create_dir_all(&root).unwrap();
        ProjectRef {
            workspace_id: format!("ws-{name}"),
            name: name.to_string(),
            root,
        }
    }

    /// Writes a claude session of `project` named `session` with `lines`,
    /// last written `age_days` ago.
    pub fn claude(
        &self,
        project: &ProjectRef,
        session: &str,
        lines: &[String],
        age_days: u64,
    ) -> PathBuf {
        let dir = self
            .claude_root()
            .join(apb_core::agent_actions::claude::encode_cwd(
                &project.root.to_string_lossy(),
            ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{session}.jsonl"));
        self.write_aged(&path, &lines.join("\n"), age_days);
        path
    }

    /// Writes a codex rollout named `name` with `lines`, last written
    /// `age_days` ago, under the date directory of that day.
    pub fn codex(&self, name: &str, lines: &[String], age_days: u64) -> PathBuf {
        let at = self.at(age_days);
        let days = at.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() / 86_400;
        let (y, m, d) = civil(days as i64);
        let dir = self.codex_root().join(format!("{y:04}/{m:02}/{d:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-{name}.jsonl"));
        self.write_aged(&path, &lines.join("\n"), age_days);
        path
    }

    /// The instant `age_days` before [`History::now`].
    fn at(&self, age_days: u64) -> SystemTime {
        self.now - DAY * age_days as u32
    }

    /// Writes `text` to `path` and sets its modification time `age_days`
    /// ago.
    pub fn write_aged(&self, path: &Path, text: &str, age_days: u64) {
        std::fs::write(path, text).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(self.at(age_days)).unwrap();
    }

    pub fn roots(&self) -> Vec<HistoryRoot> {
        vec![
            HistoryRoot {
                name: "claude_code".into(),
                agent: HistoryAgent::ClaudeCode,
                path: self.claude_root(),
            },
            HistoryRoot {
                name: "codex".into(),
                agent: HistoryAgent::Codex,
                path: self.codex_root(),
            },
        ]
    }

    pub fn request(&self, projects: &[&ProjectRef]) -> DiscoverRequest {
        DiscoverRequest {
            roots: self.roots(),
            projects: projects.iter().map(|p| (*p).clone()).collect(),
            registered: Vec::new(),
            limits: ReadLimits::default(),
            mine: MineParams::default(),
        }
    }
}

/// The civil date of a day count since the Unix epoch.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// A claude line typed by a person in `cwd`.
pub fn claude_user(cwd: &Path, text: &str) -> String {
    json!({"type": "user", "cwd": cwd, "isSidechain": false,
           "message": {"role": "user", "content": text}})
    .to_string()
}

/// A claude assistant line calling tool `name` with `input` in `cwd`.
pub fn claude_tool(cwd: &Path, name: &str, input: Value) -> String {
    json!({"type": "assistant", "cwd": cwd, "isSidechain": false,
           "message": {"role": "assistant", "content": [
               {"type": "tool_use", "id": "t", "name": name, "input": input}]}})
    .to_string()
}

/// A code routine in `cwd`: a request, then test, edit, test and commit,
/// push.
pub fn code_routine(cwd: &Path, request: &str) -> Vec<String> {
    vec![
        claude_user(cwd, request),
        claude_bash(cwd, "cargo test"),
        claude_tool(cwd, "Edit", json!({"file_path": cwd.join("src/lib.rs")})),
        claude_bash(cwd, "cargo test && git commit -m \"wip\""),
        claude_bash(cwd, "git push"),
    ]
}

/// A claude Bash call.
pub fn claude_bash(cwd: &Path, command: &str) -> String {
    claude_tool(cwd, "Bash", json!({ "command": command }))
}

/// A codex rollout's `session_meta` line naming `cwd`.
pub fn codex_meta(cwd: &Path) -> String {
    json!({"type": "session_meta", "payload": {"id": "s", "cwd": cwd}}).to_string()
}

/// A newer-form codex item line.
pub fn codex_item(item: Value) -> String {
    json!({"type": "event_msg", "payload": {"type": "item_completed", "item": item}}).to_string()
}

/// A newer-form typed request.
pub fn codex_user(text: &str) -> String {
    codex_item(json!({"type": "UserMessage", "content": [{"type": "text", "text": text}]}))
}

/// A newer-form MCP call.
pub fn codex_mcp(server: &str, tool: &str) -> String {
    codex_item(json!({"type": "McpToolCall", "server": server, "tool": tool}))
}

/// The fixture `history/<name>` with `__CWD__` as `cwd`, escaped as a
/// JSON string's content (a `\` or `"` in the path stays valid JSON).
pub fn fixture(name: &str, cwd: &Path) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/history")
        .join(name);
    let quoted = serde_json::to_string(&cwd.to_string_lossy()).unwrap();
    std::fs::read_to_string(path)
        .unwrap()
        .replace("__CWD__", &quoted[1..quoted.len() - 1])
        .lines()
        .map(str::to_string)
        .collect()
}
