//! The isolation contract table: each host and node combination gives the
//! expected holds, limits and refusals, macOS included.

use std::path::PathBuf;

use apb_core::isolation::contract::{
    ALL_CODES, ALL_REASONS, Backend, BwrapFacts, Code, HostFacts, NodeFacts, Os, ReasonCode,
    evaluate, text,
};
use apb_core::isolation::{Level, Network};

fn linux(abi: u32) -> HostFacts {
    HostFacts {
        os: Os::Linux,
        bwrap: Some(BwrapFacts {
            path: PathBuf::from("/usr/bin/bwrap"),
            version: Some("0.11.1".into()),
            setuid: false,
            trial: Ok(()),
        }),
        landlock_abi: abi,
        disabled: false,
    }
}

fn node(level: Level, network: Network) -> NodeFacts {
    NodeFacts {
        level,
        network,
        agents_without_needs: Vec::new(),
        host_mode: false,
        publish_only: false,
    }
}

fn reasons(h: &HostFacts, n: &NodeFacts) -> Vec<ReasonCode> {
    evaluate(h, n).refusals.iter().map(|r| r.code).collect()
}

#[test]
fn none_is_untouched_everywhere() {
    for host in [
        linux(0),
        linux(7),
        HostFacts {
            os: Os::MacOs,
            bwrap: None,
            landlock_abi: 0,
            disabled: true,
        },
    ] {
        let c = evaluate(&host, &node(Level::None, Network::Open));
        assert_eq!(c.backend, Backend::None);
        assert!(c.holds.is_empty() && c.limits.is_empty() && c.allowed());
    }
}

#[test]
fn full_on_linux_with_bwrap_holds_the_table() {
    let c = evaluate(&linux(6), &node(Level::Full, Network::None));
    assert_eq!(c.backend, Backend::Bwrap);
    assert!(c.allowed() && c.scope);
    for h in [
        Code::UserNamespaces,
        Code::UserNoPrivileges,
        Code::UserNewSession,
        Code::FilesDeclaredView,
        Code::FilesGitControlReadOnly,
        Code::FilesApbMasked,
        Code::CredentialsEnvAllowlist,
        Code::CredentialsSocketsDropped,
        Code::CredentialsGitConfigReplaced,
        Code::NetworkNone,
        Code::NetworkScoped,
    ] {
        assert!(c.holds.contains(&h), "{h:?}");
    }
    for l in [
        Code::UserHostUid,
        Code::FilesSystemReadable,
        Code::CredentialsAgentLoginReadable,
        Code::KernelEscapes,
        Code::ScriptsOutside,
    ] {
        assert!(c.limits.contains(&l), "{l:?}");
    }
    assert!(!c.limits.contains(&Code::NetworkOpenAnyHost));
}

#[test]
fn full_with_the_open_network_needs_the_scope() {
    let ok = evaluate(&linux(6), &node(Level::Full, Network::Open));
    assert!(ok.allowed() && ok.scope);
    assert!(ok.limits.contains(&Code::NetworkOpenAnyHost));
    assert_eq!(
        reasons(&linux(5), &node(Level::Full, Network::Open)),
        [ReasonCode::ScopeUnavailable]
    );
    // Every obstacle is listed at once: no bwrap and no scope.
    let mut bare = linux(5);
    bare.bwrap = None;
    assert_eq!(
        reasons(&bare, &node(Level::Full, Network::Open)),
        [ReasonCode::BwrapMissing, ReasonCode::ScopeUnavailable]
    );
    // Without the scope, network none still holds (own namespace).
    let none = evaluate(&linux(0), &node(Level::Full, Network::None));
    assert!(none.allowed() && !none.scope);
    // best_effort runs and lists the open abstract sockets.
    let be = evaluate(&linux(5), &node(Level::BestEffort, Network::Open));
    assert_eq!(be.backend, Backend::Bwrap);
    assert!(be.limits.contains(&Code::NetworkAbstractSocketsOpen));
}

