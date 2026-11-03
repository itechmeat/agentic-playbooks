//! Billed use at the CLI (GUARDRAILS.md "Billed use"): agent work whose
//! predicted basis is billed or unknown starts only on a person's explicit
//! command under the default `ask`, `deny` refuses everyone, a project only
//! narrows the machine's settings, a run step never confirms, and a run
//! whose login switches to billed mid-run pauses until a person confirms.
//!
//! The agent is codex, whose basis its module reads from the top-level keys
//! of `auth.json` in `$CODEX_HOME`; a stub stands in for the binary, so no
//! model is called. Every command runs with the real defaults (the suites'
//! test default is cleared).

use std::fs;
use std::path::Path;

const STUB_REPLY: &str = r#"{"type":"thread.started","thread_id":"t-1"}
{"type":"item.completed","item":{"type":"agent_message","text":"done\n\n```yaml\nstatus: success\nsummary: done\n```"}}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#;

/// `start -> [flip] -> work -> done`, `work` on the codex profile `worker`;
/// `flip` (when `flip_to_key`) turns codex's login into an api key login.
fn playbook(flip_to_key: bool) -> String {
    let (flip_node, first) = if flip_to_key {
        (
            "  - { id: flip, type: script, script: \"scripts/flip.sh\", runner: sh }\n",
            "flip",
        )
    } else {
        ("", "work")
    };
    let flip_edge = if flip_to_key {
        "  - { from: flip, to: work }\n"
    } else {
        ""
    };
    format!(
        "schema: 2\nid: pb\nname: pb\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n{flip_node}  - {{ id: work, type: agent_task, profile: worker, prompt: go }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: {first} }}\n{flip_edge}  - {{ from: work, to: done }}\n"
    )
}

struct Project {
    dir: tempfile::TempDir,
    cfg: tempfile::TempDir,
    codex: tempfile::TempDir,
}

