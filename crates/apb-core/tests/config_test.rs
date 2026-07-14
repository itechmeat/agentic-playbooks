use apb_core::config::{GlobalConfig, program_in_path};

// program_in_path via a direct path (doesn't touch the PATH env, so it's safe
// as a standalone #[test]): on Unix a regular file without the exec bit does
// not count as a program, while one with the bit set does.
#[test]
fn program_in_path_checks_executable_bit() {
    let dir = tempfile::tempdir().unwrap();
    let exec = dir.path().join("runme");
    std::fs::write(&exec, "#!/bin/sh\n").unwrap();
    let noexec = dir.path().join("data.txt");
    std::fs::write(&noexec, "x").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&exec).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&exec, p).unwrap();
    }
    assert!(
        program_in_path(exec.to_str().unwrap()),
        "executable file must be found"
    );
    #[cfg(unix)]
    assert!(
        !program_in_path(noexec.to_str().unwrap()),
        "non-executable file must be rejected on unix"
    );
    assert!(
        !program_in_path(dir.path().join("absent").to_str().unwrap()),
        "absent file must not be found"
    );
}

// Loading the global config via APB_CONFIG_DIR. All phases run sequentially
// in one test: env var changes are process-global, so this can't be split
// into parallel #[test]s (it would race over APB_CONFIG_DIR).
#[test]
fn global_config_load_paths() {
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", dir.path());
    }

    // 1. No file - empty default (the no-config path stays functional).
    let empty = GlobalConfig::load().unwrap();
    assert!(empty.agents.is_empty());
    assert!(empty.port.is_none());

    // 2. Valid config - all sections parse (schema 2: no executors).
    let yaml = r#"
port: 8080
agents:
  claude-code: { program: /usr/bin/claude }
  mock: {}
runners:
  ts: [bun, deno]
"#;
    std::fs::write(dir.path().join("config.yaml"), yaml).unwrap();
    let cfg = GlobalConfig::load().unwrap();
    assert_eq!(cfg.port, Some(8080));
    assert_eq!(
        cfg.agent_program("claude-code").as_deref(),
        Some("/usr/bin/claude")
    );
    assert_eq!(
        cfg.agent_program("mock"),
        None,
        "agent described without program yields None"
    );
    assert_eq!(
        cfg.runners.get("ts").unwrap(),
        &vec!["bun".to_string(), "deno".to_string()]
    );

    // 3. A legacy config with executors/default_executor (schema 1) still
    // LOADS (otherwise deny_unknown_fields would block every run prior to
    // migration); those fields are ignored, other sections still parse.
    let legacy = "port: 9090\ndefault_executor: cheap\nexecutors:\n  cheap: { agent: mock, model: haiku }\nagents:\n  mock: {}\n";
    std::fs::write(dir.path().join("config.yaml"), legacy).unwrap();
    let cfg = GlobalConfig::load().expect("legacy executors keys must not break load");
    assert_eq!(cfg.port, Some(9090));
    assert!(cfg.agents.contains_key("mock"));

    // 4. Malformed YAML is an Err, not a silent default.
    std::fs::write(dir.path().join("config.yaml"), "port: [not, a, number]\n").unwrap();
    let broken = GlobalConfig::load();

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
    assert!(broken.is_err(), "malformed config must surface an error");
}
