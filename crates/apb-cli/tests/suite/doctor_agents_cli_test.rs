//! `apb doctor --agents` and `apb doctor --probe-agent` on a stub agent
//! that answers `--version` and `--help`; no real agent, no model call.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Every flag claude's built-in forms pass.
const FULL_HELP: &str = "Usage: claude [options]\n  -p, --print\n  --output-format <format>\n  --model <model>\n  --append-system-prompt <prompt>\n  --permission-mode <mode>\n  -r, --resume [id]\n  --session-id <uuid>\n";

/// A `claude` stub in `dir` printing `version` and `help`.
fn stub_claude(dir: &Path, version: &str, help: &str) {
    stub_claude_to(dir, version, help, "");
}

/// [`stub_claude`] with the help sent through `redirect` (`>&2` for
/// stderr).
fn stub_claude_to(dir: &Path, version: &str, help: &str, redirect: &str) {
    fs::write(dir.join("help.txt"), help).unwrap();
    let path = dir.join("claude");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo '{version}' ;;\n  --help) cat '{}' {redirect} ;;\nesac\n",
            dir.join("help.txt").display()
        ),
    )
    .unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
}

/// `apb doctor --agents` with only `bin` on PATH and its own config and home.
fn doctor_agents(bin: &Path, home: &Path) -> Command {
    let mut cmd = crate::common::apb();
    cmd.args(["doctor", "--agents"])
        .current_dir(home)
        .env("PATH", bin)
        .env("HOME", home)
        .env("APB_CONFIG_DIR", home.join("config"));
    cmd
}

#[test]
fn a_verified_version_with_every_flag_passes() {
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    stub_claude(bin.path(), "2.1.283 (Claude Code)", FULL_HELP);
    doctor_agents(bin.path(), home.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "[ok]   agent claude form: version 2.1.283 verified on",
        ))
        .stdout(predicate::str::contains(
            "[ok]   agent claude form: every flag of the built-in form is in its help",
        ))
        .stdout(predicate::str::contains(
            "agent claude exposure: tool exposure is checked only on the stream transport",
        ))
        .stdout(predicate::str::contains("[warn]").not());
}

/// A CLI that prints its help on stderr is read all the same, a long help
/// as far as on stdout.
#[test]
fn help_on_stderr_is_read() {
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let long = format!(
        "{}{FULL_HELP}",
        "  an option described at length\n".repeat(400)
    );
    stub_claude_to(bin.path(), "2.1.283 (Claude Code)", &long, ">&2");
    doctor_agents(bin.path(), home.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "[ok]   agent claude form: every flag of the built-in form is in its help",
        ));
}

#[test]
fn a_newer_version_and_a_missing_flag_warn() {
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    stub_claude(
        bin.path(),
        "9.0.0 (Claude Code)",
        &FULL_HELP.replace("  --session-id <uuid>\n", ""),
    );
    doctor_agents(bin.path(), home.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "[warn] agent claude form: version 9.0.0 is newer than the verified",
        ))
        .stdout(predicate::str::contains(
            "[warn] agent claude form: help does not list --session-id",
        ));
}

/// A configured program is the binary a run launches, so it is the one
/// checked, by `--agents` and by the plain doctor alike, whatever else is on
/// PATH.
#[test]
fn the_configured_program_is_the_one_checked() {
    let bin = tempfile::tempdir().unwrap();
    let configured = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    stub_claude(bin.path(), "1.0.0 (Claude Code)", FULL_HELP);
    stub_claude(configured.path(), "2.1.283 (Claude Code)", FULL_HELP);
    let config = home.path().join("config");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.yaml"),
        format!(
            "agents:\n  claude: {{ program: {} }}\n",
            configured.path().join("claude").display()
        ),
    )
    .unwrap();
    doctor_agents(bin.path(), home.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "[ok]   agent claude form: version 2.1.283 verified on",
        ))
        .stdout(predicate::str::contains("1.0.0").not());

    // The plain doctor, for a profile naming the agent.
    let project = home.path().join("project");
    let profile = project.join(".apb/profiles/p");
    fs::create_dir_all(&profile).unwrap();
    fs::write(
        profile.join("profile.yaml"),
        "name: p\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(profile.join("SOUL.md"), "").unwrap();
    let mut cmd = crate::common::apb();
    cmd.arg("doctor")
        .current_dir(&project)
        .env("PATH", bin.path())
        .env("HOME", home.path())
        .env("APB_CONFIG_DIR", &config)
        .assert()
        .stdout(predicate::str::contains(
            "[ok]   agent claude form: version 2.1.283 verified on",
        ));
}

/// The paid probe on a stub: plain text from the built-in form is a drift,
/// exit 1.
#[test]
fn a_drifted_output_format_exits_1() {
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let agent = bin.path().join("claude");
    fs::write(&agent, "#!/bin/sh\necho OK\n").unwrap();
    let mut p = fs::metadata(&agent).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&agent, p).unwrap();
    crate::common::apb()
        .args(["doctor", "--probe-agent", "claude", "--model", "haiku"])
        .current_dir(home.path())
        .env("PATH", bin.path())
        .env("HOME", home.path())
        .env("APB_CONFIG_DIR", home.path().join("config"))
        .env_remove("APB_AGENT_CMD")
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "[fail] agent claude probe: the built-in form printed no machine output",
        ))
        .stderr(predicate::str::contains("paid model call"));
}

/// Refused before anything runs. The test still runs with an empty PATH and
/// its own home and config, so a broken refusal fails on a missing binary
/// instead of reaching a real agent.
#[test]
fn the_paid_probe_is_refused_inside_a_run_step() {
    let empty = tempfile::tempdir().unwrap();
    crate::common::apb()
        .args(["doctor", "--probe-agent", "claude"])
        .current_dir(empty.path())
        .env("PATH", empty.path())
        .env("HOME", empty.path())
        .env("APB_CONFIG_DIR", empty.path().join("config"))
        .env("APB_RUN_ID", "r1")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("refused inside run `r1`"))
        .stderr(predicate::str::contains("paid model call").not());
}