impl Project {
    fn new(flip_to_key: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let codex = tempfile::tempdir().unwrap();
        let root = dir.path();
        apb_core::registry::init_project(root).unwrap();
        let v = root.join(".apb/playbooks/pb/1.0.0");
        fs::create_dir_all(v.join("scripts")).unwrap();
        fs::write(v.join("playbook.yaml"), playbook(flip_to_key)).unwrap();
        fs::write(
            v.join("scripts/flip.sh"),
            "printf '{\"OPENAI_API_KEY\":\"k\"}' > \"$CODEX_HOME/auth.json\"\n",
        )
        .unwrap();
        fs::write(root.join(".apb/playbooks/pb/current"), "1.0.0").unwrap();
        let prof = root.join(".apb/profiles/worker");
        fs::create_dir_all(&prof).unwrap();
        fs::write(
            prof.join("profile.yaml"),
            "name: worker\nexecutor:\n  agent: codex\n  model: gpt-6-sol\n",
        )
        .unwrap();
        fs::write(prof.join("SOUL.md"), "").unwrap();
        fs::write(root.join("reply.jsonl"), STUB_REPLY).unwrap();
        let stub = root.join("agent.sh");
        fs::write(
            &stub,
            format!(
                "#!/bin/sh\n[ -n \"$APB_STATUS_FILE\" ] && printf '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"\ncat '{}'\n",
                root.join("reply.jsonl").display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Project { dir, cfg, codex }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The login codex finds: `api`, `account`, or none.
    fn login(&self, kind: Option<&str>) {
        let auth = self.codex.path().join("auth.json");
        match kind {
            Some("api") => fs::write(auth, r#"{"OPENAI_API_KEY":"k","tokens":null}"#).unwrap(),
            Some(_) => fs::write(auth, r#"{"OPENAI_API_KEY":null,"tokens":{"a":1}}"#).unwrap(),
            None => {
                let _ = fs::remove_file(auth);
            }
        }
    }

    fn machine(&self, yaml: &str) {
        fs::write(self.cfg.path().join("policy.yaml"), yaml).unwrap();
    }

    fn project(&self, yaml: &str) {
        fs::write(self.root().join(".apb/policy.yaml"), yaml).unwrap();
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("APB_CONFIG_DIR", self.cfg.path().display().to_string()),
            ("CODEX_HOME", self.codex.path().display().to_string()),
            (
                "APB_AGENT_CMD",
                self.root().join("agent.sh").display().to_string(),
            ),
        ]
    }

    fn apb(&self) -> assert_cmd::Command {
        let mut c = crate::common::apb();
        c.current_dir(self.root())
            .env_remove("APB_TEST_BILLING_DEFAULT")
            .env_remove("APB_RUN_ID")
            .env_remove("APB_HOST_TASK");
        for v in ["CODEX_API_KEY", "OPENAI_API_KEY", "OPENAI_BASE_URL"] {
            c.env_remove(v);
        }
        for (k, v) in self.env() {
            c.env(k, v);
        }
        c
    }

    fn runs(&self) -> Vec<String> {
        fs::read_dir(self.root().join(".apb/runs"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The only run's id.
    fn run_id(&self) -> String {
        let runs = self.runs();
        assert_eq!(runs.len(), 1, "{runs:?}");
        runs[0].clone()
    }

    /// The billing record of the run's start.
    fn billing(&self, run_id: &str) -> serde_json::Value {
        let m = fs::read_to_string(
            self.root()
                .join(".apb/runs")
                .join(run_id)
                .join("manifest.yaml"),
        )
        .unwrap();
        let doc: serde_json::Value = yaml_serde::from_str(&m).unwrap();
        doc["launch"]["billing"].clone()
    }

    fn journal(&self, run_id: &str) -> Vec<serde_json::Value> {
        fs::read_to_string(
            self.root()
                .join(".apb/runs")
                .join(run_id)
                .join("events.jsonl"),
        )
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
    }
}

fn stderr_of(a: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&a.get_output().stderr).into_owned()
}

/// The `--confirm-launch=...` flag a refusal printed, and the version and
/// choice that follow it when the refusal names them.
fn printed_flags(stderr: &str) -> Vec<String> {
    let at = stderr
        .find("--confirm-launch=")
        .unwrap_or_else(|| panic!("the refusal prints the flags: {stderr}"));
    stderr[at..]
        .split_whitespace()
        .take(3)
        .map(|s| s.trim_end_matches([',', '.']).to_string())
        .filter(|s| !s.ends_with('='))
        .collect()
}

#[test]
fn billed_work_starts_without_a_terminal_only_with_the_refusals_nonce() {
    let p = Project::new(false);
    p.login(Some("api"));
    let refused = p.apb().args(["run", "pb"]).assert().code(2);
    let err = stderr_of(&refused);
    assert!(err.contains("billing_requires_confirmation"), "{err}");
    assert!(err.contains("codex: billed"), "{err}");
    assert!(p.runs().is_empty(), "nothing started");
    let flags = printed_flags(&err);

    // A run step never confirms, not even with the right nonce.
    let step = p
        .apb()
        .env("APB_RUN_ID", "parent-1")
        .args(["run", "pb"])
        .args(&flags)
        .assert()
        .code(2);
    assert!(
        stderr_of(&step).contains("billing_refused_in_run_step"),
        "{}",
        stderr_of(&step)
    );
    // A nonce nobody was given confirms nothing.
    let mut forged = flags.clone();
    forged[0] = "--confirm-launch=launch-0000".into();
    p.apb().args(["run", "pb"]).args(&forged).assert().code(2);
    assert!(p.runs().is_empty());

    p.apb().args(["run", "pb"]).args(&flags).assert().success();
    let billing = p.billing(&p.run_id());
    assert_eq!(billing["confirmation"], "cli_flag", "{billing}");
    // The confirmation covered that one run: the same flags start no other.
    let again = stderr_of(&p.apb().args(["run", "pb"]).args(&flags).assert().code(2));
    assert!(again.contains("billing_requires_confirmation"), "{again}");
    assert_eq!(p.runs().len(), 1, "one confirmation, one run");
    // A fresh refusal's nonce confirms the next one.
    let next = printed_flags(&again);
    assert_ne!(next[0], flags[0], "a fresh nonce");
    p.apb().args(["run", "pb"]).args(&next).assert().success();
    assert_eq!(p.runs().len(), 2);
    assert_eq!(
        billing["confirmed"],
        serde_json::json!([{"agent": "codex", "basis": "billed"}]),
        "{billing}"
    );
}

/// A billing confirmation is spent by the start it confirms, not by a
/// check that refuses for another reason: a start refused for a missing
/// param after the billing check passed leaves the nonce open, and the
/// corrected start runs on it without asking the person again.
#[test]
fn a_start_refused_after_the_billing_check_keeps_the_confirmation() {
    let p = Project::new(false);
    let v = p.root().join(".apb/playbooks/pb/1.0.0/playbook.yaml");
    let yaml = fs::read_to_string(&v).unwrap().replace(
        "nodes:\n",
        "params:\n  - { name: target, type: text }\nnodes:\n",
    );
    fs::write(&v, yaml).unwrap();
    p.login(Some("api"));
    let refused = p.apb().args(["run", "pb"]).assert().code(2);
    let flags = printed_flags(&stderr_of(&refused));

    let missing = stderr_of(&p.apb().args(["run", "pb"]).args(&flags).assert().code(2));
    assert!(missing.contains("param_needs_value"), "{missing}");
    assert!(p.runs().is_empty());

    p.apb()
        .args(["run", "pb", "--param", "target=x"])
        .args(&flags)
        .assert()
        .success();
    assert_eq!(p.billing(&p.run_id())["confirmation"], "cli_flag");
}

#[test]
fn an_unknown_basis_is_held_and_a_subscription_runs_without_a_question() {
    let p = Project::new(false);
    p.login(None);
    let err = stderr_of(&p.apb().args(["run", "pb"]).assert().code(2));
    assert!(err.contains("billing_requires_confirmation"), "{err}");
    assert!(err.contains("codex: unknown"), "{err}");

    p.login(Some("account"));
    p.apb().args(["run", "pb"]).assert().success();
    let billing = p.billing(&p.run_id());
    assert_eq!(billing["confirmed"], serde_json::json!([]), "{billing}");
    assert!(billing.get("confirmation").is_none(), "{billing}");
}

/// The settings at a terminal, where the machine's file is the person's
/// own (a CLI without a terminal under a redirected config dir is held to
/// the default config dir's settings too, like the launch policy): `allow`
/// runs without a question, a project narrows `allow` to a question or a
/// refusal, a looser project value is ignored, and `deny` is never a
/// question.
// The pseudo-terminal helper is Linux-only (`script` differs on macOS).
#[cfg(target_os = "linux")]
#[test]
fn at_a_terminal_allow_runs_deny_refuses_and_a_project_only_narrows() {
    let p = Project::new(false);
    p.login(Some("api"));
    let env: Vec<(&str, String)> = p.env();
    let mut env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    env.push(("APB_TEST_BILLING_DEFAULT", ""));
    // (machine, project, asked, the refusal, or None for a run)
    let cases: [(&str, Option<&str>, bool, Option<&str>); 6] = [
        ("billed: allow", None, false, None),
        (
            "billed: allow",
            Some("billed: ask"),
            true,
            Some("billing_requires_confirmation"),
        ),
        (
            "billed: allow",
            Some("billed: deny"),
            false,
            Some("billing_denied"),
        ),
        (
            "billed: ask",
            Some("billed: allow"),
            true,
            Some("billing_requires_confirmation"),
        ),
        ("billed: deny", None, false, Some("billing_denied")),
        // A project file that does not read denies: it never drops back to
        // a looser default, and never leaves the machine's `allow` standing
        // over what the project may have narrowed.
        (
            "billed: allow",
            Some("billed: sometimes"),
            false,
            Some("billing_denied"),
        ),
    ];
    for (machine, project, asked, refusal) in cases {
        let _ = fs::remove_dir_all(p.root().join(".apb/runs"));
        p.machine(&format!("billing:\n  {machine}\n"));
        match project {
            Some(y) => p.project(&format!("billing:\n  {y}\n")),
            None => {
                let _ = fs::remove_file(p.root().join(".apb/policy.yaml"));
            }
        }
        // The person says no to any question, so only `allow` starts a run.
        let Some((code, shown)) =
            crate::common::on_a_terminal(p.root(), p.cfg.path(), &["run", "pb"], &env, b"n\n")
        else {
            eprintln!("skipped: no `script` to make a pseudo-terminal");
            return;
        };
        let case = format!("{machine} / {project:?}");
        assert_eq!(shown.contains("Launch it?"), asked, "{case}: {shown}");
        match refusal {
            Some(name) => {
                assert_ne!(code, 0, "{case}: {shown}");
                assert!(shown.contains(name), "{case}: {shown}");
                assert!(p.runs().is_empty(), "{case}");
            }
            None => {
                assert_eq!(code, 0, "{case}: {shown}");
                let billing = p.billing(&p.run_id());
                assert!(
                    billing["allowed"]
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!("billed")),
                    "{case}: {billing}"
                );
            }
        }
    }
}

/// A CLI without a terminal is held to the default config dir's settings
/// when its config dir was redirected: a redirected `allow` does not start
/// billed work for an agent channel. The default config dir is this user's
/// real one (from the password database, which no variable redirects), so
/// the test runs only where that one leaves billed work at `ask`; how the
/// two dirs combine is proven with controlled dirs in
/// `apb_engine::gate::billing`'s tests.
#[test]
fn a_redirected_config_dir_does_not_loosen_billing_for_an_agent_channel() {
    // The file itself, not this process's reading of it: the suite's test
    // default must not decide whether the real file leaves billed at `ask`.
    // Only an absent file leaves it at `ask`; one that exists but does not
    // read denies, so the test does not run there either.
    let real = match apb_core::config::default_config_dir()
        .map(|d| fs::read_to_string(d.join(apb_core::policy_file::POLICY_FILE)))
    {
        None => None,
        Some(Ok(raw)) => Some(raw),
        Some(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => None,
        Some(Err(e)) => {
            eprintln!("skipped: this user's default policy file does not read ({e})");
            return;
        }
    };
    let billed = real
        .as_deref()
        .map(|raw| apb_core::policy_file::parse(raw).map(|f| f.billing.billed));
    if !matches!(
        billed,
        None | Some(Ok(None | Some(apb_core::launch::LaunchPolicy::Ask)))
    ) {
        eprintln!("skipped: this user's default config dir sets billed ({billed:?})");
        return;
    }
    let p = Project::new(false);
    p.login(Some("api"));
    p.machine("billing:\n  billed: allow\n");
    let err = stderr_of(&p.apb().args(["run", "pb"]).assert().code(2));
    assert!(err.contains("billing_requires_confirmation"), "{err}");
    assert!(err.contains("default config dir"), "{err}");
}

/// A run a billing pause holds under a machine `deny` stays denied when the
/// project's file stops reading: the resume is refused outright, with no
/// nonce that could confirm what the machine denies.
#[test]
fn a_damaged_project_file_keeps_the_machines_deny_on_a_billing_resume() {
    let p = Project::new(true);
    p.login(Some("account"));
    p.machine("billing:\n  billed: deny\n");
    let _ = p.apb().args(["run", "pb"]).assert();
    let run_id = p.run_id();
    assert!(
        p.journal(&run_id)
            .iter()
            .any(|e| e["type"] == "run_paused" && e["reason"] == "billing (codex: billed)"),
        "the billed attempt paused the run"
    );
    p.project("billing:\n  billed: [not, a, policy]\n");
    let err = stderr_of(&p.apb().args(["resume", &run_id]).assert().code(2));
    assert!(err.contains("billing_denied"), "{err}");
    assert!(!err.contains("--confirm-launch="), "{err}");
}

// The pseudo-terminal helper is Linux-only (`script` differs on macOS).
#[cfg(target_os = "linux")]
#[test]
fn at_a_terminal_the_person_is_asked_about_the_billed_agents() {
    let p = Project::new(false);
    p.login(Some("api"));
    let env: Vec<(&str, String)> = p.env();
    let mut env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    env.push(("APB_TEST_BILLING_DEFAULT", ""));
    let Some((code, shown)) =
        crate::common::on_a_terminal(p.root(), p.cfg.path(), &["run", "pb"], &env, b"n\n")
    else {
        eprintln!("skipped: no `script` to make a pseudo-terminal");
        return;
    };
    assert_ne!(code, 0, "{shown}");
    assert!(shown.contains("codex: billed"), "{shown}");
    assert!(p.runs().is_empty(), "a no starts nothing");
    let (code, shown) =
        crate::common::on_a_terminal(p.root(), p.cfg.path(), &["run", "pb"], &env, b"y\n").unwrap();
    assert_eq!(code, 0, "{shown}");
    assert_eq!(
        p.billing(&p.run_id())["confirmation"],
        "terminal_prompt",
        "{shown}"
    );
}

#[test]
fn a_mid_run_switch_to_billed_pauses_until_a_person_confirms() {
    let p = Project::new(true);
    p.login(Some("account"));
    // A subscription at start: no question.
    let _ = p.apb().args(["run", "pb"]).assert();
    let run_id = p.run_id();
    let events = p.journal(&run_id);
    let paused = events
        .iter()
        .rfind(|e| e["type"] == "run_paused")
        .unwrap_or_else(|| panic!("the run paused: {events:?}"));
    assert_eq!(paused["reason"], "billing (codex: billed)", "{paused}");
    assert!(
        !events
            .iter()
            .any(|e| e["type"] == "attempt_started" && e["node"] == "work"),
        "the billed attempt never started"
    );

    // A plain resume and a run step are refused; the nonce resumes it.
    let err = stderr_of(&p.apb().args(["resume", &run_id]).assert().code(2));
    assert!(err.contains("billing_requires_confirmation"), "{err}");
    let flags = printed_flags(&err);
    let step = p
        .apb()
        .env("APB_RUN_ID", "parent-1")
        .args(["resume", &run_id])
        .args(&flags[..1])
        .assert()
        .code(2);
    assert!(
        stderr_of(&step).contains("billing_refused_in_run_step"),
        "{}",
        stderr_of(&step)
    );
    p.apb()
        .args(["resume", &run_id])
        .args(&flags[..1])
        .assert()
        .success();
    let events = p.journal(&run_id);
    let resumed = events
        .iter()
        .rfind(|e| e["type"] == "run_resumed")
        .expect("a resume");
    assert_eq!(
        resumed["launch"]["billing"]["confirmed"],
        serde_json::json!([{"agent": "codex", "basis": "billed"}]),
        "{resumed}"
    );
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "attempt_started" && e["node"] == "work"),
        "the confirmed work started"
    );
}

/// An unattended run never waits for a person: a mid-run switch to billed
/// work ends the run failed at once, `hitl_required` naming the node it
/// held, through the unhandled-failure stop, and nothing pauses.
#[test]
fn a_mid_run_switch_to_billed_fails_an_unattended_run_at_once() {
    let p = Project::new(true);
    p.login(Some("account"));
    let _ = p.apb().args(["run", "pb", "--unattended"]).assert();
    let run_id = p.run_id();
    let events = p.journal(&run_id);
    assert!(
        !events.iter().any(|e| e["type"] == "run_paused"),
        "an unattended run never pauses: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| e["type"] == "attempt_started" && e["node"] == "work"),
        "the billed attempt never started"
    );
    let error = events
        .iter()
        .rfind(|e| e["type"] == "run_error")
        .unwrap_or_else(|| panic!("the run failed: {events:?}"));
    assert_eq!(error["node"], "work", "{error}");
    let reason = error["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("hitl_required: billing hold `work`")
            && reason.contains("codex: billed"),
        "{reason}"
    );
    let last = events.last().unwrap();
    assert_eq!(last["type"], "run_finished", "{last}");
    assert_eq!(last["outcome"], "failed", "{last}");
}

/// The start of an unattended run whose billed work nobody confirmed is
/// refused before any run file exists, and the refusal's nonce, a person's
/// confirmation at the start, lets it run with no wait later.
#[test]
fn an_unattended_start_of_unconfirmed_billed_work_is_refused_before_any_run_file() {
    let p = Project::new(false);
    p.login(Some("api"));
    let refused = p.apb().args(["run", "pb", "--unattended"]).assert().code(2);
    let err = stderr_of(&refused);
    assert!(err.contains("billing_requires_confirmation"), "{err}");
    assert!(p.runs().is_empty(), "nothing started");
    let flags = printed_flags(&err);
    p.apb()
        .args(["run", "pb", "--unattended"])
        .args(&flags)
        .assert()
        .success();
    let events = p.journal(&p.run_id());
    assert!(!events.iter().any(|e| e["type"] == "run_paused"));
    assert_eq!(events.last().unwrap()["outcome"], "succeeded");
}
