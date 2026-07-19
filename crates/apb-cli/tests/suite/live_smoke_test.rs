//! Live smoke tests (spec 2026-07-19-official-connectors-design section 8,
//! tier 3): call each official connector's healthcheck plus one read-only
//! function against the REAL service. `#[ignore]`-marked and gated behind an
//! `APB_LIVE_TEST_*` flag plus the real credentials the account needs;
//! skipped (prints a message, returns) rather than failed when the flag or
//! credentials are absent, so `cargo test` never depends on network access
//! or secrets. Never run in CI; exist for manual pre-release verification.
//!
//! Each test installs the connector `--from-dir`, approves it and a real
//! account exactly as a user would via `apb connector approve`, runs a
//! one-node playbook whose stub agent shells out to `apb connector call` for
//! the healthcheck and one read-only function, and asserts both calls report
//! `"ok":true`. The secret credential is written to the project's
//! `.apb/secrets.env` rather than passed through the test process's own env:
//! the engine scrubs every env var an installed connector's secret field
//! references from the spawned agent's environment (spec 4.3), so the
//! credential must be resolvable through the project-dotenv fallback
//! `apb connector call` (a child of the agent) uses instead.

use std::fs;
use std::path::Path;
use std::process::Command;

fn playbook(dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_apb"));
    cmd.args(args)
        .current_dir(dir)
        .env("APB_CONFIG_DIR", dir.join("cfg"))
        .env("HOME", dir.join("home"))
        .env("APB_NO_REGISTRY", "1")
        .env_remove("APB_RUN_DIR")
        .env_remove("APB_NODE_ID");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn apb_ok(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = playbook(dir, args, &[]);
    assert!(
        out.status.success(),
        "`apb {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn repo_connector_dir(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("connectors/{name} must exist: {e}"))
}

fn seed_profile_main(dir: &Path) {
    let pdir = dir.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "test soul").unwrap();
}

const RUN_PLAYBOOK_ID: &str = "live-probe";

fn register_probe_playbook(dir: &Path, connector: &str, functions: &[&str]) {
    let functions_yaml = functions
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let yaml = format!(
        r#"schema: 2
id: {RUN_PLAYBOOK_ID}
name: {RUN_PLAYBOOK_ID}
version: 1.0.0
defaults:
  profile: main
nodes:
  - {{ id: s, type: start }}
  - id: a
    type: agent_task
    prompt: probe
    connectors:
      - {{ name: {connector}, accounts: [default], functions: [{functions_yaml}] }}
  - {{ id: f, type: finish, outcome: success }}
edges:
  - {{ from: s, to: a }}
  - {{ from: a, to: f }}
"#
    );
    let vdir = dir
        .join(".apb/playbooks")
        .join(RUN_PLAYBOOK_ID)
        .join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        dir.join(".apb/playbooks")
            .join(RUN_PLAYBOOK_ID)
            .join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// Writes a `#!/bin/sh` stub agent: calls `apb connector call` for the
/// healthcheck, then for one more function, asserting `"ok":true` appears in
/// each response before proceeding. A non-zero exit from either call, or a
/// response missing `"ok":true`, fails the node (and so the run).
fn write_stub_agent(
    dir: &Path,
    connector: &str,
    healthcheck_fn: &str,
    healthcheck_args: &str,
    call_fn: &str,
    call_args: &str,
) -> std::path::PathBuf {
    let apb_bin = env!("CARGO_BIN_EXE_apb");
    let script = format!(
        r#"#!/bin/sh
set -e
out1="$('{apb_bin}' connector call {connector} {healthcheck_fn} --args '{healthcheck_args}')"
case "$out1" in
  *'"ok":true'*) ;;
  *) echo "healthcheck {healthcheck_fn} failed: $out1" >&2; exit 1 ;;
esac
out2="$('{apb_bin}' connector call {connector} {call_fn} --args '{call_args}')"
case "$out2" in
  *'"ok":true'*) ;;
  *) echo "call {call_fn} failed: $out2" >&2; exit 1 ;;
esac
echo done
"#
    );
    let path = dir.join("stub-agent.sh");
    fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// Full install/approve/config/run sequence for one connector's live probe.
