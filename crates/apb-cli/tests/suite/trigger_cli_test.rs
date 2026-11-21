//! The person's command over the start rules (design D1 "Tests" item 6):
//! every mutation is a person's (a run step is refused, the rule's name is
//! typed at a terminal, no `--yes`, no passkey), `apb trigger test` says
//! what a start would meet and exits on the verdict, `remove` takes the
//! entry, the state and the journal away, `apb resume --all` sweeps once
//! per invocation (a run step sweeps nothing), the doctor names what holds
//! the sweep back, and a signed delivery ends in a finished run through the
//! real listener.
//!
//! In-process core calls that resolve the global config dir (registration,
//! trust, the connector store) run under this suite's env lock with
//! `APB_CONFIG_DIR` set to the fixture's own config dir and restored after;
//! spawned commands set the same variable per spawn, so no child ever reads
//! the mutated value.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use apb_core::connector::inbox::Inbox;
use apb_core::registry::init_project;
use apb_core::scope::Scope;
use apb_core::signed_store::{self, read as read_signed};
use apb_core::trigger::{RuleEntry, RulesFile, TriggerStateFile, set_rule};
use serde_json::{Value, json};

use crate::common;

/// The env lock of this suite (the pattern of `launch_cli_test.rs`).
static ENV: Mutex<()> = Mutex::new(());

/// Runs `f` with `APB_CONFIG_DIR` pointing at `cfg`, restored after, under
/// the suite's env lock. Every in-process core call that reads the global
/// config goes through this, so it sees exactly what the spawned commands
/// (which get the same directory per spawn) see.
fn with_cfg<T>(cfg: &Path, f: impl FnOnce() -> T) -> T {
    let _lock = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var_os("APB_CONFIG_DIR");
    // SAFETY: under ENV, so no other test reads or writes the variable.
    unsafe { std::env::set_var("APB_CONFIG_DIR", cfg) };
    let out = f();
    // SAFETY: still under ENV.
    unsafe {
        match prev {
            Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
            None => std::env::remove_var("APB_CONFIG_DIR"),
        }
    }
    out
}

/// The connector manifest of the fixture: a webhook block that names an
/// event type (the shape a rule's `types:` check reads), with an inbox
/// function. The same shape the MCP trigger suite uses.
const GITHUB_YAML: &str = r#"
name: github
version: 1.0.0
webhook:
  signature:
    scheme: hmac_sha256_hex
    header: X-Signature
    secret: "{{secret.app_secret}}"
  event_type_header: X-GitHub-Event
account_fields:
  - name: app_secret
    required: true
    secret: true
functions:
  - name: inbox_read
    description: Read pending events
    read_only: true
    response_pick: [events, cursor]
    inbox:
      op: read
"#;

fn install_connector(cfg: &Path) {
    let dir = cfg.join("connectors").join("github");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("connector.yaml"), GITHUB_YAML).unwrap();
    fs::create_dir_all(cfg.join("connector-config")).unwrap();
    fs::write(
        cfg.join("connector-config").join("github.yaml"),
        "accounts:\n  - name: main\n",
    )
    .unwrap();
}

/// A profile under a never-played executor, so preparation has something to
/// snapshot; its bundle is approved, as a profile a person wrote through
/// apb's own tools is.
fn seed_profile(root: &Path, cfg: &Path, name: &str, agent: &str, model: &str) {
    let dir = root.join(".apb/profiles").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("profile.yaml"),
        format!("name: {name}\ndescription: test\nexecutor:\n  agent: {agent}\n  model: {model}\n"),
    )
    .unwrap();
    fs::write(dir.join("SOUL.md"), "a role\n").unwrap();
    with_cfg(cfg, || {
        let r = apb_core::profile::QualifiedProfileRef {
            name: name.to_string(),
            scope: apb_core::profile::ProfileScope::Auto,
        };
        let (_loaded, _pairs, bundle) = apb_core::profile_store::compute_bundle(
            root,
            apb_core::profile_store::PlaybookOrigin::Project,
            &r,
        )
        .unwrap();
        let mut store = apb_core::trust::TrustStore::load();
        store
            .approve(
                &bundle,
                &format!("project/{name}"),
                apb_core::trust::OriginKind::LocallyApproved,
            )
            .unwrap();
    });
}

fn write_playbook(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// Approves the playbook's current trust digest, as a person's approval of
/// a playbook they saved does. The fire never passes
/// `acknowledge_untrusted`: a rule is no trust approval.
fn trust_current(root: &Path, cfg: &Path, id: &str) {
    with_cfg(cfg, || {
        let reg = apb_core::registry::Registry::open_dir(&root.join(".apb")).unwrap();
        let loaded = reg.load(id, Some("1.0.0")).unwrap();
        let digest = loaded.trust_digest().unwrap();
        let mut store = apb_core::trust::TrustStore::load();
        store
            .approve(&digest, id, apb_core::trust::OriginKind::LocallyApproved)
            .unwrap();
    });
}

/// `start -> done`, nothing that needs an agent, nothing that asks a
/// person: a rule's run of it is unattended and finishes on its own.
fn simple_pb(id: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    )
}

