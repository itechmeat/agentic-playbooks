//! The contract fixtures (`tests/fixtures/agents/<id>.yaml`) and the stub
//! agent each scenario runs: a script that records its argv, prints the
//! scenario's stdout and stderr and exits with its code.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use apb_core::agent_output::AgentUsage;
use apb_core::config::InvocationDef;
use apb_engine::adapter::{AgentAdapter, AgentFailure, AgentReport, AgentTask, ClaudeAdapter};
use serde::Deserialize;

use crate::common;

/// Where the fixtures live.
pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/agents");

/// The prompt every scenario sends: dash-led, so a form that let it reach the
/// CLI as an option would show in the recorded argv.
pub const PROMPT: &str = "--check the build";

/// One agent's fixture.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub agent: String,
    /// `captured` (replays recorded CLI output) or `synthetic`.
    pub provenance: Provenance,
    /// The CLI version the captures were recorded with; required for a
    /// captured fixture.
    #[serde(default)]
    pub checked_against: Option<String>,
    pub scenarios: BTreeMap<String, Scenario>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Captured,
    Synthetic,
}

/// What the stub prints, and what the scenario expects of it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Overrides the fixture's provenance for this scenario.
    #[serde(default)]
    pub provenance: Option<Provenance>,
    #[serde(default)]
    pub stdout: Option<String>,
    /// Relative to [`FIXTURES`].
    #[serde(default)]
    pub stdout_file: Option<String>,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub stderr_file: Option<String>,
    #[serde(default)]
    pub exit: i32,
    #[serde(default)]
    pub expect: Expect,
}

/// The expectations a scenario may state; each scenario reads its own.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub final_record: Option<bool>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub model_turn: Option<bool>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub retry_after_secs: Option<u64>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub fresh: Option<String>,
    #[serde(default)]
    pub continues: Option<bool>,
    #[serde(default)]
    pub missing: Option<String>,
    #[serde(default)]
    pub usage: Option<AgentUsage>,
    #[serde(default)]
    pub status: Option<String>,
}

/// The ids of every fixture on disk, sorted.
pub fn fixture_ids() -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(FIXTURES)
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            name.strip_suffix(".yaml").map(str::to_string)
        })
        .collect();
    ids.sort();
    ids
}

pub fn load(agent: &str) -> Fixture {
    let path = Path::new(FIXTURES).join(format!("{agent}.yaml"));
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let fixture: Fixture =
        yaml_serde::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    assert_eq!(
        fixture.agent,
        agent,
        "{} names another agent",
        path.display()
    );
    fixture
}

impl Fixture {
    pub fn scenario(&self, name: &str) -> &Scenario {
        self.scenarios
            .get(name)
            .unwrap_or_else(|| panic!("{}: no `{name}` scenario", self.agent))
    }
}

impl Scenario {
    fn text(inline: &Option<String>, file: &Option<String>) -> String {
        match (inline, file) {
            (Some(s), _) => s.clone(),
            (None, Some(f)) => fs::read_to_string(Path::new(FIXTURES).join(f))
                .unwrap_or_else(|e| panic!("read {f}: {e}")),
            (None, None) => String::new(),
        }
    }

    pub fn stdout(&self) -> String {
        Self::text(&self.stdout, &self.stdout_file)
    }

    pub fn stderr(&self) -> String {
        Self::text(&self.stderr, &self.stderr_file)
    }
}

/// A stub agent in its own directory: records its argv (each element followed
/// by `\x01`) in `argv.txt`, prints `stdout` and `stderr` from files and exits
/// with `exit`.
pub struct Stub {
    pub dir: tempfile::TempDir,
    pub program: String,
}

impl Stub {
    pub fn new(stdout: &str, stderr: &str, exit: i32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("stdout");
        let err = dir.path().join("stderr");
        fs::write(&out, stdout).unwrap();
        fs::write(&err, stderr).unwrap();
        let argv = dir.path().join("argv.txt");
        let path = dir.path().join("agent");
        common::write_sync(
            &path,
            &format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\001' \"$a\" >> '{}'; done\ncat '{}'\ncat '{}' >&2\nexit {exit}\n",
                argv.display(),
                out.display(),
                err.display()
            ),
        );
        let mut perm = fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&path, perm).unwrap();
        Stub {
            program: path.to_string_lossy().into_owned(),
            dir,
        }
    }

    pub fn for_scenario(s: &Scenario) -> Self {
        Self::new(&s.stdout(), &s.stderr(), s.exit)
    }

    /// The argv the stub was started with.
    pub fn argv(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("argv.txt"))
            .unwrap_or_default()
            .split('\u{1}')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn workdir(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    /// Runs `agent` through the real headless adapter with `spec`. HOME
    /// points at the stub's own directory and no zcode provider config is
    /// named, so an agent's spawn setup (zcode's) never reads the
    /// developer's own files.
    pub fn run(&self, agent: &str, spec: InvocationDef) -> Result<AgentReport, AgentFailure> {
        let _env = common::env_lock();
        let _home = [
            common::EnvVar::set("HOME", self.dir.path()),
            common::EnvVar::remove(apb_core::zcode::BUILTIN_CONFIG_ENV),
            common::EnvVar::remove(apb_core::zcode::PERSONAL_CONFIG_ENV),
        ];
        let adapter = ClaudeAdapter {
            program: self.program.clone(),
            spec,
        };
        let workdir = self.workdir();
        let env = Default::default();
        adapter.run(&AgentTask {
            prompt: PROMPT,
            // zcode takes its model through a provider config, which needs a
            // ZCode login; an empty model keeps the user's own selection.
            model: if agent == "zcode" { "" } else { "m-1" },
            workdir: &workdir,
            timeout: None,
            stream_log: None,
            soul: None,
            grant_autonomy: true,
            spawn_env: &env,
            interactive: false,
            report_contract: true,
            node: "contract",
            agent,
            extract: None,
            status_file: None,
            hermetic_settings: None,
            transcript_dir: None,
            output_limits: Default::default(),
        })
    }
}
