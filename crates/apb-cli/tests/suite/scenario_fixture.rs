//! The project, shims and snapshots the scenario run tests share: a temp
//! project holding playbooks copied from `tests/fixtures/scenario/`, its
//! own config directory, and a `PATH` of shims that leave a line in a
//! sentinel file whenever anything starts one of them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::SystemTime;

use serde_json::Value;

use crate::common::apb_std;

/// The fixture playbooks, one directory each (`playbook.yaml`, `scripts/`,
/// `evals/`). A script listed in [`WRITTEN`] is written at install instead.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scenario")
}

/// The programs a scenario run must never start: the shells, git, `ps`,
/// and every agent CLI apb knows.
fn shimmed() -> Vec<String> {
    let mut names: Vec<String> = ["sh", "bash", "git", "ps", "curl", "env"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for agent in apb_core::agents::all() {
        names.extend(agent.descriptor().bins.iter().map(|b| b.to_string()));
    }
    names.sort();
    names.dedup();
    names
}

pub(crate) struct Project {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) cfg: tempfile::TempDir,
    shims: tempfile::TempDir,
}

impl Project {
    /// A project holding the fixture playbooks `ids`, each installed as
    /// version 1.0.0 and current, with its scenarios next to the versions.
    pub(crate) fn new(ids: &[&str]) -> Project {
        let dir = tempfile::tempdir().unwrap();
        apb_core::registry::init_project(dir.path()).unwrap();
        for id in ids {
            install(dir.path(), id, &fixtures().join(id));
        }
        // Where scenario runs land, made now so that the first run does
        // not change `.apb` itself.
        fs::create_dir_all(dir.path().join(".apb/scenario-runs")).unwrap();
        let shims = tempfile::tempdir().unwrap();
        let sentinel = shims.path().join("sentinel");
        for name in shimmed() {
            let path = shims.path().join(&name);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\necho \"{name} $*\" >> '{}'\nexit 0\n",
                    sentinel.display()
                ),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        Project {
            dir,
            cfg: tempfile::tempdir().unwrap(),
            shims,
        }
    }

    pub(crate) fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The playbook directory of `id`.
    pub(crate) fn playbook(&self, id: &str) -> PathBuf {
        self.root().join(".apb/playbooks").join(id)
    }

    /// Puts `playbook` (its yaml) as version 1.0.0 and current of `id` in
    /// this project's global store (`<config dir>/playbooks/<id>`), with
    /// `scenarios` as its `evals/scenarios/<name>.yaml`.
    pub(crate) fn global_playbook(&self, id: &str, playbook: &str, scenarios: &[(&str, &str)]) {
        let pb = self.cfg.path().join("playbooks").join(id);
        fs::create_dir_all(pb.join("1.0.0")).unwrap();
        fs::write(pb.join("1.0.0/playbook.yaml"), playbook).unwrap();
        fs::write(pb.join("current"), "1.0.0").unwrap();
        let dir = pb.join("evals/scenarios");
        fs::create_dir_all(&dir).unwrap();
        for (name, body) in scenarios {
            fs::write(dir.join(format!("{name}.yaml")), body).unwrap();
        }
    }

    /// Writes scenario `name` of playbook `id`.
    pub(crate) fn scenario(&self, id: &str, name: &str, body: &str) {
        let path = self
            .playbook(id)
            .join(format!("evals/scenarios/{name}.yaml"));
        fs::write(path, body).unwrap();
    }

    /// Replaces text in version 1.0.0 of playbook `id`.
    pub(crate) fn edit_playbook(&self, id: &str, from: &str, to: &str) {
        let path = self.playbook(id).join("1.0.0/playbook.yaml");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "`{from}` is not in the fixture");
        fs::write(&path, text.replacen(from, to, 1)).unwrap();
    }

    /// What the shims saw started, one line each.
    pub(crate) fn started(&self) -> String {
        fs::read_to_string(self.shims.path().join("sentinel")).unwrap_or_default()
    }

    /// `apb <args>` in the project, with its own config directory and
    /// nothing on `PATH` but the shims.
    pub(crate) fn apb(&self, args: &[&str]) -> Output {
        self.apb_env(args, &[])
    }

    pub(crate) fn apb_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = apb_std();
        cmd.args(args)
            .current_dir(self.root())
            .env("APB_CONFIG_DIR", self.cfg.path())
            .env("APB_NO_REGISTRY", "1")
            .env("PATH", self.shims.path())
            .stdin(std::process::Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    /// `apb eval <id> --level <level> --scenario <name> --json <extra>`:
    /// the exit code and the one report.
    pub(crate) fn play(&self, id: &str, level: &str, name: &str, extra: &[&str]) -> (i32, Value) {
        let mut args = vec!["eval", id, "--level", level, "--scenario", name, "--json"];
        args.extend_from_slice(extra);
        let out = self.apb(&args);
        let code = out.status.code().unwrap_or(-1);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "not json ({e}), exit {code}: {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        let report = v["reports"][0].clone();
        assert!(report.is_object(), "no report: {v:#}");
        (code, report)
    }
}

