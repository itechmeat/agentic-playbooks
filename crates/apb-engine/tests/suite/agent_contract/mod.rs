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
    claude, codex, agy, opencode, hermes, grok, cursor, qoder, zcode
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
