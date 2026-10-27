//! Remembered run input: values bound to the params they were saved for,
//! secrets and credential shapes never stored, a corrupt file moved aside
//! by the next save, the legacy draft as the last instruction fallback,
//! memory kept per execution root, and a global playbook's memory bound to
//! this installation.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::run_memory::{
    Incompat, InstructionSource, MemoryKey, RememberInput, ValueStatus, What, copy_key, forget,
    recall, rekey, remember,
};
use apb_core::schema::{Param, Playbook};
use apb_core::scope::Scope;

fn params(yaml: &str) -> Vec<Param> {
    Playbook::from_yaml(&format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\nparams:\n{yaml}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    ))
    .unwrap()
    .params
}

const V1: &str = "  - { name: count, type: int }\n  - { name: mode, type: text }\n  - { name: gone, type: text }\n  - { name: token, type: text, secret: true }\n";

fn key() -> MemoryKey {
    MemoryKey::new(Scope::Project, "p").unwrap()
}

fn values(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
    Some(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

fn store(root: &Path, params: &[Param], pairs: &[(&str, &str)], instruction: Option<&str>) {
    let input = RememberInput {
        params: values(pairs),
        instruction: instruction.map(str::to_string),
    };
    remember(root, &key(), params, "1.0.0", input).unwrap();
}

#[test]
fn a_changed_schema_reports_each_value_and_keeps_the_compatible_ones() {
    let root = tempfile::tempdir().unwrap();
    let v1 = params(V1);
    store(
        root.path(),
        &v1,
        &[("count", "3"), ("mode", "fast"), ("gone", "x")],
        None,
    );

    let v2 = params(
        "  - { name: count, type: enum, options: [low, high] }\n  - { name: mode, type: text }\n  - { name: fresh, type: text }\n  - { name: dflt, type: text, default: d }\n",
    );
    let r = recall(root.path(), &key(), &v2, None).unwrap();
    assert!(r.schema_changed);
    let status = |name: &str| r.values.iter().find(|v| v.name == name).unwrap().status;
    assert_eq!(status("mode"), ValueStatus::Compatible);
    assert_eq!(
        status("count"),
        ValueStatus::Incompatible {
            reason: Incompat::OptionGone
        }
    );
    assert_eq!(
        status("gone"),
        ValueStatus::Incompatible {
            reason: Incompat::Removed
        }
    );
    assert_eq!(r.missing, vec!["count", "fresh"]);
    assert_eq!(r.compatible().collect::<Vec<_>>(), vec![("mode", "fast")]);

    let same = recall(root.path(), &key(), &v1, None).unwrap();
    assert!(!same.schema_changed);
}

#[test]
fn secrets_and_credential_shapes_are_never_stored() {
    let root = tempfile::tempdir().unwrap();
    let v1 = params(V1);
    let input = RememberInput {
        params: values(&[
            ("token", "plain"),
            ("mode", "ghp_abcdefghijklmnop1234"),
            ("gone", "eyJhbGciOi.eyJzdWIiOiIx.c2lnbmF0dXJl"),
            ("count", "many"),
            ("nope", "x"),
        ]),
        instruction: Some("use Bearer abc.DEF-ghi_123456".to_string()),
    };
    let done = remember(root.path(), &key(), &v1, "1.0.0", input).unwrap();
    assert!(done.stored.is_empty());
    let reasons: BTreeMap<_, _> = done
        .refused
        .iter()
        .map(|r| (r.name.as_str(), r.reason.as_str()))
        .collect();
    assert_eq!(reasons["token"], "secret_param");
    assert_eq!(reasons["mode"], "value_looks_secret");
    assert_eq!(reasons["gone"], "value_looks_secret");
    assert_eq!(reasons["count"], "type_changed");
    assert_eq!(reasons["nope"], "unknown_param");
    assert_eq!(reasons["instruction"], "instruction_looks_secret");
    assert!(
        !root.path().join(".apb/memory").exists(),
        "nothing refused is written"
    );

    store(root.path(), &v1, &[("mode", "fast")], None);
    let now_secret = params("  - { name: mode, type: text, secret: true }\n");
    let r = recall(root.path(), &key(), &now_secret, None).unwrap();
    assert_eq!(
        r.values[0].value, None,
        "a value of a param now secret is never shown"
    );
}

#[test]
fn a_param_named_like_a_credential_is_never_stored() {
    let root = tempfile::tempdir().unwrap();
    let v = params("  - { name: db_password, type: text }\n  - { name: mode, type: text }\n");
    let input = RememberInput {
        params: values(&[("db_password", "plain"), ("mode", "fast")]),
        instruction: None,
    };
    let done = remember(root.path(), &key(), &v, "1.0.0", input).unwrap();
    assert_eq!(done.stored, vec!["mode"]);
    assert_eq!(done.refused[0].reason, "param_looks_secret");
}

#[test]
fn a_value_whose_param_became_secret_is_dropped_by_the_next_save() {
    let root = tempfile::tempdir().unwrap();
    let v1 = params(V1);
    store(root.path(), &v1, &[("mode", "fast"), ("gone", "x")], None);

    let v2 =
        params("  - { name: mode, type: text, secret: true }\n  - { name: count, type: int }\n");
    store(root.path(), &v2, &[("count", "3")], None);

    let raw = std::fs::read_to_string(root.path().join(".apb/memory/project/p.json")).unwrap();
    assert!(
        !raw.contains("fast"),
        "the now-secret value is gone from disk"
    );
    let r = recall(root.path(), &key(), &v2, None).unwrap();
    assert!(!r.schema_changed);
    let names: Vec<&str> = r.values.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, vec!["count"]);
}

#[test]
fn a_corrupt_file_reads_empty_and_the_next_save_moves_it_aside() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(".apb/memory/project");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("p.json"), "{ not json").unwrap();

    let r = recall(root.path(), &key(), &params(V1), None).unwrap();
    assert!(r.values.is_empty());
    assert_eq!(r.diagnostics.len(), 1);
    assert!(dir.join("p.json").is_file(), "a read changes nothing");

    let input = RememberInput {
        params: values(&[("mode", "fast")]),
        instruction: None,
    };
    let done = remember(root.path(), &key(), &params(V1), "1.0.0", input).unwrap();
    assert_eq!(done.diagnostics.len(), 1, "the save says what it replaced");
    let aside = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("p.json.corrupt-")
        })
        .count();
    assert_eq!(aside, 1);
    let r = recall(root.path(), &key(), &params(V1), None).unwrap();
    assert_eq!(r.compatible().collect::<Vec<_>>(), vec![("mode", "fast")]);
}

