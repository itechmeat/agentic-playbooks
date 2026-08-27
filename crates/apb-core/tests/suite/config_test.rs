use apb_core::config::{GlobalConfig, program_in_path};

use crate::common::env_lock;

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
// into parallel #[test]s (it would race over APB_CONFIG_DIR). This test
// previously ran unguarded (safe only because it had its own test binary);
// now that it shares a process with every other module in this crate's
// integration binary, it must take the shared lock too.
#[test]
fn global_config_load_paths() {
    let _l = env_lock();
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

#[test]
fn suggestions_section_loads_and_validates() {
    let _lock = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", dir.path());
    }
    let yaml = "suggestions:\n  soft_backoff_days: [2, 14]\n  hard_ttl_days: 30\n";
    std::fs::write(dir.path().join("config.yaml"), yaml).unwrap();
    let cfg = apb_core::config::GlobalConfig::load().unwrap();
    assert_eq!(cfg.suggestions.soft_backoff_days, Some(vec![2, 14]));
    assert_eq!(cfg.suggestions.hard_ttl_days, Some(30));
    assert!(cfg.suggestions.validate().is_ok());

    // A config with no section at all keeps the defaults (both None).
    std::fs::write(dir.path().join("config.yaml"), "port: 7321\n").unwrap();
    let cfg = apb_core::config::GlobalConfig::load().unwrap();
    assert_eq!(cfg.suggestions.soft_backoff_days, None);
    assert_eq!(cfg.suggestions.hard_ttl_days, None);

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[test]
fn suggestion_settings_reject_empty_arrays_and_zero_days() {
    use apb_core::config::SuggestionSettings;
    let empty = SuggestionSettings {
        soft_backoff_days: Some(Vec::new()),
        hard_ttl_days: None,
    };
    assert!(
        empty.validate().unwrap_err().contains("soft_backoff_days"),
        "an empty schedule must be a validation error"
    );
    let zero = SuggestionSettings {
        soft_backoff_days: Some(vec![1, 0, 7]),
        hard_ttl_days: None,
    };
    assert!(zero.validate().is_err(), "a zero-day step is not positive");
    let zero_ttl = SuggestionSettings {
        soft_backoff_days: None,
        hard_ttl_days: Some(0),
    };
    assert!(zero_ttl.validate().unwrap_err().contains("hard_ttl_days"));
    let ok = SuggestionSettings {
        soft_backoff_days: Some(vec![1, 7, 30, 90]),
        hard_ttl_days: Some(90),
    };
    assert!(ok.validate().is_ok());
}

#[test]
fn project_suggestion_settings_read_the_project_config() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".apb")).unwrap();
    // The project config carries unrelated keys too, which must not break the
    // partial read.
    std::fs::write(
        root.path().join(".apb/config.yaml"),
        "skills_dir: .agents/skills\nsuggestions:\n  hard_ttl_days: 7\n",
    )
    .unwrap();
    let s = apb_core::config::project_suggestion_settings(root.path()).unwrap();
    assert_eq!(s.hard_ttl_days, Some(7));
    assert_eq!(s.soft_backoff_days, None);

    // No project config at all is an empty settings block, not an error.
    let bare = tempfile::tempdir().unwrap();
    let s = apb_core::config::project_suggestion_settings(bare.path()).unwrap();
    assert_eq!(s.hard_ttl_days, None);
}

#[test]
fn server_section_loads_and_defaults() {
    let _lock = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", dir.path());
    }

    // A config with no server section keeps every default.
    std::fs::write(dir.path().join("config.yaml"), "port: 7321\n").unwrap();
    let cfg = GlobalConfig::load().unwrap();
    assert_eq!(cfg.server.bind, None);
    assert_eq!(cfg.server.public_base_url, None);
    assert!(cfg.server.trusted_proxies.is_empty());

    let yaml = "port: 7321\nserver:\n  bind: \"0.0.0.0\"\n  public_base_url: https://apb.example.com\n  trusted_proxies: [\"127.0.0.1\", \"10.0.0.7\"]\n";
    std::fs::write(dir.path().join("config.yaml"), yaml).unwrap();
    let cfg = GlobalConfig::load().unwrap();
    assert_eq!(cfg.server.bind.as_deref(), Some("0.0.0.0"));
    assert_eq!(
        cfg.server.public_base_url.as_deref(),
        Some("https://apb.example.com")
    );
    assert_eq!(cfg.server.trusted_proxies.len(), 2);
    assert!(cfg.server.public_scheme_is_https());

    // An unknown key inside the section is a hard error, like every other
    // section in this file.
    std::fs::write(dir.path().join("config.yaml"), "server:\n  bnid: 0.0.0.0\n").unwrap();
    let broken = GlobalConfig::load();

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
    assert!(
        broken.is_err(),
        "a typo in the server section must not be ignored"
    );
}

