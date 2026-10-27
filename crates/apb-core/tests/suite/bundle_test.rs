use std::fs;
use std::path::Path;

use apb_core::bundle::{BundleError, PlaybookBundle, export_bundle, import_bundle};
use apb_core::registry::{Registry, init_project};
use apb_core::versioning::VersioningError;

const PLAYBOOK: &str = r#"schema: 1
id: va
name: Valid Agent
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;

const LAYOUT: &str = "nodes:\n  start: { x: 0, y: 0 }\n  w: { x: 120, y: 0 }\n";

fn seed(root: &Path) {
    init_project(root).unwrap();
    let base = root.join(".apb/playbooks/va");
    fs::create_dir_all(base.join("1.0.0")).unwrap();
    fs::write(base.join("1.0.0/playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(base.join("current"), "1.0.0").unwrap();
    fs::create_dir_all(base.join("layouts")).unwrap();
    fs::write(base.join("layouts/1.0.0.yaml"), LAYOUT).unwrap();
}

#[test]
fn export_import_round_trip_preserves_playbook_and_layout() {
    let _cfg = crate::common::config_sandbox();
    // Export from project A.
    let a = tempfile::tempdir().unwrap();
    seed(a.path());
    let bundle = export_bundle(a.path(), "va", None).unwrap();
    assert_eq!(bundle.apb_bundle, 2);
    assert_eq!(bundle.id, "va");
    assert_eq!(bundle.version, "1.0.0");
    assert!(bundle.playbook.contains("agent_task"));
    assert!(bundle.layout.is_some(), "layout must be captured");
    // The seeded version has no scripts dir: still format 2, empty list.
    assert!(
        bundle.scripts.is_empty(),
        "a version without scripts must export scripts: [], got {:?}",
        bundle.scripts
    );

    // JSON round-trip.
    let json = bundle.to_json().unwrap();
    assert!(
        json.contains("\"scripts\""),
        "format 2 always writes the scripts field"
    );
    let back = PlaybookBundle::from_json(&json).unwrap();
    assert_eq!(back.playbook, bundle.playbook);
    assert_eq!(back.layout, bundle.layout);

    // Import into a clean project B: new project -> version 1.0.0.
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let assigned = import_bundle(b.path(), &back, true).unwrap().version;
    assert_eq!(assigned, "1.0.0");

    // The playbook and layout are in place in B.
    let reg = Registry::open(b.path()).unwrap();
    let loaded = reg.load("va", None).unwrap();
    assert_eq!(loaded.version, "1.0.0");
    assert!(loaded.playbook.nodes.iter().any(|n| n.id == "w"));
    assert!(loaded.layout.is_some(), "imported layout must be restored");
}

#[test]
fn an_unchanged_import_keeps_the_current_layout() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    seed(b.path());
    let mut bundle = export_bundle(b.path(), "va", None).unwrap();
    bundle.version = String::new();
    bundle.layout = Some(yaml_serde::from_str("nodes:\n  start: { x: 999, y: 999 }\n").unwrap());

    let saved = import_bundle(b.path(), &bundle, true).unwrap();
    assert!(saved.unchanged);
    assert_eq!(saved.version, "1.0.0");
    assert_eq!(
        fs::read_to_string(b.path().join(".apb/playbooks/va/layouts/1.0.0.yaml")).unwrap(),
        LAYOUT
    );
}

#[test]
fn import_invalid_playbook_surfaces_formatted_validation_lines_through_bundle_error() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let invalid = PLAYBOOK.replace(r#"prompt: "do""#, r#"prompt: "{{outputs.x}}""#);
    let bundle = PlaybookBundle {
        apb_bundle: 1,
        id: "va".to_string(),
        version: "1.0.0".to_string(),
        playbook: invalid,
        layout: None,
        scripts: Vec::new(),
    };

    let err = import_bundle(b.path(), &bundle, true).unwrap_err();
    assert!(matches!(
        err,
        BundleError::Versioning(VersioningError::Validation(_))
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("validation failed:"),
        "expected `validation failed:` to survive through BundleError's Display, got: {msg}"
    );
    assert!(
        msg.lines()
            .any(|l| l.starts_with("- V13 error (node `w`):")),
        "expected a `- V13 error (node `w`):` line, got: {msg}"
    );
}

#[test]
fn import_rejects_unknown_schema() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let bad = PlaybookBundle {
        apb_bundle: 99,
        id: "va".to_string(),
        version: "1.0.0".to_string(),
        playbook: PLAYBOOK.to_string(),
        layout: None,
        scripts: Vec::new(),
    };
    let err = import_bundle(b.path(), &bad, true).unwrap_err();
    assert!(
        format!("{err}").contains("unsupported bundle schema"),
        "got: {err}"
    );
}

#[test]
fn import_honors_bundle_version_1_0_0_on_a_fresh_id() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let bundle = PlaybookBundle {
        apb_bundle: 1,
        id: "va".to_string(),
        version: "1.0.0".to_string(),
        playbook: PLAYBOOK.to_string(),
        layout: None,
        scripts: Vec::new(),
    };

    let assigned = import_bundle(b.path(), &bundle, true).unwrap().version;
    assert_eq!(assigned, "1.0.0");
}

