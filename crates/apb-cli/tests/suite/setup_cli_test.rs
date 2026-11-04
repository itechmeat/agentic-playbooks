//! `apb setup plan|apply|remove`, the `--json` shapes of `detect` and
//! `doctor`, and the `apb init` flags, each in its own temporary HOME and
//! config dir with stub host CLIs on a temporary PATH: never the real
//! `~/.claude`, `~/.codex` or `~/.config`.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

const CODEX_CONFIG: &str =
    "# keep me\nmodel = \"m\" # and me\n\n[mcp_servers.other]\ncommand = \"other\"\nargs = []\n";
const OPENCODE_CONFIG: &str = "{\n  \"theme\": \"dark\",\n  \"mcp\": {\n    \"other\": { \"type\": \"local\", \"command\": [\"o\"] }\n  }\n}\n";

/// A machine: its HOME (holding the config dir) and the PATH of stubs.
struct Machine {
    home: tempfile::TempDir,
    bin: tempfile::TempDir,
}

fn stub(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
}

impl Machine {
    /// A machine with the given host CLIs installed as stubs.
    fn with(hosts: &[&str]) -> Self {
        let m = Machine {
            home: tempfile::tempdir().unwrap(),
            bin: tempfile::tempdir().unwrap(),
        };
        let log = m.home().join("claude.log");
        for h in hosts {
            match *h {
                // Registers into its own user config like the real CLI.
                "claude" => stub(
                    m.bin.path(),
                    "claude",
                    &format!(
                        "case \"$1\" in\n  --version) echo '2.1.283 (Claude Code)' ;;\n  mcp) echo \"$@\" >> '{log}'\n    if [ \"$2\" = add ]; then printf '%s' '{{\"mcpServers\":{{\"agentic-playbooks\":{{\"type\":\"stdio\",\"command\":\"apb\",\"args\":[\"mcp\"]}}}}}}' > \"$HOME/.claude.json\"; else printf '%s' '{{}}' > \"$HOME/.claude.json\"; fi ;;\nesac\n",
                        log = log.display()
                    ),
                ),
                "codex" => stub(
                    m.bin.path(),
                    "codex",
                    "case \"$1\" in\n  --version) echo 'codex-cli 0.157.0' ;;\nesac\n",
                ),
                other => panic!("no stub for {other}"),
            }
        }
        m
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn config(&self) -> PathBuf {
        self.home().join("config")
    }

    /// `apb args` on this machine only: its HOME, config dir and PATH, with
    /// every variable that points at a real host config or marks a run
    /// step removed.
    fn command(&self, args: &[&str]) -> std::process::Command {
        let mut cmd = crate::common::apb_std();
        cmd.args(args)
            .current_dir(self.home())
            .env("HOME", self.home())
            .env("PATH", self.bin.path())
            .env("APB_CONFIG_DIR", self.config())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("APB_RUN_ID")
            .env_remove("APB_HOST_TASK")
            .env("APB_PROBE_TIMEOUT_MS", "5000");
        cmd
    }

    fn apb(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// [`Self::apb`] as a step of a run: `var` (`APB_RUN_ID` or
    /// `APB_HOST_TASK`) set to `value`.
    fn in_run_step(&self, (var, value): (&str, &str), args: &[&str]) -> Output {
        self.command(args).env(var, value).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> (Value, Option<i32>) {
        let out = self.apb(args);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: {e}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (v, out.status.code())
    }

    fn plan(&self) -> Value {
        let (v, code) = self.json(&["setup", "plan", "--json"]);
        assert_eq!(code, Some(0), "{v}");
        v
    }

    fn codex_config(&self) -> PathBuf {
        self.home().join(".codex/config.toml")
    }

    fn opencode_config(&self) -> PathBuf {
        self.home().join(".config/opencode/opencode.json")
    }

    /// Every file under HOME with its bytes.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        walk(self.home(), &mut out);
        out
    }

    /// [`Self::snapshot`] without what every apb command may refresh: the
    /// detection memo (re-probed when a host's config changed) and the
    /// installation key.
    fn settings(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = self.snapshot();
        out.retain(|p, _| {
            !p.ends_with("state/agents-detect.json")
                && !p.ends_with("run-origin.key")
                && !p.ends_with(".setup.flock")
        });
        out
    }
}

fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let meta = fs::symlink_metadata(&p).unwrap();
        if meta.is_dir() {
            walk(&p, out);
        } else if meta.is_file() {
            out.insert(p.clone(), fs::read(&p).unwrap());
        } else {
            out.insert(p.clone(), Vec::new());
        }
    }
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn kinds(plan: &Value) -> Vec<(String, String)> {
    plan["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["kind"].as_str().unwrap().to_string(),
                a["target"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Claude and Codex installed, an OpenCode config in place.
fn full_machine() -> Machine {
    let m = Machine::with(&["claude", "codex"]);
    fs::create_dir_all(m.codex_config().parent().unwrap()).unwrap();
    fs::write(m.codex_config(), CODEX_CONFIG).unwrap();
    fs::create_dir_all(m.opencode_config().parent().unwrap()).unwrap();
    fs::write(m.opencode_config(), OPENCODE_CONFIG).unwrap();
    m
}

#[test]
fn plan_writes_nothing_and_has_a_stable_shape() {
    let m = full_machine();
    // Detection keeps a memo, as `apb detect` does; warm it first.
    assert!(m.apb(&["detect", "--json"]).status.success());
    let before = m.snapshot();
    let plan = m.plan();
    assert_eq!(m.snapshot(), before, "setup plan wrote something");

    assert_eq!(
        keys(&plan),
        [
            "actions",
            "digest",
            "manual",
            "needs_person",
            "observed",
            "ready",
            "schema",
            "suggestions",
            "warnings"
        ]
    );
    assert_eq!(
        keys(&plan["observed"]),
        [
            "agents",
            "hooks",
            "hosts",
            "models_table_as_of",
            "playbooks",
            "role_profiles",
            "sandbox",
            "skills",
            "subscriptions",
            "trackers"
        ]
    );
    for a in plan["actions"].as_array().unwrap() {
        assert_eq!(
            keys(a),
            ["detail", "kind", "params", "reversible", "target", "undo"]
        );
        assert_eq!(a["reversible"], true);
        assert_eq!(a["undo"], "apb setup remove");
    }
    let kinds = kinds(&plan);
    for kind in [
        "install_playbook",
        "install_skill",
        "write_role_profile",
        "register_mcp",
        "register_hook",
    ] {
        assert!(kinds.iter().any(|(k, _)| k == kind), "{kind}: {kinds:?}");
    }
    let hosts: Vec<&str> = kinds
        .iter()
        .filter(|(k, _)| k == "register_mcp")
        .map(|(_, t)| t.as_str())
        .collect();
    assert_eq!(hosts.len(), 3, "{hosts:?}");
    let needs: Vec<&str> = plan["needs_person"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        needs,
        [
            "launch_floor",
            "feedback_loop",
            "notify_target",
            "save_nudge_model",
            "trust_fork"
        ]
    );
    assert_eq!(plan["ready"], true);
    // The same state gives the same digest.
    assert_eq!(m.plan()["digest"], plan["digest"]);
}

/// After an apply the plan still writes nothing, also where a default
/// playbook holds a move a crash interrupted: reading a default's state
/// takes no lock and recovers nothing (a writer does that).
#[test]
fn plan_writes_nothing_after_an_apply_even_with_a_move_to_recover() {
    let m = full_machine();
    let digest = m.plan()["digest"].as_str().unwrap().to_string();
    let (out, code) = m.json(&["setup", "apply", "--plan", &digest, "--json"]);
    assert_eq!(code, Some(0), "{out}");

    let dir = m.config().join("playbooks/apb-research");
    let history = fs::read_to_string(dir.join("current-history.jsonl")).unwrap();
    let line: Value = serde_json::from_str(history.lines().last().unwrap()).unwrap();
    let mut entry = line.clone();
    entry["change_id"] = "interrupted".into();
    let current = fs::read_to_string(dir.join("current")).unwrap();
    let intent = serde_json::json!({
        "change_id": "interrupted",
        "from": null,
        "to": current.trim(),
        "by": line["by"],
        "supersede": null,
        "finish_candidate": null,
        "history_line": entry,
    });
    fs::write(dir.join("current.intent.json"), intent.to_string()).unwrap();
    // A person's edit in place, so the state read goes past the shipped
    // content to the history.
    let file = dir.join(current.trim()).join("playbook.yaml");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, format!("# mine\n{text}")).unwrap();

    assert!(m.apb(&["detect", "--json"]).status.success());
    let before = m.snapshot();
    let plan = m.plan();
    assert_eq!(m.snapshot(), before, "setup plan wrote something");
    assert!(
        kinds(&plan).is_empty(),
        "nothing left to do after the apply: {plan}"
    );
    // A writer still finishes the move.
    let (_, code) = m.json(&[
        "version",
        "history",
        "apb-research",
        "--scope",
        "global",
        "--json",
    ]);
    assert_eq!(code, Some(0));
    assert!(
        !dir.join("current.intent.json").exists(),
        "recovered by a writer"
    );
}

#[test]
fn apply_twice_is_a_no_op_and_remove_takes_back_what_setup_wrote() {
    let m = full_machine();
    let plan = m.plan();
    let digest = plan["digest"].as_str().unwrap();
    let (out, code) = m.json(&["setup", "apply", "--plan", digest, "--json"]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(out["status"], "applied");
    assert!(out["failed"].as_array().unwrap().is_empty(), "{out}");

    // Installed, registered, and other host config content untouched.
    let store = m.config().join("playbooks");
    let shipped: Vec<String> = plan["observed"]["playbooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    for id in &shipped {
        assert!(store.join(id).join("current").is_file(), "{id}");
    }
    assert!(
        m.home()
            .join(".agents/skills/apb-review/SKILL.md")
            .is_file()
    );
    assert!(
        m.config()
            .join("profiles/apb-reviewer/profile.yaml")
            .is_file()
    );
    let codex = fs::read_to_string(m.codex_config()).unwrap();
    assert!(codex.starts_with(CODEX_CONFIG), "{codex}");
    assert!(codex.contains("[mcp_servers.agentic-playbooks]"));
    let opencode = fs::read_to_string(m.opencode_config()).unwrap();
    assert!(opencode.contains("\"theme\": \"dark\""));
    let doc: Value = serde_json::from_str(&opencode).unwrap();
    assert_eq!(doc["mcp"]["agentic-playbooks"]["command"][0], "apb");
    assert!(doc["mcp"]["other"].is_object());
    let log = fs::read_to_string(m.home().join("claude.log")).unwrap();
    assert!(log.contains("mcp add --scope user agentic-playbooks -- apb mcp"));
    // Claude Code had no settings file: setup created one with the hook.
    let hook_settings = m.home().join(".claude/settings.json");
    let doc: Value = serde_json::from_str(&fs::read_to_string(&hook_settings).unwrap()).unwrap();
    assert_eq!(
        doc["hooks"]["Stop"][0]["hooks"][0]["command"],
        "apb hook stop"
    );
    // Each edited host config was backed up as it stood before the edit.
    let mut backups: Vec<String> = fs::read_dir(m.config().join("setup/backups"))
        .unwrap()
        .map(|e| fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    backups.sort();
    let mut originals = [CODEX_CONFIG.to_string(), OPENCODE_CONFIG.to_string()];
    originals.sort();
    assert_eq!(backups, originals);

    // Applying the same plan again changes nothing.
    let before = m.settings();
    let (again, code) = m.json(&["setup", "apply", "--plan", digest, "--json"]);
    assert_eq!(code, Some(0), "{again}");
    assert_eq!(again["status"], "no_op");
    assert_eq!(m.settings(), before);
    assert!(m.plan()["actions"].as_array().unwrap().is_empty());

    // A person changes a skill and forks a default under their own id.
    let skill = m.home().join(".agents/skills/apb-review");
    fs::write(skill.join("NOTES.md"), "mine").unwrap();
    let fork = store.join("my-review");
    fs::create_dir_all(fork.join("1.0.0")).unwrap();
    fs::write(fork.join("1.0.0/playbook.yaml"), "id: my-review\n").unwrap();

    let (removed, code) = m.json(&["setup", "remove", "--json"]);
    assert_eq!(code, Some(0), "{removed}");
    let kept: Vec<&str> = removed["kept"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["target"].as_str().unwrap())
        .collect();
    assert_eq!(kept, ["apb-review"], "{removed}");
    assert!(skill.join("NOTES.md").is_file(), "a changed skill stays");
    assert!(fork.join("1.0.0/playbook.yaml").is_file(), "a fork stays");
    for id in &shipped {
        assert!(!store.join(id).exists(), "{id} left the store");
    }
    let trash = fs::read_dir(m.config().join("trash")).unwrap().count();
    assert_eq!(trash, shipped.len());
    assert!(!m.home().join(".agents/skills/apb-delivery").exists());
    assert!(!m.config().join("profiles/apb-reviewer").exists());
    assert_eq!(fs::read_to_string(m.codex_config()).unwrap(), CODEX_CONFIG);
    assert_eq!(
        fs::read_to_string(m.opencode_config()).unwrap(),
        OPENCODE_CONFIG
    );
    let log = fs::read_to_string(m.home().join("claude.log")).unwrap();
    assert!(log.contains("mcp remove --scope user agentic-playbooks"));
    assert!(
        !hook_settings.exists(),
        "the settings file setup created is gone"
    );
}

/// An apply where one action fails records the others, and remove takes
/// back exactly those.
#[test]
fn remove_after_a_partial_apply_takes_back_what_was_done() {
    let m = full_machine();
    let digest = m.plan()["digest"].as_str().unwrap().to_string();
    // Another install of one skill holds its lock: that action fails.
    let skills = m.home().join(".agents/skills");
    fs::create_dir_all(&skills).unwrap();
    fs::write(skills.join(".apb-review.install.lock"), "held").unwrap();
    let (out, code) = m.json(&["setup", "apply", "--plan", &digest, "--json"]);
    assert_eq!(code, Some(1), "{out}");
    assert_eq!(out["status"], "failed");
    let failed: Vec<&str> = out["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["target"].as_str().unwrap())
        .collect();
    // The skill, and the role profiles that carry it.
    assert_eq!(failed, ["apb-review", "apb-main", "apb-reviewer"], "{out}");
    assert!(skills.join("apb-delivery/SKILL.md").is_file());
    assert!(!skills.join("apb-review").exists());

    let (removed, code) = m.json(&["setup", "remove", "--json"]);
    assert_eq!(code, Some(0), "{removed}");
    assert!(removed["kept"].as_array().unwrap().is_empty(), "{removed}");
    assert!(
        removed["removed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["target"] != "apb-review"),
        "only what was done: {removed}"
    );
    assert!(!skills.join("apb-delivery").exists());
    assert_eq!(fs::read_to_string(m.codex_config()).unwrap(), CODEX_CONFIG);
    let (again, _) = m.json(&["setup", "remove", "--json"]);
    assert!(again["removed"].as_array().unwrap().is_empty(), "{again}");
}

/// A host entry of another command, or a config that does not read, is
/// reported and never written.
#[test]
fn a_different_or_unreadable_host_config_is_left_alone() {
    let different = format!(
        "{CODEX_CONFIG}\n[mcp_servers.agentic-playbooks]\ncommand = \"elsewhere\"\nargs = []\n"
    );
    for (codex, config, code) in [
        (true, different.as_str(), "mcp_entry_differs"),
        (false, "{ \"mcp\": ", "host_config_unreadable"),
    ] {
        let m = Machine::with(&["codex"]);
        let path = if codex {
            m.codex_config()
        } else {
            m.opencode_config()
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, config).unwrap();
        let host = if codex { "codex" } else { "opencode" };
        let plan = m.plan();
        assert!(
            plan["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["code"] == code),
            "{code}: {plan}"
        );
        assert!(
            !kinds(&plan).contains(&("register_mcp".to_string(), host.to_string())),
            "{code}: {plan}"
        );
        let digest = plan["digest"].as_str().unwrap();
        let (out, exit) = m.json(&["setup", "apply", "--plan", digest, "--json"]);
        assert_eq!(exit, Some(0), "{code}: {out}");
        assert_eq!(fs::read_to_string(&path).unwrap(), config, "{code}");
        let (_, exit) = m.json(&["setup", "remove", "--json"]);
        assert_eq!(exit, Some(0));
        assert_eq!(fs::read_to_string(&path).unwrap(), config, "{code}");
    }
}

/// A plan read before a host config changed is refused: exit 2 and nothing
/// written, the new config included.
#[test]
fn a_stale_plan_exits_2_and_writes_nothing() {
    let m = full_machine();
    let digest = m.plan()["digest"].as_str().unwrap().to_string();
    let changed = format!("{CODEX_CONFIG}\n[mcp_servers.third]\ncommand = \"t\"\nargs = []\n");
    fs::write(m.codex_config(), &changed).unwrap();
    assert!(m.apb(&["detect", "--json"]).status.success());
    let before = m.settings();
    let (out, code) = m.json(&["setup", "apply", "--plan", &digest, "--json"]);
    assert_eq!(code, Some(2), "{out}");
    assert_eq!(out["status"], "plan_changed");
    assert_ne!(out["digest"], digest.as_str(), "{out}");
    assert_eq!(m.settings(), before, "nothing written");
    assert!(!m.config().join("playbooks").exists());
    assert_eq!(fs::read_to_string(m.codex_config()).unwrap(), changed);
}

/// Without a terminal the digest is required: on a machine where the plan
/// is ready, only the missing `--plan` refuses, and nothing is written.
#[test]
fn apply_without_a_plan_and_a_terminal_is_refused_with_3() {
    let m = full_machine();
    assert_eq!(m.plan()["ready"], true);
    assert!(m.apb(&["detect", "--json"]).status.success());
    let before = m.settings();
    let out = m.apb(&["setup", "apply"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("--plan <digest> is required without a terminal"),
        "{out:?}"
    );
    assert_eq!(m.settings(), before, "nothing written");
}

#[test]
fn without_a_usable_agent_apply_is_refused_with_3() {
    let m = Machine::with(&[]);
    let plan = m.plan();
    assert_eq!(plan["ready"], false);
    assert_eq!(plan["refusal"]["code"], "no_usable_agent");
    let digest = plan["digest"].as_str().unwrap();
    let (out, code) = m.json(&["setup", "apply", "--plan", digest, "--json"]);
    assert_eq!(code, Some(3), "{out}");
    assert!(!m.config().join("playbooks").exists());
}

#[test]
fn one_agent_reviews_as_the_same_agent_and_doctor_warns() {
    let m = Machine::with(&["claude"]);
    let plan = m.plan();
    assert_eq!(
        plan["observed"]["role_profiles"]["independence"],
        "same_agent"
    );
    assert!(
        plan["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "same_agent_reviewer"),
        "{plan}"
    );
    let digest = plan["digest"].as_str().unwrap();
    let (_, code) = m.json(&["setup", "apply", "--plan", digest, "--json"]);
    assert_eq!(code, Some(0));

    let (doctor, _) = m.json(&["doctor", "--json"]);
    assert_eq!(keys(&doctor), ["checks", "ok"]);
    let checks = doctor["checks"].as_array().unwrap();
    for c in checks {
        assert_eq!(keys(c), ["detail", "status", "subject"]);
    }
    let find = |s: &str| checks.iter().find(|c| c["subject"] == s).cloned();
    assert_eq!(find("reviewer").unwrap()["status"], "warn");
    let defaults = find("defaults").unwrap();
    assert_eq!(defaults["status"], "ok", "{defaults}");
    assert!(
        defaults["detail"]
            .as_str()
            .unwrap()
            .starts_with("installed")
    );
}

#[test]
fn detect_json_has_a_stable_shape() {
    let m = Machine::with(&["claude"]);
    let (v, code) = m.json(&["detect", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(keys(&v), ["agents", "options_by_agent"]);
    let (subs, code) = m.json(&["subscriptions", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(keys(&subs), ["onboarding", "subscriptions"]);
}

#[test]
fn init_without_flags_and_terminal_is_unchanged_and_flags_answer() {
    let m = Machine::with(&[]);
    let project = tempfile::tempdir().unwrap();
    let out = crate::common::apb_std()
        .arg("init")
        .current_dir(project.path())
        .env("HOME", m.home())
        .env("APB_CONFIG_DIR", m.config())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!(
            "initialized {}\n",
            project
                .path()
                .canonicalize()
                .unwrap()
                .join(".apb")
                .display()
        )
    );
    assert!(out.stderr.is_empty());
    assert!(!project.path().join("CLAUDE.md").exists());
    assert!(!project.path().join("AGENTS.md").exists());

    let flagged = tempfile::tempdir().unwrap();
    let out = crate::common::apb_std()
        .args([
            "init",
            "--standing-block",
            "yes",
            "--feedback-loop",
            "no",
            "--json",
        ])
        .current_dir(flagged.path())
        .env("HOME", m.home())
        .env("APB_CONFIG_DIR", m.config())
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        keys(&v),
        [
            "feedback_loop",
            "initialized",
            "standing_block",
            "subscriptions"
        ]
    );
    assert_eq!(v["feedback_loop"]["answer"], "no");
    let claude = fs::read_to_string(flagged.path().join("CLAUDE.md")).unwrap();
    assert!(claude.contains("## apb playbooks"));
    assert!(!claude.contains("## apb feedback loop"));
}

#[test]
fn defaults_are_reconciled_by_the_hidden_command_and_at_mcp_start_only_after_setup() {
    let m = Machine::with(&["codex"]);
    // Before setup: nothing to check, nothing said, nothing written.
    let before = m.settings();
    let out = m.apb(&["__reconcile-defaults"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
    assert_eq!(m.settings(), before);

    let plan = m.plan();
    let digest = plan["digest"].as_str().unwrap();
    let (applied, code) = m.json(&["setup", "apply", "--plan", digest, "--json"]);
    assert_eq!(code, Some(0), "{applied}");

    // Everything is the built-in copy: a reconcile is silent.
    let out = m.apb(&["__reconcile-defaults"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "{out:?}");

    // A person changed a shipped skill: it is kept and the update offered.
    let skill = m.home().join(".agents/skills/apb-review/SKILL.md");
    let text = fs::read_to_string(&skill).unwrap();
    fs::write(&skill, format!("{text}\nmine\n")).unwrap();
    let out = m.apb(&["__reconcile-defaults"]);
    assert_eq!(out.status.code(), Some(0));
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("apb: skill `apb-review`") && said.contains("changed or chosen locally"),
        "{said}"
    );
    assert!(fs::read_to_string(&skill).unwrap().ends_with("mine\n"));

    // The MCP server reports the same at start, on stderr only: its stdout
    // is the protocol.
    let mcp = |args: &[&str]| {
        m.command(args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    // Both servers get as far as the protocol handshake, which a closed
    // stdin ends: the read-only one is checked, not skipped.
    let out = mcp(&["mcp"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("initialize"), "{out:?}");
    assert!(
        err.contains("apb mcp: skill `apb-review`") && err.contains("changed or chosen locally"),
        "{err}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("apb-review"),
        "stdout is the protocol"
    );

    // A read-only server does not reconcile: the same changed skill goes
    // unmentioned, and nothing is written.
    let before = m.settings();
    let out = mcp(&["mcp", "--read-only"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("initialize"), "{out:?}");
    assert!(!err.contains("apb-review"), "{err}");
    assert_eq!(m.settings(), before, "a read-only server wrote nothing");
}

#[test]
fn the_feedback_loop_needs_the_person_at_a_terminal() {
    let m = Machine::with(&[]);
    let project = tempfile::tempdir().unwrap();
    let init = |run: Option<&str>, answer: &str| {
        let mut cmd = crate::common::apb_std();
        cmd.args(["init", "--feedback-loop", answer, "--json"])
            .current_dir(project.path())
            .env("HOME", m.home())
            .env("APB_CONFIG_DIR", m.config());
        if let Some(run) = run {
            cmd.env("APB_RUN_ID", run);
        }
        cmd.output().unwrap()
    };
    // No terminal: refused, and nothing written.
    let out = init(None, "yes");
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["refused"], "feedback_loop_needs_person");
    assert!(v["message"].as_str().unwrap().contains("terminal"), "{v}");
    assert!(!project.path().join("CLAUDE.md").exists());
    assert!(!project.path().join(".apb").exists());
    // A run step: refused, naming the run.
    let out = init(Some("r-step"), "yes");
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["message"].as_str().unwrap().contains("r-step"), "{v}");
    assert!(!project.path().join("AGENTS.md").exists());
    // `no` is anybody's answer.
    let out = init(Some("r-step"), "no");
    assert!(out.status.success(), "{out:?}");
    assert!(!project.path().join("CLAUDE.md").exists());
}

#[test]
fn setup_writes_are_refused_inside_a_run_step_and_the_plan_is_read() {
    let m = full_machine();
    let before = m.settings();
    for step in [("APB_RUN_ID", "r-step"), ("APB_HOST_TASK", "t-step")] {
        // The plan only reads: a step (the apb-setup playbook) may read it.
        let out = m.in_run_step(step, &["setup", "plan", "--json"]);
        assert_eq!(out.status.code(), Some(0), "{step:?}: {out:?}");
        let plan: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(plan["digest"].as_str().is_some(), "{plan}");
        assert_eq!(m.settings(), before, "the plan wrote nothing");
        for args in [
            &[
                "setup",
                "apply",
                "--plan",
                plan["digest"].as_str().unwrap(),
                "--json",
            ][..],
            &["setup", "remove", "--json"][..],
        ] {
            let out = m.in_run_step(step, args);
            assert_eq!(out.status.code(), Some(3), "{step:?} {args:?}: {out:?}");
            let v: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(v["refused"], "run_step", "{v}");
            assert!(v["message"].as_str().unwrap().contains(step.1), "{v}");
        }
    }
    assert_eq!(m.settings(), before, "nothing written");
}

#[test]
fn an_inline_codex_servers_table_becomes_a_manual_step() {
    let m = Machine::with(&["codex"]);
    let inline = "model = \"m\"\nmcp_servers = { other = { command = \"o\" } }\n";
    fs::create_dir_all(m.codex_config().parent().unwrap()).unwrap();
    fs::write(m.codex_config(), inline).unwrap();
    let plan = m.plan();
    assert!(
        !kinds(&plan)
            .iter()
            .any(|(k, t)| k == "register_mcp" && t == "codex"),
        "{plan}"
    );
    let manual = plan["manual"].as_array().unwrap();
    let codex = manual
        .iter()
        .find(|s| s["host"] == "codex")
        .expect("a manual step");
    assert_eq!(keys(codex), ["host", "path", "snippet", "why"]);
    assert!(
        codex["snippet"]
            .as_str()
            .unwrap()
            .starts_with("[mcp_servers.agentic-playbooks]\ncommand = \"apb\"")
    );
    let digest = plan["digest"].as_str().unwrap();
    let (out, code) = m.json(&["setup", "apply", "--plan", digest, "--json"]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(fs::read_to_string(m.codex_config()).unwrap(), inline);
}

#[test]
fn a_symlinked_private_host_config_stays_a_link_with_its_mode() {
    let m = Machine::with(&["codex"]);
    let real = m.home().join("dotfiles/codex.toml");
    fs::create_dir_all(real.parent().unwrap()).unwrap();
    fs::write(&real, CODEX_CONFIG).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir_all(m.codex_config().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, m.codex_config()).unwrap();
    let is_link = || {
        fs::symlink_metadata(m.codex_config())
            .unwrap()
            .file_type()
            .is_symlink()
    };
    let mode = || fs::metadata(&real).unwrap().permissions().mode() & 0o777;

    let digest = m.plan()["digest"].as_str().unwrap().to_string();
    let (out, code) = m.json(&["setup", "apply", "--plan", &digest, "--json"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(is_link(), "the config stays a link");
    assert!(
        fs::read_to_string(&real)
            .unwrap()
            .contains("[mcp_servers.agentic-playbooks]")
    );
    assert_eq!(mode(), 0o600);

    let (out, code) = m.json(&["setup", "remove", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(is_link());
    assert_eq!(fs::read_to_string(&real).unwrap(), CODEX_CONFIG);
    assert_eq!(mode(), 0o600);
}

#[test]
fn a_changed_skill_is_replaced_only_on_request_and_its_copy_is_kept() {
    let m = Machine::with(&["codex"]);
    let digest = m.plan()["digest"].as_str().unwrap().to_string();
    let (out, code) = m.json(&["setup", "apply", "--plan", &digest, "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let skill = m.home().join(".agents/skills/apb-review/SKILL.md");
    let shipped = fs::read_to_string(&skill).unwrap();
    fs::write(&skill, format!("{shipped}\nmine\n")).unwrap();

    // Without the option: a warning that names it.
    let plan = m.plan();
    let warning = plan["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "update_available")
        .expect("update available");
    assert!(
        warning["detail"]
            .as_str()
            .unwrap()
            .contains("apb setup plan --replace-modified apb-review"),
        "{warning}"
    );
    assert!(kinds(&plan).is_empty(), "{plan}");

    // With it: a replace action, applied with the same option.
    let (plan, code) = m.json(&[
        "setup",
        "plan",
        "--replace-modified",
        "apb-review",
        "--json",
    ]);
    assert_eq!(code, Some(0));
    assert_eq!(
        kinds(&plan),
        [(
            "replace_modified_skill".to_string(),
            "apb-review".to_string()
        )]
    );
    assert_eq!(plan["actions"][0]["reversible"], true);
    let digest = plan["digest"].as_str().unwrap();
    // The same digest without the option is another plan.
    let (_, code) = m.json(&["setup", "apply", "--plan", digest, "--json"]);
    assert_eq!(code, Some(2));
    let (out, code) = m.json(&[
        "setup",
        "apply",
        "--plan",
        digest,
        "--replace-modified",
        "apb-review",
        "--json",
    ]);
    assert_eq!(code, Some(0), "{out}");
    let backup = out["done"][0]["backup"]
        .as_str()
        .expect("the backup is named");
    assert!(
        fs::read_to_string(Path::new(backup).join("SKILL.md"))
            .unwrap()
            .ends_with("mine\n")
    );
    assert_eq!(fs::read_to_string(&skill).unwrap(), shipped);
    assert!(
        m.plan()["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["code"] != "update_available")
    );

    // Every changed default at once needs a person at a terminal.
    let (out, code) = m.json(&[
        "setup",
        "apply",
        "--plan",
        digest,
        "--replace-modified",
        "--json",
    ]);
    assert_eq!(code, Some(3), "{out}");
    assert_eq!(out["refused"], "replace_needs_names");

    // Remove gives the person's copy back instead of deleting the skill.
    let (out, code) = m.json(&["setup", "remove", "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let restored: Vec<&str> = out["restored"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["target"].as_str().unwrap())
        .collect();
    assert_eq!(restored, ["apb-review"], "{out}");
    assert!(
        out["removed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["target"] != "apb-review"),
        "{out}"
    );
    assert!(fs::read_to_string(&skill).unwrap().ends_with("mine\n"));
    assert!(!Path::new(backup).exists());
}

#[test]
fn an_update_available_warning_carries_what_the_shipped_content_changed() {
    let m = Machine::with(&["codex"]);
    let digest = m.plan()["digest"].as_str().unwrap().to_string();
    let (out, code) = m.json(&["setup", "apply", "--plan", &digest, "--json"]);
    assert_eq!(code, Some(0), "{out}");
    // A person changes a default skill and a default playbook in place.
    let skill = m.home().join(".agents/skills/apb-review");
    let shipped = fs::read_to_string(skill.join("SKILL.md")).unwrap();
    fs::write(skill.join("SKILL.md"), format!("{shipped}\nmine\n")).unwrap();
    fs::write(skill.join("NOTES.md"), "mine\n").unwrap();
    let dir = m.config().join("playbooks/apb-research");
    let current = fs::read_to_string(dir.join("current")).unwrap();
    let file = dir.join(current.trim()).join("playbook.yaml");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, text.replace("name: Bounded research", "name: Mine")).unwrap();

    let plan = m.plan();
    let update = |name: &str| {
        plan["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| {
                w["code"] == "update_available" && w["detail"].as_str().unwrap().contains(name)
            })
            .unwrap_or_else(|| panic!("no update for {name}: {plan}"))
            .clone()
    };
    let skill_warning = update("apb-review");
    assert_eq!(keys(&skill_warning), ["code", "detail", "diff"]);
    assert_eq!(
        skill_warning["diff"],
        serde_json::json!({
            "base": "installed_copy",
            "added": [],
            "removed": ["NOTES.md"],
            "changed": ["SKILL.md"]
        })
    );
    assert!(
        skill_warning["detail"]
            .as_str()
            .unwrap()
            .ends_with("; differs from the shipped version: files +0 -1 ~1"),
        "{skill_warning}"
    );
    let playbook_warning = update("apb-research");
    let diff = &playbook_warning["diff"];
    // Edited in place: no apb-written version is left, so the diff starts
    // at the person's current and says so.
    assert_eq!(diff["base"], "current", "{diff}");
    assert_eq!(diff["nodes_added"], serde_json::json!([]), "{diff}");
    assert!(
        diff["yaml_diff"]
            .as_str()
            .unwrap()
            .contains("+name: Bounded research"),
        "{diff}"
    );
    assert!(
        playbook_warning["detail"]
            .as_str()
            .unwrap()
            .ends_with("(text only)"),
        "{playbook_warning}"
    );
    // A warning without a diff has no `diff` key.
    assert!(
        plan["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["code"] != "update_available")
            .all(|w| w.get("diff").is_none()),
        "{plan}"
    );

    // The digest is stable across reads and covers the diff.
    assert_eq!(plan["digest"], m.plan()["digest"]);
    fs::write(skill.join("MORE.md"), "more\n").unwrap();
    assert_ne!(
        plan["digest"],
        m.plan()["digest"],
        "the diff is in the digest"
    );

    // The doctor names the summary per changed default.
    let (doctor, _) = m.json(&["doctor", "--json"]);
    let defaults = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["subject"] == "defaults")
        .cloned()
        .unwrap();
    let detail = defaults["detail"].as_str().unwrap();
    assert!(
        detail.contains("apb-review (differs from the shipped version: files +0 -2 ~1)"),
        "{detail}"
    );
    assert!(
        detail.contains("apb-research (differs from the shipped version: "),
        "{detail}"
    );
}

#[test]
fn trackers_and_helpers_are_reported_and_nothing_is_installed() {
    let m = Machine::with(&["codex"]);
    let project = tempfile::tempdir().unwrap();
    fs::create_dir_all(project.path().join(".beads")).unwrap();
    fs::create_dir_all(project.path().join(".git")).unwrap();
    fs::write(
        project.path().join(".git/config"),
        "[remote \"origin\"]\n\turl = https://github.com/o/r.git\n",
    )
    .unwrap();
    let dir = project.path().to_str().unwrap();
    let (plan, code) = m.json(&["setup", "plan", "--project", dir, "--json"]);
    assert_eq!(code, Some(0), "{plan}");
    let trackers: Vec<&str> = plan["observed"]["trackers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["kind"].as_str().unwrap())
        .collect();
    assert_eq!(trackers, ["beads", "github"]);
    let suggestions = plan["suggestions"].as_array().unwrap();
    assert!(!suggestions.is_empty());
    for s in suggestions {
        assert_eq!(
            keys(s),
            ["check", "examples", "found", "required", "role", "why"]
        );
        assert_eq!(s["required"], false, "nothing is installed yet: {s}");
    }
    // A helper installed on this machine is required; the others stay
    // optional suggestions.
    let helper = suggestions[0]["examples"][0].as_str().unwrap().to_string();
    stub(m.bin.path(), &helper, "exit 0\n");
    let (detected, code) = m.json(&["setup", "plan", "--project", dir, "--json"]);
    assert_eq!(code, Some(0), "{detected}");
    for s in detected["suggestions"].as_array().unwrap() {
        let found = !s["found"].as_array().unwrap().is_empty();
        assert_eq!(s["required"], found, "{s}");
    }
    assert_eq!(detected["suggestions"][0]["required"], true, "{detected}");
    // Setup only installs what apb ships and registers itself.
    let known = [
        "install_playbook",
        "install_skill",
        "write_role_profile",
        "register_mcp",
        "write_standing_block",
    ];
    assert!(
        kinds(&plan)
            .iter()
            .all(|(k, _)| known.contains(&k.as_str())),
        "{plan}"
    );
    let bin_before: Vec<_> = fs::read_dir(m.bin.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    let digest = plan["digest"].as_str().unwrap();
    let (out, code) = m.json(&[
        "setup",
        "apply",
        "--plan",
        digest,
        "--project",
        dir,
        "--json",
    ]);
    assert_eq!(code, Some(0), "{out}");
    let bin_after: Vec<_> = fs::read_dir(m.bin.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(bin_before, bin_after, "no program was installed");

    // `bd` on PATH alone is a program on this machine, not this project's
    // tracker.
    stub(m.bin.path(), "bd", "");
    let other = tempfile::tempdir().unwrap();
    let (plan, code) = m.json(&[
        "setup",
        "plan",
        "--project",
        other.path().to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, Some(0), "{plan}");
    let seen: Vec<(&str, &str)> = plan["observed"]["trackers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["kind"].as_str().unwrap(), t["status"].as_str().unwrap()))
        .collect();
    assert_eq!(seen, [("beads", "available")], "{plan}");
    let (with_beads, _) = m.json(&["setup", "plan", "--project", dir, "--json"]);
    assert_eq!(
        with_beads["observed"]["trackers"][0]["status"], "in_project",
        "{with_beads}"
    );
}

/// The end-of-turn save check: one Stop hook group in Claude Code's user
/// settings, every other key kept in place, and taken back byte for byte.
#[test]
fn the_stop_hook_is_registered_beside_other_settings_and_removed_exactly() {
    let m = Machine::with(&["claude"]);
    let settings = m.home().join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let original = "{\n  \"model\": \"x\",\n  \"permissions\": {\"allow\": [\"Bash(ls)\"]},\n  \"hooks\": {\n    \"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": []}]\n  }\n}\n";
    fs::write(&settings, original).unwrap();

    let plan = m.plan();
    assert!(
        kinds(&plan).contains(&("register_hook".to_string(), "claude".to_string())),
        "{plan}"
    );
    assert_eq!(plan["observed"]["hooks"][0]["registration"], "absent");
    let digest = plan["digest"].as_str().unwrap();
    let (out, code) = m.json(&["setup", "apply", "--plan", digest, "--json"]);
    assert_eq!(code, Some(0), "{out}");

    let text = fs::read_to_string(&settings).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["model"], "x");
    assert_eq!(doc["permissions"]["allow"][0], "Bash(ls)");
    assert!(doc["hooks"]["PreToolUse"].is_array());
    assert_eq!(
        doc["hooks"]["Stop"][0]["hooks"][0]["command"],
        "apb hook stop"
    );
    assert!(
        text.starts_with("{\n  \"model\": \"x\",\n  \"permissions\""),
        "{text}"
    );
    assert_eq!(m.plan()["observed"]["hooks"][0]["registration"], "same");

    let (removed, code) = m.json(&["setup", "remove", "--json"]);
    assert_eq!(code, Some(0), "{removed}");
    assert_eq!(fs::read_to_string(&settings).unwrap(), original);
}

/// A role profile an earlier setup wrote keeps its skills, also when this
/// apb gives the role more: the plan names what it lacks instead of leaving
/// a reviewer that its playbooks tell to run an audit skill it lacks.
#[test]
fn a_role_profile_without_the_roles_skills_is_reported() {
    let m = full_machine();
    let plan = m.plan();
    let digest = plan["digest"].as_str().unwrap();
    let (out, code) = m.json(&["setup", "apply", "--plan", digest, "--json"]);
    assert_eq!(code, Some(0), "{out}");
    let codes = |plan: &Value| -> Vec<String> {
        plan["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(!codes(&m.plan()).contains(&"role_profile_missing_skills".to_string()));

    // The reviewer as an earlier apb wrote it: the review skill only.
    let file = m.config().join("profiles/apb-reviewer/profile.yaml");
    let mut doc: yaml_serde::Value =
        yaml_serde::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    let skills = doc["skills"].as_sequence_mut().unwrap();
    skills.retain(|s| yaml_serde::to_string(s).unwrap().contains("apb-review"));
    assert_eq!(skills.len(), 1);
    fs::write(&file, yaml_serde::to_string(&doc).unwrap()).unwrap();

    let plan = m.plan();
    let warning = plan["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "role_profile_missing_skills")
        .unwrap_or_else(|| panic!("{plan}"));
    let detail = warning["detail"].as_str().unwrap();
    assert!(
        detail.contains("apb-reviewer")
            && detail.contains("apb-test-audit")
            && detail.contains("apb-security-audit"),
        "{detail}"
    );
}

/// A helper the checkout or the project ships is never run for its version
/// check, whether its directory is on PATH as an absolute entry or a link
/// elsewhere on PATH leads into the checkout: setup's probe would run the
/// project's code as the operator. One outside the checkout still counts.
#[test]
fn a_helper_inside_the_checkout_is_never_run() {
    let m = Machine::with(&["codex"]);
    let repo = tempfile::tempdir().unwrap();
    let project = repo.path().join("app");
    fs::create_dir_all(repo.path().join(".git")).unwrap();
    for dir in ["bin", "app/bin", "tools"] {
        fs::create_dir_all(repo.path().join(dir)).unwrap();
    }
    let ran = repo.path().join("ran");
    let planted = format!(
        "echo \"$0\" >> '{}'\necho 'code-ranker 5.0.4'\n",
        ran.display()
    );
    // In the checkout root, in the project, and behind a link on PATH.
    stub(&repo.path().join("bin"), "code-ranker", &planted);
    stub(&project.join("bin"), "semgrep", &planted);
    stub(&repo.path().join("tools"), "ast-grep", &planted);
    std::os::unix::fs::symlink(
        repo.path().join("tools/ast-grep"),
        m.bin.path().join("ast-grep"),
    )
    .unwrap();
    let path = std::env::join_paths([
        project.join("bin"),
        repo.path().join("bin"),
        m.bin.path().to_path_buf(),
    ])
    .unwrap();
    let dir = project.to_str().unwrap();
    let out = m
        .command(&["setup", "plan", "--project", dir, "--json"])
        .env("PATH", &path)
        .output()
        .unwrap();
    let plan: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{plan}");
    assert!(
        !ran.exists(),
        "{}",
        fs::read_to_string(&ran).unwrap_or_default()
    );
    let found = |plan: &Value| -> Vec<String> {
        plan["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["found"].as_array().unwrap().clone())
            .map(|f| f.as_str().unwrap().to_string())
            .collect()
    };
    assert!(found(&plan).is_empty(), "{plan}");
    // The same helper installed outside the checkout is found.
    stub(m.bin.path(), "code-ranker", "echo 'code-ranker 5.0.4'\n");
    let out = m
        .command(&["setup", "plan", "--project", dir, "--json"])
        .env("PATH", &path)
        .output()
        .unwrap();
    let plan: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(found(&plan), ["code-ranker"], "{plan}");
    assert!(!ran.exists());
}
