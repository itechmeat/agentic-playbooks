//! The workspace stays publishable to crates.io (design of the change
//! `cargo-install`): the six library packages carry the `agentic-playbooks-*`
//! names and keep their library names, every internal dependency names the
//! exact workspace version, and the CLI package stays `apb` in the
//! repository (only `scripts/crates.sh` renames it, in a throwaway
//! checkout, right before packaging).

use std::path::{Path, PathBuf};

use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// `cargo metadata --no-deps` of the workspace, offline.
fn metadata() -> Value {
    let out = std::process::Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--no-deps",
        ])
        .current_dir(repo_root())
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("metadata json")
}

/// The six library packages: directory, package name, library name.
const LIBRARIES: [(&str, &str, &str); 6] = [
    ("apb-core", "agentic-playbooks-core", "apb_core"),
    ("apb-engine", "agentic-playbooks-engine", "apb_engine"),
    ("apb-server", "agentic-playbooks-server", "apb_server"),
    ("apb-mcp", "agentic-playbooks-mcp", "apb_mcp"),
    (
        "apb-mcp-client",
        "agentic-playbooks-mcp-client",
        "apb_mcp_client",
    ),
    ("apb-decide", "agentic-playbooks-decide", "apb_decide"),
];

/// The member packages of the workspace, each with the name of its directory.
fn members(meta: &Value) -> Vec<(String, &Value)> {
    meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|p| {
            let manifest = Path::new(p["manifest_path"].as_str().expect("manifest_path"));
            let dir = manifest
                .parent()
                .and_then(Path::file_name)
                .and_then(|d| d.to_str())
                .expect("crate dir")
                .to_string();
            (dir, p)
        })
        .collect()
}

fn package_in<'a>(meta: &'a Value, dir: &str) -> &'a Value {
    members(meta)
        .into_iter()
        .find(|(d, _)| d == dir)
        .map(|(_, p)| p)
        .unwrap_or_else(|| panic!("a member in crates/{dir}"))
}

#[test]
fn every_internal_dependency_is_exact() {
    let meta = metadata();
    let members = members(&meta);
    let version = members[0].1["version"].as_str().expect("version");
    let by_name: Vec<(&str, &str)> = members
        .iter()
        .map(|(dir, p)| (p["name"].as_str().expect("name"), dir.as_str()))
        .collect();
    let mut checked = 0;
    for (dir, package) in &members {
        assert_eq!(
            package["version"].as_str(),
            Some(version),
            "crates/{dir} takes the workspace version"
        );
        for dep in package["dependencies"].as_array().expect("dependencies") {
            let name = dep["name"].as_str().expect("dependency name");
            let Some((_, target_dir)) = by_name.iter().find(|(n, _)| *n == name) else {
                continue;
            };
            if !dep["kind"].is_null() {
                continue; // dev-dependencies stay path-only and are not published
            }
            checked += 1;
            assert_eq!(
                dep["req"].as_str(),
                Some(format!("={version}").as_str()),
                "crates/{dir}: the dependency on {name} must require ={version}"
            );
            assert_eq!(
                dep["rename"].as_str(),
                Some(*target_dir),
                "crates/{dir}: the dependency on {name} keeps the key {target_dir}"
            );
        }
    }
    assert!(
        checked >= 10,
        "the internal dependencies were found: {checked}"
    );
}

#[test]
fn library_names_stay() {
    let meta = metadata();
    for (dir, package_name, lib_name) in LIBRARIES {
        let package = package_in(&meta, dir);
        assert_eq!(
            package["name"].as_str(),
            Some(package_name),
            "the package in crates/{dir}"
        );
        let libs: Vec<&str> = package["targets"]
            .as_array()
            .expect("targets")
            .iter()
            .filter(|t| {
                t["kind"]
                    .as_array()
                    .is_some_and(|k| k.iter().any(|k| k == "lib"))
            })
            .map(|t| t["name"].as_str().expect("target name"))
            .collect();
        assert_eq!(libs, vec![lib_name], "the library target of crates/{dir}");
    }
}