#[test]
fn import_honors_a_free_non_default_bundle_version() {
    let _cfg = crate::common::config_sandbox();
    // Seed an existing playbook whose auto-assign scheme would never land on
    // 9.9.9 next (it would compute 1.1.0). A free, non-sequential bundle
    // version must still be honored exactly.
    let b = tempfile::tempdir().unwrap();
    seed(b.path());
    let bundle = PlaybookBundle {
        apb_bundle: 1,
        id: "va".to_string(),
        version: "9.9.9".to_string(),
        playbook: PLAYBOOK.to_string(),
        layout: None,
        scripts: Vec::new(),
    };

    let assigned = import_bundle(b.path(), &bundle, true).unwrap().version;
    assert_eq!(assigned, "9.9.9");
}

#[test]
fn second_import_of_the_same_version_errors_naming_the_conflict() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let bundle = PlaybookBundle {
        apb_bundle: 1,
        id: "va".to_string(),
        version: "1.0.0".to_string(),
        playbook: PLAYBOOK.to_string(),
        layout: None,
        scripts: Vec::new(),
    };

    let first = import_bundle(b.path(), &bundle, true).unwrap().version;
    assert_eq!(first, "1.0.0");

    let err = import_bundle(b.path(), &bundle, true).unwrap_err();
    assert!(
        matches!(err, BundleError::Versioning(VersioningError::Conflict(_))),
        "got {err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("va"), "expected id `va` named, got: {msg}");
    assert!(
        msg.contains("1.0.0"),
        "expected version `1.0.0` named, got: {msg}"
    );
}

#[test]
fn version_less_bundle_auto_assigns_exactly_as_today() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    seed(b.path());
    let bundle = PlaybookBundle {
        apb_bundle: 1,
        id: "va".to_string(),
        version: String::new(),
        playbook: PLAYBOOK.replace("name: Valid Agent", "name: Valid Agent Two"),
        layout: None,
        scripts: Vec::new(),
    };

    // Seeded project already has `va` @ 1.0.0 current; a version-less bundle
    // falls back to the ordinary next-minor auto-assign.
    let assigned = import_bundle(b.path(), &bundle, true).unwrap().version;
    assert_eq!(assigned, "1.1.0");

    // The same definition again is no change (issue #67 item 9): nothing is
    // written and the import lands on the current version.
    let again = import_bundle(b.path(), &bundle, true).unwrap().version;
    assert_eq!(again, "1.1.0");
    let versions = apb_core::registry::list_versions(&b.path().join(".apb/playbooks/va")).unwrap();
    assert_eq!(versions, vec!["1.0.0".to_string(), "1.1.0".to_string()]);
}

