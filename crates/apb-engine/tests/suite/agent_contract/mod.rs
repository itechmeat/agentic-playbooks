//! One scenario set for every built-in agent with a launch form: success,
//! error, quota, session, usage, verdict and a custom invocation form. Each
//! scenario runs the real headless adapter with the agent's built-in form on
//! a stub that replays the agent's fixture (`tests/fixtures/agents/<id>.yaml`):
//! captured CLI output where the repository has a capture, synthetic output
//! marked as such everywhere else. Adding an agent means adding its fixture
//! and one line to the `contract!` list below.

mod scenarios;
mod stub;

/// A named test per agent and scenario: `agent_contract::<agent>::<scenario>`.
macro_rules! contract {
    ($($agent:ident),* $(,)?) => {
        $(
            mod $agent {
                #[test]
                fn success() {
                    super::scenarios::success(stringify!($agent));
                }
                #[test]
                fn error() {
                    super::scenarios::error(stringify!($agent));
                }
                #[test]
                fn quota() {
                    super::scenarios::quota(stringify!($agent));
                }
                #[test]
                fn session() {
                    super::scenarios::session(stringify!($agent));
                }
                #[test]
                fn usage() {
                    super::scenarios::usage(stringify!($agent));
                }
                #[test]
                fn verdict() {
                    super::scenarios::verdict(stringify!($agent));
                }
                #[test]
                fn custom_form() {
                    super::scenarios::custom_form(stringify!($agent));
                }
            }
        )*

        /// The agents the macro covers.
        const COVERED: &[&str] = &[$(stringify!($agent)),*];
    };
}

contract!(
    claude, codex, agy, opencode, hermes, grok, cursor, qoder, zcode, dsh
);

/// Every built-in agent with a launch form has a fixture and a test module,
/// and no fixture names an agent without one.
#[test]
fn every_agent_with_a_form_has_a_fixture() {
    let mut with_form: Vec<String> = apb_core::agents::all()
        .iter()
        .filter(|a| a.forms().is_some())
        .map(|a| a.descriptor().id.to_string())
        .collect();
    with_form.sort();
    assert_eq!(stub::fixture_ids(), with_form);
    let mut covered: Vec<String> = COVERED.iter().map(|s| s.to_string()).collect();
    covered.sort();
    assert_eq!(covered, with_form);
}

/// A captured fixture names the CLI version its captures come from, which is
/// the version the agent module says it was checked against; a synthetic one
/// names none. Only a captured fixture can mark single scenarios synthetic.
#[test]
fn captured_fixtures_match_the_verified_version() {
    for id in stub::fixture_ids() {
        let fx = stub::load(&id);
        let verified = apb_core::agents::lookup(&id)
            .and_then(|a| a.descriptor().verified)
            .map(|v| v.version.to_string());
        match fx.provenance {
            stub::Provenance::Captured => {
                assert!(
                    verified.is_some(),
                    "{id}: a captured fixture needs a verified version"
                );
                assert_eq!(fx.checked_against, verified, "{id}");
            }
            stub::Provenance::Synthetic => assert!(fx.checked_against.is_none(), "{id}"),
        }
        for (name, s) in &fx.scenarios {
            if let Some(p) = s.provenance {
                assert_eq!(
                    (fx.provenance, p),
                    (stub::Provenance::Captured, stub::Provenance::Synthetic),
                    "{id}: scenario {name}"
                );
            }
        }
    }
}

/// An agent whose run-scoped file is an argument gets it in front of the
/// built-in form, on the one spawn path (decision record 0013): dsh's model
/// patch leads the launcher's own flags.
#[test]
fn the_run_scoped_arguments_lead_the_form() {
    let fx = stub::load("dsh");
    let s = fx.scenario("success");
    let stub = stub::Stub::for_scenario(s);
    stub.run("dsh", apb_engine::invocation::builtin("dsh").unwrap())
        .unwrap();
    let argv = stub.argv();
    assert_eq!(argv[0], "--patch", "{argv:?}");
    assert_eq!(&argv[2..4], ["--profile", "headless"], "{argv:?}");
    let patch = std::fs::read_to_string(&argv[1]).unwrap();
    assert!(patch.contains("model: \"m-1\""), "{patch}");
    assert!(patch.contains("provider: \"deepseek-official\""), "{patch}");
}

/// A failed attempt's session is recovered only from a verified record the
/// agent prints before any work (`Capabilities::printed_session`), through
/// its own reader, and only an id that cannot read as an option.
#[test]
fn a_failed_attempts_session_comes_only_from_a_verified_opening_record() {
    use apb_engine::adapter::capture_failed_session;
    let dsh = "{\"type\":\"session\",\"sessionId\":\"session-ok\",\"cwd\":\"/w\"}\n";
    assert_eq!(
        capture_failed_session("dsh", dsh).as_deref(),
        Some("session-ok")
    );
    let dashed = "{\"type\":\"session\",\"sessionId\":\"--last\",\"cwd\":\"/w\"}\n";
    assert_eq!(capture_failed_session("dsh", dashed), None);
    // An answer of a plain-text agent that happens to hold a session field.
    let answer = "{\"session_id\":\"existing-other-session\"}\n";
    assert_eq!(capture_failed_session("grok", answer), None);
    assert_eq!(capture_failed_session("hermes", answer), None);
    // codex declares no verified opening record, and a dash-led id is
    // never a session on any path, its stderr header included.
    assert_eq!(
        capture_failed_session("codex", "session id: --last\n"),
        None
    );
    assert_eq!(
        apb_engine::adapter::capture_session_header("codex", "session id: --last\n"),
        None
    );
}

/// The agents that declare a verified opening session record are exactly
/// the ones whose reader finds it in their success fixture.
#[test]
fn the_printed_session_capability_matches_the_reader() {
    for a in apb_core::agents::all() {
        let id = a.descriptor().id;
        if a.forms().is_none() {
            continue;
        }
        let fx = stub::load(id);
        let found = a
            .output()
            .opening_session(&fx.scenario("success").stdout())
            .is_some();
        assert_eq!(a.descriptor().capabilities.printed_session, found, "{id}");
    }
}