/// Scripts of the fixture playbooks kept here rather than as files:
/// `(playbook, path in the version, text)`.
const WRITTEN: &[(&str, &str, &str)] = &[(
    "kid",
    "scripts/build.sh",
    "#!/bin/sh\necho \"docs built for real\" >&2\nexit 99\n",
)];

fn install(root: &Path, id: &str, from: &Path) {
    let pb = root.join(".apb/playbooks").join(id);
    let version = pb.join("1.0.0");
    fs::create_dir_all(&version).unwrap();
    fs::copy(from.join("playbook.yaml"), version.join("playbook.yaml")).unwrap();
    copy_tree(&from.join("scripts"), &version.join("scripts"));
    for (_, path, text) in WRITTEN.iter().filter(|(pb_id, _, _)| *pb_id == id) {
        let target = version.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, text).unwrap();
    }
    copy_tree(&from.join("evals"), &pb.join("evals"));
    fs::write(pb.join("current"), "1.0.0").unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    let Ok(entries) = fs::read_dir(from) else {
        return;
    };
    fs::create_dir_all(to).unwrap();
    for e in entries.flatten() {
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &target);
        } else {
            fs::copy(e.path(), &target).unwrap();
        }
    }
}

/// Every path below `root`, each file with its size and modification
/// time and each directory with its modification time, leaving out
/// `.apb/scenario-runs`.
pub(crate) fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, Option<SystemTime>)> {
    let mut out = BTreeMap::new();
    // The root's own time shows an entry made and removed at the top level.
    out.insert(
        PathBuf::new(),
        (0, fs::metadata(root).unwrap().modified().ok()),
    );
    walk(root, root, &mut out);
    out
}

#[test]
fn a_file_made_and_removed_at_the_root_changes_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let before = snapshot(dir.path());
    std::thread::sleep(std::time::Duration::from_millis(20));
    let stray = dir.path().join(".temporary");
    fs::write(&stray, b"x").unwrap();
    fs::remove_file(&stray).unwrap();
    assert_ne!(before, snapshot(dir.path()));
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (u64, Option<SystemTime>)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        if rel == Path::new(".apb/scenario-runs") {
            continue;
        }
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            // A directory's own time changes when an entry is made or
            // removed in it, so a file created and deleted again shows.
            // `.apb/scenario-runs` exists from the start (`Project::new`).
            out.insert(rel, (0, meta.modified().ok()));
            walk(root, &path, out);
        } else {
            out.insert(rel, (meta.len(), meta.modified().ok()));
        }
    }
}

/// The check named `name` of a report: `(passed, observed)`.
pub(crate) fn check<'a>(report: &'a Value, name: &str) -> Option<(bool, &'a str)> {
    report["checks"].as_array()?.iter().find_map(|c| {
        (c["check"] == name).then(|| {
            (
                c["passed"].as_bool().unwrap_or(false),
                c["observed"].as_str().unwrap_or(""),
            )
        })
    })
}

/// The names of the failed checks of a report.
pub(crate) fn failed_checks(report: &Value) -> Vec<String> {
    report["checks"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .filter(|c| c["passed"] == false)
                .filter_map(|c| c["check"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The run directory a report names.
pub(crate) fn run_dir(project: &Project, report: &Value) -> PathBuf {
    project
        .root()
        .join(".apb/scenario-runs")
        .join(report["run_id"].as_str().expect("a run id"))
}

/// The journal of a scenario run, one JSON value per line.
pub(crate) fn journal(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}
