//! The scenarios every built-in agent with a launch form passes, each run on
//! the real headless adapter with the agent's built-in form (see `mod.rs`).

use apb_core::agents::{Agent, Conformance};
use apb_core::config::{GlobalConfig, InvocationDef};
use apb_engine::adapter::ErrorClass;
use apb_engine::failure_class::{FailureKind, classify, retry_after, session_missing};
use apb_engine::invocation::{FreshSession, builtin, fresh_session, resume_spec, spec_for};
use apb_engine::state::NodeStatus;

use super::stub::{self, PROMPT, Stub};

fn module(agent: &str) -> &'static dyn Agent {
    apb_core::agents::lookup(agent).unwrap_or_else(|| panic!("{agent} is not a built-in agent"))
}

fn form(agent: &str) -> InvocationDef {
    builtin(agent).unwrap_or_else(|| panic!("{agent} has no built-in form"))
}

/// The prompt reaches the CLI as text: after an end-of-options `--` and last,
/// or as an option's value that does not start with `-`.
fn assert_prompt_is_not_an_option(agent: &str, argv: &[String]) {
    let at = argv
        .iter()
        .position(|a| a.contains(PROMPT))
        .unwrap_or_else(|| panic!("{agent}: no prompt in {argv:?}"));
    if at > 0 && argv[at - 1] == "--" {
        assert_eq!(
            at,
            argv.len() - 1,
            "{agent}: the prompt must be last after `--`: {argv:?}"
        );
    } else {
        assert!(
            !argv[at].starts_with('-'),
            "{agent}: an option value must not start with `-`: {argv:?}"
        );
    }
}

/// The reply is unwrapped from the machine output, the attempt succeeds, the
/// closing record is found where the format has one, and the prompt is never
/// read as an option.
pub fn success(agent: &str) {
    let fx = stub::load(agent);
    let s = fx.scenario("success");
    let stub = Stub::for_scenario(s);
    let report = stub
        .run(agent, form(agent))
        .unwrap_or_else(|e| panic!("{agent}: {e:?}"));
    assert_eq!(report.status, NodeStatus::Succeeded, "{agent}");
    assert_eq!(Some(&report.output), s.expect.output.as_ref(), "{agent}");
    let promises = module(agent).output().promises();
    assert_eq!(
        apb_core::agent_output::has_final_record(agent, &s.stdout()),
        s.expect.final_record,
        "{agent}: closing record"
    );
    assert_eq!(
        s.expect.final_record == Some(true),
        promises.final_record,
        "{agent}: the fixture's closing record is the one the module promises"
    );
    let conformance = apb_core::agents::conformance(module(agent), true, &report.raw);
    let want = if promises.reply {
        Conformance::Machine
    } else {
        Conformance::PlainByDesign
    };
    assert_eq!(conformance, want, "{agent}");
    assert_prompt_is_not_an_option(agent, &stub.argv());
}

/// A non-zero exit is a process failure carrying the CLI's own message, and
/// marks a model turn only when the output shows one.
pub fn error(agent: &str) {
    let fx = stub::load(agent);
    let s = fx.scenario("error");
    let err = Stub::for_scenario(s)
        .run(agent, form(agent))
        .expect_err("a non-zero exit fails the attempt");
    assert_eq!(err.class, ErrorClass::ProcessExit, "{agent}");
    let want = s.expect.message.as_deref().expect("expect.message");
    assert!(err.message.contains(want), "{agent}: {}", err.message);
    assert_eq!(
        Some(err.model_turn),
        s.expect.model_turn,
        "{agent}: model turn"
    );
}

/// A quota stop classifies as a quota (or, for a plan's spending cap, a
/// budget) failure, and its `Retry-After` is read where the CLI prints one.
pub fn quota(agent: &str) {
    let fx = stub::load(agent);
    let s = fx.scenario("quota");
    let err = Stub::for_scenario(s)
        .run(agent, form(agent))
        .expect_err("a quota stop fails the attempt");
    let kind = match s.expect.kind.as_deref() {
        Some("quota") => FailureKind::Quota,
        Some("budget") => FailureKind::Budget,
        other => panic!("{agent}: unknown expect.kind {other:?}"),
    };
    assert_eq!(classify(&err.message), kind, "{agent}: {}", err.message);
    assert_eq!(
        retry_after(&err.message, 0).map(|d| d.as_secs()),
        s.expect.retry_after_secs,
        "{agent}: retry-after"
    );
}