/// `account_fields` is every non-secret account field as `(name, value)`;
/// `secret_field`/`secret_var`/`secret_value` describe the one secret field
/// (written to `.apb/secrets.env` as `secret_var=secret_value`, referenced
/// from the account config as `{{env.<secret_var>}}`).
#[allow(clippy::too_many_arguments)]
fn run_live_probe(
    connector: &str,
    account_fields: &[(&str, &str)],
    secret_field: &str,
    secret_var: &str,
    secret_value: &str,
    healthcheck_fn: &str,
    healthcheck_args: &str,
    call_fn: &str,
    call_args: &str,
) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("cfg")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    apb_ok(root, &["init"]);

    let src = repo_connector_dir(connector);
    apb_ok(
        root,
        &["connector", "install", "--from-dir", src.to_str().unwrap()],
    );

    let mut account_yaml = String::from("accounts:\n  - name: default\n");
    for (k, v) in account_fields {
        account_yaml.push_str(&format!("    {k}: \"{v}\"\n"));
    }
    account_yaml.push_str(&format!(
        "    {secret_field}: \"{{{{env.{secret_var}}}}}\"\n"
    ));
    let cfg_path = root
        .join(".apb/connector-config")
        .join(format!("{connector}.yaml"));
    fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    fs::write(&cfg_path, account_yaml).unwrap();

    apb_ok(root, &["connector", "approve", connector]);
    apb_ok(
        root,
        &["connector", "approve", connector, "--account", "default"],
    );

    let secrets_path = root.join(".apb/secrets.env");
    fs::create_dir_all(secrets_path.parent().unwrap()).unwrap();
    fs::write(&secrets_path, format!("{secret_var}={secret_value}\n")).unwrap();

    seed_profile_main(root);
    register_probe_playbook(root, connector, &[healthcheck_fn, call_fn]);
    let stub = write_stub_agent(
        root,
        connector,
        healthcheck_fn,
        healthcheck_args,
        call_fn,
        call_args,
    );

    let out = playbook(
        root,
        &["run", RUN_PLAYBOOK_ID],
        &[("APB_AGENT_CMD", stub.to_str().unwrap())],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The demo-style `a -> f` edge is unconditional (spec 6's grant-allowlist
    // style), so the run's own top-level outcome reaches "succeeded" via the
    // finish node even when node `a`'s agent attempt failed - an unconditional
    // edge is taken regardless of the source node's status. The only
    // trustworthy signal is node `a`'s own `node_finished` status in the run's
    // event log (TESTING-GUIDELINES: "fold the event log and check state").
    let status = probe_node_status(root, &stdout).unwrap_or_else(|| {
        panic!("could not find node `a`'s status: stdout={stdout} stderr={stderr}")
    });
    assert_eq!(
        status, "succeeded",
        "live probe for `{connector}` node `a` did not succeed (status={status}): stdout={stdout} stderr={stderr}"
    );
}

/// Parses the run id out of `apb run`'s `run <id> finished: <outcome>` line
/// and returns the last `node_finished` status recorded for node `a` in that
/// run's `events.jsonl`.
fn probe_node_status(root: &Path, stdout: &str) -> Option<String> {
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run ")?.split(' ').next())?;
    let events_path = root.join(".apb/runs").join(run_id).join("events.jsonl");
    let content = fs::read_to_string(&events_path).ok()?;
    let mut status = None;
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v["type"] == "node_finished" && v["node"] == "a" {
            status = v["status"].as_str().map(str::to_string);
        }
    }
    status
}

/// Reads every named env var, returning `None` (and printing a skip message)
/// if any is absent or empty - the standard "skip, do not fail" gate for a
/// tier-3 live test.
fn require_env(names: &[&str]) -> Option<Vec<String>> {
    let mut values = Vec::with_capacity(names.len());
    for name in names {
        match std::env::var(name) {
            Ok(v) if !v.is_empty() => values.push(v),
            _ => {
                println!("skipping live test: {name} not set");
                return None;
            }
        }
    }
    Some(values)
}