/// A rule document as a person writes it: the fixture's github account,
/// `issues` deliveries whose body names `action: opened`, the start key of
/// the event seq (one run per event).
fn rule_doc(name: &str, id: &str, key: &str) -> String {
    format!(
        "name: {name}\nplaybook: {{ id: {id}, version: 1.0.0, scope: project }}\nsource:\n  inbox:\n    connector: github\n    account: main\n    types: [issues]\n    where:\n      - {{ path: action, equals: opened }}\n    key: \"{key}\"\nlimits: {{ max_per_hour: 5 }}\nbudget: {{ tokens: 100000, on_no_estimate: continue }}\nexternal_data: true\nrequire_ready: false\n"
    )
}

struct Project {
    dir: tempfile::TempDir,
    cfg: tempfile::TempDir,
}

impl Project {
    /// A project with the fixture connector, the playbook, a profile and
    /// the registration. `register: false` skips only the registration, so
    /// a command in it is refused `trigger_project_unregistered`.
    fn new(id: &str, register: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        install_connector(cfg.path());
        init_project(dir.path()).unwrap();
        write_playbook(dir.path(), id, &simple_pb(id));
        trust_current(dir.path(), cfg.path(), id);
        seed_profile(dir.path(), cfg.path(), "main", "claude-code", "haiku");
        if register {
            with_cfg(cfg.path(), || {
                apb_core::run_origin::ensure_key().unwrap();
                apb_core::projects::bind_id(dir.path()).unwrap();
            });
        }
        Project { dir, cfg }
    }

    fn new_registered(id: &str) -> Self {
        Self::new(id, true)
    }

