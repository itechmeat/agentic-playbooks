//! The free local check of an agent CLI (`apb_core::agents::check`): the
//! installed version against the verified one, the built-in form's flags in
//! the help text, and the custom-form note.

use apb_core::agents::check::{CheckInput, Level, check, form_flags, missing_flags, version_line};
use apb_core::agents::{Agent, Verified, lookup};

const VERIFIED: Verified = Verified {
    version: "2.1.283",
    on: "2026-09-27",
};

fn agent(id: &str) -> &'static dyn Agent {
    lookup(id).expect("a built-in agent")
}

#[test]
fn the_version_is_compared_with_the_verified_one() {
    let same = version_line("claude", Some(VERIFIED), Some("2.1.283 (Claude Code)"));
    assert_eq!(same.level, Level::Ok, "{}", same.detail);
    let newer = version_line("claude", Some(VERIFIED), Some("2.1.300 (Claude Code)"));
    assert_eq!(newer.level, Level::Warn);
    assert!(
        newer.detail.contains("newer than the verified 2.1.283"),
        "{}",
        newer.detail
    );
    assert!(
        newer.detail.contains("apb doctor --probe-agent claude"),
        "{}",
        newer.detail
    );
    let older = version_line("claude", Some(VERIFIED), Some("2.0.9"));
    assert_eq!(older.level, Level::Warn);
    assert!(older.detail.contains("older"), "{}", older.detail);
}

/// A missing segment counts as 0, so a shorter or longer spelling of the
/// same version is that version.
#[test]
fn a_missing_version_segment_counts_as_zero() {
    let verified = Verified {
        version: "2.1.0",
        on: "2026-09-27",
    };
    let short = version_line("claude", Some(verified), Some("2.1 (Claude Code)"));
    assert_eq!(short.level, Level::Ok, "{}", short.detail);
    let long = version_line("claude", Some(verified), Some("2.1.0.0"));
    assert_eq!(long.level, Level::Ok, "{}", long.detail);
    let build = version_line("claude", Some(verified), Some("2.1.0.1"));
    assert!(build.detail.contains("newer"), "{}", build.detail);
}

#[test]
fn nothing_recorded_or_nothing_readable_is_an_info_line() {
    for line in [
        version_line("pi", None, Some("1.0.0")),
        version_line("claude", Some(VERIFIED), None),
        version_line("claude", Some(VERIFIED), Some("nightly build")),
    ] {
        assert_eq!(line.level, Level::Info, "{}", line.detail);
    }
}

/// Every flag of the launch, resume, soul, autonomy and fresh-session forms,
/// once each, and no value or separator.
#[test]
fn the_flags_come_from_the_built_in_forms() {
    let flags = form_flags(&agent("claude").forms().unwrap());
    for f in [
        "-p",
        "--output-format",
        "--model",
        "--append-system-prompt",
        "--permission-mode",
        "--resume",
        "--session-id",
    ] {
        assert_eq!(
            flags.iter().filter(|x| *x == f).count(),
            1,
            "{f} in {flags:?}"
        );
    }
    assert!(
        !flags.iter().any(|f| f == "--" || !f.starts_with('-')),
        "{flags:?}"
    );
    // A profile's permission mode adds its own flag, once.
    let zcode = form_flags(&agent("zcode").forms().unwrap());
    assert_eq!(zcode.iter().filter(|f| *f == "--mode").count(), 1);
}

#[test]
fn a_flag_counts_only_as_a_whole_word() {
    let flags = vec!["-p".to_string(), "--model".to_string()];
    assert_eq!(
        missing_flags(&flags, "  --permission-mode <mode>\n  --models"),
        flags
    );
    assert!(missing_flags(&flags, "  -p, --print\n  --model <id>").is_empty());
}

#[test]
fn a_help_without_a_flag_of_the_form_warns() {
    let lines = check(
        agent("claude"),
        &CheckInput {
            version: Some("2.1.283"),
            help: Some(Ok("Usage: claude [options]\n  -p, --print\n")),
            custom_form: false,
        },
    );
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1].level, Level::Warn);
    assert!(
        lines[1].detail.contains("--session-id"),
        "{}",
        lines[1].detail
    );
    assert!(
        lines[1]
            .detail
            .contains("the built-in form may have changed")
    );

    let unreadable = check(
        agent("claude"),
        &CheckInput {
            version: Some("2.1.283"),
            help: Some(Err("probe timed out")),
            custom_form: false,
        },
    );
    assert_eq!(unreadable[1].level, Level::Warn);
}

#[test]
fn a_custom_form_is_a_note_and_its_help_is_not_checked() {
    let lines = check(
        agent("codex"),
        &CheckInput {
            version: Some("codex-cli 0.157.0"),
            help: Some(Ok("")),
            custom_form: true,
        },
    );
    assert_eq!(lines[0].level, Level::Ok, "{}", lines[0].detail);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1].level, Level::Ok);
    assert_eq!(
        lines[1].detail,
        "custom invocation; output read as plain text when it is not codex's machine format"
    );
}