#[test]
fn import_rejects_a_non_semver_bundle_version_cleanly() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let bundle = PlaybookBundle {
        apb_bundle: 1,
        id: "va".to_string(),
        version: "not-semver".to_string(),
        playbook: PLAYBOOK.to_string(),
        layout: None,
        scripts: Vec::new(),
    };

    let err = import_bundle(b.path(), &bundle, true).unwrap_err();
    assert!(
        matches!(err, BundleError::Versioning(VersioningError::Conflict(_))),
        "got {err:?}"
    );
    assert!(err.to_string().contains("not-semver"), "got: {err}");
}

// --- bundle format 2: the version's scripts --------------------------------

/// Project with playbook `va` saved through apb and the given files written
/// into its version's `scripts/` (`(path, content, executable)`).
fn seed_with_scripts(root: &Path, files: &[(&str, &str, bool)]) -> String {
    init_project(root).unwrap();
    let saved = apb_core::versioning::save_definition(root, "va", PLAYBOOK, None, true).unwrap();
    let scripts = root
        .join(".apb/playbooks/va")
        .join(&saved.version)
        .join("scripts");
    for (path, content, executable) in files {
        let dst = scripts.join(path);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, content).unwrap();
        set_mode(&dst, *executable);
    }
    saved.version
}

fn set_mode(path: &Path, executable: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = (path, executable);
}

fn trust_digest(root: &Path, id: &str, version: &str) -> String {
    Registry::open(root)
        .unwrap()
        .load(id, Some(version))
        .unwrap()
        .trust_digest()
        .unwrap()
}

fn script(path: &str, content: &[u8]) -> apb_core::bundle::BundleScript {
    use base64::Engine as _;
    apb_core::bundle::BundleScript {
        path: path.to_string(),
        content_base64: base64::engine::general_purpose::STANDARD.encode(content),
        executable: false,
    }
}

fn bundle_with(scripts: Vec<apb_core::bundle::BundleScript>) -> PlaybookBundle {
    PlaybookBundle {
        apb_bundle: 2,
        id: "va".to_string(),
        version: "1.0.0".to_string(),
        playbook: PLAYBOOK.to_string(),
        layout: None,
        scripts,
    }
}

#[test]
fn scripts_round_trip_with_content_mode_and_digest() {
    let _cfg = crate::common::config_sandbox();
    let a = tempfile::tempdir().unwrap();
    let version = seed_with_scripts(
        a.path(),
        &[
            ("check.sh", "#!/bin/sh\nexit 0\n", true),
            ("lib/data.json", "{\"n\": 1}\n", false),
        ],
    );
    let bundle = export_bundle(a.path(), "va", None).unwrap();
    let paths: Vec<&str> = bundle.scripts.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, ["check.sh", "lib/data.json"]);

    let back = PlaybookBundle::from_json(&bundle.to_json().unwrap()).unwrap();
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let assigned = import_bundle(b.path(), &back, true).unwrap().version;
    assert_eq!(assigned, version);

    let scripts = b
        .path()
        .join(".apb/playbooks/va")
        .join(&assigned)
        .join("scripts");
    assert_eq!(
        fs::read_to_string(scripts.join("check.sh")).unwrap(),
        "#!/bin/sh\nexit 0\n"
    );
    assert_eq!(
        fs::read_to_string(scripts.join("lib/data.json")).unwrap(),
        "{\"n\": 1}\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &str| fs::metadata(scripts.join(p)).unwrap().permissions().mode() & 0o111;
        assert_ne!(mode("check.sh"), 0, "the executable bit survives");
        assert_eq!(mode("lib/data.json"), 0, "a plain file stays plain");
    }
    assert_eq!(
        trust_digest(b.path(), "va", &assigned),
        trust_digest(a.path(), "va", &version),
        "the imported version is the same content the source version is"
    );
}