#[test]
fn a_newer_format_is_left_untouched_even_when_its_fields_changed() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(".apb/memory/project");
    std::fs::create_dir_all(&dir).unwrap();
    let newer = r#"{"format": 2, "values": {"mode": {"text": "fast"}}}"#;
    std::fs::write(dir.join("p.json"), newer).unwrap();

    let r = recall(root.path(), &key(), &params(V1), None).unwrap();
    assert!(r.values.is_empty());
    assert_eq!(r.diagnostics.len(), 1);
    let input = RememberInput {
        params: values(&[("mode", "slow")]),
        instruction: None,
    };
    assert!(remember(root.path(), &key(), &params(V1), "1.0.0", input).is_err());
    assert_eq!(std::fs::read_to_string(dir.join("p.json")).unwrap(), newer);
}

#[test]
fn the_legacy_draft_is_the_fallback_until_memory_holds_an_instruction() {
    let root = tempfile::tempdir().unwrap();
    let v1 = params(V1);
    let draft = || Some("from draft".to_string());

    let r = recall(root.path(), &key(), &v1, draft()).unwrap();
    assert_eq!(
        (r.instruction.as_deref(), r.instruction_source),
        (Some("from draft"), InstructionSource::LegacyDraft)
    );

    store(root.path(), &v1, &[], Some(""));
    let r = recall(root.path(), &key(), &v1, draft()).unwrap();
    assert_eq!(
        (r.instruction, r.instruction_source),
        (None, InstructionSource::Memory),
        "an instruction cleared on purpose stops the fallback"
    );

    forget(root.path(), &key(), What::Instruction).unwrap();
    let r = recall(root.path(), &key(), &v1, draft()).unwrap();
    assert_eq!(r.instruction_source, InstructionSource::LegacyDraft);
}