#[test]
fn the_cli_package_stays_apb() {
    let meta = metadata();
    let cli = package_in(&meta, "apb-cli");
    assert_eq!(cli["name"].as_str(), Some("apb"));
    let kinds: Vec<(String, String)> = cli["targets"]
        .as_array()
        .expect("targets")
        .iter()
        .filter(|t| {
            t["kind"]
                .as_array()
                .is_some_and(|k| k.iter().any(|k| k == "bin" || k == "lib"))
        })
        .map(|t| {
            (
                t["kind"][0].as_str().expect("kind").to_string(),
                t["name"].as_str().expect("name").to_string(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        vec![("bin".to_string(), "apb".to_string())],
        "one bin target apb and no library"
    );
}

/// The paragraph every library's `src/lib.rs` starts with.
const NOT_AN_API: &str = "//! Internal crate of the `apb` command, published only so that
//! `cargo install agentic-playbooks` can build it. It is not a public API:
//! any item may change or disappear in any release, patch releases
//! included. Do not depend on it; use the `apb` command or its MCP server.
";

#[test]
fn libraries_say_they_are_not_an_api() {
    let meta = metadata();
    for (dir, _, _) in LIBRARIES {
        let package = package_in(&meta, dir);
        let description = package["description"].as_str().unwrap_or_default();
        assert!(
            description.contains("Not a public API"),
            "crates/{dir}: the description says it is not a public API: {description:?}"
        );
        for list in ["keywords", "categories"] {
            assert!(
                package[list].as_array().is_none_or(|v| v.is_empty()),
                "crates/{dir} has no {list}"
            );
        }
        let lib = std::fs::read_to_string(repo_root().join(format!("crates/{dir}/src/lib.rs")))
            .expect("read lib.rs");
        assert!(
            lib.starts_with(NOT_AN_API),
            "crates/{dir}/src/lib.rs starts with the not-an-API paragraph"
        );
    }
}

/// The seven names of the publish order line of `scripts/crates.sh`.
fn publish_order() -> Vec<String> {
    let script =
        std::fs::read_to_string(repo_root().join("scripts/crates.sh")).expect("read crates.sh");
    let line = script
        .lines()
        .find_map(|l| l.strip_prefix("PUBLISH_ORDER=\""))
        .expect("the PUBLISH_ORDER line of crates.sh");
    line.trim_end_matches('"')
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_publish_order_is_a_dependency_order() {
    let meta = metadata();
    let published = |name: &str| {
        if name == "apb" {
            "agentic-playbooks".to_string()
        } else {
            name.to_string()
        }
    };
    let packages: Vec<(String, Vec<String>)> = members(&meta)
        .iter()
        .map(|(_, p)| {
            let deps = p["dependencies"]
                .as_array()
                .expect("dependencies")
                .iter()
                .filter(|d| d["kind"].is_null() || d["kind"] == "build")
                .map(|d| d["name"].as_str().expect("name").to_string())
                .collect();
            (published(p["name"].as_str().expect("name")), deps)
        })
        .collect();
    let order = publish_order();
    let mut expected: Vec<String> = packages.iter().map(|(n, _)| n.clone()).collect();
    expected.sort();
    let mut listed = order.clone();
    listed.sort();
    assert_eq!(listed, expected, "the order lists every package once");
    assert_eq!(order.last().map(String::as_str), Some("agentic-playbooks"));
    for (name, deps) in &packages {
        let at = order.iter().position(|n| n == name).expect("listed");
        for dep in deps.iter().filter(|d| order.contains(d)) {
            let dep_at = order.iter().position(|n| n == dep).expect("listed");
            assert!(
                dep_at < at,
                "{dep} must come before {name} in the publish order"
            );
        }
    }
}

/// A directory with `cargo` and `curl` stubs that record every call into
/// `calls` and fail.
#[cfg(unix)]
fn recording_stubs(dir: &Path, calls: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for tool in ["cargo", "curl"] {
        let stub = dir.join(tool);
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho \"{tool} $*\" >> '{}'\nexit 1\n",
                calls.display()
            ),
        )
        .expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

/// `bash <script> <args>` with `PATH` led by `bin`, no registry token
/// inherited, and the extra variables `env`.
fn run_script(
    script: &Path,
    args: &[&str],
    bin: &Path,
    env: &[(&str, &str)],
) -> std::process::Output {
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(script).args(args);
    for (name, _) in std::env::vars() {
        if name == "CARGO_REGISTRY_TOKEN"
            || (name.starts_with("CARGO_REGISTRIES_") && name.ends_with("_TOKEN"))
        {
            cmd.env_remove(name);
        }
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    cmd.env("PATH", path).envs(env.iter().copied());
    cmd.output().expect("run crates.sh")
}

#[cfg(unix)]
#[test]
fn the_script_refuses_a_token_where_code_runs() {
    let root = repo_root();
    let script = root.join("scripts/crates.sh");
    let manifest = std::fs::read(root.join("crates/apb-cli/Cargo.toml")).expect("manifest");
    let lock = std::fs::read(root.join("Cargo.lock")).expect("lockfile");
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("calls");
    recording_stubs(bin.path(), &calls);

    let token_runs: [&[&str]; 4] = [
        &["check"],
        &["preflight", "0.0.0", "/dev/null"],
        &["repack", "0.0.0", "/nonexistent"],
        &["installed", "0.0.0"],
    ];
    for token in ["CARGO_REGISTRY_TOKEN", "CARGO_REGISTRIES_OTHER_TOKEN"] {
        for args in token_runs {
            let out = run_script(&script, args, bin.path(), &[(token, "x")]);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(!out.status.success(), "{args:?} with {token} is refused");
            assert!(
                stderr.contains(&format!(
                    "refusing to run {} with a registry token in the environment",
                    args[0]
                )),
                "{args:?} with {token}: {stderr}"
            );
        }
    }
    let out = run_script(
        &script,
        &["publish", "0.0.0", "/nonexistent"],
        bin.path(),
        &[],
    );
    assert!(!out.status.success(), "publish without a token is refused");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("refusing to run publish without CARGO_REGISTRY_TOKEN"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Another registry's token would reach every program publish starts.
    let out = run_script(
        &script,
        &["publish", "0.0.0", "/nonexistent"],
        bin.path(),
        &[
            ("CARGO_REGISTRY_TOKEN", "x"),
            ("CARGO_REGISTRIES_OTHER_TOKEN", "y"),
        ],
    );
    assert!(
        !out.status.success(),
        "publish with another token is refused"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(
            "refusing to run publish with a CARGO_REGISTRIES_<name>_TOKEN in the environment"
        ),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!calls.exists(), "no cargo or curl call happened");
    assert_eq!(
        std::fs::read(root.join("crates/apb-cli/Cargo.toml")).expect("manifest"),
        manifest
    );
    assert_eq!(
        std::fs::read(root.join("Cargo.lock")).expect("lockfile"),
        lock
    );
}

/// A committed stand-in of the repository in `dir`: the root manifest at the
/// workspace version, the CLI manifest, a lockfile entry, the built
/// dashboard and a copy of `scripts/crates.sh`.
#[cfg(unix)]
fn stand_in_repository(dir: &Path, version: &str) {
    let write = |rel: &str, text: &str| {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    };
    write(
        "Cargo.toml",
        &format!("[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"{version}\"\n"),
    );
    write(
        "crates/apb-cli/Cargo.toml",
        "[package]\nname = \"apb\"\n\n[[bin]]\nname = \"apb\"\npath = \"src/main.rs\"\n",
    );
    write(
        "Cargo.lock",
        &format!("version = 4\n\n[[package]]\nname = \"apb\"\nversion = \"{version}\"\n"),
    );
    write("crates/apb-server/dashboard/index.html", "<html></html>\n");
    let script = std::fs::read_to_string(repo_root().join("scripts/crates.sh")).expect("script");
    write("scripts/crates.sh", &script);
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q"]);
    git(&[
        "add",
        "Cargo.toml",
        "Cargo.lock",
        "crates/apb-cli",
        "scripts",
    ]);
    git(&["commit", "-qm", "stand-in"]);
}

#[cfg(unix)]
#[test]
fn an_all_skipped_plan_runs_no_cargo_command() {
    use std::os::unix::fs::PermissionsExt;

    let meta = metadata();
    let version = members(&meta)[0].1["version"]
        .as_str()
        .expect("version")
        .to_string();
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    stand_in_repository(&repo, &version);
    let script = repo.join("scripts/crates.sh");

    let order = publish_order();
    let verified = tmp.path().join("verified");
    std::fs::create_dir_all(&verified).expect("mkdir");
    let sums: String = order
        .iter()
        .enumerate()
        .map(|(i, name)| format!("{:064x}  {name}-{version}.crate\n", i + 1))
        .collect();
    std::fs::write(verified.join("SHA256SUMS"), &sums).expect("sums");
    let plan = |publishing: Option<&str>| -> String {
        let mut text = format!("{version}\n");
        for name in &order {
            let what = if Some(name.as_str()) == publishing {
                "publish"
            } else {
                "skip"
            };
            text.push_str(&format!("{name} {what}\n"));
        }
        text
    };
    std::fs::write(verified.join("publish-plan"), plan(None)).expect("plan");

    // `cargo` records its call; it answers `metadata` with an empty workspace
    // (the preparation reads it) and fails every other command. `curl`
    // answers every index URL with the version at the sums file's checksum.
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    let calls = tmp.path().join("calls");
    let target = tmp.path().join("target");
    let cargo = format!(
        "#!/bin/sh\necho \"$*\" >> '{calls}'\n\
         if [ \"$1\" = metadata ]; then\n  \
         echo '{{\"packages\":[],\"workspace_members\":[],\"target_directory\":\"{target}\"}}'\n  \
         exit 0\nfi\nexit 1\n",
        calls = calls.display(),
        target = target.display()
    );
    let curl = format!(
        "#!/bin/sh\nout=''\nurl=''\nwhile [ $# -gt 0 ]; do\n  \
         case \"$1\" in -o) out=\"$2\"; shift ;; https://*) url=\"$1\" ;; esac\n  shift\ndone\n\
         case \"$url\" in\n  https://index.crates.io/*)\n    \
         name=\"${{url##*/}}\"\n    \
         sum=$(grep \" $name-{version}.crate$\" '{sums}' | cut -c1-64)\n    \
         printf '{{\"name\":\"%s\",\"vers\":\"{version}\",\"cksum\":\"%s\"}}\\n' \"$name\" \"$sum\" > \"$out\"\n    \
         printf 200 ;;\n  *) printf 404 ;;\nesac\n",
        sums = verified.join("SHA256SUMS").display()
    );
    for (tool, text) in [("cargo", cargo), ("curl", curl)] {
        let stub = bin.join(tool);
        std::fs::write(&stub, text).expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let dir = verified.to_str().expect("utf-8 path");

    let out = run_script(&script, &["repack", &version, dir], &bin, &[]);
    assert!(
        out.status.success(),
        "repack: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(
            "nothing to repack: all seven crates are on crates.io with the verified checksums"
        ),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let out = run_script(
        &script,
        &["publish", &version, dir],
        &bin,
        &[("CARGO_REGISTRY_TOKEN", "x")],
    );
    assert!(
        out.status.success(),
        "publish: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!calls.exists(), "the cargo stub was never called");

    // One crate to publish: repack packs it alone, excluding the other six.
    std::fs::write(
        verified.join("publish-plan"),
        plan(Some("agentic-playbooks-engine")),
    )
    .expect("plan");
    let out = run_script(&script, &["repack", &version, dir], &bin, &[]);
    assert!(!out.status.success(), "the stub's failure stops repack");
    let recorded = std::fs::read_to_string(&calls).expect("cargo was called");
    let package = recorded
        .lines()
        .find(|l| l.starts_with("package "))
        .unwrap_or_else(|| panic!("a package call: {recorded}"));
    assert_eq!(
        package.matches("--exclude").count(),
        6,
        "one --exclude per skip line: {package}"
    );
    assert!(!package.contains("--exclude agentic-playbooks-engine "));
    assert!(package.contains("--no-verify"), "{package}");
    // The stand-in's manifest and lockfile are restored.
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(&repo)
        .output()
        .expect("git status");
    assert!(status.stdout.is_empty(), "{status:?}");
}

/// The first `tool` on `PATH`.
#[cfg(unix)]
fn on_path(tool: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join(tool))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{tool} on PATH"))
}

/// `publish` with a planted `sitecustomize.py`, `usercustomize.py` and
/// `json.py` in the checkout, `PYTHONPATH` pointing at it, and recording
/// stubs of `python3`, `git`, `curl` and `cargo` that log their arguments
/// and environment: the token reaches `cargo publish` and nothing else, and
/// no file of the checkout is imported.
#[cfg(unix)]
#[test]
fn the_token_reaches_cargo_publish_only() {
    use std::os::unix::fs::PermissionsExt;

    const TOKEN: &str = "stand-in-registry-token-4711";
    let meta = metadata();
    let version = members(&meta)[0].1["version"]
        .as_str()
        .expect("version")
        .to_string();
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    stand_in_repository(&repo, &version);
    let script = repo.join("scripts/crates.sh");

    // Any import of these files is the failure, token or not.
    let imported = tmp.path().join("imported");
    let planted = format!(
        "import os\nwith open({:?}, 'a') as f:\n    f.write(__file__ + ' ' + str('{TOKEN}' in repr(os.environ)) + '\\n')\n",
        imported.display().to_string()
    );
    for dir in [repo.clone(), repo.join("scripts")] {
        for module in [
            "sitecustomize.py",
            "usercustomize.py",
            "json.py",
            "tomllib.py",
        ] {
            std::fs::write(dir.join(module), &planted).expect("plant");
        }
    }

    let order = publish_order();
    let publishing = "agentic-playbooks-engine";
    let verified = tmp.path().join("verified");
    std::fs::create_dir_all(&verified).expect("mkdir");
    let sums: String = order
        .iter()
        .enumerate()
        .map(|(i, name)| format!("{:064x}  {name}-{version}.crate\n", i + 1))
        .collect();
    std::fs::write(verified.join("SHA256SUMS"), &sums).expect("sums");
    let mut plan = format!("{version}\n");
    for name in &order {
        let what = if name == publishing {
            "publish"
        } else {
            "skip"
        };
        plan.push_str(&format!("{name} {what}\n"));
    }
    std::fs::write(verified.join("publish-plan"), plan).expect("plan");

    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    let log = tmp.path().join("log");
    let published = tmp.path().join("published");
    let target = tmp.path().join("target");
    let record = |tool: &str| {
        format!(
            "#!/bin/sh\n{{ echo \"== {tool} $*\"; env; }} >> '{}'\n",
            log.display()
        )
    };
    let python = format!(
        "{}exec '{}' \"$@\"\n",
        record("python3"),
        on_path("python3").display()
    );
    let git = format!(
        "{}exec '{}' \"$@\"\n",
        record("git"),
        on_path("git").display()
    );
    let cargo = format!(
        "{}case \"$1\" in\n  \
         metadata) echo '{{\"packages\":[],\"workspace_members\":[],\"target_directory\":\"{target}\"}}' ;;\n  \
         publish) touch '{published}' ;;\n  \
         *) exit 1 ;;\nesac\n",
        record("cargo"),
        target = target.display(),
        published = published.display()
    );
    let curl = format!(
        "{}out=''\nurl=''\nwhile [ $# -gt 0 ]; do\n  \
         case \"$1\" in -o) out=\"$2\"; shift ;; https://*) url=\"$1\" ;; esac\n  shift\ndone\n\
         name=\"${{url##*/}}\"\n\
         if [ \"$name\" = {publishing} ] && [ ! -e '{published}' ]; then printf 404; exit 0; fi\n\
         sum=$(grep \" $name-{version}.crate$\" '{sums}' | cut -c1-64)\n\
         printf '{{\"name\":\"%s\",\"vers\":\"{version}\",\"cksum\":\"%s\"}}\\n' \"$name\" \"$sum\" > \"$out\"\n\
         printf 200\n",
        record("curl"),
        published = published.display(),
        sums = verified.join("SHA256SUMS").display()
    );
    for (tool, text) in [
        ("python3", python),
        ("git", git),
        ("cargo", cargo),
        ("curl", curl),
    ] {
        let stub = bin.join(tool);
        std::fs::write(&stub, text).expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let repo_path = repo.to_str().expect("utf-8 path");
    let out = run_script(
        &script,
        &["publish", &version, verified.to_str().expect("utf-8 path")],
        &bin,
        &[
            ("CARGO_REGISTRY_TOKEN", TOKEN),
            ("PYTHONPATH", repo_path),
            (
                "PYTHONSTARTUP",
                &repo.join("sitecustomize.py").display().to_string(),
            ),
        ],
    );
    assert!(
        out.status.success(),
        "publish: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(published.exists(), "cargo publish ran");
    assert!(
        !imported.exists(),
        "a planted module was imported: {}",
        std::fs::read_to_string(&imported).unwrap_or_default()
    );

    let log = std::fs::read_to_string(&log).expect("log");
    let calls: Vec<&str> = log.split("== ").filter(|c| !c.is_empty()).collect();
    let first = |c: &&str| c.lines().next().unwrap_or_default().to_string();
    let with_token: Vec<String> = calls
        .iter()
        .filter(|c| c.contains(TOKEN))
        .map(first)
        .collect();
    assert_eq!(
        with_token.len(),
        1,
        "only cargo publish sees the token: {with_token:?}"
    );
    assert!(
        with_token[0].starts_with("cargo publish "),
        "{with_token:?}"
    );
    assert!(
        calls.iter().any(|c| first(c).starts_with("cargo publish ")
            && c.lines()
                .any(|l| l == format!("CARGO_REGISTRY_TOKEN={TOKEN}"))),
        "cargo publish gets the token in its environment"
    );
    let pythons: Vec<String> = calls
        .iter()
        .map(first)
        .filter(|c| c.starts_with("python3"))
        .collect();
    assert!(
        pythons.len() >= 3,
        "the preparation ran python3: {pythons:?}"
    );
    assert!(
        pythons.iter().all(|c| c.starts_with("python3 -I -S ")),
        "every python3 runs isolated: {pythons:?}"
    );
    assert!(
        calls.iter().any(|c| first(c).starts_with("git ")),
        "the preparation ran git"
    );
    assert!(
        calls.iter().any(|c| first(c).starts_with("cargo metadata")),
        "the preparation ran cargo metadata"
    );
    // The stand-in's manifest and lockfile are restored.
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(&repo)
        .output()
        .expect("git status");
    assert!(status.stdout.is_empty(), "{status:?}");
}