#[test]
fn an_imported_version_is_never_trusted_with_or_without_scripts() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let store = || apb_core::trust::TrustStore::load();

    let with_scripts = import_bundle(
        b.path(),
        &bundle_with(vec![script("x.sh", b"echo\n")]),
        true,
    )
    .unwrap()
    .version;
    assert!(
        !store().is_approved(&trust_digest(b.path(), "va", &with_scripts)),
        "imported scripts wait for an explicit approval"
    );

    let mut yaml_only = bundle_with(Vec::new());
    yaml_only.id = "vb".into();
    yaml_only.playbook = PLAYBOOK.replace("id: va", "id: vb");
    let plain = import_bundle(b.path(), &yaml_only, true).unwrap().version;
    assert!(
        !store().is_approved(&trust_digest(b.path(), "vb", &plain)),
        "a bundle from elsewhere is not vouched for by importing it"
    );

    let mut format_1 = yaml_only.clone();
    format_1.apb_bundle = 1;
    format_1.id = "vc".into();
    format_1.playbook = PLAYBOOK.replace("id: va", "id: vc");
    let old = import_bundle(b.path(), &format_1, true).unwrap().version;
    assert!(!store().is_approved(&trust_digest(b.path(), "vc", &old)));
}

/// The file names under `<id>/<version>/scripts` of project `root`.
fn script_names(root: &Path, version: &str) -> Vec<String> {
    let dir = root.join(".apb/playbooks/va").join(version).join("scripts");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .map(|it| {
            it.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn a_format_2_bundle_without_scripts_gives_a_version_without_scripts() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    seed_with_scripts(b.path(), &[("local.sh", "echo local\n", true)]);
    let mut bundle = bundle_with(Vec::new());
    bundle.version = String::new();
    bundle.playbook = PLAYBOOK.replace("name: Valid Agent", "name: Imported");

    let saved = import_bundle(b.path(), &bundle, true).unwrap();
    assert!(!saved.unchanged);
    assert_eq!(
        script_names(b.path(), &saved.version),
        Vec::<String>::new(),
        "the current version's scripts must not ride along"
    );

    // The same YAML as the current version, but the bundle lists no
    // scripts while the current version has one: that is a change, not the
    // current version again.
    let c = tempfile::tempdir().unwrap();
    seed_with_scripts(c.path(), &[("local.sh", "echo local\n", true)]);
    let mut same_yaml = bundle_with(Vec::new());
    same_yaml.version = String::new();
    let again = import_bundle(c.path(), &same_yaml, true).unwrap();
    assert!(
        !again.unchanged,
        "differing scripts are not the same version"
    );
    assert_eq!(script_names(c.path(), &again.version), Vec::<String>::new());
}

#[test]
fn a_format_1_bundle_keeps_the_current_scripts_and_may_not_carry_any() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    seed_with_scripts(b.path(), &[("local.sh", "echo local\n", true)]);
    let mut bundle = bundle_with(Vec::new());
    bundle.apb_bundle = 1;
    bundle.version = String::new();
    bundle.playbook = PLAYBOOK.replace("name: Valid Agent", "name: Imported");

    let saved = import_bundle(b.path(), &bundle, true).unwrap();
    assert!(!saved.unchanged);
    assert_eq!(script_names(b.path(), &saved.version), ["local.sh"]);

    let mut smuggled = bundle_with(vec![script("x.sh", b"echo\n")]);
    smuggled.apb_bundle = 1;
    smuggled.version = String::new();
    let err = import_bundle(b.path(), &smuggled, true).unwrap_err();
    assert!(matches!(err, BundleError::Format(_)), "got {err}");
}

#[test]
fn a_format_1_bundle_still_imports() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    // A file written by an older apb: no `scripts` key at all.
    let json = serde_json::json!({
        "apb_bundle": 1,
        "id": "va",
        "version": "1.0.0",
        "playbook": PLAYBOOK,
    })
    .to_string();
    let bundle = PlaybookBundle::from_json(&json).unwrap();
    assert_eq!(
        import_bundle(b.path(), &bundle, true).unwrap().version,
        "1.0.0"
    );
    assert!(Registry::open(b.path()).unwrap().load("va", None).is_ok());
}

