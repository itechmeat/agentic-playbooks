use super::*;

/// Runs `f` with a config dir of its own (the installation key the MACs
/// use), under the crate's env lock.
fn keyed<R>(f: impl FnOnce() -> R) -> R {
    let _lock = crate::env_test_lock();
    let cfg = tempfile::tempdir().unwrap();
    let prev = std::env::var_os("APB_CONFIG_DIR");
    // SAFETY: the env lock is held.
    unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
    let out = f();
    // SAFETY: as above.
    unsafe {
        match prev {
            Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
            None => std::env::remove_var("APB_CONFIG_DIR"),
        }
    }
    out
}

#[test]
fn a_key_is_checked_without_repeating_it() {
    assert!(StartKey::parse("digest-2026-11-09").is_ok());
    assert!(StartKey::parse(&"x".repeat(KEY_MAX_CHARS)).is_ok());
    let token = format!(
        "ghp_{}",
        "a1B2c3D4e5".repeat(4).chars().take(36).collect::<String>()
    );
    for bad in [
        String::new(),
        "x".repeat(KEY_MAX_CHARS + 1),
        "run\u{7}1".into(),
        "run\u{200B}1".into(),
        " run-1".into(),
        "run-1 ".into(),
        token.clone(),
    ] {
        let e = StartKey::parse(&bad).unwrap_err();
        assert!(bad.is_empty() || !e.contains(&bad), "{e}");
    }
    assert!(!format!("{:?}", StartKey::parse("secret-ish").unwrap()).contains("secret"));
}

#[test]
fn params_in_any_order_are_one_request() {
    keyed(|| {
        let mut a = BTreeMap::new();
        a.insert("b".to_string(), "2".to_string());
        a.insert("a".to_string(), "1".to_string());
        let mut b = BTreeMap::new();
        b.insert("a".to_string(), "1".to_string());
        b.insert("b".to_string(), "2".to_string());
        let req = |params| {
            StartRequest::capture(&StartRequestFields {
                v: REQUEST_V,
                params,
                ..Default::default()
            })
            .unwrap()
        };
        assert_eq!(req(a), req(b));
    })
}

/// From one base, a copy that differs in exactly one field is another
/// request, for every field. The struct is destructured without `..`, so
/// a field added without a row here does not compile.
#[test]
fn every_field_changes_the_request() {
    keyed(|| {
        let base = StartRequestFields {
            v: REQUEST_V,
            version: None,
            params: BTreeMap::new(),
            instruction: None,
            worktree: None,
            overrides: None,
            execution: None,
            supervision: "autonomous".into(),
            cache: "auto".into(),
            max_parallel: None,
            max_patches_per_run: None,
            context_max_bytes: None,
            context_compact_model: None,
            deadline_ms: None,
            budget: None,
            unattended: false,
            allow_shared_workdir: false,
            continued_from: None,
            external_data: false,
        };
        let StartRequestFields {
            v: _,
            version: _,
            params: _,
            instruction: _,
            worktree: _,
            overrides: _,
            execution: _,
            supervision: _,
            cache: _,
            max_parallel: _,
            max_patches_per_run: _,
            context_max_bytes: _,
            context_compact_model: _,
            deadline_ms: _,
            budget: _,
            unattended: _,
            allow_shared_workdir: _,
            continued_from: _,
            external_data: _,
        } = &base;
        let variants: Vec<(&str, StartRequestFields)> = vec![
            (
                "v",
                StartRequestFields {
                    v: 2,
                    ..base.clone()
                },
            ),
            (
                "version",
                StartRequestFields {
                    version: Some("1.0.0".into()),
                    ..base.clone()
                },
            ),
            (
                "params",
                StartRequestFields {
                    params: BTreeMap::from([("a".to_string(), "1".to_string())]),
                    ..base.clone()
                },
            ),
            (
                "instruction",
                StartRequestFields {
                    instruction: Some("i".into()),
                    ..base.clone()
                },
            ),
            (
                "worktree",
                StartRequestFields {
                    worktree: Some("w".into()),
                    ..base.clone()
                },
            ),
            (
                "overrides",
                StartRequestFields {
                    overrides: Some("o".into()),
                    ..base.clone()
                },
            ),
            (
                "execution",
                StartRequestFields {
                    execution: Some("host".into()),
                    ..base.clone()
                },
            ),
            (
                "supervision",
                StartRequestFields {
                    supervision: "supervised".into(),
                    ..base.clone()
                },
            ),
            (
                "cache",
                StartRequestFields {
                    cache: "off".into(),
                    ..base.clone()
                },
            ),
            (
                "max_parallel",
                StartRequestFields {
                    max_parallel: Some(2),
                    ..base.clone()
                },
            ),
            (
                "max_patches_per_run",
                StartRequestFields {
                    max_patches_per_run: Some(2),
                    ..base.clone()
                },
            ),
            (
                "context_max_bytes",
                StartRequestFields {
                    context_max_bytes: Some(9),
                    ..base.clone()
                },
            ),
            (
                "context_compact_model",
                StartRequestFields {
                    context_compact_model: Some("m".into()),
                    ..base.clone()
                },
            ),
            (
                "deadline_ms",
                StartRequestFields {
                    deadline_ms: Some(60_000),
                    ..base.clone()
                },
            ),
            (
                "budget",
                StartRequestFields {
                    budget: Some(serde_json::json!({"tokens": 1})),
                    ..base.clone()
                },
            ),
            (
                "unattended",
                StartRequestFields {
                    unattended: true,
                    ..base.clone()
                },
            ),
            (
                "allow_shared_workdir",
                StartRequestFields {
                    allow_shared_workdir: true,
                    ..base.clone()
                },
            ),
            (
                "continued_from",
                StartRequestFields {
                    continued_from: Some("r".into()),
                    ..base.clone()
                },
            ),
            (
                "external_data",
                StartRequestFields {
                    external_data: true,
                    ..base.clone()
                },
            ),
        ];
        let tag = |f: &StartRequestFields| StartRequest::capture(f).unwrap().tag().to_string();
        let base_tag = tag(&base);
        for (name, f) in &variants {
            assert_ne!(tag(f), base_tag, "`{name}` does not change the request");
        }
        assert_eq!(variants.len(), 19);
    })
}

fn entry(key: &str, at_ms: u64) -> StartKeyEntry {
    StartKeyEntry {
        key: key.into(),
        request: "r".into(),
        state: EntryState::Bound,
        reservation: None,
        run_id: Some("run-1".into()),
        at_ms,
        reused: 0,
    }
}

#[test]
fn an_entry_past_its_days_is_gone() {
    const DAY: u64 = 86_400_000;
    let mut f = StartKeysFile::fresh(Scope::Project, "p");
    f.entries = vec![entry("old", 0), entry("new", 6 * DAY)];
    let now = 8 * DAY;
    assert!(f.find("old", now, 7).is_none());
    assert!(f.find("new", now, 7).is_some());
    assert!(f.find("old", now, 9).is_some());
    f.expire(now, 7);
    assert_eq!(f.entries.len(), 1);
    assert_eq!(f.entries[0].key, "new");
}