#[test]
fn bind_precedence_is_flag_then_config_then_loopback() {
    use apb_core::config::ServerConfig;
    use std::net::{IpAddr, Ipv4Addr};

    let empty = ServerConfig::default();
    assert_eq!(
        empty.resolve_bind(None).unwrap(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "no flag and no config means loopback"
    );
    assert_eq!(
        empty.resolve_bind(Some("0.0.0.0")).unwrap(),
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    );

    let configured = ServerConfig {
        bind: Some("10.0.0.5".to_string()),
        ..Default::default()
    };
    assert_eq!(
        configured.resolve_bind(None).unwrap(),
        "10.0.0.5".parse::<IpAddr>().unwrap()
    );
    assert_eq!(
        configured.resolve_bind(Some("127.0.0.1")).unwrap(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "the flag wins over the config"
    );

    let bad = ServerConfig {
        bind: Some("not-an-ip".to_string()),
        ..Default::default()
    };
    let err = bad.resolve_bind(None).unwrap_err();
    assert!(
        err.contains("not-an-ip"),
        "the error names the value: {err}"
    );
    assert!(!err.contains('!'), "no exclamation marks: {err}");
}

#[test]
fn trusted_proxies_parse_into_a_set() {
    use apb_core::config::ServerConfig;
    use std::net::IpAddr;

    let cfg = ServerConfig {
        trusted_proxies: vec!["127.0.0.1".to_string(), " 10.0.0.7 ".to_string()],
        ..Default::default()
    };
    let set = cfg.trusted_proxy_set().unwrap();
    assert!(set.contains(&"127.0.0.1".parse::<IpAddr>().unwrap()));
    assert!(set.contains(&"10.0.0.7".parse::<IpAddr>().unwrap()));

    let cidr = ServerConfig {
        trusted_proxies: vec!["10.0.0.0/8".to_string()],
        ..Default::default()
    };
    let err = cidr.trusted_proxy_set().unwrap_err();
    assert!(
        err.contains("10.0.0.0/8"),
        "CIDR is not supported in v1: {err}"
    );
}

#[test]
fn ingest_section_loads_and_defaults() {
    let _lock = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", dir.path());
    }

    // No ingest section: disabled, loopback, port 7322, no public base URL.
    std::fs::write(dir.path().join("config.yaml"), "port: 7321\n").unwrap();
    let cfg = GlobalConfig::load().unwrap();
    assert!(!cfg.ingest.enabled, "ingest is opt-in");
    assert_eq!(cfg.ingest.bind, None);
    assert_eq!(cfg.ingest.port, None);
    assert_eq!(cfg.ingest.public_base_url, None);

    let yaml = "ingest:\n  enabled: true\n  bind: \"127.0.0.1\"\n  port: 7400\n  public_base_url: https://hooks.example.com\n";
    std::fs::write(dir.path().join("config.yaml"), yaml).unwrap();
    let cfg = GlobalConfig::load().unwrap();
    assert!(cfg.ingest.enabled);
    assert_eq!(cfg.ingest.bind.as_deref(), Some("127.0.0.1"));
    assert_eq!(cfg.ingest.port, Some(7400));
    assert_eq!(
        cfg.ingest.public_base_url.as_deref(),
        Some("https://hooks.example.com")
    );

    // A typo inside the section is a hard error, like every other section.
    std::fs::write(dir.path().join("config.yaml"), "ingest:\n  enbaled: true\n").unwrap();
    let broken = GlobalConfig::load();

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
    assert!(
        broken.is_err(),
        "a typo in the ingest section must not be ignored"
    );
}

#[test]
fn ingest_bind_and_port_precedence() {
    use apb_core::config::{DEFAULT_INGEST_PORT, IngestConfig};
    use std::net::{IpAddr, Ipv4Addr};

    let empty = IngestConfig::default();
    assert_eq!(
        empty.resolve_bind(None).unwrap(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "the ingest listener sits behind a reverse proxy on the same host by default"
    );
    assert_eq!(empty.resolve_port(None), DEFAULT_INGEST_PORT);
    assert_eq!(DEFAULT_INGEST_PORT, 7322);

    let configured = IngestConfig {
        bind: Some("0.0.0.0".to_string()),
        port: Some(7400),
        ..Default::default()
    };
    assert_eq!(
        configured.resolve_bind(None).unwrap(),
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    );
    assert_eq!(configured.resolve_port(None), 7400);
    assert_eq!(
        configured.resolve_bind(Some("127.0.0.1")).unwrap(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "the flag wins over the config"
    );
    assert_eq!(configured.resolve_port(Some(9000)), 9000);

    let bad = IngestConfig {
        bind: Some("not-an-ip".to_string()),
        ..Default::default()
    };
    let err = bad.resolve_bind(None).unwrap_err();
    assert!(
        err.contains("not-an-ip"),
        "the error names the value: {err}"
    );
    assert!(!err.contains('!'), "no exclamation marks: {err}");
}

#[test]
fn callback_url_is_printable_only_with_a_public_base() {
    use apb_core::config::{CallbackGap, IngestConfig};

    let none = IngestConfig::default();
    assert_eq!(
        none.callback_url("whatsapp", "main"),
        Err(CallbackGap::NoPublicBase)
    );
    // A base that trims to nothing is the same gap, not a URL of slashes.
    let blank = IngestConfig {
        public_base_url: Some("   ".to_string()),
        ..Default::default()
    };
    assert_eq!(
        blank.callback_url("whatsapp", "main"),
        Err(CallbackGap::NoPublicBase)
    );

    let configured = IngestConfig {
        public_base_url: Some("https://hooks.example.com/".to_string()),
        ..Default::default()
    };
    assert_eq!(
        configured.callback_url("whatsapp", "main").as_deref(),
        Ok("https://hooks.example.com/hooks/whatsapp/main"),
        "a trailing slash on the base must not double up"
    );

    let no_slash = IngestConfig {
        public_base_url: Some("https://hooks.example.com".to_string()),
        ..Default::default()
    };
    assert_eq!(
        no_slash.callback_url("whatsapp", "main").as_deref(),
        Ok("https://hooks.example.com/hooks/whatsapp/main")
    );

    // Segments that could not be routed are refused rather than printed as a
    // URL nobody could register, and they are refused for their own reason:
    // the caller reports what to fix, and it is not the public base.
    assert_eq!(
        no_slash.callback_url("../evil", "main"),
        Err(CallbackGap::UnroutableSegment("../evil".to_string()))
    );
    assert_eq!(
        no_slash.callback_url("whatsapp", "Not An Account"),
        Err(CallbackGap::UnroutableSegment("Not An Account".to_string()))
    );
    // Including when the public base is missing too: the segment is named
    // first, because no base would make that account addressable.
    assert_eq!(
        none.callback_url("whatsapp", "Not An Account"),
        Err(CallbackGap::UnroutableSegment("Not An Account".to_string()))
    );
}

// The workdir-queue ceiling has three settings, and the difference between the
// last two decides whether a machine-driven run start survives a busy engine.
#[test]
fn the_workdir_queue_wait_defaults_on_and_zero_turns_it_off() {
    use apb_core::config::{DEFAULT_WORKDIR_QUEUE_WAIT_SECONDS, ServerConfig};
    use std::time::Duration;

    assert_eq!(
        ServerConfig::default().workdir_queue_wait(),
        Some(Duration::from_secs(DEFAULT_WORKDIR_QUEUE_WAIT_SECONDS)),
        "an operator who never touches the section gets the queue, not a refusal"
    );
    assert_eq!(
        ServerConfig {
            workdir_queue_wait_seconds: Some(30),
            ..Default::default()
        }
        .workdir_queue_wait(),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        ServerConfig {
            workdir_queue_wait_seconds: Some(0),
            ..Default::default()
        }
        .workdir_queue_wait(),
        None,
        "zero is the explicit opt-out back to the immediate refusal"
    );
}