/// The session is captured from the output (or stderr header) where the
/// agent prints one, goes back into the resume form in front of `--`, and
/// the CLI's "missing session" text is recognised. A plain-text agent
/// captures none, so a resume round downgrades.
pub fn session(agent: &str) {
    let fx = stub::load(agent);
    let s = fx.scenario("session");
    let report = Stub::for_scenario(s)
        .run(agent, form(agent))
        .unwrap_or_else(|e| panic!("{agent}: {e:?}"));
    assert_eq!(report.session, s.expect.session, "{agent}: session");
    assert_eq!(
        s.expect.session.is_some(),
        module(agent).output().promises().session,
        "{agent}: the fixture's session is the one the module promises"
    );
    if let Some(id) = &report.session {
        let resumed = resume_spec(&form(agent), agent, id)
            .unwrap_or_else(|| panic!("{agent}: a captured session needs a resume form"));
        let at = resumed
            .argv
            .iter()
            .position(|a| a == id)
            .expect("session in argv");
        if let Some(end) = resumed.argv.iter().position(|a| a == "--") {
            assert!(
                at < end,
                "{agent}: the session must precede `--`: {:?}",
                resumed.argv
            );
        }
    }
    let fresh = match fresh_session(agent, "run-node-1") {
        FreshSession::Assigned { .. } => "assigned",
        FreshSession::Titled { .. } => "titled",
        FreshSession::Printed => "printed",
    };
    assert_eq!(
        Some(fresh),
        s.expect.fresh.as_deref(),
        "{agent}: fresh session"
    );
    assert_eq!(
        Some(apb_core::detect::continues_sessions(agent)),
        s.expect.continues,
        "{agent}: continues sessions"
    );
    if let Some(text) = &s.expect.missing {
        assert!(session_missing(text), "{agent}: {text}");
    }
}

/// The exact usage the CLI printed, or none for a plain-text agent.
pub fn usage(agent: &str) {
    let fx = stub::load(agent);
    let s = fx.scenario("usage");
    let report = Stub::for_scenario(s)
        .run(agent, form(agent))
        .unwrap_or_else(|e| panic!("{agent}: {e:?}"));
    assert_eq!(report.usage, s.expect.usage, "{agent}: usage");
    assert_eq!(
        s.expect.usage.is_some(),
        module(agent).output().promises().usage,
        "{agent}: the fixture's usage is the one the module promises"
    );
}

/// A failure in the report block, or the CLI's own error flag, fails the
/// attempt.
pub fn verdict(agent: &str) {
    let fx = stub::load(agent);
    let s = fx.scenario("verdict");
    let report = Stub::for_scenario(s)
        .run(agent, form(agent))
        .unwrap_or_else(|e| panic!("{agent}: {e:?}"));
    assert_eq!(s.expect.status.as_deref(), Some("failed"), "{agent}");
    assert_eq!(report.status, NodeStatus::Failed, "{agent}");
}

/// Plain text through a custom invocation form is the reply as it is, with no
/// usage, and allowed. The same plain text through the built-in form is read
/// just as leniently, but for an agent with a machine format it means the
/// output format changed. The form and whether it is the built-in one come
/// from the config, through the lookups a run and the probe make.
pub fn custom_form(agent: &str) {
    const PLAIN: &str = "a plain reply\n";
    let custom: GlobalConfig = yaml_serde::from_str(&format!(
        "agents:\n  {agent}:\n    invocation:\n      argv: [\"{{prompt}}\"]\n"
    ))
    .unwrap();
    let built_in = if module(agent).output().promises().reply {
        Conformance::Drifted
    } else {
        Conformance::PlainByDesign
    };
    for (global, want) in [
        (custom, Conformance::PlainByDesign),
        (GlobalConfig::default(), built_in),
    ] {
        let spec = spec_for(agent, &global).unwrap_or_else(|e| panic!("{agent}: {e}"));
        let is_builtin = !global.has_custom_form(agent);
        let report = Stub::new(PLAIN, "", 0)
            .run(agent, spec)
            .unwrap_or_else(|e| panic!("{agent}: {e:?}"));
        assert_eq!(report.status, NodeStatus::Succeeded, "{agent}");
        assert_eq!(report.output, PLAIN.trim(), "{agent}");
        assert_eq!(report.usage, None, "{agent}");
        assert_eq!(
            apb_core::agents::conformance(module(agent), is_builtin, &report.raw),
            want,
            "{agent}: builtin {is_builtin}"
        );
    }
}