    fn new_unregistered(id: &str) -> Self {
        Self::new(id, false)
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn cfg_dir(&self) -> &Path {
        self.cfg.path()
    }

    /// `apb` in this project with this project's config dir.
    fn apb(&self) -> assert_cmd::Command {
        let mut c = common::apb();
        c.current_dir(self.root())
            .env("APB_CONFIG_DIR", self.cfg.path())
            .env_remove("APB_NO_REGISTRY");
        c
    }

    /// Writes a rule document into the project and returns its path.
    fn write_rule_doc(&self, name: &str, body: &str) -> PathBuf {
        let path = self.root().join(format!("rule-{name}.yaml"));
        fs::write(&path, body).unwrap();
        path
    }

    /// Saves a rule the way `apb trigger set` ends up doing (in-process,
    /// the same core write), so a test can arrange a rule without driving
    /// the terminal flow first.
    fn save_rule(&self, doc: &str) {
        with_cfg(self.cfg.path(), || {
            set_rule(self.root(), doc, "test", apb_core::clock::Millis::now()).unwrap();
        });
    }

    /// Approves the rule through the real tools a person's approve command
    /// runs (`approval_need`, then `approve`).
    fn approve(&self, name: &str) {
        with_cfg(self.cfg.path(), || {
            let entry = self.entry_of_raw(name);
            let need = apb_mcp::tools::trigger::approval_need(self.root(), &entry)
                .expect("the approval need");
            apb_mcp::tools::trigger::approve(self.root(), name, &need, "terminal")
                .expect("approved");
        });
    }

    fn rules_path(&self) -> PathBuf {
        signed_store::file::<RulesFile>(self.root(), Scope::Project, "rules")
    }

    /// The entries of the signed rules file, under the suite's env lock.
    /// The lock is not re-entrant: a caller already inside `with_cfg` uses
    /// [`Self::read_rules_raw`] instead.
    fn rules_entries(&self) -> Vec<RuleEntry> {
        with_cfg(self.cfg.path(), || self.read_rules_raw())
    }

    /// The entries of the signed rules file, without the env lock. Only for
    /// a caller already inside `with_cfg`.
    fn read_rules_raw(&self) -> Vec<RuleEntry> {
        read_signed::<RulesFile>(self.root(), Scope::Project, "rules")
            .unwrap()
            .file
            .rules
    }

    /// The entry of the rule, under the suite's env lock.
    fn entry_of(&self, name: &str) -> RuleEntry {
        with_cfg(self.cfg.path(), || self.entry_of_raw(name))
    }

    /// The entry of the rule, without the env lock. Only for a caller
    /// already inside `with_cfg`.
    fn entry_of_raw(&self, name: &str) -> RuleEntry {
        self.read_rules_raw()
            .into_iter()
            .find(|e| e.rule.name == name)
            .unwrap_or_else(|| panic!("no rule `{name}`"))
    }

    /// Whether the state holds an entry of this rule's current incarnation.
    fn state_has(&self, name: &str) -> bool {
        with_cfg(self.cfg.path(), || {
            let Some(entry) = self
                .read_rules_raw()
                .into_iter()
                .find(|e| e.rule.name == name)
            else {
                return false;
            };
            let state = read_signed::<TriggerStateFile>(self.root(), Scope::Project, "state")
                .unwrap()
                .file;
            state
                .rules
                .iter()
                .any(|s| s.rule == name && s.incarnation == entry.incarnation)
        })
    }

    /// The journal file of the rule's current incarnation, if any.
    fn journal_path(&self, name: &str) -> Option<PathBuf> {
        let entry = self.entry_of(name);
        let path = self
            .root()
            .join(".apb/trigger-state/log")
            .join(format!("{}.jsonl", entry.incarnation));
        path.is_file().then(|| path.clone())
    }

    /// The run ids the trigger journal of the rule says its incarnation
    /// started, newest last.
    fn started_runs_of(&self, name: &str) -> Vec<String> {
        with_cfg(self.cfg.path(), || {
            let entry = self.entry_of_raw(name);
            apb_core::trigger::read(self.root(), &entry.incarnation, 100)
                .into_iter()
                .filter_map(|l| match l.verdict {
                    apb_core::trigger::Verdict::Started => l.run_id,
                    _ => None,
                })
                .collect()
        })
    }

    /// The run directories of the project.
    fn runs(&self) -> Vec<String> {
        fs::read_dir(self.root().join(".apb/runs"))
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Appends one delivery of type `issues` to the account's inbox, at the
    /// inbox root the listener of this config dir reads.
    fn deliver(&self, provider_id: &str, body: &Value) {
        let inbox = Inbox::at(&self.cfg_dir().join("connector-inbox"), "github", "main").unwrap();
        inbox
            .append_typed(provider_id, Some("issues"), body)
            .unwrap();
    }
}

/// Polls `pred` until it is true or `secs` pass, then panics with `what`.
fn wait_for<T: Default>(what: &str, secs: u64, mut pred: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = pred() {
            return v;
        }
        if Instant::now() > deadline {
            panic!("{what}: timed out");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Kills the child on drop, so a failing assertion does not leave a
/// listener behind sweeping a deleted project.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// --- a person's command (D0 "A person's command" item 1) -------------------

/// `set` without a terminal is refused with the sentence, and writes
/// nothing.
#[test]
fn set_refuses_without_a_terminal() {
    let p = Project::new_registered("tcli1");
    let doc = p.write_rule_doc("triage", &rule_doc("triage", "tcli1", "{{event.seq}}"));
    p.apb()
        .args(["trigger", "set", "triage", "--file", doc.to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "a person changes a rule at an interactive terminal or in a signed-in dashboard",
        ));
    assert!(!p.rules_path().exists(), "nothing was written");
    assert!(p.rules_entries().is_empty());
}

/// A run step is refused before anything else, even before the terminal is
/// missed.
#[test]
fn set_refuses_in_a_run_step() {
    let p = Project::new_registered("tcli2");
    p.save_rule(&rule_doc("triage", "tcli2", "{{event.seq}}"));
    let before = fs::read(p.rules_path()).unwrap();
    // A change of the saved rule, which a person's set would write.
    let changed =
        rule_doc("triage", "tcli2", "{{event.seq}}").replace("max_per_hour: 5", "max_per_hour: 6");
    let doc = p.write_rule_doc("triage", &changed);
    p.apb()
        .env("APB_RUN_ID", "r1")
        .args(["trigger", "set", "triage", "--file", doc.to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("inside run `r1`"))
        .stderr(predicates::str::contains("a run step does not decide"));
    assert_eq!(
        fs::read(p.rules_path()).unwrap(),
        before,
        "the saved rules file is byte for byte what it was"
    );
}

/// `disable` without a terminal is refused the same way.
#[test]
fn disable_refuses_without_a_terminal() {
    let p = Project::new_registered("tcli3");
    p.apb()
        .args(["trigger", "disable", "triage"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "a person changes a rule at an interactive terminal or in a signed-in dashboard",
        ));
}

/// `approve` prints the need it would confirm, then refuses without a
/// terminal.
#[test]
fn approve_refuses_without_a_terminal() {
    let p = Project::new_registered("tcli4");
    p.save_rule(&rule_doc("triage", "tcli4", "{{event.seq}}"));
    p.apb()
        .args(["trigger", "approve", "triage"])
        .assert()
        .code(2)
        .stdout(predicates::str::contains("rule_digest"))
        .stdout(predicates::str::contains(
            "Approving rule `triage` lets it start tcli4 1.0.0 with nobody at the keyboard.",
        ))
        .stderr(predicates::str::contains(
            "a person changes a rule at an interactive terminal or in a signed-in dashboard",
        ));
    // Still unapproved.
    let entry = p.entry_of("triage");
    assert!(entry.approval.is_none(), "nothing was approved");
}

/// The rule's name typed at a terminal is what confirms a change: another
/// name changes nothing, the rule's name writes the rule.
#[cfg(target_os = "linux")]
#[test]
fn set_needs_the_name_typed() {
    let p = Project::new_registered("tcli5");
    let doc = p.write_rule_doc("triage", &rule_doc("triage", "tcli5", "{{event.seq}}"));
    let args = ["trigger", "set", "triage", "--file", doc.to_str().unwrap()];
    let (code, out) = common::on_a_terminal(p.root(), p.cfg_dir(), &args, &[], b"other\n")
        .expect("a pseudo terminal");
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("not confirmed: the name typed is not `triage`"),
        "{out}"
    );
    assert!(!p.rules_path().exists(), "the wrong name wrote nothing");

    let (code, out) = common::on_a_terminal(p.root(), p.cfg_dir(), &args, &[], b"triage\n")
        .expect("a pseudo terminal");
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("needs approval: apb trigger approve triage"),
        "{out}"
    );
    let entries = p.rules_entries();
    assert_eq!(entries.len(), 1, "{out}");
    assert_eq!(entries[0].rule.name, "triage");
    assert!(
        entries[0].approval.is_none(),
        "a new rule starts unapproved"
    );
}

/// A project that is not registered cannot save a rule: the refusal comes
/// before the terminal is asked (a listener would never sweep it).
#[test]
fn set_refuses_an_unregistered_project() {
    let p = Project::new_unregistered("tcli6");
    let doc = p.write_rule_doc("triage", &rule_doc("triage", "tcli6", "{{event.seq}}"));
    p.apb()
        .env("APB_NO_REGISTRY", "1")
        .args(["trigger", "set", "triage", "--file", doc.to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("trigger_project_unregistered"));
    assert!(!p.rules_path().exists(), "nothing was written");
}

/// The file's `enabled` field is refused whatever its value: only `enable`
/// and `disable` change it.
#[test]
fn set_refuses_enabled_in_the_file() {
    let p = Project::new_registered("tcli7");
    for value in ["true", "false"] {
        let doc = p.write_rule_doc(
            "triage",
            &format!(
                "{}\nenabled: {value}\n",
                rule_doc("triage", "tcli7", "{{event.seq}}")
            ),
        );
        p.apb()
            .args(["trigger", "set", "triage", "--file", doc.to_str().unwrap()])
            .assert()
            .code(2)
            .stderr(predicates::str::contains("trigger_enabled_not_settable"));
    }
    assert!(!p.rules_path().exists(), "nothing was written");
}

/// Every issue is printed, and the command exits 1 before it asks (with no
/// terminal the ask would exit 2, so 1 proves it stopped at the issues).
/// The two errors: a key template that does not parse (V260) and a param
/// the playbook does not declare (V262).
#[test]
fn set_prints_every_issue() {
    let p = Project::new_registered("tcli8");
    let doc = "name: triage\nplaybook: { id: tcli8, version: 1.0.0, scope: project }\nsource:\n  inbox:\n    connector: github\n    account: main\n    types: [issues]\n    key: \"{{body.}}\"\nparams:\n  title: \"{{body.title}}\"\nlimits: { max_per_hour: 5 }\nbudget: { tokens: 100000, on_no_estimate: continue }\nexternal_data: true\nrequire_ready: false\n";
    let doc = p.write_rule_doc("triage", doc);
    p.apb()
        .args(["trigger", "set", "triage", "--file", doc.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("V260"))
        .stdout(predicates::str::contains("V262"))
        .stdout(predicates::str::contains("error"));
    assert!(!p.rules_path().exists(), "an invalid rule is not saved");
}

// --- the dry test ----------------------------------------------------------

/// An event the rule's `where` condition fails exits 3; a matching one
/// passes the checks before start (exit 0); the first event by seq still
/// skips when named again.
#[test]
fn test_exits_three_for_a_skipped_event() {
    let p = Project::new_registered("tcli9");
    p.save_rule(&rule_doc("news", "tcli9", "{{event.seq}}"));
    p.approve("news");
    // The body names `closed`, not the `opened` the rule asks for.
    p.deliver("e1", &json!({ "action": "closed" }));
    p.apb()
        .args(["trigger", "test", "news"])
        .assert()
        .code(3)
        .stdout(predicates::str::contains("where"));

    // A matching delivery passes everything before preparation.
    p.deliver("e2", &json!({ "action": "opened" }));
    p.apb()
        .args(["trigger", "test", "news"])
        .assert()
        .code(0)
        .stdout(predicates::str::contains("passes the checks before start"));

    // The stored event by seq: the newest is a match, the first is not.
    p.apb()
        .args(["trigger", "test", "news", "--event", "1"])
        .assert()
        .code(3);
    p.apb()
        .args(["trigger", "test", "news", "--event", "2"])
        .assert()
        .code(0);
}

// --- remove ----------------------------------------------------------------

/// `remove` takes the entry, the state entry and the journal file away, in
/// one command under both rule locks.
#[cfg(target_os = "linux")]
#[test]
fn remove_deletes_entry_state_and_journal() {
    let p = Project::new_registered("tcli10");
    p.save_rule(&rule_doc("triage", "tcli10", "{{event.seq}}"));
    p.approve("triage");
    // One pass of the sweep positions the rule: a state entry and a
    // `positioned` journal line exist. The journal file of the incarnation
    // is known before the remove: after it the rule has no entry to name
    // the incarnation by.
    with_cfg(p.cfg_dir(), || {
        apb_mcp::tools::trigger::sweep_root(p.root());
    });
    assert!(p.state_has("triage"), "the pass positioned the rule");
    let journal = p.journal_path("triage").expect("the journal exists");

    let (code, out) = common::on_a_terminal(
        p.root(),
        p.cfg_dir(),
        &["trigger", "remove", "triage"],
        &[],
        b"triage\n",
    )
    .expect("a pseudo terminal");
    assert_eq!(code, 0, "{out}");
    assert!(
        !p.rules_entries().iter().any(|e| e.rule.name == "triage"),
        "{out}"
    );
    assert!(!p.state_has("triage"), "the state entry is gone");
    assert!(!journal.is_file(), "the journal file is gone");
}

/// A journal the remove could not delete beneath the anchor (a link at
/// `log`) never fails the removal: the command answers the rule removed and
/// prints the `journal_left` warning with the path.
#[cfg(target_os = "linux")]
#[test]
fn remove_reports_a_journal_it_could_not_delete() {
    let p = Project::new_registered("tcli-jleft");
    p.save_rule(&rule_doc("triage", "tcli-jleft", "{{event.seq}}"));
    let inc = p.entry_of("triage").incarnation;
    // `log` becomes a link to another directory, whose journal of the same
    // incarnation name stays exactly as it is.
    let elsewhere = tempfile::tempdir().unwrap();
    let log = p.root().join(".apb/trigger-state/log");
    std::fs::create_dir_all(&log).unwrap();
    std::fs::create_dir_all(elsewhere.path().join("log")).unwrap();
    std::fs::rename(&log, elsewhere.path().join("log-real")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("log"), &log).unwrap();
    let other_journal = elsewhere.path().join("log").join(format!("{inc}.jsonl"));
    std::fs::write(&other_journal, b"keep me\n").unwrap();

    let (code, out) = common::on_a_terminal(
        p.root(),
        p.cfg_dir(),
        &["trigger", "remove", "triage"],
        &[],
        b"triage\n",
    )
    .expect("a pseudo terminal");
    assert_eq!(code, 0, "{out}");
    assert!(
        !p.rules_entries().iter().any(|e| e.rule.name == "triage"),
        "the rule is removed: {out}"
    );
    assert!(!p.state_has("triage"), "the state entry is gone: {out}");
    assert!(
        out.contains("warning: journal_left: .apb/trigger-state/log/"),
        "{out}"
    );
    assert_eq!(
        std::fs::read(&other_journal).unwrap(),
        b"keep me\n",
        "the other journal is untouched"
    );
}

/// A state entry the remove could not delete (a state file that does not
/// verify) never fails the removal: the command answers the rule removed
/// and prints the `state_left` warning with the path.
#[cfg(target_os = "linux")]
#[test]
fn remove_reports_a_state_entry_it_could_not_delete() {
    let p = Project::new_registered("tcli-sleft");
    p.save_rule(&rule_doc("triage", "tcli-sleft", "{{event.seq}}"));
    // The state file stops verifying: the store refuses to write it.
    let state = p.root().join(".apb/trigger-state/project/state.json");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(&state, b"not a signed state").unwrap();

    let (code, out) = common::on_a_terminal(
        p.root(),
        p.cfg_dir(),
        &["trigger", "remove", "triage"],
        &[],
        b"triage\n",
    )
    .expect("a pseudo terminal");
    assert_eq!(code, 0, "{out}");
    assert!(
        !p.rules_entries().iter().any(|e| e.rule.name == "triage"),
        "the rule is removed: {out}"
    );
    assert!(
        out.contains("warning: state_left: .apb/trigger-state/project/state.json"),
        "{out}"
    );
}

// --- the sweep hosts -------------------------------------------------------

/// A delivery ends in a finished run through the real listener: the first
/// pass positions the approved rule, a signed delivery starts a run, `apb
/// wait` sees it succeed and `apb runs` names the rule that started it.
#[test]
fn a_delivery_starts_a_run_end_to_end() {
    let p = Project::new_registered("tcli11");
    p.save_rule(&rule_doc("news", "tcli11", "{{event.seq}}"));
    p.approve("news");

    let ingest = common::apb_std()
        .current_dir(p.root())
        .env("APB_CONFIG_DIR", p.cfg_dir())
        .env("APB_TRIGGER_SWEEP_MS", "200")
        .args(["ingest", "--bind", "127.0.0.1", "--port", "0"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn apb ingest");
    let _listener = ChildGuard(ingest);

    // The first pass positions the rule at the empty head of its source.
    wait_for("the listener positions the rule", 30, || {
        p.state_has("news").then_some(())
    });
    assert!(
        p.started_runs_of("news").is_empty(),
        "an empty inbox starts nothing"
    );

    // A delivery arrives; the next pass starts its run.
    p.deliver("e1", &json!({ "action": "opened" }));
    let run_id = wait_for("the rule starts a run", 30, || {
        p.started_runs_of("news").pop()
    });

    // The run finishes on its own and the surfaces name the rule.
    p.apb()
        .args(["wait", &run_id, "--timeout", "60"])
        .assert()
        .code(0);
    p.apb()
        .args(["runs", &run_id])
        .assert()
        .success()
        .stdout(predicates::str::contains("started by rule"));
}

/// `apb resume --all` sweeps once after its health sweep: the first
/// invocation positions the rule, a delivery after it starts its run.
#[test]
fn resume_all_sweeps_new_events() {
    let p = Project::new_registered("tcli12");
    p.save_rule(&rule_doc("news", "tcli12", "{{event.seq}}"));
    p.approve("news");

    p.apb().args(["resume", "--all"]).assert().success();
    assert!(p.state_has("news"), "the pass positioned the rule");
    assert!(p.runs().is_empty(), "an empty inbox starts nothing");

    p.deliver("e1", &json!({ "action": "opened" }));
    let from = apb_core::clock::now_ms_u64();
    p.apb()
        .args(["resume", "--all"])
        .assert()
        .success()
        .stdout(predicates::str::contains("trigger: news: started run"));
    let to = apb_core::clock::now_ms_u64();
    assert_eq!(p.runs().len(), 1, "the pass started one run");
    // The journal times of this host are milliseconds, as the listener's.
    let entry = p.entry_of("news");
    let lines = with_cfg(p.cfg_dir(), || {
        apb_core::trigger::read(p.root(), &entry.incarnation, 100)
    });
    let started = lines
        .iter()
        .find(|l| l.verdict == apb_core::trigger::Verdict::Started)
        .expect("the started line");
    assert!(
        (from..=to).contains(&started.at),
        "`at` {} is not a millisecond time between {from} and {to}",
        started.at
    );
}

/// Inside a run step the sweep is skipped before any read: the sentence is
/// printed, no position is taken, nothing starts.
#[test]
fn resume_all_in_a_run_step_sweeps_nothing() {
    let p = Project::new_registered("tcli13");
    p.save_rule(&rule_doc("news", "tcli13", "{{event.seq}}"));
    p.approve("news");
    p.deliver("e1", &json!({ "action": "opened" }));

    p.apb()
        .env("APB_RUN_ID", "r1")
        .args(["resume", "--all"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "rule sweep skipped: apb resume --all runs inside a run step",
        ));
    assert!(!p.state_has("news"), "no position was taken");
    assert!(p.runs().is_empty(), "nothing started");
    assert!(p.journal_path("news").is_none(), "no journal line");
}

// --- the doctor ------------------------------------------------------------

/// The doctor warns when rules are enabled and no sweep runs, and names
/// every rule that still needs an approval.
#[test]
fn doctor_warns_without_a_sweep() {
    let p = Project::new_registered("tcli14");
    p.save_rule(&rule_doc("ready", "tcli14", "{{event.seq}}"));
    p.approve("ready");
    p.save_rule(&rule_doc("wants", "tcli14", "{{event.seq}}"));

    p.apb()
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "2 enabled rule(s) and no trigger sweep runs",
        ))
        .stdout(predicates::str::contains(
            "rule `wants` needs approval: apb trigger approve wants",
        ))
        .stdout(predicates::str::contains("[warn] triggers"));
    // The approved rule is not named as needing an approval.
    let out = String::from_utf8_lossy(
        &p.apb()
            .args(["doctor"])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .into_owned();
    assert!(!out.contains("rule `ready` needs approval"), "{out}");
}

// --- the docs --------------------------------------------------------------

const TRIGGERS_DOC: &str = include_str!("../../../../docs/TRIGGERS.md");

/// The fenced yaml rule blocks of docs/TRIGGERS.md, in order.
fn yaml_blocks(doc: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = doc.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() != "```yaml" {
            continue;
        }
        let mut body = Vec::new();
        for line in lines.by_ref() {
            if line.trim() == "```" {
                break;
            }
            body.push(line);
        }
        blocks.push(body.join("\n"));
    }
    blocks
}

/// The rule a yaml block holds, through the store's own document parser.
fn rule_from_yaml(block: &str) -> apb_core::trigger::Rule {
    apb_core::trigger::parse_rule_document(block).expect("a docs rule is a rule")
}

/// Every rule yaml block of docs/TRIGGERS.md validates against the github
/// connector and an issue-triage playbook with the params the docs rule
/// fills.
#[test]
fn docs_examples_validate() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    install_connector(cfg.path());
    init_project(dir.path()).unwrap();
    let vdir = dir
        .path()
        .join(".apb/playbooks")
        .join("issue-triage")
        .join("1.2.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: issue-triage\nname: issue-triage\nversion: 1.2.0\nparams:\n  - { name: repo, type: text, untrusted: true }\n  - { name: issue, type: text, untrusted: true }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
    )
    .unwrap();
    fs::write(
        dir.path()
            .join(".apb/playbooks")
            .join("issue-triage")
            .join("current"),
        "1.2.0",
    )
    .unwrap();

    let blocks = yaml_blocks(TRIGGERS_DOC);
    assert_eq!(blocks.len(), 1, "the doc holds exactly one rule block");
    for block in &blocks {
        let rule = rule_from_yaml(block);
        let issues = with_cfg(cfg.path(), || {
            apb_core::trigger::check_rule(
                &rule,
                &apb_core::trigger::RuleContext::load(dir.path(), &rule),
            )
        });
        let errors: Vec<String> = issues
            .iter()
            .filter(|i| i.severity == apb_core::validate::Severity::Error)
            .map(|i| format!("{} {}", i.code, i.message))
            .collect();
        assert!(
            errors.is_empty(),
            "the docs rule `{}` must validate: {errors:?}",
            rule.name
        );
    }
}

// --- review round 1: the journal as a person reads it -----------------------

/// Spec "Why it did not start": an event that fails a `where` condition is
/// shown `skipped` by `apb trigger log` (and `show`), with the event's seq
/// and each condition's index and whether it passed, never the
/// condition's literal.
#[test]
fn log_shows_why_an_event_did_not_start() {
    let p = Project::new_registered("tcli40");
    p.save_rule(&rule_doc("news", "tcli40", "{{event.seq}}"));
    p.approve("news");
    p.apb().args(["resume", "--all"]).assert().success();
    p.deliver("e1", &json!({ "action": "closed" }));
    p.apb().args(["resume", "--all"]).assert().success();
    assert!(p.runs().is_empty(), "the event did not start");
    for verb in ["log", "show"] {
        let out = p.apb().args(["trigger", verb, "news"]).assert().success();
        let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
        let line = text
            .lines()
            .find(|l| l.contains("event 1:"))
            .unwrap_or_else(|| panic!("`{verb}` shows the event: {text}"));
        assert!(
            line.contains("the event did not pass the rule's where conditions"),
            "{verb}: {line}"
        );
        assert!(
            line.contains("conditions #0 passed, #1 failed"),
            "{verb}: the type check passed, the `where` condition failed: {line}"
        );
        assert!(!line.contains("opened"), "{verb}: no literal: {line}");
    }
}

/// A rules file that does not verify (here, another project's) is refused
/// with the one remedy that works, naming the file: `set` refuses before
/// it asks anything, and the read-only commands say the same.
#[test]
fn an_unverified_rules_file_names_moving_it_aside() {
    let other = Project::new_registered("tcli41");
    other.save_rule(&rule_doc("news", "tcli41", "{{event.seq}}"));
    let p = Project::new_registered("tcli41");
    fs::create_dir_all(p.rules_path().parent().unwrap()).unwrap();
    fs::copy(other.rules_path(), p.rules_path()).unwrap();
    let before = fs::read(p.rules_path()).unwrap();
    let path = p.rules_path().display().to_string();
    let doc = p.write_rule_doc("news", &rule_doc("news", "tcli41", "{{event.seq}}"));
    p.apb()
        .args(["trigger", "set", "news", "--file", doc.to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "move it aside to write rules again",
        ))
        .stderr(predicates::str::contains(path.as_str()));
    for args in [vec!["trigger", "log", "news"], vec!["trigger", "list"]] {
        p.apb()
            .args(&args)
            .assert()
            .stderr(predicates::str::contains(
                "move it aside to write rules again",
            ))
            .stderr(predicates::str::contains(path.as_str()));
    }
    assert_eq!(fs::read(p.rules_path()).unwrap(), before, "nothing written");
}

/// A save that would push the rules file past what its reader reads is
/// refused `rules_store_full` at exit 2, and the file stays exactly what
/// the last good save wrote.
#[cfg(target_os = "linux")]
#[test]
fn set_refuses_a_save_past_the_rules_read_cap() {
    let p = Project::new_registered("tcli-cap");
    let cap = apb_core::signed_store::NO_LINKS_CAP;
    // The saves up to the cap go through the same core write the command
    // ends in, so the last good save is the file on disk.
    let bulk = "k".repeat(512 * 1024);
    let mut saved = 0;
    for n in 1..40 {
        let before = fs::read(p.rules_path()).unwrap_or_default();
        let res = with_cfg(p.cfg_dir(), || {
            set_rule(
                p.root(),
                &rule_doc(&format!("bulk-{n}"), "tcli-cap", &bulk),
                "test",
                apb_core::clock::Millis::now(),
            )
        });
        match res {
            Ok(_) => {
                assert!((before.len() as u64) < cap, "save {n} passed the cap");
                saved = n;
            }
            Err(e) => {
                assert!(
                    e.to_string().contains("rules_store_full"),
                    "the in-process save is refused the same way: {e}"
                );
                assert!(saved >= 2, "the cap was reached too early: saved={saved}");
                break;
            }
        }
    }
    let as_saved = fs::read(p.rules_path()).unwrap();
    // One more rule of the same bulk through the person's command: the
    // name is typed, the write itself is refused at exit 2.
    let doc = p.write_rule_doc("triage", &rule_doc("triage", "tcli-cap", &bulk));
    let (code, out) = common::on_a_terminal(
        p.root(),
        p.cfg_dir(),
        &["trigger", "set", "triage", "--file", doc.to_str().unwrap()],
        &[],
        b"triage\n",
    )
    .expect("a pseudo terminal");
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("rules_store_full"), "{out}");
    assert_eq!(
        fs::read(p.rules_path()).unwrap(),
        as_saved,
        "nothing written"
    );
}

/// A journal the reader refuses is reported by `apb trigger show`, the
/// same one sentence a sweep pass answers the rule with: the problem once,
/// then the remedy with the path to delete.
#[test]
fn show_reports_a_refused_journal() {
    let p = Project::new_registered("tcli-refused");
    p.save_rule(&rule_doc("triage", "tcli-refused", "{{event.seq}}"));
    let inc = p.entry_of("triage").incarnation;
    let path = p
        .root()
        .join(".apb/trigger-state/log")
        .join(format!("{inc}.jsonl"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, vec![b'x'; 17 << 20]).unwrap();
    let out = p
        .apb()
        .args(["trigger", "show", "triage"])
        .assert()
        .code(0)
        .get_output()
        .stdout
        .to_owned();
    let text = String::from_utf8_lossy(&out).to_string();
    assert!(text.contains("journal: refused:"), "{text}");
    assert!(text.contains("over the read cap"), "{text}");
    assert!(
        text.contains("; remove the rule and save it again, or delete .apb/trigger-state/log/"),
        "{text}"
    );
    assert!(
        !text.contains("cannot be read: the trigger journal of"),
        "the problem is named once: {text}"
    );
}

/// Rule bookkeeping times are milliseconds taken at the real commands:
/// every stored time of `set`, `enable`, `disable` and an approval lies
/// between a now taken before and one taken after the command, and the
/// approval time `show` prints is within a minute of now (W6 item 1).
#[cfg(target_os = "linux")]
#[test]
fn rule_times_are_milliseconds_at_the_real_commands() {
    let p = Project::new_registered("tcli-time");
    let doc = p.write_rule_doc("triage", &rule_doc("triage", "tcli-time", "{{event.seq}}"));
    let file = doc.to_str().unwrap().to_string();

    let before = apb_core::clock::now_ms_u64();
    let (code, out) = common::on_a_terminal(
        p.root(),
        p.cfg_dir(),
        &["trigger", "set", "triage", "--file", &file],
        &[],
        b"triage\n",
    )
    .expect("a pseudo terminal");
    let after = apb_core::clock::now_ms_u64();
    assert_eq!(code, 0, "{out}");
    let created = p.entry_of("triage").created_at;
    assert!(
        created >= before && created <= after,
        "created_at {created} lies in [{before}, {after}]"
    );

    let run = |args: &[&str]| {
        let before = apb_core::clock::now_ms_u64();
        let (code, out) = common::on_a_terminal(p.root(), p.cfg_dir(), args, &[], b"triage\n")
            .expect("a pseudo terminal");
        let after = apb_core::clock::now_ms_u64();
        assert_eq!(code, 0, "{args:?}: {out}");
        (before, after, out)
    };

    let (before, after, _) = run(&["trigger", "enable", "triage"]);
    let updated = p.entry_of("triage").updated_at;
    assert!(
        updated >= before && updated <= after,
        "the enable's updated_at {updated} lies in [{before}, {after}]"
    );

    let (before, after, _) = run(&["trigger", "disable", "triage"]);
    let updated = p.entry_of("triage").updated_at;
    assert!(
        updated >= before && updated <= after,
        "the disable's updated_at {updated} lies in [{before}, {after}]"
    );

    let (before, after, _) = run(&["trigger", "approve", "triage"]);
    let approved_at = p.entry_of("triage").approval.expect("approved").approved_at;
    assert!(
        approved_at >= before && approved_at <= after,
        "approved_at {approved_at} lies in [{before}, {after}]"
    );

    // The approval time `show` prints is within a minute of now: ISO UTC
    // strings of this fixed width read chronologically as text.
    let (_, _, out) = run(&["trigger", "show", "triage"]);
    let printed = out
        .lines()
        .find_map(|l| l.split_once(" at ").map(|(_, ts)| ts.trim().to_string()))
        .expect("the approval line");
    let now = apb_core::clock::now_ms_u64();
    let low = apb_core::dismiss::iso_utc(now.saturating_sub(60_000));
    let high = apb_core::dismiss::iso_utc(now + 60_000);
    assert!(
        printed >= low && printed <= high,
        "the printed approval time {printed} is within a minute of [{low}, {high}]"
    );
}