#[test]
fn import_rejects_every_unsafe_script_path_and_writes_nothing() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let cases: Vec<Vec<apb_core::bundle::BundleScript>> = vec![
        vec![script("/etc/passwd", b"x")],
        vec![script("../escape.sh", b"x")],
        vec![script("a/../../escape.sh", b"x")],
        vec![script("a//b.sh", b"x")],
        vec![script("", b"x")],
        vec![script("./a.sh", b"x")],
        vec![script("a\\b.sh", b"x")],
        vec![script("a.sh", b"x"), script("a.sh", b"y")],
        vec![script("a", b"x"), script("a/b.sh", b"y")],
        vec![script("run.sh", b"x"), script("RUN.sh", b"y")],
        vec![script("A", b"x"), script("a/b.sh", b"y")],
        vec![script("a\0b.sh", b"x")],
        vec![script(&"n".repeat(256), b"x")],
        vec![apb_core::bundle::BundleScript {
            path: "a.sh".into(),
            content_base64: "not base64 at all".into(),
            executable: false,
        }],
    ];
    for scripts in cases {
        let label = format!("{:?}", scripts.iter().map(|s| &s.path).collect::<Vec<_>>());
        let err = import_bundle(b.path(), &bundle_with(scripts), true).unwrap_err();
        assert!(
            matches!(err, BundleError::Scripts(_)),
            "{label}: expected a scripts error, got {err}"
        );
    }
    assert!(
        !b.path().join(".apb/playbooks/va").exists(),
        "a rejected bundle writes nothing"
    );
}

#[test]
fn import_rejects_scripts_over_the_tree_limits() {
    let _cfg = crate::common::config_sandbox();
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let limits = apb_core::content::TreeLimits::default();

    let too_many: Vec<_> = (0..=limits.max_files)
        .map(|i| script(&format!("f{i}.sh"), b"x"))
        .collect();
    // The oversized payloads are built as base64 text directly (`AAAA` is
    // three zero bytes): the import refuses them from their declared size,
    // before decoding anything.
    let named = |path: String, bytes: u64| apb_core::bundle::BundleScript {
        path,
        content_base64: "AAAA".repeat(bytes.div_ceil(3) as usize),
        executable: false,
    };
    let big = vec![named("big.bin".into(), limits.max_file_bytes + 1)];
    // Every file is within the per-file limit, together they are not.
    let per_file = limits.max_file_bytes / 3 * 3;
    let over_total: Vec<_> = (0..=limits.max_total_bytes / per_file)
        .map(|i| named(format!("part{i}.bin"), per_file))
        .collect();
    for (scripts, expected) in [
        (too_many, "entries"),
        (big, "per-file limit"),
        (over_total, "total limit"),
    ] {
        let err = import_bundle(b.path(), &bundle_with(scripts), true).unwrap_err();
        assert!(
            matches!(err, BundleError::Scripts(_)) && err.to_string().contains(expected),
            "expected `{expected}`, got {err}"
        );
    }
    assert!(!b.path().join(".apb/playbooks/va").exists());
}

#[test]
fn export_refuses_what_a_file_list_cannot_carry() {
    let _cfg = crate::common::config_sandbox();
    let limits = apb_core::content::TreeLimits::default();
    let big = "x".repeat(limits.max_file_bytes as usize + 1);
    let over_limit = tempfile::tempdir().unwrap();
    seed_with_scripts(over_limit.path(), &[("big.txt", &big, false)]);
    let empty_dir = tempfile::tempdir().unwrap();
    let v = seed_with_scripts(empty_dir.path(), &[("a.sh", "x", false)]);
    fs::create_dir_all(
        empty_dir
            .path()
            .join(".apb/playbooks/va")
            .join(&v)
            .join("scripts/empty"),
    )
    .unwrap();
    let mut roots = vec![over_limit, empty_dir];
    #[cfg(unix)]
    {
        let link = tempfile::tempdir().unwrap();
        let v = seed_with_scripts(link.path(), &[("a.sh", "x", false)]);
        std::os::unix::fs::symlink(
            "a.sh",
            link.path()
                .join(".apb/playbooks/va")
                .join(&v)
                .join("scripts/b.sh"),
        )
        .unwrap();
        roots.push(link);
    }
    for root in &roots {
        let err = export_bundle(root.path(), "va", None).unwrap_err();
        assert!(matches!(err, BundleError::Scripts(_)), "got {err}");
    }
}