#[test]
fn a_global_playbooks_memory_stays_in_its_project() {
    let _cfg = crate::common::config_sandbox();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let v1 = params(V1);
    let global = MemoryKey::new(Scope::Global, "p").unwrap();
    let input = RememberInput {
        params: values(&[("mode", "fast")]),
        instruction: None,
    };
    remember(a.path(), &global, &v1, "1.0.0", input).unwrap();

    let found = |root: &Path| recall(root, &global, &v1, None).unwrap().values.len();
    assert_eq!(found(a.path()), 1);
    assert_eq!(found(b.path()), 0);
    assert_eq!(recall(a.path(), &key(), &v1, None).unwrap().values.len(), 0);
}

#[test]
fn a_global_playbooks_memory_a_checkout_brought_is_ignored() {
    let _cfg = crate::common::config_sandbox();
    let root = tempfile::tempdir().unwrap();
    let v1 = params(V1);
    let global = MemoryKey::new(Scope::Global, "p").unwrap();
    let input = RememberInput {
        params: values(&[("mode", "fast")]),
        instruction: Some("saved here".to_string()),
    };
    remember(root.path(), &global, &v1, "1.0.0", input).unwrap();
    let file = root.path().join(".apb/memory/global/p.json");
    let saved = std::fs::read_to_string(&file).unwrap();

    // A value changed outside apb, as a repository could ship it: the MAC
    // no longer fits.
    let mut planted: serde_json::Value = serde_json::from_str(&saved).unwrap();
    planted["params"]["mode"] = serde_json::json!("planted");
    std::fs::write(&file, planted.to_string()).unwrap();
    let r = recall(root.path(), &global, &v1, None).unwrap();
    assert!(r.values.is_empty());
    assert_eq!(r.instruction, None);
    assert_eq!(r.diagnostics.len(), 1);

    std::fs::write(&file, saved).unwrap();
    let r = recall(root.path(), &global, &v1, None).unwrap();
    assert_eq!(r.compatible().collect::<Vec<_>>(), vec![("mode", "fast")]);
}

#[test]
fn a_rekey_moves_memory_and_seeds_the_legacy_draft() {
    let _cfg = crate::common::config_sandbox();
    let root = tempfile::tempdir().unwrap();
    let v1 = params(V1);
    store(root.path(), &v1, &[("mode", "fast")], None);
    let global = MemoryKey::new(Scope::Global, "p").unwrap();

    rekey(
        root.path(),
        &key(),
        &global,
        &v1,
        Some("old draft".to_string()),
    )
    .unwrap();
    let moved = recall(root.path(), &global, &v1, None).unwrap();
    assert_eq!(
        moved.compatible().collect::<Vec<_>>(),
        vec![("mode", "fast")]
    );
    assert_eq!(moved.instruction.as_deref(), Some("old draft"));
    assert!(
        recall(root.path(), &key(), &v1, None)
            .unwrap()
            .values
            .is_empty()
    );

    store(root.path(), &v1, &[("mode", "slow")], None);
    assert!(rekey(root.path(), &key(), &global, &v1, None).is_err());
}

#[test]
fn a_rekey_never_carries_a_draft_that_looks_like_a_credential() {
    let _cfg = crate::common::config_sandbox();
    let root = tempfile::tempdir().unwrap();
    let v1 = params(V1);
    store(root.path(), &v1, &[("mode", "fast")], None);
    let global = MemoryKey::new(Scope::Global, "p").unwrap();

    let moved = rekey(
        root.path(),
        &key(),
        &global,
        &v1,
        Some("deploy with ghp_abcdefghijklmnop1234".to_string()),
    )
    .unwrap();
    assert_eq!(moved.diagnostics.len(), 1);
    assert!(!moved.seeded_instruction);
    let moved = recall(root.path(), &global, &v1, None).unwrap();
    assert_eq!(moved.instruction, None);
    assert_eq!(moved.compatible().count(), 1);
}