#[test]
#[ignore]
fn live_github_healthcheck_and_list_issues() {
    if std::env::var("APB_LIVE_TEST_GITHUB")
        .unwrap_or_default()
        .is_empty()
    {
        println!("skipping live test: APB_LIVE_TEST_GITHUB not set");
        return;
    }
    let Some(vals) = require_env(&["GITHUB_TOKEN", "GITHUB_TEST_OWNER", "GITHUB_TEST_REPO"]) else {
        return;
    };
    let (token, owner, repo) = (&vals[0], &vals[1], &vals[2]);

    run_live_probe(
        "github",
        &[("api_base", "https://api.github.com")],
        "token",
        "GITHUB_TOKEN",
        token,
        "get_rate_limit",
        "{}",
        "list_issues",
        &format!(r#"{{"owner":"{owner}","repo":"{repo}","state":"open","labels":"","page":1}}"#),
    );
}

#[test]
#[ignore]
fn live_telegram_healthcheck_and_get_chat() {
    if std::env::var("APB_LIVE_TEST_TELEGRAM")
        .unwrap_or_default()
        .is_empty()
    {
        println!("skipping live test: APB_LIVE_TEST_TELEGRAM not set");
        return;
    }
    let Some(vals) = require_env(&["TELEGRAM_BOT_TOKEN", "TELEGRAM_TEST_CHAT_ID"]) else {
        return;
    };
    let (token, chat_id) = (&vals[0], &vals[1]);

    run_live_probe(
        "telegram",
        &[("api_base", "https://api.telegram.org")],
        "token",
        "TELEGRAM_BOT_TOKEN",
        token,
        "get_me",
        "{}",
        "get_chat",
        &format!(r#"{{"chat_id":"{chat_id}"}}"#),
    );
}

#[test]
#[ignore]
fn live_smtp_verify_and_send() {
    if std::env::var("APB_LIVE_TEST_SMTP")
        .unwrap_or_default()
        .is_empty()
    {
        println!("skipping live test: APB_LIVE_TEST_SMTP not set");
        return;
    }
    let Some(vals) = require_env(&[
        "SMTP_TEST_HOST",
        "SMTP_TEST_PORT",
        "SMTP_TEST_USERNAME",
        "SMTP_TEST_PASSWORD",
        "SMTP_TEST_FROM",
        "SMTP_TEST_TO",
    ]) else {
        return;
    };
    let (host, port, username, password, from, to) =
        (&vals[0], &vals[1], &vals[2], &vals[3], &vals[4], &vals[5]);

    run_live_probe(
        "smtp",
        &[
            ("host", host),
            ("port", port),
            ("username", username),
            ("from_email", from),
            ("from_name", "apb live smoke test"),
            ("use_tls", "true"),
        ],
        "password",
        "SMTP_TEST_PASSWORD",
        password,
        "verify",
        "{}",
        "send_email",
        &format!(
            r#"{{"to":"{to}","subject":"apb live smoke test","body_text":"apb live smoke test body"}}"#
        ),
    );
}

#[test]
#[ignore]
fn live_sentry_healthcheck_and_list_issues() {
    if std::env::var("APB_LIVE_TEST_SENTRY")
        .unwrap_or_default()
        .is_empty()
    {
        println!("skipping live test: APB_LIVE_TEST_SENTRY not set");
        return;
    }
    let Some(vals) = require_env(&["SENTRY_TOKEN", "SENTRY_TEST_ORG", "SENTRY_TEST_PROJECT"])
    else {
        return;
    };
    let (token, org, project) = (&vals[0], &vals[1], &vals[2]);

    run_live_probe(
        "sentry",
        &[("base_url", "https://sentry.io"), ("org", org)],
        "token",
        "SENTRY_TOKEN",
        token,
        "list_projects",
        "{}",
        "list_issues",
        &format!(r#"{{"query":"is:unresolved","project":"{project}","cursor":""}}"#),
    );
}

#[test]
#[ignore]
fn live_asana_healthcheck_and_list_workspaces() {
    if std::env::var("APB_LIVE_TEST_ASANA")
        .unwrap_or_default()
        .is_empty()
    {
        println!("skipping live test: APB_LIVE_TEST_ASANA not set");
        return;
    }
    let Some(vals) = require_env(&["ASANA_TOKEN"]) else {
        return;
    };
    let token = &vals[0];

    run_live_probe(
        "asana",
        &[("api_base", "https://app.asana.com/api/1.0")],
        "token",
        "ASANA_TOKEN",
        token,
        "get_me",
        "{}",
        "list_workspaces",
        r#"{"limit":10}"#,
    );
}

#[test]
#[ignore]
fn live_imap_verify_and_list_folders() {
    if std::env::var("APB_LIVE_TEST_IMAP")
        .unwrap_or_default()
        .is_empty()
    {
        println!("skipping live test: APB_LIVE_TEST_IMAP not set");
        return;
    }
    let Some(vals) = require_env(&[
        "IMAP_TEST_HOST",
        "IMAP_TEST_PORT",
        "IMAP_TEST_AUTH_METHOD",
        "IMAP_TEST_USERNAME",
        "IMAP_TEST_PASSWORD",
    ]) else {
        return;
    };
    let (host, port, auth_method, username, password) =
        (&vals[0], &vals[1], &vals[2], &vals[3], &vals[4]);

    run_live_probe(
        "imap",
        &[
            ("host", host),
            ("port", port),
            ("use_tls", "true"),
            ("auth_method", auth_method),
            ("username", username),
        ],
        "password",
        "IMAP_TEST_PASSWORD",
        password,
        "verify",
        "{}",
        "list_folders",
        "{}",
    );
}

/// Live smoke for the `hermes` agent (not a connector probe): invokes the
/// real `hermes` binary directly with the exact flag order `builtin("hermes")`
/// uses (crates/apb-engine/src/invocation.rs, `-z {prompt} -m {model}`), so
/// this exercises the shipped one-shot invocation form rather than a
/// hand-rolled one. Gated behind `APB_LIVE_TEST_HERMES=1`; skipped (not
/// failed) when the flag or the model env var is absent.
#[test]
#[ignore]
fn live_hermes_oneshot() {
    if std::env::var("APB_LIVE_TEST_HERMES")
        .unwrap_or_default()
        .is_empty()
    {
        println!("skipping live test: APB_LIVE_TEST_HERMES not set");
        return;
    }
    // APB_LIVE_HERMES_MODEL: the user's configured hermes default model id
    // (for example `glm-5.2`). Hermes has no entry in the curated models
    // table (docs/PROFILES.md's "Choosing agent and model"), so there is no
    // built-in default to fall back to - the model must come from the
    // environment.
    let Some(vals) = require_env(&["APB_LIVE_HERMES_MODEL"]) else {
        return;
    };
    let model = &vals[0];

    let out = Command::new("hermes")
        .args([
            "-z",
            "Reply with exactly the text APB_OK and nothing else",
            "-m",
            model,
        ])
        .output()
        .expect("failed to spawn hermes");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "hermes -z ... -m {model} exited non-zero: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("APB_OK"),
        "hermes stdout did not contain APB_OK: stdout={stdout} stderr={stderr}"
    );
}