#[test]
fn full_refuses_what_the_machine_cannot_enforce() {
    let mac = HostFacts {
        os: Os::MacOs,
        bwrap: None,
        landlock_abi: 0,
        disabled: false,
    };
    assert_eq!(
        reasons(&mac, &node(Level::Full, Network::None)),
        [ReasonCode::OsUnsupported]
    );
    let mut missing = linux(6);
    missing.bwrap = None;
    assert_eq!(
        reasons(&missing, &node(Level::Full, Network::None)),
        [ReasonCode::BwrapMissing]
    );
    let mut setuid = linux(6);
    setuid.bwrap.as_mut().unwrap().setuid = true;
    assert_eq!(
        reasons(&setuid, &node(Level::Full, Network::None)),
        [ReasonCode::BwrapSetuid]
    );
    let mut denied = linux(6);
    denied.bwrap.as_mut().unwrap().trial = Err("setting up uid map: Permission denied".into());
    let r = evaluate(&denied, &node(Level::Full, Network::None));
    assert_eq!(r.refusals[0].code, ReasonCode::UsernsDenied);
    assert!(r.refusals[0].detail.contains("uid map"));
    let mut off = linux(6);
    off.disabled = true;
    assert_eq!(
        reasons(&off, &node(Level::Full, Network::None)),
        [ReasonCode::Disabled]
    );
    let mut unknown = node(Level::Full, Network::None);
    unknown.agents_without_needs = vec!["opencode".into()];
    assert_eq!(
        reasons(&linux(6), &unknown),
        [ReasonCode::AgentNeedsUnknown]
    );
    let mut host = node(Level::Full, Network::None);
    host.host_mode = true;
    assert_eq!(reasons(&linux(6), &host), [ReasonCode::HostMode]);
}

#[test]
fn best_effort_without_a_sandbox_lists_its_limits() {
    let mac = HostFacts {
        os: Os::MacOs,
        bwrap: None,
        landlock_abi: 0,
        disabled: false,
    };
    let c = evaluate(&mac, &node(Level::BestEffort, Network::None));
    assert!(c.allowed());
    assert_eq!(c.backend, Backend::None);
    assert_eq!(c.holds, [Code::CredentialsEnvAllowlist]);
    for l in [
        Code::NoSandbox,
        Code::FilesNotSeparated,
        Code::CredentialsHomeReadable,
    ] {
        assert!(c.limits.contains(&l), "{l:?}");
    }
    assert_eq!(c.fallbacks[0].code, ReasonCode::OsUnsupported);
    let mut unknown = node(Level::BestEffort, Network::Open);
    unknown.agents_without_needs = vec!["zcode".into()];
    let c = evaluate(&linux(6), &unknown);
    assert_eq!(c.backend, Backend::None);
    assert_eq!(c.fallbacks[0].code, ReasonCode::AgentNeedsUnknown);
}

#[test]
fn every_code_has_a_sentence() {
    for c in ALL_CODES {
        assert_eq!(text(c.id()), Some(c.text()));
        assert!(!c.text().contains('\u{2014}') && !c.text().contains('!'));
    }
    assert!(text("credentials.env_passed:GH_TOKEN").is_some());
    assert_eq!(text("nope"), None);
}

#[test]
fn every_reason_has_its_own_sentence() {
    let mut seen = std::collections::BTreeSet::new();
    let mut texts = std::collections::BTreeSet::new();
    for r in ALL_REASONS {
        let t = r.text();
        assert!(t.ends_with('.'), "{}", r.id());
        assert!(!t.contains('\u{2014}') && !t.contains('!'));
        assert!(seen.insert(r.id()), "duplicate id {}", r.id());
        assert!(texts.insert(t), "duplicate sentence for {}", r.id());
        // The wire id is what serde writes.
        assert_eq!(serde_json::to_value(r).unwrap(), r.id());
    }
}

#[test]
fn publish_only_turns_a_passed_credential_into_a_refusal() {
    use apb_core::isolation::contract::credentials_refusal;
    let plain = node(Level::Full, Network::None);
    let names = vec!["GH_TOKEN".to_string()];
    assert_eq!(credentials_refusal(&plain, &names), None);
    let strict = NodeFacts {
        publish_only: true,
        ..plain
    };
    assert_eq!(credentials_refusal(&strict, &[]), None);
    let why = credentials_refusal(&strict, &names).unwrap();
    assert_eq!(why.code, ReasonCode::CredentialsPassed);
    assert_eq!(why.code.id(), "credentials_passed");
    assert!(why.to_string().contains("GH_TOKEN"), "{why}");
}