#[test]
fn a_copy_keeps_the_source_memory() {
    let _cfg = crate::common::config_sandbox();
    let root = tempfile::tempdir().unwrap();
    let v1 = params(V1);
    store(root.path(), &v1, &[("mode", "fast")], None);
    let global = MemoryKey::new(Scope::Global, "p").unwrap();

    copy_key(root.path(), &key(), &global, &v1, None).unwrap();
    for k in [key(), global] {
        let r = recall(root.path(), &k, &v1, None).unwrap();
        assert_eq!(r.compatible().collect::<Vec<_>>(), vec![("mode", "fast")]);
    }
}

#[test]
fn an_unknown_scope_label_is_refused() {
    assert!(MemoryKey::for_label("Global", "p").is_err());
    assert!(MemoryKey::for_label("", "p").is_err());
    assert_eq!(
        MemoryKey::for_label("global", "p").unwrap().scope(),
        Scope::Global
    );
}

#[cfg(unix)]
#[test]
fn the_memory_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    store(root.path(), &params(V1), &[("mode", "fast")], None);
    let meta = std::fs::metadata(root.path().join(".apb/memory/project/p.json")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

/// A move re-checks the values against the params now, so a value a save
/// would refuse never reaches memory bound to this installation.
#[test]
fn a_move_leaves_behind_what_a_save_would_refuse() {
    let _cfg = crate::common::config_sandbox();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(".apb/memory/project");
    std::fs::create_dir_all(&dir).unwrap();
    // A file a checkout brought, with a credential-shaped value.
    let planted = serde_json::json!({
        "format": 1, "scope": "project", "id": "p", "params_schema": "",
        "saved_version": "1.0.0", "saved_at_ms": 0,
        "params": { "mode": "ghp_abcdefghijklmnop1234", "gone": "x" }
    });
    std::fs::write(dir.join("p.json"), planted.to_string()).unwrap();
    let global = MemoryKey::new(Scope::Global, "p").unwrap();
    let v1 = params(V1);

    let moved = rekey(root.path(), &key(), &global, &v1, None).unwrap();
    assert_eq!(moved.diagnostics.len(), 1);
    let r = recall(root.path(), &global, &v1, None).unwrap();
    assert_eq!(r.compatible().collect::<Vec<_>>(), vec![("gone", "x")]);
}

/// A signed global memory file put under another playbook's name does not
/// seed that playbook, and forgetting removes such a file.
#[test]
fn a_global_file_under_another_name_is_ignored_and_can_be_forgotten() {
    let _cfg = crate::common::config_sandbox();
    let root = tempfile::tempdir().unwrap();
    let v1 = params(V1);
    let a = MemoryKey::new(Scope::Global, "a").unwrap();
    let b = MemoryKey::new(Scope::Global, "b").unwrap();
    let input = RememberInput {
        params: values(&[("mode", "fast")]),
        instruction: None,
    };
    remember(root.path(), &a, &v1, "1.0.0", input).unwrap();
    let dir = root.path().join(".apb/memory/global");
    std::fs::copy(dir.join("a.json"), dir.join("b.json")).unwrap();

    let r = recall(root.path(), &b, &v1, None).unwrap();
    assert!(r.values.is_empty());
    assert_eq!(r.diagnostics.len(), 1);
    let notes = forget(root.path(), &b, What::All).unwrap();
    assert_eq!(notes.len(), 1);
    assert!(!dir.join("b.json").exists());
    assert!(dir.join("a.json").is_file());
}

/// Global memory that cannot be bound to this installation is not saved,
/// and the save says so instead of reporting values that would not stick.
#[test]
fn a_global_save_without_an_installation_key_fails() {
    let _cfg = crate::common::config_sandbox();
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("not-a-dir");
    std::fs::write(&blocked, "x").unwrap();
    // SAFETY: the sandbox holds the env lock and restores the variable.
    unsafe { std::env::set_var("APB_CONFIG_DIR", &blocked) };
    let global = MemoryKey::new(Scope::Global, "p").unwrap();
    let input = RememberInput {
        params: values(&[("mode", "fast")]),
        instruction: None,
    };
    assert!(remember(root.path(), &global, &params(V1), "1.0.0", input).is_err());
    assert!(!root.path().join(".apb/memory/global/p.json").exists());
}
